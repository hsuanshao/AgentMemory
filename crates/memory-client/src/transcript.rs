//! Host transcript formats → one message model.
//!
//! The model mirrors Core's skill buffer: `user` / `assistant` text plus
//! `tool_call` / `tool_result` entries. Text goes to L0; everything goes to
//! the skill buffer so tool-heavy sessions can become skills.

use serde::Serialize;
use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Msg {
    pub role: &'static str,
    pub content: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
}

impl Msg {
    fn text(role: &'static str, content: String) -> Self {
        Self {
            role,
            content,
            tool_name: None,
            tool_call_id: None,
        }
    }

    fn tool(role: &'static str, content: String, name: Option<String>, id: Option<String>) -> Self {
        Self {
            role,
            content,
            tool_name: name,
            tool_call_id: id,
        }
    }

    pub fn is_text(&self) -> bool {
        matches!(self.role, "user" | "assistant")
    }
}

/// Appends, merging consecutive text of the same role and dropping blanks.
fn push(out: &mut Vec<Msg>, msg: Msg) {
    if msg.content.trim().is_empty() && msg.is_text() {
        return;
    }
    if let Some(last) = out.last_mut() {
        if msg.is_text() && last.role == msg.role {
            last.content.push_str("\n\n");
            last.content.push_str(&msg.content);
            return;
        }
    }
    out.push(msg);
}

fn str_of<'a>(v: &'a Value, key: &str) -> Option<&'a str> {
    v.get(key).and_then(Value::as_str)
}

/// Removes host-injected `<system-reminder>` blocks from user text.
fn strip_reminders(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find("<system-reminder>") {
        out.push_str(&rest[..start]);
        match rest[start..].find("</system-reminder>") {
            Some(end) => rest = &rest[start + end + "</system-reminder>".len()..],
            None => {
                rest = "";
            }
        }
    }
    out.push_str(rest);
    out.trim().to_owned()
}

/// Text of a tool result: a string or an array of `{type:"text", text}`.
fn block_text(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Array(items) => items
            .iter()
            .filter_map(|b| str_of(b, "text").or_else(|| b.as_str()))
            .collect::<Vec<_>>()
            .join("\n"),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

/// Complete (newline-terminated) lines of `text`; a trailing partial line is
/// left for the next read.
pub fn complete_lines(text: &str) -> Vec<&str> {
    let mut lines: Vec<&str> = text.split('\n').collect();
    lines.pop();
    lines
}

// ── Claude Code (~/.claude/projects/<cwd>/<session>.jsonl) ──────────────

pub fn claude_code(lines: &[&str]) -> Vec<Msg> {
    let mut out = Vec::new();
    for line in lines {
        let Ok(entry) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let flag = |k: &str| entry.get(k).and_then(Value::as_bool) == Some(true);
        if flag("isSidechain") || flag("isMeta") || flag("isCompactSummary") {
            continue;
        }
        let kind = str_of(&entry, "type").unwrap_or_default();
        let content = entry.get("message").and_then(|m| m.get("content"));
        match (kind, content) {
            ("user", Some(Value::String(s))) => {
                push(&mut out, Msg::text("user", strip_reminders(s)))
            }
            ("user", Some(Value::Array(blocks))) => {
                for b in blocks {
                    match str_of(b, "type") {
                        Some("text") => push(
                            &mut out,
                            Msg::text(
                                "user",
                                strip_reminders(str_of(b, "text").unwrap_or_default()),
                            ),
                        ),
                        Some("tool_result") => push(
                            &mut out,
                            Msg::tool(
                                "tool_result",
                                block_text(b.get("content").unwrap_or(&Value::Null)),
                                None,
                                str_of(b, "tool_use_id").map(str::to_owned),
                            ),
                        ),
                        _ => {}
                    }
                }
            }
            ("assistant", Some(Value::Array(blocks))) => {
                for b in blocks {
                    match str_of(b, "type") {
                        Some("text") => push(
                            &mut out,
                            Msg::text(
                                "assistant",
                                str_of(b, "text").unwrap_or_default().to_owned(),
                            ),
                        ),
                        Some("tool_use") => push(
                            &mut out,
                            Msg::tool(
                                "tool_call",
                                b.get("input").map(Value::to_string).unwrap_or_default(),
                                str_of(b, "name").map(str::to_owned),
                                str_of(b, "id").map(str::to_owned),
                            ),
                        ),
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }
    out
}

// ── Antigravity (<app>/brain/<conversation>/.system_generated/logs/transcript.jsonl) ──

/// The text inside `<USER_REQUEST>`, or the whole input when absent.
fn user_request(content: &str) -> String {
    match (
        content.find("<USER_REQUEST>"),
        content.find("</USER_REQUEST>"),
    ) {
        (Some(s), Some(e)) if e > s => content[s + "<USER_REQUEST>".len()..e].trim().to_owned(),
        _ => content.trim().to_owned(),
    }
}

/// Steps after `after_step`; returns the messages and the last step index seen.
pub fn antigravity(lines: &[&str], after_step: Option<i64>) -> (Vec<Msg>, Option<i64>) {
    let mut out = Vec::new();
    let mut last = after_step;
    // Results carry no call id; they answer the preceding calls in order.
    let mut open_calls: std::collections::VecDeque<String> = Default::default();
    for line in lines {
        let Ok(step) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let Some(index) = step.get("step_index").and_then(Value::as_i64) else {
            continue;
        };
        if after_step.is_some_and(|a| index <= a) {
            continue;
        }
        last = Some(last.map_or(index, |l| l.max(index)));
        let content = str_of(&step, "content").unwrap_or_default();
        match (
            str_of(&step, "type").unwrap_or_default(),
            str_of(&step, "source").unwrap_or_default(),
        ) {
            ("USER_INPUT", _) => push(&mut out, Msg::text("user", user_request(content))),
            ("PLANNER_RESPONSE", _) => {
                push(&mut out, Msg::text("assistant", content.to_owned()));
                for (i, call) in step
                    .get("tool_calls")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .enumerate()
                {
                    let id = format!("step{index}-{i}");
                    open_calls.push_back(id.clone());
                    push(
                        &mut out,
                        Msg::tool(
                            "tool_call",
                            call.get("args").map(Value::to_string).unwrap_or_default(),
                            str_of(call, "name").map(str::to_owned),
                            Some(id),
                        ),
                    );
                }
            }
            (_, "SYSTEM") | ("CONVERSATION_HISTORY", _) => {}
            (kind, _) if !content.is_empty() => {
                let id = open_calls
                    .pop_front()
                    .unwrap_or_else(|| format!("step{index}"));
                push(
                    &mut out,
                    Msg::tool(
                        "tool_result",
                        content.to_owned(),
                        Some(kind.to_lowercase()),
                        Some(id),
                    ),
                );
            }
            _ => {}
        }
    }
    (out, last)
}

/// The latest user request, used as the recall query before a model call.
pub fn antigravity_last_request(lines: &[&str]) -> Option<String> {
    lines.iter().rev().find_map(|line| {
        let step = serde_json::from_str::<Value>(line).ok()?;
        (str_of(&step, "type")? == "USER_INPUT")
            .then(|| user_request(str_of(&step, "content").unwrap_or_default()))
    })
}

// ── OpenAI-style message lists (Hermes) ─────────────────────────────────

pub fn openai(messages: &[Value]) -> Vec<Msg> {
    let mut out = Vec::new();
    for m in messages {
        let content = block_text(m.get("content").unwrap_or(&Value::Null));
        match str_of(m, "role").unwrap_or_default() {
            "user" => push(&mut out, Msg::text("user", content)),
            "assistant" => {
                push(&mut out, Msg::text("assistant", content));
                for call in m
                    .get("tool_calls")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                {
                    let function = call.get("function").unwrap_or(&Value::Null);
                    push(
                        &mut out,
                        Msg::tool(
                            "tool_call",
                            str_of(function, "arguments")
                                .map(str::to_owned)
                                .unwrap_or_else(|| {
                                    function
                                        .get("arguments")
                                        .map(Value::to_string)
                                        .unwrap_or_default()
                                }),
                            str_of(function, "name").map(str::to_owned),
                            str_of(call, "id").map(str::to_owned),
                        ),
                    );
                }
            }
            "tool" => push(
                &mut out,
                Msg::tool(
                    "tool_result",
                    content,
                    str_of(m, "name").map(str::to_owned),
                    str_of(m, "tool_call_id").map(str::to_owned),
                ),
            ),
            _ => {}
        }
    }
    out
}

// ── Codex hook payloads ─────────────────────────────────────────────────

/// Codex documents its transcript as unstable, so a turn is rebuilt from the
/// hook payloads recorded during it: `UserPromptSubmit` (prompt),
/// `PostToolUse` (tool call and result) and `Stop` (final reply), in order.
pub fn codex(events: &[Value]) -> Vec<Msg> {
    let as_text = |v: &Value| {
        v.as_str().map_or_else(
            || {
                if v.is_null() {
                    String::new()
                } else {
                    v.to_string()
                }
            },
            str::to_owned,
        )
    };
    let mut out = Vec::new();
    for e in events {
        match str_of(e, "hook_event_name").unwrap_or_default() {
            "UserPromptSubmit" => push(
                &mut out,
                Msg::text(
                    "user",
                    strip_reminders(str_of(e, "prompt").unwrap_or_default()),
                ),
            ),
            "PostToolUse" => {
                let name = str_of(e, "tool_name").map(str::to_owned);
                let id = str_of(e, "tool_use_id").map(str::to_owned);
                push(
                    &mut out,
                    Msg::tool(
                        "tool_call",
                        as_text(&e["tool_input"]),
                        name.clone(),
                        id.clone(),
                    ),
                );
                push(
                    &mut out,
                    Msg::tool("tool_result", as_text(&e["tool_response"]), name, id),
                );
            }
            "Stop" => push(
                &mut out,
                Msg::text(
                    "assistant",
                    str_of(e, "last_assistant_message")
                        .unwrap_or_default()
                        .to_owned(),
                ),
            ),
            _ => {}
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn claude_code_turn_with_tools() {
        let lines = [
            json!({"type":"user","message":{"content":"<system-reminder>ctx</system-reminder>\nfix the build"}}).to_string(),
            json!({"type":"assistant","message":{"content":[{"type":"thinking","thinking":"..."}]}}).to_string(),
            json!({"type":"assistant","message":{"content":[{"type":"text","text":"Looking."}]}}).to_string(),
            json!({"type":"assistant","message":{"content":[{"type":"tool_use","id":"t1","name":"Bash","input":{"command":"cargo build"}}]}}).to_string(),
            json!({"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"t1","content":[{"type":"text","text":"ok"}]}]}}).to_string(),
            json!({"type":"assistant","isSidechain":true,"message":{"content":[{"type":"text","text":"subagent"}]}}).to_string(),
            json!({"type":"attachment"}).to_string(),
            json!({"type":"assistant","message":{"content":[{"type":"text","text":"Fixed."}]}}).to_string(),
        ];
        let refs: Vec<&str> = lines.iter().map(String::as_str).collect();
        let msgs = claude_code(&refs);
        let roles: Vec<_> = msgs.iter().map(|m| m.role).collect();
        assert_eq!(
            roles,
            ["user", "assistant", "tool_call", "tool_result", "assistant"]
        );
        assert_eq!(msgs[0].content, "fix the build");
        assert_eq!(msgs[2].tool_name.as_deref(), Some("Bash"));
        assert_eq!(msgs[2].content, r#"{"command":"cargo build"}"#);
        assert_eq!(msgs[3].content, "ok");
        assert_eq!(msgs[3].tool_call_id.as_deref(), Some("t1"));
    }

    #[test]
    fn antigravity_steps_after_cursor() {
        let lines = [
            json!({"step_index":0,"source":"USER_EXPLICIT","type":"USER_INPUT","content":"<USER_REQUEST>\n翻成繁中\n</USER_REQUEST>\n<ADDITIONAL_METADATA>x</ADDITIONAL_METADATA>"}).to_string(),
            json!({"step_index":1,"source":"MODEL","type":"PLANNER_RESPONSE","tool_calls":[{"name":"run_command","args":{"CommandLine":"ls"}}]}).to_string(),
            json!({"step_index":2,"source":"MODEL","type":"RUN_COMMAND","content":"README.md"}).to_string(),
            json!({"step_index":3,"source":"SYSTEM","type":"SYSTEM_MESSAGE","content":"internal"}).to_string(),
            json!({"step_index":4,"source":"MODEL","type":"PLANNER_RESPONSE","content":"完成"}).to_string(),
        ];
        let refs: Vec<&str> = lines.iter().map(String::as_str).collect();
        let (msgs, last) = antigravity(&refs, None);
        let roles: Vec<_> = msgs.iter().map(|m| m.role).collect();
        assert_eq!(roles, ["user", "tool_call", "tool_result", "assistant"]);
        assert_eq!(msgs[0].content, "翻成繁中");
        assert_eq!(msgs[2].tool_name.as_deref(), Some("run_command"));
        assert_eq!(
            msgs[2].tool_call_id, msgs[1].tool_call_id,
            "results pair with their call"
        );
        assert_eq!(last, Some(4));
        let (again, _) = antigravity(&refs, Some(4));
        assert!(again.is_empty(), "the cursor skips captured steps");
        assert_eq!(antigravity_last_request(&refs).as_deref(), Some("翻成繁中"));
    }

    #[test]
    fn openai_messages() {
        let msgs = openai(&[
            json!({"role":"system","content":"sys"}),
            json!({"role":"user","content":"weather?"}),
            json!({"role":"assistant","content":null,"tool_calls":[{"id":"c1","function":{"name":"get_weather","arguments":"{\"city\":\"Taipei\"}"}}]}),
            json!({"role":"tool","tool_call_id":"c1","name":"get_weather","content":"sunny"}),
            json!({"role":"assistant","content":"Sunny."}),
        ]);
        let roles: Vec<_> = msgs.iter().map(|m| m.role).collect();
        assert_eq!(roles, ["user", "tool_call", "tool_result", "assistant"]);
        assert_eq!(msgs[1].content, r#"{"city":"Taipei"}"#);
    }

    #[test]
    fn codex_turn_from_hook_payloads() {
        let events = [
            json!({"hook_event_name":"UserPromptSubmit","prompt":"run echo hi"}),
            json!({"hook_event_name":"PostToolUse","tool_name":"Bash","tool_input":{"command":"echo hi"},"tool_response":"hi\n","tool_use_id":"exec-1"}),
            json!({"hook_event_name":"Stop","last_assistant_message":"It printed hi.","stop_hook_active":false}),
            json!({"hook_event_name":"Stop","last_assistant_message":null}),
        ];
        let msgs = codex(&events);
        let roles: Vec<_> = msgs.iter().map(|m| m.role).collect();
        assert_eq!(roles, ["user", "tool_call", "tool_result", "assistant"]);
        assert_eq!(msgs[1].content, r#"{"command":"echo hi"}"#);
        assert_eq!(
            (
                msgs[1].tool_call_id.as_deref(),
                msgs[2].tool_call_id.as_deref()
            ),
            (Some("exec-1"), Some("exec-1"))
        );
        assert_eq!(msgs[2].content, "hi\n");
        assert_eq!(msgs[3].content, "It printed hi.");
    }

    #[test]
    fn partial_last_line_is_not_consumed() {
        assert_eq!(complete_lines("a\nb\npart"), ["a", "b"]);
        assert_eq!(complete_lines("a\n"), ["a"]);
    }
}
