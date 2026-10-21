//! Capture pipeline: redact → trim → deliver (or queue) one finished turn.
//!
//! Each turn becomes up to two requests: the text to `conversation/add`
//! (L0 → L1–L3) and the full turn, tool calls included, to
//! `skill/conversation/add` so tool-heavy work can be distilled into skills.

use serde_json::{json, Value};

use crate::api::{Api, ApiError};
use crate::queue::Queue;
use crate::redact::redact;
use crate::transcript::Msg;

/// Tool output is mostly noise for memory; keep the head of it.
pub const MAX_TOOL_CHARS: usize = 8_000;
pub const MAX_TEXT_CHARS: usize = 32_000;
/// Core's limits (`memory_domain::memory::MAX_MESSAGE_CHARS`, and the
/// message counts of `conversation/add` and `skill/conversation/add`). A
/// request over any of them is rejected whole, so a turn would be lost.
pub const CORE_MESSAGE_CHARS: usize = 8_192;
pub const CORE_TEXT_MESSAGES: usize = 100;
pub const CORE_SKILL_MESSAGES: usize = 500;

fn truncate(text: &str, max: usize) -> String {
    match text.char_indices().nth(max) {
        None => text.to_owned(),
        Some((cut, _)) => format!(
            "{}\n…[truncated {} chars]",
            &text[..cut],
            text[cut..].chars().count()
        ),
    }
}

/// Pieces of at most `max` characters, cut after a line break when one falls
/// in the second half of a piece.
fn split(text: &str, max: usize) -> Vec<String> {
    let mut pieces = Vec::new();
    let mut rest = text;
    while let Some((cut, _)) = rest.char_indices().nth(max) {
        let piece = &rest[..cut];
        let cut = match piece.rfind('\n') {
            Some(nl) if piece[..nl].chars().count() >= max / 2 => nl + 1,
            _ => cut,
        };
        pieces.push(rest[..cut].to_owned());
        rest = &rest[cut..];
    }
    pieces.push(rest.to_owned());
    pieces
}

/// Redacts secrets and bounds sizes. Redaction runs first so a secret can
/// never survive because truncation cut its pattern in half.
/// Core requires an id on every tool message; hosts that omit one get a
/// synthesized id. Text longer than Core accepts per message is sent as
/// consecutive messages of the same role.
pub fn prepare(msgs: Vec<Msg>) -> Vec<Msg> {
    msgs.into_iter()
        .enumerate()
        .flat_map(|(i, mut m)| {
            if !m.is_text() {
                m.content = truncate(&redact(&m.content), MAX_TOOL_CHARS);
                if m.tool_call_id.as_deref().is_none_or(str::is_empty) {
                    m.tool_call_id = Some(format!("auto-{i}"));
                }
                return vec![m];
            }
            let text = truncate(&redact(&m.content), MAX_TEXT_CHARS);
            split(&text, CORE_MESSAGE_CHARS)
                .into_iter()
                .map(|content| Msg {
                    content,
                    ..m.clone()
                })
                .collect()
        })
        .collect()
}

/// The requests for one turn, without the team/agent scope.
pub fn requests(session_id: &str, msgs: &[Msg]) -> Vec<(&'static str, Value)> {
    let text: Vec<Value> = msgs
        .iter()
        .filter(|m| m.is_text())
        .map(|m| match &m.timestamp {
            Some(at) => json!({"role": m.role, "content": m.content, "timestamp": at}),
            None => json!({"role": m.role, "content": m.content}),
        })
        .collect();
    let mut out = Vec::new();
    for batch in text.chunks(CORE_TEXT_MESSAGES) {
        out.push((
            "conversation/add",
            json!({"session_id": session_id, "messages": batch}),
        ));
    }
    if msgs.iter().any(|m| !m.is_text()) {
        for batch in msgs.chunks(CORE_SKILL_MESSAGES) {
            out.push((
                "skill/conversation/add",
                json!({"session_id": session_id, "messages": batch}),
            ));
        }
    }
    out
}

#[derive(Debug, Default)]
pub struct CaptureReport {
    pub sent: usize,
    pub queued: usize,
    pub error: Option<String>,
}

/// Flushes older queued jobs first (keeps order), then sends this turn;
/// anything undeliverable is queued.
pub fn capture(api: &Api, queue: &Queue, session_id: &str, msgs: Vec<Msg>) -> CaptureReport {
    let mut report = CaptureReport::default();
    let msgs = prepare(msgs);
    let mut blocked = queue.flush(api).blocked;
    for (path, body) in requests(session_id, &msgs) {
        let body = api.scoped(body);
        if blocked.is_none() {
            match api.post(path, &body) {
                Ok(_) => {
                    report.sent += 1;
                    continue;
                }
                Err(ApiError::Rejected(why)) => {
                    report.error = Some(why);
                    continue;
                }
                Err(ApiError::Retryable(why)) => blocked = Some(why),
            }
        }
        match queue.push(path, &body) {
            Ok(()) => report.queued += 1,
            Err(e) => report.error = Some(e),
        }
    }
    if report.error.is_none() {
        report.error = blocked;
    }
    report
}

#[cfg(test)]
mod tests {
    use super::*;

    fn msg(role: &'static str, content: &str) -> Msg {
        Msg {
            role,
            content: content.into(),
            tool_name: None,
            tool_call_id: None,
            timestamp: None,
            interjected: false,
        }
    }

    #[test]
    fn redacts_then_truncates() {
        let long = format!("token=abcdefgh123 {}", "x".repeat(MAX_TOOL_CHARS + 10));
        let out = prepare(vec![msg("tool_result", &long)]);
        assert!(out[0]
            .content
            .starts_with("token=[REDACTED:secret-assignment] "));
        let redacted_len = "token=[REDACTED:secret-assignment] ".len() + MAX_TOOL_CHARS + 10;
        let dropped = redacted_len - MAX_TOOL_CHARS;
        assert!(out[0]
            .content
            .ends_with(&format!("[truncated {dropped} chars]")));
    }

    #[test]
    fn long_text_is_split_to_fit_core() {
        let long = format!("{}\n{}", "a".repeat(6_000), "b".repeat(6_000));
        let out = prepare(vec![msg("assistant", &long)]);
        assert_eq!(out.len(), 2, "cut after the line break");
        assert_eq!(out[0].content, format!("{}\n", "a".repeat(6_000)));
        assert_eq!(out[1].content, "b".repeat(6_000));

        // No line break: plain cuts, and every piece within the limit.
        let out = prepare(vec![msg("user", &"字".repeat(20_000))]);
        assert_eq!(out.len(), 3);
        assert!(out.iter().all(|m| m.role == "user"));
        assert!(out
            .iter()
            .all(|m| m.content.chars().count() <= CORE_MESSAGE_CHARS));
        assert_eq!(
            out.iter().map(|m| m.content.as_str()).collect::<String>(),
            "字".repeat(20_000)
        );
    }

    #[test]
    fn requests_stay_within_core_message_counts() {
        let mut msgs = Vec::new();
        for _ in 0..300 {
            msgs.push(msg("user", "q"));
            msgs.push(msg("tool_call", "{}"));
        }
        let reqs = requests("s", &msgs);
        let paths: Vec<_> = reqs.iter().map(|r| r.0).collect();
        assert_eq!(
            paths,
            [
                "conversation/add",
                "conversation/add",
                "conversation/add",
                "skill/conversation/add",
                "skill/conversation/add",
            ]
        );
        assert_eq!(reqs[2].1["messages"].as_array().unwrap().len(), 100);
        assert_eq!(reqs[4].1["messages"].as_array().unwrap().len(), 100);
    }

    #[test]
    fn text_goes_to_l0_and_tool_turns_also_to_skills() {
        let text_only = requests("s", &[msg("user", "hi"), msg("assistant", "hello")]);
        assert_eq!(text_only.len(), 1);
        assert_eq!(text_only[0].0, "conversation/add");

        let with_tools = requests(
            "s",
            &[
                msg("user", "ls"),
                msg("tool_call", "{}"),
                msg("tool_result", "a"),
                msg("assistant", "done"),
            ],
        );
        assert_eq!(
            with_tools.iter().map(|r| r.0).collect::<Vec<_>>(),
            ["conversation/add", "skill/conversation/add"]
        );
        assert_eq!(with_tools[0].1["messages"].as_array().unwrap().len(), 2);
        assert_eq!(with_tools[1].1["messages"].as_array().unwrap().len(), 4);
    }

    #[test]
    fn text_carries_its_timestamp_when_known() {
        let mut dated = msg("user", "hi");
        dated.timestamp = Some("2026-10-12T05:00:00Z".into());
        let reqs = requests("s", &[dated, msg("assistant", "hello")]);
        let messages = reqs[0].1["messages"].as_array().unwrap();
        assert_eq!(messages[0]["timestamp"], "2026-10-12T05:00:00Z");
        assert!(messages[1].get("timestamp").is_none());
    }
}
