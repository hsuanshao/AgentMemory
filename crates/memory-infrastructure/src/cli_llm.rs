//! An LLM served by a local command-line tool (e.g. `claude -p`,
//! `ollama run <model>`, `codex exec`, `gemini -p`), run inside a project
//! directory so the tool can use that project's context and settings.
//!
//! The process is spawned directly (no shell). The prompt goes to stdin by
//! default, which avoids argument-length limits and keeps conversation text
//! out of the process list; `Arg` mode substitutes it into the arguments.

use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use async_trait::async_trait;
use memory_application::ports::system::{ChatMessage, LlmClient};
use memory_domain::error::{DomainError, DomainResult, ErrorKind};
use tokio::io::AsyncWriteExt;
use tokio::process::Command;
use tokio::sync::Semaphore;

pub const PROMPT_PLACEHOLDER: &str = "{prompt}";
const STDERR_SNIPPET_CHARS: usize = 300;
/// Linux rejects any single argument longer than `MAX_ARG_STRLEN` (128 KiB).
const MAX_ARG_BYTES: usize = 128 * 1024 - 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromptMode {
    /// Write the prompt to the process's stdin.
    Stdin,
    /// Replace `{prompt}` in the arguments (appended when absent).
    Arg,
}

#[derive(Debug, Clone)]
pub struct CliLlmSettings {
    pub command: String,
    pub args: Vec<String>,
    pub prompt_mode: PromptMode,
    /// Project base path the tool runs in (its working directory).
    pub working_dir: Option<PathBuf>,
    pub env: Vec<(String, String)>,
    pub timeout: Duration,
    /// CLI agents are heavy; bound how many run at once.
    pub max_concurrency: usize,
}

pub struct CliLlm {
    settings: CliLlmSettings,
    permits: Semaphore,
    label: String,
}

impl CliLlm {
    pub fn new(settings: CliLlmSettings) -> Self {
        let name = std::path::Path::new(&settings.command)
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| settings.command.clone());
        let label = match model_arg(&settings.args) {
            Some(model) => format!("cli:{name}:{model}"),
            None => format!("cli:{name}"),
        };
        Self {
            permits: Semaphore::new(settings.max_concurrency.max(1)),
            label,
            settings,
        }
    }

    fn arguments(&self, prompt: &str) -> Vec<String> {
        match self.settings.prompt_mode {
            PromptMode::Stdin => self.settings.args.clone(),
            PromptMode::Arg
                if self
                    .settings
                    .args
                    .iter()
                    .any(|a| a.contains(PROMPT_PLACEHOLDER)) =>
            {
                self.settings
                    .args
                    .iter()
                    .map(|a| a.replace(PROMPT_PLACEHOLDER, prompt))
                    .collect()
            }
            PromptMode::Arg => self
                .settings
                .args
                .iter()
                .cloned()
                .chain([prompt.to_owned()])
                .collect(),
        }
    }

    async fn run(&self, prompt: String) -> DomainResult<String> {
        if self.settings.prompt_mode == PromptMode::Arg && prompt.len() > MAX_ARG_BYTES {
            return Err(unavailable(format!(
                "prompt is {} bytes, over the {MAX_ARG_BYTES}-byte argument limit; use a CLI that reads stdin",
                prompt.len()
            )));
        }
        let mut command = Command::new(&self.settings.command);
        command
            .args(self.arguments(&prompt))
            .envs(
                self.settings
                    .env
                    .iter()
                    .map(|(k, v)| (k.as_str(), v.as_str())),
            )
            .stdin(if self.settings.prompt_mode == PromptMode::Stdin {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        if let Some(dir) = &self.settings.working_dir {
            command.current_dir(dir);
        }
        let mut child = command
            .spawn()
            .map_err(|e| unavailable(format!("cannot start `{}`: {e}", self.settings.command)))?;
        if let Some(mut stdin) = child.stdin.take() {
            stdin
                .write_all(prompt.as_bytes())
                .await
                .map_err(|e| unavailable(format!("writing prompt failed: {e}")))?;
            // Dropping stdin sends EOF so the tool starts answering.
        }
        let output = tokio::time::timeout(self.settings.timeout, child.wait_with_output())
            .await
            .map_err(|_| {
                unavailable(format!(
                    "`{}` timed out after {:?}",
                    self.settings.command, self.settings.timeout
                ))
            })?
            .map_err(|e| unavailable(format!("`{}` failed: {e}", self.settings.command)))?;
        if !output.status.success() {
            return Err(unavailable(format!(
                "`{}` exited with {}: {}",
                self.settings.command,
                output.status,
                failure_reason(&output.stderr, &output.stdout, &prompt)
            )));
        }
        let text = String::from_utf8_lossy(&output.stdout).trim().to_owned();
        if text.is_empty() {
            // A tool can exit 0 and still explain itself on stderr (a refused
            // or throttled request); keep that, without the echoed prompt.
            return Err(unavailable(format!(
                "`{}` produced no output; stderr: {}",
                self.settings.command,
                failure_reason(&output.stderr, b"", &prompt)
            )));
        }
        Ok(text)
    }
}

/// Some tools print errors (e.g. "Not logged in") on stdout, not stderr.
/// Tools such as `codex exec` echo a banner and the whole prompt first, and
/// print the actual error last. Only what a tool prints after its echo of
/// the prompt is used, so the user conversation never reaches logs or the
/// Panel. From that, starts at the last line that begins with `error:` (any
/// case) and cuts a long one at its end (`head…`); without such a line,
/// keeps the end of the output (`…tail`).
fn failure_reason(stderr: &[u8], stdout: &[u8], prompt: &str) -> String {
    let stderr = String::from_utf8_lossy(stderr);
    let stdout = String::from_utf8_lossy(stdout);
    let source = match after_echo(&stderr, prompt) {
        "" => after_echo(&stdout, prompt),
        stderr => stderr,
    };
    if source.is_empty() {
        return "no error output".to_owned();
    }
    if let Some(start) = last_error_line(source) {
        let error = &source[start..];
        if error.chars().count() <= STDERR_SNIPPET_CHARS {
            return error.to_owned();
        }
        let head: String = error.chars().take(STDERR_SNIPPET_CHARS).collect();
        return format!("{head}…");
    }
    let chars = source.chars().count();
    if chars <= STDERR_SNIPPET_CHARS {
        return source.to_owned();
    }
    let tail: String = source.chars().skip(chars - STDERR_SNIPPET_CHARS).collect();
    format!("…{tail}")
}

/// The part of `output` after the last verbatim echo of `prompt`, or all of
/// it when the tool did not echo the prompt; trimmed.
fn after_echo<'a>(output: &'a str, prompt: &str) -> &'a str {
    let prompt = prompt.trim();
    match output.rfind(prompt).filter(|_| !prompt.is_empty()) {
        Some(i) => output[i + prompt.len()..].trim(),
        None => output.trim(),
    }
}

/// Byte offset of the last line starting with `error:` (ignoring case and
/// leading whitespace), e.g. codex's `ERROR: {...}`.
fn last_error_line(source: &str) -> Option<usize> {
    let mut offset = 0;
    let mut found = None;
    for line in source.split_inclusive('\n') {
        let text = line.trim_start();
        if text
            .get(..6)
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case("error:"))
        {
            found = Some(offset + line.len() - text.len());
        }
        offset += line.len();
    }
    found
}

/// The model the arguments select (`--model X`, `--model=X`, `-m X`), so
/// generation logs say which model ran rather than only the command.
fn model_arg(args: &[String]) -> Option<&str> {
    args.iter()
        .enumerate()
        .find_map(|(i, arg)| match arg.as_str() {
            "--model" | "-m" => args.get(i + 1).map(String::as_str),
            other => other.strip_prefix("--model="),
        })
        .filter(|m| !m.is_empty() && !m.contains(PROMPT_PLACEHOLDER))
}

fn unavailable(detail: String) -> DomainError {
    DomainError::new(ErrorKind::Unavailable, "LLM_UNAVAILABLE", detail)
}

/// Flattens chat messages into one prompt: the system prompt is wrapped in
/// tags so tools without a system-prompt flag still keep it apart.
pub fn render_prompt(messages: &[ChatMessage]) -> String {
    messages
        .iter()
        .map(|m| match m.role.as_str() {
            "system" => format!("<system>\n{}\n</system>", m.content),
            _ => m.content.clone(),
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}

#[async_trait]
impl LlmClient for CliLlm {
    fn is_configured(&self) -> bool {
        !self.settings.command.trim().is_empty()
    }

    fn model(&self) -> &str {
        &self.label
    }

    async fn complete(&self, messages: &[ChatMessage]) -> DomainResult<String> {
        let _permit = self
            .permits
            .acquire()
            .await
            .map_err(|_| unavailable("CLI LLM is shutting down".into()))?;
        self.run(render_prompt(messages)).await
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    fn cli(command: &str, args: &[&str], mode: PromptMode) -> CliLlm {
        CliLlm::new(CliLlmSettings {
            command: command.into(),
            args: args.iter().map(|a| a.to_string()).collect(),
            prompt_mode: mode,
            working_dir: None,
            env: Vec::new(),
            timeout: Duration::from_secs(5),
            max_concurrency: 2,
        })
    }

    fn ask() -> Vec<ChatMessage> {
        vec![ChatMessage::system("be brief"), ChatMessage::user("hello")]
    }

    #[tokio::test]
    async fn stdin_mode_sends_the_rendered_prompt() {
        let out = cli("cat", &[], PromptMode::Stdin)
            .complete(&ask())
            .await
            .unwrap();
        assert_eq!(out, "<system>\nbe brief\n</system>\n\nhello");
    }

    #[tokio::test]
    async fn arg_mode_substitutes_or_appends_the_prompt() {
        let llm = cli("echo", &["Q:", "{prompt}"], PromptMode::Arg);
        assert!(llm
            .complete(&ask())
            .await
            .unwrap()
            .starts_with("Q: <system>"));
        let appended = cli("echo", &["Q:"], PromptMode::Arg);
        assert!(appended.complete(&ask()).await.unwrap().ends_with("hello"));
    }

    #[tokio::test]
    async fn runs_in_the_project_directory() {
        let dir = tempfile::tempdir().unwrap();
        let mut llm = cli("pwd", &[], PromptMode::Arg);
        llm.settings.args.clear();
        llm.settings.prompt_mode = PromptMode::Stdin;
        llm.settings.working_dir = Some(dir.path().to_path_buf());
        let out = llm.complete(&ask()).await.unwrap();
        assert_eq!(
            std::fs::canonicalize(out).unwrap(),
            std::fs::canonicalize(dir.path()).unwrap()
        );
    }

    #[test]
    fn failure_reason_cuts_only_past_the_snippet_limit() {
        // Counted in characters, not bytes: "é" is two bytes.
        let exact = "é".repeat(STDERR_SNIPPET_CHARS);
        assert_eq!(failure_reason(exact.as_bytes(), b"", ""), exact);
        let over = format!("x{exact}");
        assert_eq!(
            failure_reason(over.as_bytes(), b"", ""),
            format!("…{exact}")
        );
    }

    #[tokio::test]
    async fn empty_output_keeps_what_the_tool_said_on_stderr() {
        let prompt = "secret conversation tok_live_ABC";
        let quiet = cli(
            "sh",
            &[
                "-c",
                "cat >&2; echo >&2; echo request was throttled >&2; exit 0",
            ],
            PromptMode::Stdin,
        )
        .complete(&[ChatMessage::user(prompt)])
        .await
        .unwrap_err();
        assert!(
            quiet
                .detail
                .ends_with("produced no output; stderr: request was throttled"),
            "{}",
            quiet.detail
        );
        assert!(!quiet.detail.contains("tok_live"));
        let silent = cli("true", &[], PromptMode::Arg)
            .complete(&ask())
            .await
            .unwrap_err();
        assert!(
            silent.detail.ends_with("stderr: no error output"),
            "{}",
            silent.detail
        );
    }

    #[test]
    fn the_label_names_the_model_from_the_arguments() {
        let label = |args: &[&str]| cli("agy", args, PromptMode::Arg).model().to_owned();
        assert_eq!(label(&["-p", "{prompt}"]), "cli:agy");
        assert_eq!(
            label(&["-p", "{prompt}", "--model", "gemini-3.8-flash-high"]),
            "cli:agy:gemini-3.8-flash-high"
        );
        assert_eq!(label(&["--model=sonnet", "-p"]), "cli:agy:sonnet");
        assert_eq!(label(&["-m", "o4"]), "cli:agy:o4");
        assert_eq!(
            label(&["-p", "--model"]),
            "cli:agy",
            "no value after the flag"
        );
    }

    #[tokio::test]
    async fn failures_and_timeouts_are_unavailable_errors() {
        assert_eq!(
            failure_reason(b"", b"Not logged in\n", "hi"),
            "Not logged in"
        );
        assert_eq!(failure_reason(b"boom", b"ignored", "hi"), "boom");
        // Only what follows the echoed prompt counts, so an `error:` line in
        // the conversation is not taken for the tool's own error.
        let prompt = "error: could not compile `app`\nMy API token is tok_live_ABC";
        let echoed = format!(
            "OpenAI Codex\n--------\nuser\n{prompt}\n\nstream disconnected before completion\n"
        );
        assert_eq!(
            failure_reason(echoed.as_bytes(), b"", prompt),
            "stream disconnected before completion"
        );
        let echoed = format!("user\n{prompt}\nERROR: stream disconnected\n");
        assert_eq!(
            failure_reason(echoed.as_bytes(), b"", prompt),
            "ERROR: stream disconnected"
        );
        // Nothing after the echo: say so instead of returning the prompt.
        assert_eq!(
            failure_reason(format!("user\n{prompt}\n").as_bytes(), b"", prompt),
            "no error output"
        );
        // The last error line wins, and a long one keeps its start.
        let long = format!("error: earlier\n  Error: {}", "x".repeat(400));
        let reason = failure_reason(long.as_bytes(), b"", "hi");
        assert!(reason.starts_with("Error: xxx"));
        assert!(reason.ends_with('…'));
        assert_eq!(reason.chars().count(), STDERR_SNIPPET_CHARS + 1);
        // Without an error line, the end of the output is kept.
        let unmarked = format!("{}\nmodel is at capacity", "log ".repeat(200));
        let reason = failure_reason(unmarked.as_bytes(), b"", "hi");
        assert!(reason.starts_with('…'));
        assert!(reason.ends_with("model is at capacity"));
        assert_eq!(reason.chars().count(), STDERR_SNIPPET_CHARS + 1);
        // End to end: a tool that echoes its stdin prompt to stderr.
        let echoing = cli(
            "sh",
            &[
                "-c",
                "cat >&2; echo >&2; echo stream disconnected >&2; exit 1",
            ],
            PromptMode::Stdin,
        )
        .complete(&[ChatMessage::user(prompt)])
        .await
        .unwrap_err();
        assert!(echoing.detail.ends_with(": stream disconnected"));
        assert!(!echoing.detail.contains("tok_live"));
        let failing = cli("false", &[], PromptMode::Arg)
            .complete(&ask())
            .await
            .unwrap_err();
        assert_eq!(failing.code, "LLM_UNAVAILABLE");
        let mut slow = cli("sleep", &["5"], PromptMode::Stdin);
        slow.settings.timeout = Duration::from_millis(100);
        assert!(slow
            .complete(&ask())
            .await
            .unwrap_err()
            .detail
            .contains("timed out"));
        let huge = vec![ChatMessage::user("x".repeat(MAX_ARG_BYTES + 1))];
        let too_long = cli("echo", &[], PromptMode::Arg)
            .complete(&huge)
            .await
            .unwrap_err();
        assert!(too_long.detail.contains("argument limit"));
        let missing = cli("definitely-not-a-command", &[], PromptMode::Stdin)
            .complete(&ask())
            .await
            .unwrap_err();
        assert!(missing.detail.contains("cannot start"));
    }
}
