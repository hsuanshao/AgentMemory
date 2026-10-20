use std::io::Read;
use std::process::ExitCode;
use std::time::Duration;

use memory_client::api::{recall_context, Api};
use memory_client::capture::capture;
use memory_client::config::{config_path, data_dir, Config};
use memory_client::queue::Queue;
use memory_client::{hooks, mcp, transcript};
use serde_json::Value;

const USAGE: &str = "amem — Agent Memory client

USAGE:
  amem login --server-url URL --user-key KEY|- --team-id ID --agent-id ID [--department-id ID] [--no-verify]
  amem status                      Show configuration (key masked) and queue size
  amem recall [--json] QUERY...    Recall memories for QUERY
  amem capture --format openai --session ID < messages.json
  amem flush                       Send queued captures
  amem hook claude-code user-prompt-submit|stop
  amem hook antigravity pre-invocation|stop
  amem hook codex user-prompt-submit|post-tool-use|stop
  amem mcp                         Run the stdio MCP server

`--user-key -` reads the key from stdin, keeping it out of shell history.
Settings can be overridden with AMEM_SERVER_URL, AMEM_USER_KEY, AMEM_TEAM_ID,
AMEM_AGENT_ID, AMEM_DEPARTMENT_ID; AMEM_SKIP_PROMPT_PREFIXES (comma-separated)
skips recall and capture for prompts starting with one of them; AMEM_CONFIG
and AMEM_DATA_DIR move the files.";

fn flag(args: &[String], name: &str) -> Option<String> {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1))
        .cloned()
}

fn mask(key: &str) -> String {
    let visible: String = key.chars().take(10).collect();
    if key.is_empty() {
        "(unset)".into()
    } else {
        format!("{visible}…")
    }
}

fn login(args: &[String]) -> Result<(), String> {
    let existing = Config::load().unwrap_or_default();
    let pick = |name: &str, current: &str| flag(args, name).unwrap_or_else(|| current.to_owned());
    let mut user_key = pick("--user-key", &existing.user_key);
    if user_key == "-" {
        user_key.clear();
        std::io::stdin()
            .read_line(&mut user_key)
            .map_err(|e| e.to_string())?;
    }
    let cfg = Config {
        server_url: pick("--server-url", &existing.server_url)
            .trim_end_matches('/')
            .to_owned(),
        department_id: pick(
            "--department-id",
            if existing.department_id.is_empty() {
                "default"
            } else {
                &existing.department_id
            },
        ),
        user_key: user_key.trim().to_owned(),
        team_id: pick("--team-id", &existing.team_id),
        agent_id: pick("--agent-id", &existing.agent_id),
        skip_prompt_prefixes: existing.skip_prompt_prefixes.clone(),
    };
    if let Some(missing) = cfg.missing().first() {
        return Err(format!("missing --{}", missing.replace('_', "-")));
    }
    if !args.iter().any(|a| a == "--no-verify") {
        // A recall exercises the key, the team membership and the agent.
        Api::new(cfg.clone(), Duration::from_secs(15))?
            .recall("connection check", Some(1))
            .map_err(|e| format!("verification failed: {e}"))?;
    }
    let path = config_path();
    cfg.save(&path)?;
    println!(
        "saved {} (team {}, agent {})",
        path.display(),
        cfg.team_id,
        cfg.agent_id
    );
    Ok(())
}

fn status() -> Result<(), String> {
    let cfg = Config::load()?;
    println!("config:        {}", config_path().display());
    println!("server_url:    {}", cfg.server_url);
    println!("department_id: {}", cfg.department_id);
    println!("user_key:      {}", mask(&cfg.user_key));
    println!("team_id:       {}", cfg.team_id);
    println!("agent_id:      {}", cfg.agent_id);
    if !cfg.skip_prompt_prefixes.is_empty() {
        println!("skip_prompt_prefixes: {}", cfg.skip_prompt_prefixes);
    }
    let queue = Queue::new(&data_dir());
    println!(
        "queued:        {} (in {})",
        queue.pending().len(),
        data_dir().join("queue").display()
    );
    if !cfg.missing().is_empty() {
        return Err(format!("not configured: {}", cfg.missing().join(", ")));
    }
    Ok(())
}

fn recall(args: &[String]) -> Result<(), String> {
    let json = args.iter().any(|a| a == "--json");
    let query: Vec<&str> = args
        .iter()
        .filter(|a| *a != "--json")
        .map(String::as_str)
        .collect();
    let api = Api::new(Config::load()?, Duration::from_secs(15))?;
    let data = api
        .recall(&query.join(" "), None)
        .map_err(|e| e.to_string())?;
    if json {
        println!("{data}");
    } else {
        println!("{}", recall_context(&data));
    }
    Ok(())
}

fn capture_cmd(args: &[String]) -> Result<(), String> {
    let session = flag(args, "--session").ok_or("missing --session")?;
    let format = flag(args, "--format").unwrap_or_else(|| "openai".into());
    let mut raw = String::new();
    std::io::stdin()
        .read_to_string(&mut raw)
        .map_err(|e| e.to_string())?;
    let msgs = match format.as_str() {
        "openai" => {
            let list: Vec<Value> = serde_json::from_str(&raw).map_err(|e| format!("stdin: {e}"))?;
            transcript::openai(&list)
        }
        other => return Err(format!("unsupported --format {other}")),
    };
    let api = Api::new(Config::load()?, Duration::from_secs(15))?;
    let report = capture(&api, &Queue::new(&data_dir()), &session, msgs);
    match report.error {
        Some(e) if report.queued == 0 && report.sent == 0 => Err(e),
        Some(e) => {
            hooks::log(&format!("capture {session}: {e}"));
            Ok(())
        }
        None => Ok(()),
    }
}

fn flush() -> Result<(), String> {
    let api = Api::new(Config::load()?, Duration::from_secs(15))?;
    let r = Queue::new(&data_dir()).flush(&api);
    println!(
        "sent {}, failed {}, remaining {}",
        r.sent, r.failed, r.remaining
    );
    r.blocked.map_or(Ok(()), Err)
}

fn run(args: &[String]) -> Result<(), String> {
    let rest = args.get(1..).unwrap_or_default();
    match args.first().map(String::as_str) {
        Some("login") => login(rest),
        Some("status") => status(),
        Some("recall") => recall(rest),
        Some("capture") => capture_cmd(rest),
        Some("flush") => flush(),
        Some("mcp") => {
            mcp::serve();
            Ok(())
        }
        Some("hook") => {
            match (
                rest.first().map(String::as_str),
                rest.get(1).map(String::as_str),
            ) {
                (Some("claude-code"), Some("user-prompt-submit")) => {
                    hooks::claude_code_user_prompt_submit()
                }
                (Some("claude-code"), Some("stop")) => hooks::claude_code_stop(),
                (Some("antigravity"), Some("pre-invocation")) => {
                    hooks::antigravity_pre_invocation()
                }
                (Some("antigravity"), Some("stop")) => hooks::antigravity_stop(),
                (Some("codex"), Some("user-prompt-submit")) => hooks::codex_user_prompt_submit(),
                (Some("codex"), Some("post-tool-use")) => hooks::codex_post_tool_use(),
                (Some("codex"), Some("stop")) => hooks::codex_stop(),
                other => hooks::log(&format!("unknown hook {other:?}")),
            }
            Ok(())
        }
        Some("--version" | "-V" | "version") => {
            println!("amem {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        Some("--help" | "-h" | "help") | None => {
            println!("{USAGE}");
            Ok(())
        }
        Some(other) => Err(format!("unknown command {other}\n\n{USAGE}")),
    }
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match run(&args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("amem: {e}");
            ExitCode::FAILURE
        }
    }
}
