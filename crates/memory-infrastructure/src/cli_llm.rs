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
use memory_application::ports::system::{ChatMessage, LlmClient, LLM_TOOL_REQUEST};
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
    /// Set for the tool on top of what it inherits.
    pub env: Vec<(String, String)>,
    /// More variables the tool may inherit: names, or `PREFIX_*`.
    pub pass_env: Vec<String>,
    pub timeout: Duration,
    /// CLI agents are heavy; bound how many run at once.
    pub max_concurrency: usize,
}

pub struct CliLlm {
    settings: CliLlmSettings,
    permits: Semaphore,
    label: String,
    /// The tool's whole environment, fixed at start-up.
    env: Vec<(String, String)>,
}

/// What any program needs to run: paths, user, locale, time zone,
/// temporary and config directories, certificates and proxies.
const BASE_ENV: &[&str] = &[
    "PATH",
    "HOME",
    "USER",
    "LOGNAME",
    "SHELL",
    "LANG",
    "LANGUAGE",
    "TZ",
    "TMPDIR",
    "TERM",
    "XDG_CONFIG_HOME",
    "XDG_DATA_HOME",
    "XDG_CACHE_HOME",
    "XDG_STATE_HOME",
    "XDG_RUNTIME_DIR",
    "SSL_CERT_FILE",
    "SSL_CERT_DIR",
    "NODE_EXTRA_CA_CERTS",
    "HTTP_PROXY",
    "HTTPS_PROXY",
    "NO_PROXY",
    "ALL_PROXY",
    "http_proxy",
    "https_proxy",
    "no_proxy",
    "all_proxy",
];

/// memory-core's own settings and secrets; never passed on, even when a
/// pattern would match them.
const OWN_PREFIX: &str = "AGENT_MEMORY_";

/// The settings and credentials of a known CLI, by executable name.
fn cli_prefixes(command: &str) -> &'static [&'static str] {
    match command_name(command).as_str() {
        "claude" => &["ANTHROPIC_", "CLAUDE_"],
        "codex" => &["OPENAI_", "CODEX_"],
        "agy" | "antigravity" | "gemini" => &["GEMINI_", "GOOGLE_"],
        _ => &[],
    }
}

/// Why a `pass_env` entry is refused, if it is: it would hand the tool
/// memory-core's own variables, or everything.
pub fn refused_pass_env(entry: &str) -> Option<&'static str> {
    let entry = entry.trim();
    match entry.strip_suffix('*') {
        _ if entry.is_empty() => Some("an empty name"),
        Some(prefix) if OWN_PREFIX.starts_with(prefix) => {
            Some("it matches memory-core's own AGENT_MEMORY_* variables")
        }
        _ if entry.starts_with(OWN_PREFIX) => {
            Some("memory-core's own AGENT_MEMORY_* variables stay with memory-core")
        }
        _ => None,
    }
}

/// The tool's environment: from `inherited`, the basics, the CLI's own
/// settings and `pass_env`; then `explicit` on top. Nothing else of
/// memory-core's environment (its API key, database and Redis URLs, LLM and
/// embedding keys) reaches a tool that reads untrusted conversation text.
pub fn child_env(
    command: &str,
    pass_env: &[String],
    explicit: &[(String, String)],
    inherited: impl IntoIterator<Item = (String, String)>,
) -> Vec<(String, String)> {
    let own = cli_prefixes(command);
    let passed = |name: &str| {
        pass_env
            .iter()
            .map(|p| p.trim())
            .any(|p| match p.strip_suffix('*') {
                Some(prefix) => name.starts_with(prefix),
                None => name == p,
            })
    };
    let keep = |name: &str| {
        !name.starts_with(OWN_PREFIX)
            && (BASE_ENV.contains(&name)
                || name.starts_with("LC_")
                || own.iter().any(|p| name.starts_with(p))
                || passed(name))
    };
    let mut env: std::collections::BTreeMap<String, String> = inherited
        .into_iter()
        .filter(|(name, _)| keep(name))
        .collect();
    env.extend(explicit.iter().cloned());
    env.into_iter().collect()
}

/// What `codex` turns off: connectors, plugins, shell and code execution,
/// sub-agents, browsing and image tools, and web search; the sandbox is
/// pinned read-only. Feature names come from `codex features list` (0.163);
/// codex rejects an unknown one, so a rename shows up as a failed run rather
/// than a silently armed tool. `unified_exec` does not turn off on 0.163, but
/// `shell_tool`, which carries the command tools, does.
#[rustfmt::skip]
const CODEX_HARDENING: &[&str] = &[
    "--disable", "apps",
    "--disable", "plugins",
    "--disable", "remote_plugin",
    "--disable", "shell_tool",
    "--disable", "unified_exec",
    "--disable", "code_mode_host",
    "--disable", "multi_agent",
    "--disable", "goals",
    "--disable", "skill_search",
    "--disable", "tool_suggest",
    "--disable", "browser_use",
    "--disable", "browser_use_external",
    "--disable", "computer_use",
    "--disable", "in_app_browser",
    "--disable", "image_generation",
    "-c", "web_search=disabled",
    "-c", "sandbox_mode=\"read-only\"",
];

/// Put after codex's `exec`: MCP servers and profiles live in
/// `$CODEX_HOME/config.toml`, which no argument can switch off, so the file is
/// not read at all. Sign-in still comes from `CODEX_HOME`; settings the
/// distillation needs go in the arguments (`-c model=…`).
const CODEX_EXEC_HARDENING: &str = "--ignore-user-config";

/// What `claude` turns off: every built-in tool, MCP servers from settings,
/// and skills and slash commands.
const CLAUDE_HARDENING: &[&str] = &[
    "--tools",
    "",
    "--strict-mcp-config",
    "--disable-slash-commands",
];

/// `args` with a known CLI's own tools turned off before it reads untrusted
/// conversation text (#37); the added arguments only remove capability.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Hardened {
    /// Every known switch was applied.
    Full(Vec<String>),
    /// Applied in part; the string says what could not be.
    Partial(Vec<String>, &'static str),
    /// No known way for this CLI (e.g. `agy`): its tool attempts are refused
    /// in headless mode and the input is dropped.
    Unknown,
}

pub fn harden(command: &str, args: &[String]) -> Hardened {
    let owned = |list: &[&str]| list.iter().map(|a| (*a).to_owned()).collect::<Vec<_>>();
    match command_name(command).as_str() {
        "claude" => Hardened::Full(
            owned(CLAUDE_HARDENING)
                .into_iter()
                .chain(args.iter().cloned())
                .collect(),
        ),
        "codex" => {
            let mut out = owned(CODEX_HARDENING);
            match args.iter().position(|a| a == "exec" || a == "e") {
                Some(i) => {
                    out.extend(args[..=i].iter().cloned());
                    out.push(CODEX_EXEC_HARDENING.to_owned());
                    out.extend(args[i + 1..].iter().cloned());
                    Hardened::Full(out)
                }
                None => {
                    out.extend(args.iter().cloned());
                    Hardened::Partial(out, "no `exec` in the arguments, so codex still reads config.toml (MCP servers, profiles)")
                }
            }
        }
        _ => Hardened::Unknown,
    }
}

/// The first argument that would give the tool more than it has by default:
/// skipped permission checks, auto-approval, allow-lists, extra MCP servers,
/// web search, re-enabled features, profiles or a writable sandbox.
/// Distillation never gets any of these, whatever the hardening setting (#37).
pub fn granting_arg<'a>(command: &str, args: &'a [String]) -> Option<&'a str> {
    let name = command_name(command);
    args.iter().enumerate().find_map(|(i, arg)| {
        let (flag, inline) = match arg.split_once('=') {
            Some((flag, value)) if flag.starts_with('-') => (flag, Some(value)),
            _ => (arg.as_str(), None),
        };
        // codex's short options also take their value attached: `-cKEY=VALUE`,
        // `-sMODE`, `-pNAME`.
        let (flag, inline) = match attached_short(&name, arg) {
            Some((short, value)) => (short, Some(value)),
            None => (flag, inline),
        };
        let value = || inline.or_else(|| args.get(i + 1).map(String::as_str));
        let grants = match flag {
            f if f.starts_with("--dangerously") || f.starts_with("--allow-dangerously") => true,
            "--yolo" | "--full-auto" | "--allowedTools" | "--allowed-tools" => true,
            "--mcp-config" | "--permission-prompt-tool" => true,
            "--permission-mode" => value().is_some_and(|v| v != "default" && v != "plan"),
            "--tools" => name == "claude" && value().is_some_and(|v| !v.trim().is_empty()),
            "--enable" | "--search" | "-p" | "--profile" => name == "codex",
            "-s" | "--sandbox" => name == "codex" && value().is_some_and(|v| v != "read-only"),
            "-c" | "--config" => name == "codex" && value().is_some_and(codex_setting_grants),
            _ => false,
        };
        grants.then_some(arg.as_str())
    })
}

/// A codex short option written with its value attached (`-cKEY=VALUE`,
/// `-sMODE`, `-pNAME`, also `-c=KEY=VALUE`), split into the option and the
/// value.
fn attached_short<'a>(name: &str, arg: &'a str) -> Option<(&'static str, &'a str)> {
    if name != "codex" {
        return None;
    }
    ["-c", "-s", "-p"].into_iter().find_map(|short| {
        let rest = arg.strip_prefix(short)?;
        let rest = rest.strip_prefix('=').unwrap_or(rest);
        (!rest.is_empty()).then_some((short, rest))
    })
}

/// Whether codex runs through `exec`, where `--ignore-user-config` keeps
/// config.toml (MCP servers, profiles) out of distillation.
pub fn codex_without_exec(command: &str, args: &[String]) -> bool {
    command_name(command) == "codex" && !args.iter().any(|a| a == "exec" || a == "e")
}

/// Whether a codex `-c key=value` override grants capability: MCP servers,
/// a feature switched on, web search, a sandbox other than read-only, sandbox
/// permissions, an approval policy or a profile. Other settings (model,
/// reasoning effort, service tier) are fine.
fn codex_setting_grants(setting: &str) -> bool {
    let (key, value) = setting.split_once('=').unwrap_or((setting, ""));
    let key = key.trim();
    let value = value
        .trim()
        .trim_matches(|c| c == '"' || c == '\'')
        .to_ascii_lowercase();
    let under = |prefix: &str| key == prefix || key.starts_with(&format!("{prefix}."));
    if under("mcp_servers")
        || under("profiles")
        || under("sandbox_permissions")
        || under("sandbox_workspace_write")
    {
        return true;
    }
    match key {
        "profile" | "approval_policy" | "features" => true,
        "sandbox_mode" => value != "read-only",
        k if k == "web_search" || k.ends_with(".web_search") => {
            value != "disabled" && value != "false"
        }
        k if k.starts_with("features.") => value != "false",
        _ => false,
    }
}

/// The executable's file name, without its directory.
fn command_name(command: &str) -> String {
    std::path::Path::new(command)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| command.to_owned())
}

impl CliLlm {
    pub fn new(settings: CliLlmSettings) -> Self {
        Self::with_environment(settings, std::env::vars())
    }

    /// `inherited` stands for memory-core's environment.
    pub fn with_environment(
        settings: CliLlmSettings,
        inherited: impl IntoIterator<Item = (String, String)>,
    ) -> Self {
        let env = child_env(
            &settings.command,
            &settings.pass_env,
            &settings.env,
            inherited,
        );
        let name = command_name(&settings.command);
        let label = match model_arg(&settings.args) {
            Some(model) => format!("cli:{name}:{model}"),
            None => format!("cli:{name}"),
        };
        Self {
            permits: Semaphore::new(settings.max_concurrency.max(1)),
            label,
            settings,
            env,
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
            .env_clear()
            .envs(self.env.iter().map(|(k, v)| (k.as_str(), v.as_str())))
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
            return Err(failure(
                format!(
                    "`{}` exited with {}: {}",
                    self.settings.command,
                    output.status,
                    failure_reason(&output.stderr, &output.stdout, &prompt)
                ),
                &output.stderr,
                &prompt,
            ));
        }
        let text = String::from_utf8_lossy(&output.stdout).trim().to_owned();
        if text.is_empty() {
            // A tool can exit 0 and still explain itself on stderr (a refused
            // or throttled request); keep that, without the echoed prompt.
            return Err(failure(
                format!(
                    "`{}` produced no output; stderr: {}",
                    self.settings.command,
                    failure_reason(&output.stderr, b"", &prompt)
                ),
                &output.stderr,
                &prompt,
            ));
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

/// A failed run: a refused tool call gets its own code; anything else may
/// pass next time. The refusal is recognised by the CLI's own message as a
/// whole line of its stderr, after its echo of the prompt, never by a word
/// that conversation text quoted in the output could also contain.
fn failure(detail: String, stderr: &[u8], prompt: &str) -> DomainError {
    let stderr = String::from_utf8_lossy(stderr);
    if after_echo(&stderr, prompt).lines().any(is_tool_refusal) {
        DomainError::new(ErrorKind::Unprocessable, LLM_TOOL_REQUEST, detail)
    } else {
        unavailable(detail)
    }
}

/// agy (jetski) 1.4: `jetski: no output produced — a tool required the
/// "command" permission that headless mode cannot prompt for, so it was
/// auto-denied. …`
fn is_tool_refusal(line: &str) -> bool {
    let line = line.trim_start();
    line.starts_with("jetski: ")
        && line.contains("headless mode cannot prompt for")
        && line.contains("so it was auto-denied")
}

/// Told to every CLI agent, since they are built to reach for tools: a tool
/// call is refused, and some CLIs then fail the whole run (#37).
const NO_TOOLS: &str =
    "You have no tools in this task: do not read files, run commands, search or \
     browse. Everything you need is in this prompt; answer from it alone.";

/// Flattens chat messages into one prompt: the system prompt is wrapped in
/// tags so tools without a system-prompt flag still keep it apart.
pub fn render_prompt(messages: &[ChatMessage]) -> String {
    messages
        .iter()
        .map(|m| match m.role.as_str() {
            "system" => format!("<system>\n{NO_TOOLS}\n\n{}\n</system>", m.content),
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
            pass_env: Vec::new(),
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
        assert_eq!(
            out,
            format!("<system>\n{NO_TOOLS}\n\nbe brief\n</system>\n\nhello")
        );
    }

    #[test]
    fn known_clis_get_their_tools_turned_off() {
        let args = |s: &str| s.split(' ').map(str::to_owned).collect::<Vec<_>>();
        let Hardened::Full(claude) = harden("/usr/local/bin/claude", &args("-p")) else {
            panic!("claude is fully hardened");
        };
        assert_eq!(&claude[..2], ["--tools", ""]);
        assert_eq!(claude.last().map(String::as_str), Some("-p"));
        let Hardened::Full(codex) =
            harden("codex", &args("exec --skip-git-repo-check -s read-only -"))
        else {
            panic!("codex with exec is fully hardened");
        };
        assert!(codex.windows(2).any(|w| w == ["--disable", "apps"]));
        assert!(codex.windows(2).any(|w| w == ["--disable", "shell_tool"]));
        assert!(codex.windows(2).any(|w| w == ["-c", "web_search=disabled"]));
        assert!(codex
            .windows(2)
            .any(|w| w == ["-c", "sandbox_mode=\"read-only\""]));
        // config.toml (MCP servers, profiles) is skipped, right after `exec`.
        assert!(codex
            .windows(2)
            .any(|w| w == ["exec", "--ignore-user-config"]));
        assert!(matches!(
            harden("codex", &args("--version")),
            Hardened::Partial(..)
        ));
        assert_eq!(harden("agy", &args("-p")), Hardened::Unknown);
        assert_eq!(harden("ollama", &args("run m")), Hardened::Unknown);
        // Hardening itself never trips the check for granting arguments.
        assert_eq!(granting_arg("claude", &claude), None);
        assert_eq!(granting_arg("codex", &codex), None);
    }

    #[test]
    fn granting_arguments_are_found_in_either_form() {
        let args = |s: &str| s.split(' ').map(str::to_owned).collect::<Vec<_>>();
        assert_eq!(
            granting_arg("agy", &args("-p {prompt} --dangerously-skip-permissions")),
            Some("--dangerously-skip-permissions")
        );
        assert_eq!(
            granting_arg("claude", &args("-p --permission-mode=acceptEdits")),
            Some("--permission-mode=acceptEdits")
        );
        assert_eq!(
            granting_arg("codex", &args("exec -s danger-full-access")),
            Some("-s")
        );
        assert_eq!(granting_arg("codex", &args("exec -s read-only")), None);
        // codex settings: MCP servers, web search, profiles, approval and a
        // writable sandbox grant; model and effort settings do not.
        for granting in [
            "exec -c mcp_servers.x.command=\"true\"",
            "exec -c web_search=live",
            "exec --config=web_search=\"cached\"",
            "exec -c features.apps=true",
            "exec -c features={apps=true}",
            "exec -c sandbox_mode=\"workspace-write\"",
            "exec -c approval_policy=never",
            "exec -c profile=work",
            "exec -p work",
            "--search exec",
            "exec -cmcp_servers.x.command=\"true\"",
            "exec -cweb_search=live",
            "exec -sdanger-full-access",
            "exec -pwork",
            "exec -c=mcp_servers.x.command=\"true\"",
            "exec -c=web_search=live",
            "exec -s=danger-full-access",
            "exec -p=work",
        ] {
            assert!(
                granting_arg("codex", &args(granting)).is_some(),
                "{granting}"
            );
        }
        for fine in [
            "exec -c service_tier=fast -c model_reasoning_effort=high",
            "exec -c features.hooks=false -c web_search=disabled",
            "exec -c sandbox_mode=\"read-only\"",
            "exec -c=service_tier=fast -s=read-only",
        ] {
            assert_eq!(granting_arg("codex", &args(fine)), None, "{fine}");
        }
        // A flag only grants for the tool it belongs to.
        assert_eq!(granting_arg("ollama", &args("run m --enable x")), None);
        assert_eq!(granting_arg("claude", &args("-p --model sonnet")), None);
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

    fn vars(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect()
    }

    /// What memory-core's process might hold.
    fn server_env() -> Vec<(String, String)> {
        vars(&[
            ("PATH", "/usr/bin:/bin"),
            ("HOME", "/home/memory"),
            ("LC_ALL", "C.UTF-8"),
            ("AGENT_MEMORY_API_KEY", "server-key"),
            (
                "AGENT_MEMORY_POSTGRES_URL",
                "postgres://memory:pw@db/memory",
            ),
            ("AGENT_MEMORY_LLM_API_KEY", "llm-key"),
            ("POSTGRES_PASSWORD", "pw"),
            ("REDIS_PASSWORD", "pw"),
            ("ANTHROPIC_API_KEY", "claude-key"),
            ("CLAUDE_CONFIG_DIR", "/home/memory/.claude"),
            ("OPENAI_API_KEY", "openai-key"),
            ("GEMINI_API_KEY", "gemini-key"),
            ("CUSTOM_TOKEN", "custom"),
        ])
    }

    fn names(env: &[(String, String)]) -> Vec<&str> {
        env.iter().map(|(k, _)| k.as_str()).collect()
    }

    #[test]
    fn a_tool_inherits_the_basics_and_its_own_settings_only() {
        let env = child_env("/usr/local/bin/claude", &[], &[], server_env());
        assert_eq!(
            names(&env),
            [
                "ANTHROPIC_API_KEY",
                "CLAUDE_CONFIG_DIR",
                "HOME",
                "LC_ALL",
                "PATH"
            ]
        );
        assert_eq!(
            names(&child_env("codex", &[], &[], server_env())),
            ["HOME", "LC_ALL", "OPENAI_API_KEY", "PATH"]
        );
        assert_eq!(
            names(&child_env("agy", &[], &[], server_env())),
            ["GEMINI_API_KEY", "HOME", "LC_ALL", "PATH"]
        );
        // An unknown tool gets the basics, plus what pass_env names.
        let env = child_env(
            "my-llm",
            &["CUSTOM_*".into(), "OPENAI_API_KEY".into()],
            &vars(&[("MODEL", "small"), ("HOME", "/tmp/tool")]),
            server_env(),
        );
        assert_eq!(
            env,
            vars(&[
                ("CUSTOM_TOKEN", "custom"),
                ("HOME", "/tmp/tool"),
                ("LC_ALL", "C.UTF-8"),
                ("MODEL", "small"),
                ("OPENAI_API_KEY", "openai-key"),
                ("PATH", "/usr/bin:/bin"),
            ])
        );
        // memory-core's own variables never pass, whatever pass_env says.
        let env = child_env(
            "my-llm",
            &["AGENT_MEMORY_API_KEY".into(), "*".into()],
            &[],
            server_env(),
        );
        assert!(names(&env).iter().all(|n| !n.starts_with("AGENT_MEMORY_")));
    }

    #[test]
    fn pass_env_may_not_reach_memory_cores_own_variables() {
        for refused in [
            "",
            "*",
            "A*",
            "AGENT_*",
            "AGENT_MEMORY_*",
            "AGENT_MEMORY_API_KEY",
        ] {
            assert!(refused_pass_env(refused).is_some(), "{refused:?}");
        }
        for allowed in ["CUSTOM_TOKEN", "CUSTOM_*", "AGENTS_HOME", "B*"] {
            assert_eq!(refused_pass_env(allowed), None, "{allowed:?}");
        }
    }

    #[tokio::test]
    async fn the_running_tool_sees_only_the_allowed_environment() {
        // Reads the prompt first, so writing it never hits a closed pipe.
        let settings = CliLlmSettings {
            command: "sh".into(),
            args: vec!["-c".into(), "cat >/dev/null; exec env".into()],
            prompt_mode: PromptMode::Stdin,
            working_dir: None,
            env: vars(&[("MODEL", "small")]),
            pass_env: Vec::new(),
            timeout: Duration::from_secs(5),
            max_concurrency: 1,
        };
        let out = CliLlm::with_environment(settings, server_env())
            .complete(&ask())
            .await
            .unwrap();
        let mut seen: Vec<&str> = out
            .lines()
            .filter_map(|l| l.split_once('=').map(|(k, _)| k))
            // What the shell itself sets.
            .filter(|k| !matches!(*k, "PWD" | "SHLVL" | "_"))
            .collect();
        seen.sort_unstable();
        assert_eq!(seen, ["HOME", "LC_ALL", "MODEL", "PATH"]);
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

    #[tokio::test]
    async fn a_refused_tool_call_is_a_tool_request() {
        let refused = cli(
            "sh",
            &[
                "-c",
                "cat >/dev/null; echo 'jetski: no output produced — a tool required the \"command\" permission that headless mode cannot prompt for, so it was auto-denied. Add an allow-rule.' >&2",
            ],
            PromptMode::Stdin,
        )
        .complete(&ask())
        .await
        .unwrap_err();
        assert_eq!(refused.code, LLM_TOOL_REQUEST, "{}", refused.detail);
        // The phrase in the model's output, or inside another error line, is
        // not agy's refusal.
        for quoting in [
            "cat >/dev/null; echo 'it was auto-denied, the user said'; exit 1",
            "cat >/dev/null; echo 'error: quota; earlier: so it was auto-denied' >&2",
        ] {
            let other = cli("sh", &["-c", quoting], PromptMode::Stdin)
                .complete(&ask())
                .await
                .unwrap_err();
            assert_eq!(other.code, "LLM_UNAVAILABLE", "{quoting}");
        }
        let other = cli(
            "sh",
            &["-c", "cat >/dev/null; echo quota >&2"],
            PromptMode::Stdin,
        )
        .complete(&ask())
        .await
        .unwrap_err();
        assert_eq!(other.code, "LLM_UNAVAILABLE");
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
