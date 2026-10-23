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
    antigravity, antigravity_last_request, claude_code, codex, complete_lines, drop_skipped_turns,
    Msg,
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
    fn path(host: &str, session: &str) -> PathBuf {
        data_dir()
            .join("state")
            .join(host)
            .join(format!("{}.json", safe_name(session)))
    }

    fn open(host: &str, session: &str) -> Self {
        Self::at(Self::path(host, session))
    }

    fn at(path: PathBuf) -> Self {
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

    /// Reads the cursor at `path`, lets `change` edit it and writes it back.
    /// Every writer of a Claude Code cursor goes through here, so no write
    /// lands between another writer's read and its write.
    fn update<R>(path: PathBuf, change: impl FnOnce(&mut Value) -> R) -> R {
        let _lock = CursorLock::acquire(&path);
        let mut cursor = Self::at(path);
        let out = change(&mut cursor.state);
        cursor.save();
        out
    }
}

/// An exclusive `flock` on `<cursor>.lock`, held for one read-change-write.
/// The lock file lives as long as the cursor; whoever cleans up old session
/// state removes both.
/// The cursor itself is replaced by rename on every save, so a lock on it
/// would be on a file that is already gone. The kernel drops the lock when
/// the process exits, so a crash leaves nothing to clean up.
struct CursorLock(#[allow(dead_code)] Option<std::fs::File>);

impl CursorLock {
    fn acquire(cursor: &Path) -> Self {
        let mut path = cursor.as_os_str().to_owned();
        path.push(".lock");
        let file = Self::open(Path::new(&path)).and_then(|file| {
            Self::lock(&file)?;
            Ok(file)
        });
        match file {
            Ok(file) => Self(Some(file)),
            Err(e) => {
                // Better an unlocked write than a hook that fails.
                log(&format!("cursor lock failed: {e}"));
                #[cfg(test)]
                tests::LOCK_FAILURES.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                Self(None)
            }
        }
    }

    fn open(path: &Path) -> std::io::Result<std::fs::File> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let mut options = std::fs::OpenOptions::new();
        options.create(true).truncate(false).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        options.open(path)
    }

    /// How long a hook waits for the lock. A holder only reads and writes
    /// a small local file; one that has not let go in this time is stopped
    /// or stuck, and the hook writes without the lock rather than hang.
    const WAIT: Duration = Duration::from_secs(1);

    /// Waits in the kernel's queue with a blocking `flock` on a clone of
    /// the descriptor, so waiters take turns. Polling with `LOCK_NB` let a
    /// writer that releases and locks again in a loop starve the other one
    /// past `WAIT`, which then wrote unlocked and lost updates. On timeout
    /// the waiting thread is left behind: if it gets the lock later, its
    /// clone is closed right away, and the lock goes with the last one.
    #[cfg(unix)]
    fn lock(file: &std::fs::File) -> std::io::Result<()> {
        use std::io::ErrorKind;
        use std::os::unix::io::AsRawFd;
        let waiter = file.try_clone()?;
        let (done, locked) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let result = loop {
                // SAFETY: flock on a descriptor this thread owns; the lock
                // belongs to the open file, shared with `file`.
                if unsafe { libc::flock(waiter.as_raw_fd(), libc::LOCK_EX) } == 0 {
                    break Ok(());
                }
                let e = std::io::Error::last_os_error();
                if e.kind() != ErrorKind::Interrupted {
                    break Err(e);
                }
            };
            let _ = done.send(result);
        });
        match locked.recv_timeout(Self::WAIT) {
            Ok(result) => result,
            Err(_) => Err(std::io::Error::new(
                ErrorKind::TimedOut,
                format!("held by another process for {:?}", Self::WAIT),
            )),
        }
    }

    /// Releases ship for Linux and macOS only; elsewhere writes stay
    /// unlocked, as before.
    #[cfg(not(unix))]
    fn lock(_file: &std::fs::File) -> std::io::Result<()> {
        Ok(())
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
            let msgs = drop_skipped_turns(msgs, |p| api.config().skips_prompt(p));
            if msgs.is_empty() {
                return;
            }
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

/// The recall response for `query`, or nothing when the prompt is skipped,
/// too short, or the server failed (logged).
fn recall_data(host: &str, query: &str) -> Option<(Api, Value)> {
    if query.trim().chars().count() < 2 {
        return None;
    }
    let api = match api(RECALL_TIMEOUT) {
        Ok(api) if api.config().skips_prompt(query) => return None,
        Ok(api) => api,
        Err(e) => {
            log(&format!("{host} recall: {e}"));
            return None;
        }
    };
    match api.recall(query, None) {
        Ok(data) => Some((api, data)),
        Err(e) => {
            log(&format!("{host} recall: {e}"));
            None
        }
    }
}

fn recall_or_log(host: &str, query: &str) -> Option<String> {
    let (_, data) = recall_data(host, query)?;
    Some(clip(&recall_context(&data), MAX_CONTEXT_CHARS)).filter(|c| !c.is_empty())
}

// ── Fixed part of recall (persona, scene navigation) ────────────────────
//
// `append_system_context` is the same on most prompts of a session, and the
// host keeps earlier context in the conversation, so it is sent again only
// when it changed (#39). The decision lives in the session cursor next to
// the Stop keys.

const SYSTEM_CONTEXT_HASH: &str = "system_context_hash";
const SYSTEM_CONTEXT_AGE: &str = "system_context_age";

/// FNV-1a: stable across builds, unlike `DefaultHasher`, so a cursor written
/// by an older `amem` still matches.
fn fnv1a(text: &str) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in text.bytes() {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    format!("{h:016x}")
}

/// Whether `system` (the persona and scene navigation) goes to the host on
/// this prompt: when it differs from what the session last received, or
/// every `every` prompts when that is above 0. `every == 1` sends it on
/// every prompt. Empty content is never sent but is remembered, so a persona
/// that disappears and comes back is sent again.
pub(crate) fn system_context_due(state: &mut Value, system: &str, every: u64) -> bool {
    let hash = fnv1a(system);
    let changed = state[SYSTEM_CONTEXT_HASH].as_str() != Some(hash.as_str());
    let age = state[SYSTEM_CONTEXT_AGE].as_u64().unwrap_or(0) + 1;
    let due = !system.is_empty() && (changed || (every > 0 && age >= every));
    state[SYSTEM_CONTEXT_HASH] = json!(hash);
    state[SYSTEM_CONTEXT_AGE] = json!(if due || changed { 0 } else { age });
    due
}

/// The next prompt sends the fixed part again, whatever it is. Used when the
/// host starts a session or compacts its context.
pub(crate) fn forget_system_context(state: &mut Value) {
    if let Some(map) = state.as_object_mut() {
        map.remove(SYSTEM_CONTEXT_HASH);
        map.remove(SYSTEM_CONTEXT_AGE);
    }
}

fn hook_context(event: &str, context: &str) -> Value {
    json!({"hookSpecificOutput": {"hookEventName": event, "additionalContext": context}})
}

// ── Claude Code ─────────────────────────────────────────────────────────

/// Recalls for the prompt. The memories go out on every prompt; the persona
/// and scene navigation only when they changed since this session last got
/// them, and every `system_context_every` prompts (20 unless set) as a
/// fallback for a compaction or `/rewind` without a `SessionStart`. Without a session id
/// there is nothing to compare against, so everything is sent.
pub fn claude_code_user_prompt_submit() {
    let input = stdin_json();
    let prompt = input["prompt"].as_str().unwrap_or_default();
    let Some((api, data)) = recall_data("claude-code", prompt) else {
        return;
    };
    let memories = data["prepend_context"].as_str().unwrap_or_default().trim();
    let system = data["append_system_context"]
        .as_str()
        .unwrap_or_default()
        .trim();
    let every = api.config().system_context_every();
    let decide = |state: &mut Value| prompt_context(state, memories, system, every);
    // The recall is done: nothing below waits on the network.
    let context = match input["session_id"]
        .as_str()
        .filter(|s| !s.is_empty() && every != 1)
    {
        Some(session) => Cursor::update(Cursor::path("claude-code", session), decide),
        // Nothing to compare against: a fresh state sends everything.
        None => decide(&mut json!({})),
    };
    if !context.is_empty() {
        println!("{}", hook_context("UserPromptSubmit", &context));
    }
}

/// Decides on the fixed part for this prompt, records it in `state` and
/// returns the context to inject.
fn prompt_context(state: &mut Value, memories: &str, system: &str, every: u64) -> String {
    let send_system = system_context_due(state, system, every);
    let full = compose_context(memories, system, send_system);
    let context = clip(&full, MAX_CONTEXT_CHARS);
    if send_system && context.len() < full.len() {
        // The host got a cut persona: try again next prompt.
        forget_system_context(state);
    }
    context.to_owned()
}

/// The memories, then the fixed part when it is due.
fn compose_context(memories: &str, system: &str, send_system: bool) -> String {
    [memories, system]
        .into_iter()
        .enumerate()
        .filter(|(i, s)| !s.is_empty() && (*i == 0 || send_system))
        .map(|(_, s)| s)
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// A new, resumed, cleared, forked or compacted session no longer has the
/// persona and scene navigation in its context: the next prompt sends them
/// again. Prints nothing.
pub fn claude_code_session_start() {
    let input = stdin_json();
    let Some(session) = input["session_id"].as_str().filter(|s| !s.is_empty()) else {
        return;
    };
    Cursor::update(Cursor::path("claude-code", session), forget_system_context);
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
    let cursor = Cursor::path("claude-code", session);
    claude_code_stop_at(&text, &input, cursor, |msgs| {
        deliver("claude-code", session, msgs)
    });
}

/// The Stop above, with the cursor file and the delivery pluggable.
fn claude_code_stop_at(text: &str, input: &Value, cursor: PathBuf, deliver: impl FnOnce(Vec<Msg>)) {
    let lines = complete_lines(text);
    let cursor = Cursor::at(cursor);
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
                interjected: false,
            });
            tail = json!(last);
        }
    }

    deliver(msgs);
    // Read again, not across the delivery: a UserPromptSubmit that ran
    // during the capture may have changed the system-context keys, and
    // Stop owns only `line` and `tail`.
    Cursor::update(cursor.path, |state| record_stop(state, lines.len(), tail));
}

/// Stop's part of the cursor; every other key is left as it is.
fn record_stop(state: &mut Value, line: usize, tail: Value) {
    state["line"] = json!(line);
    state["tail"] = tail;
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Times a writer gave up on the lock, for failure messages. Global:
    /// the held-lock test gives up once on purpose.
    pub(super) static LOCK_FAILURES: std::sync::atomic::AtomicUsize =
        std::sync::atomic::AtomicUsize::new(0);

    #[test]
    fn system_context_goes_once_until_it_changes() {
        let mut state = json!({});
        assert!(system_context_due(&mut state, "a", 0));
        assert!(!system_context_due(&mut state, "a", 0));
        assert!(!system_context_due(&mut state, "a", 0));
        assert!(system_context_due(&mut state, "b", 0));
        assert!(system_context_due(&mut state, "a", 0));
    }

    #[test]
    fn every_one_keeps_the_old_behaviour() {
        let mut state = json!({});
        assert!(system_context_due(&mut state, "p", 1));
        assert!(system_context_due(&mut state, "p", 1));
    }

    #[test]
    fn every_n_resends_unchanged_context_on_the_nth_prompt() {
        let mut state = json!({});
        assert!(system_context_due(&mut state, "p", 3));
        assert!(!system_context_due(&mut state, "p", 3));
        assert!(!system_context_due(&mut state, "p", 3));
        assert!(system_context_due(&mut state, "p", 3));
        assert!(!system_context_due(&mut state, "p", 3));
    }

    #[test]
    fn forgetting_resends_unchanged_context() {
        let mut state = json!({});
        assert!(system_context_due(&mut state, "p", 0));
        forget_system_context(&mut state);
        assert!(system_context_due(&mut state, "p", 0));
        assert!(!system_context_due(&mut state, "p", 0));
    }

    #[test]
    fn empty_context_is_never_due_but_is_remembered() {
        let mut state = json!({});
        assert!(system_context_due(&mut state, "p", 0));
        assert!(!system_context_due(&mut state, "", 0));
        assert!(system_context_due(&mut state, "p", 0));
    }

    #[test]
    fn compose_puts_memories_first_and_drops_the_fixed_part_when_not_due() {
        assert_eq!(compose_context("m", "s", true), "m\n\ns");
        assert_eq!(compose_context("m", "s", false), "m");
        assert_eq!(compose_context("", "s", true), "s");
        assert_eq!(compose_context("", "s", false), "");
    }

    #[test]
    fn stop_keeps_the_system_context_keys() {
        let mut state = json!({});
        assert!(system_context_due(&mut state, "p", 0));
        record_stop(&mut state, 7, json!("t"));
        assert_eq!(state["line"], 7);
        assert_eq!(state["tail"], "t");
        assert!(!system_context_due(&mut state, "p", 0));
        record_stop(&mut state, 9, Value::Null);
        assert!(state["tail"].is_null());
        assert!(!system_context_due(&mut state, "p", 0));
    }

    /// prompt → Stop → prompt: the Stop entry point must not forget what the
    /// prompt sent, even when a prompt writes the cursor during the capture.
    #[test]
    fn stop_entry_point_keeps_what_the_prompt_recorded() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s.json");
        let mut prompt = Cursor::at(path.clone());
        assert!(system_context_due(&mut prompt.state, "persona", 0));
        prompt.save();

        let input = json!({"session_id": "s", "last_assistant_message": "done"});
        let concurrent = path.clone();
        claude_code_stop_at("", &input, path.clone(), move |msgs| {
            assert_eq!(msgs.len(), 1);
            // A UserPromptSubmit lands while the capture is in flight.
            let mut c = Cursor::at(concurrent);
            assert!(!system_context_due(&mut c.state, "persona", 0));
            c.save();
        });

        let mut after = Cursor::at(path.clone());
        assert_eq!(after.state["line"], 0);
        assert_eq!(after.state["tail"], "done");
        assert_eq!(after.state[SYSTEM_CONTEXT_AGE], 1);
        assert!(!system_context_due(&mut after.state, "persona", 0));

        // A second Stop with nothing new keeps them too.
        claude_code_stop_at("", &json!({"session_id": "s"}), path.clone(), |msgs| {
            assert!(msgs.is_empty());
        });
        let mut again = Cursor::at(path);
        assert!(!system_context_due(&mut again.state, "persona", 0));
        assert!(again.state["tail"].is_null());
    }

    #[test]
    fn prompt_keeps_what_stop_recorded_meanwhile() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s.json");
        let (inside, entered) = std::sync::mpsc::channel();
        let prompt_path = path.clone();
        let prompt = std::thread::spawn(move || {
            Cursor::update(prompt_path, |state| {
                inside.send(()).unwrap();
                // Stop tries to record while the prompt is deciding.
                std::thread::sleep(Duration::from_millis(200));
                prompt_context(state, "memories", "persona", 0)
            })
        });
        entered.recv().unwrap();
        Cursor::update(path.clone(), |state| record_stop(state, 7, json!("done")));
        assert!(prompt.join().unwrap().contains("persona"));

        let mut after = Cursor::at(path);
        assert_eq!(after.state["line"], 7);
        assert_eq!(after.state["tail"], "done");
        assert!(!system_context_due(&mut after.state, "persona", 0));
    }

    #[test]
    fn cursor_updates_from_two_writers_are_not_lost() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s.json");
        let writers: Vec<_> = ["a", "b"]
            .into_iter()
            .map(|key| {
                let path = path.clone();
                std::thread::spawn(move || {
                    for _ in 0..20 {
                        Cursor::update(path.clone(), |state| {
                            let n = state[key].as_u64().unwrap_or(0);
                            // Widen the read-to-write window.
                            std::thread::sleep(Duration::from_millis(1));
                            state[key] = json!(n + 1);
                        });
                    }
                })
            })
            .collect();
        for w in writers {
            w.join().unwrap();
        }
        let after = Cursor::at(path);
        let failures = LOCK_FAILURES.load(std::sync::atomic::Ordering::Relaxed);
        assert_eq!(after.state["a"], 20, "lock failures: {failures}");
        assert_eq!(after.state["b"], 20, "lock failures: {failures}");
    }

    #[test]
    fn a_held_lock_delays_a_writer_by_about_a_second_not_forever() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s.json");
        // Another process stopped while holding the lock.
        let held = CursorLock::acquire(&path);
        let (done, finished) = std::sync::mpsc::channel();
        let writer_path = path.clone();
        let writer = std::thread::spawn(move || {
            Cursor::update(writer_path, |state| state["x"] = json!(1));
            done.send(()).unwrap();
        });
        let result = finished.recv_timeout(Duration::from_secs(3));
        drop(held);
        writer.join().unwrap();
        assert!(result.is_ok(), "the writer waited for the lock past 3 s");
        assert_eq!(Cursor::at(path).state["x"], 1);
    }

    #[test]
    fn stop_cursor_keys_survive() {
        let mut state = json!({"line": 42, "tail": "t"});
        system_context_due(&mut state, "p", 0);
        forget_system_context(&mut state);
        assert_eq!(state["line"], 42);
        assert_eq!(state["tail"], "t");
    }
}
