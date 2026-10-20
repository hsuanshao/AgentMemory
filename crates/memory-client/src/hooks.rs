//! Host hook entry points. A hook must never break the host: every failure
//! is logged to `<data dir>/amem.log` and the process still exits 0.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::{json, Value};

use crate::api::{recall_context, Api};
use crate::capture::capture;
use crate::config::{data_dir, write_private, Config};
use crate::queue::Queue;
use crate::transcript::{
    antigravity, antigravity_last_request, claude_code, codex, complete_lines, Msg,
};

/// Claude Code caps injected context at 10,000 characters.
const MAX_CONTEXT_CHARS: usize = 9_500;
const RECALL_TIMEOUT: Duration = Duration::from_secs(8);
const CAPTURE_TIMEOUT: Duration = Duration::from_secs(15);

pub fn log(message: &str) {
    let path = data_dir().join("amem.log");
    if std::fs::metadata(&path).is_ok_and(|m| m.len() > 1_000_000) {
        let _ = std::fs::remove_file(&path);
    }
    let _ = std::fs::create_dir_all(data_dir());
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        use std::io::Write;
        let secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        let _ = writeln!(f, "{secs} {message}");
    }
}

fn stdin_json() -> Value {
    let mut raw = String::new();
    let _ = std::io::stdin().read_to_string(&mut raw);
    serde_json::from_str(&raw).unwrap_or(Value::Null)
}

fn api(timeout: Duration) -> Result<Api, String> {
    Api::new(Config::load()?, timeout)
}

fn clip(text: &str, max: usize) -> String {
    match text.char_indices().nth(max) {
        None => text.to_owned(),
        Some((cut, _)) => text[..cut].to_owned(),
    }
}

/// Host session ids become file names.
fn safe_name(session: &str) -> String {
    session
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// Per-host, per-session cursor so each Stop only sends what is new.
struct Cursor {
    path: PathBuf,
    state: Value,
}

impl Cursor {
    fn open(host: &str, session: &str) -> Self {
        let path = data_dir()
            .join("state")
            .join(host)
            .join(format!("{}.json", safe_name(session)));
        let state = std::fs::read(&path)
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_else(|| json!({}));
        Self { path, state }
    }

    fn save(&self) {
        if let Err(e) = write_private(&self.path, self.state.to_string().as_bytes()) {
            log(&format!("cursor save failed: {e}"));
        }
    }
}

fn read_transcript(path: &str) -> Option<String> {
    (!path.is_empty())
        .then(|| std::fs::read_to_string(Path::new(path)).ok())
        .flatten()
}

fn deliver(host: &str, session: &str, msgs: Vec<Msg>) {
    if msgs.is_empty() {
        return;
    }
    match api(CAPTURE_TIMEOUT) {
        Ok(api) => {
            let r = capture(&api, &Queue::new(&data_dir()), session, msgs);
            if let Some(e) = r.error {
                log(&format!(
                    "{host} capture: sent {} queued {}: {e}",
                    r.sent, r.queued
                ));
            }
        }
        Err(e) => log(&format!("{host} capture skipped: {e}")),
    }
}

fn recall_or_log(host: &str, query: &str) -> Option<String> {
    if query.trim().chars().count() < 2 {
        return None;
    }
    let result = api(RECALL_TIMEOUT).and_then(|a| a.recall(query, None).map_err(|e| e.to_string()));
    match result {
        Ok(data) => Some(clip(&recall_context(&data), MAX_CONTEXT_CHARS)).filter(|c| !c.is_empty()),
        Err(e) => {
            log(&format!("{host} recall: {e}"));
            None
        }
    }
}

// ── Claude Code ─────────────────────────────────────────────────────────

pub fn claude_code_user_prompt_submit() {
    let input = stdin_json();
    let prompt = input["prompt"].as_str().unwrap_or_default();
    if let Some(context) = recall_or_log("claude-code", prompt) {
        println!(
            "{}",
            json!({"hookSpecificOutput": {"hookEventName": "UserPromptSubmit", "additionalContext": context}})
        );
    }
}

/// Sends the transcript lines added since the last Stop. The transcript can
/// lag the final reply, so `last_assistant_message` is appended when missing
/// and remembered as `tail` to skip it once it lands in the file.
pub fn claude_code_stop() {
    let input = stdin_json();
    let session = input["session_id"].as_str().unwrap_or("unknown");
    let Some(text) = read_transcript(input["transcript_path"].as_str().unwrap_or_default()) else {
        log("claude-code stop: transcript not readable");
        return;
    };
    let lines = complete_lines(&text);
    let mut cursor = Cursor::open("claude-code", session);
    let mut from = cursor.state["line"].as_u64().unwrap_or(0) as usize;
    if from > lines.len() {
        from = 0;
    }
    let mut msgs = claude_code(&lines[from..]);

    if let Some(tail) = cursor.state["tail"]
        .as_str()
        .map(str::trim)
        .filter(|t| !t.is_empty())
    {
        if let Some(first) = msgs.iter().position(|m| m.role == "assistant") {
            let rest = msgs[first]
                .content
                .trim()
                .strip_prefix(tail)
                .map(|r| r.trim().to_owned());
            match rest {
                Some(r) if r.is_empty() => {
                    msgs.remove(first);
                }
                Some(r) => msgs[first].content = r,
                None => {}
            }
        }
    }
    let mut tail = Value::Null;
    if let Some(last) = input["last_assistant_message"]
        .as_str()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        let present = msgs
            .iter()
            .rev()
            .find(|m| m.role == "assistant")
            .is_some_and(|m| m.content.trim().ends_with(last));
        if !present {
            msgs.push(Msg {
                role: "assistant",
                content: last.to_owned(),
                tool_name: None,
                tool_call_id: None,
                timestamp: None,
            });
            tail = json!(last);
        }
    }

    deliver("claude-code", session, msgs);
    cursor.state = json!({"line": lines.len(), "tail": tail});
    cursor.save();
}

// ── Antigravity ─────────────────────────────────────────────────────────

/// Recalls once per user turn: PreInvocation fires on every model call, and
/// only invocation 0 starts a turn.
pub fn antigravity_pre_invocation() {
    let input = stdin_json();
    if input["invocationNum"].as_i64().unwrap_or(0) > 0 {
        return;
    }
    let Some(text) = read_transcript(input["transcriptPath"].as_str().unwrap_or_default()) else {
        return;
    };
    let query = antigravity_last_request(&complete_lines(&text)).unwrap_or_default();
    if let Some(context) = recall_or_log("antigravity", &query) {
        println!(
            "{}",
            json!({"injectSteps": [{"ephemeralMessage": context}]})
        );
    }
}

pub fn antigravity_stop() {
    let input = stdin_json();
    let session = input["conversationId"].as_str().unwrap_or("unknown");
    let Some(text) = read_transcript(input["transcriptPath"].as_str().unwrap_or_default()) else {
        log("antigravity stop: transcript not readable");
        return;
    };
    let mut cursor = Cursor::open("antigravity", session);
    let (msgs, last) = antigravity(&complete_lines(&text), cursor.state["step"].as_i64());
    deliver("antigravity", session, msgs);
    if let Some(step) = last {
        cursor.state = json!({"step": step});
        cursor.save();
    }
}

// ── Codex ───────────────────────────────────────────────────────────────

fn codex_events_dir(session: &str) -> PathBuf {
    data_dir()
        .join("state")
        .join("codex")
        .join(safe_name(session))
}

/// Keeps one hook payload for the turn's Stop. One file per event, named by
/// time, so parallel PostToolUse hooks never race on a shared file.
fn codex_record(input: &Value) {
    let session = input["session_id"].as_str().unwrap_or("unknown");
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let path = codex_events_dir(session).join(format!("{nanos:020}-{}.json", std::process::id()));
    if let Err(e) = write_private(&path, input.to_string().as_bytes()) {
        log(&format!("codex record failed: {e}"));
    }
}

pub fn codex_user_prompt_submit() {
    let input = stdin_json();
    codex_record(&input);
    if let Some(context) = recall_or_log("codex", input["prompt"].as_str().unwrap_or_default()) {
        println!(
            "{}",
            json!({"hookSpecificOutput": {"hookEventName": "UserPromptSubmit", "additionalContext": context}})
        );
    }
}

pub fn codex_post_tool_use() {
    codex_record(&stdin_json());
}

/// Sends the turn recorded since the previous Stop (a turn interrupted
/// before its Stop is sent with the next one), then forgets it.
pub fn codex_stop() {
    let input = stdin_json();
    let session = input["session_id"].as_str().unwrap_or("unknown");
    let dir = codex_events_dir(session);
    let mut files: Vec<PathBuf> = std::fs::read_dir(&dir)
        .map(|entries| {
            entries
                .filter_map(|e| e.ok().map(|e| e.path()))
                .filter(|p| p.extension().is_some_and(|x| x == "json"))
                .collect()
        })
        .unwrap_or_default();
    files.sort();
    let mut events: Vec<Value> = files
        .iter()
        .filter_map(|f| {
            std::fs::read(f)
                .ok()
                .and_then(|b| serde_json::from_slice(&b).ok())
        })
        .collect();
    events.push(input.clone());
    deliver("codex", session, codex(&events));
    for f in &files {
        let _ = std::fs::remove_file(f);
    }
    let _ = std::fs::remove_dir(&dir);
}
