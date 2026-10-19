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

/// Redacts secrets and bounds sizes. Redaction runs first so a secret can
/// never survive because truncation cut its pattern in half.
/// Core requires an id on every tool message; hosts that omit one get a
/// synthesized id.
pub fn prepare(msgs: Vec<Msg>) -> Vec<Msg> {
    msgs.into_iter()
        .enumerate()
        .map(|(i, mut m)| {
            let max = if m.is_text() {
                MAX_TEXT_CHARS
            } else {
                MAX_TOOL_CHARS
            };
            m.content = truncate(&redact(&m.content), max);
            if !m.is_text() && m.tool_call_id.as_deref().is_none_or(str::is_empty) {
                m.tool_call_id = Some(format!("auto-{i}"));
            }
            m
        })
        .collect()
}

/// The requests for one turn, without the team/agent scope.
pub fn requests(session_id: &str, msgs: &[Msg]) -> Vec<(&'static str, Value)> {
    let text: Vec<Value> = msgs
        .iter()
        .filter(|m| m.is_text())
        .map(|m| json!({"role": m.role, "content": m.content}))
        .collect();
    let mut out = Vec::new();
    if !text.is_empty() {
        out.push((
            "conversation/add",
            json!({"session_id": session_id, "messages": text}),
        ));
    }
    if msgs.iter().any(|m| !m.is_text()) {
        out.push((
            "skill/conversation/add",
            json!({"session_id": session_id, "messages": msgs}),
        ));
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
}
