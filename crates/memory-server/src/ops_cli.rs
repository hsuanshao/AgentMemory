//! Operator commands that read and steer a running memory-core over its own
//! HTTP API, so they behave the same on every storage and coordination
//! backend (`memory-server generations`, `memory-server dead-letters`).
//! Run them where the server runs, e.g.
//! `docker compose exec memory-core memory-server dead-letters`.

use anyhow::{anyhow, bail, Context, Result};
use chrono::{TimeZone, Utc};
use serde_json::{json, Value};

use crate::config::Config;

const GENERATIONS_HELP: &str = "\
memory-server generations [--status succeeded|empty|failed|dropped] [--layer l1|l2|l3]
                          [--hours N] [--limit N] [--department ID] [--json]

Counts of distillation runs per layer and status over the last N hours
(default 24), then the latest runs (default: failed ones, 10).";

const DEAD_LETTERS_HELP: &str = "\
memory-server dead-letters [list] [--kind l1|l2|l3|skill] [--limit N] [--department ID] [--json]
memory-server dead-letters redrive [--kind l1|l2|l3|skill] [--limit N] [--department ID]

Pipeline jobs that failed every retry. `redrive` queues them again as first
attempts, oldest first (default limit 10), so a fixed LLM catches up gradually.";

struct Args {
    rest: Vec<String>,
}

impl Args {
    fn new(args: impl Iterator<Item = String>) -> Self {
        Self {
            rest: args.collect(),
        }
    }

    fn flag(&mut self, name: &str) -> bool {
        let before = self.rest.len();
        self.rest.retain(|a| a != name);
        self.rest.len() != before
    }

    fn value(&mut self, name: &str) -> Result<Option<String>> {
        let Some(i) = self.rest.iter().position(|a| a == name) else {
            return Ok(None);
        };
        if i + 1 >= self.rest.len() {
            bail!("{name} needs a value");
        }
        let value = self.rest.remove(i + 1);
        self.rest.remove(i);
        Ok(Some(value))
    }

    fn number(&mut self, name: &str) -> Result<Option<u64>> {
        self.value(name)?
            .map(|v| {
                v.parse()
                    .with_context(|| format!("{name} must be a number"))
            })
            .transpose()
    }

    fn finish(self) -> Result<()> {
        match self.rest.first() {
            Some(extra) => bail!("unexpected argument: {extra}"),
            None => Ok(()),
        }
    }
}

/// Calls `POST /v3/{path}` on the local server and returns `data`.
struct Core {
    http: reqwest::Client,
    base: String,
    api_key: Option<String>,
    department: String,
}

impl Core {
    fn new(department: String) -> Result<Self> {
        let config = Config::load()?;
        let host = match config.server.host.as_str() {
            "0.0.0.0" | "::" | "" => "127.0.0.1".to_owned(),
            h => h.to_owned(),
        };
        Ok(Self {
            http: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(30))
                .build()?,
            base: format!("http://{host}:{}", config.server.port),
            api_key: config.server.api_key,
            department,
        })
    }

    async fn call(&self, path: &str, body: Value) -> Result<Value> {
        let mut req = self
            .http
            .post(format!("{}/v3/{path}", self.base))
            .header("x-memory-department-id", &self.department)
            .json(&body);
        if let Some(key) = &self.api_key {
            req = req.bearer_auth(key);
        }
        let res = req
            .send()
            .await
            .with_context(|| format!("cannot reach memory-core at {}", self.base))?;
        let status = res.status();
        let envelope: Value = res.json().await.context("reading the response")?;
        if !status.is_success() || envelope["code"] != 0 {
            return Err(anyhow!(
                "{path}: HTTP {status}: {}",
                envelope["message"].as_str().unwrap_or("request failed")
            ));
        }
        Ok(envelope["data"].clone())
    }
}

fn when(ms: i64) -> String {
    match Utc.timestamp_millis_opt(ms).single() {
        Some(t) if ms > 0 => t.format("%Y-%m-%d %H:%M:%SZ").to_string(),
        _ => "-".into(),
    }
}

fn one_line(text: &str, max: usize) -> String {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    match flat.char_indices().nth(max) {
        Some((cut, _)) => format!("{}…", &flat[..cut]),
        None => flat,
    }
}

pub async fn generations(args: impl Iterator<Item = String>) -> Result<()> {
    let mut a = Args::new(args);
    if a.flag("--help") || a.flag("-h") {
        println!("{GENERATIONS_HELP}");
        return Ok(());
    }
    let json_out = a.flag("--json");
    let status = a.value("--status")?.unwrap_or_else(|| "failed".into());
    let layer = a.value("--layer")?;
    let hours = a.number("--hours")?.unwrap_or(24);
    let limit = a.number("--limit")?.unwrap_or(10);
    let department = a.value("--department")?.unwrap_or_else(|| "default".into());
    a.finish()?;
    if !["succeeded", "empty", "failed", "dropped"].contains(&status.as_str()) {
        bail!("--status must be succeeded, empty, failed or dropped");
    }

    let core = Core::new(department)?;
    let summary = core
        .call("memory-generation-log/summary", json!({ "hours": hours }))
        .await?;
    let now = Utc::now().timestamp_millis();
    let mut list_body = json!({
        "status": status,
        "limit": limit,
        "start_time": summary["from_ms"],
        "end_time": now,
    });
    if let Some(layer) = &layer {
        list_body["layer"] = json!(layer);
    }
    let recent = core.call("memory-generation-log/list", list_body).await?;
    if json_out {
        println!(
            "{}",
            serde_json::to_string_pretty(
                &json!({ "summary": summary, "recent": recent["items"] })
            )?
        );
        return Ok(());
    }

    println!(
        "LLM: {} ({})",
        summary["llm"]["model"].as_str().unwrap_or("-"),
        if summary["llm"]["configured"] == true {
            "configured"
        } else {
            "not configured"
        }
    );
    println!("Last {hours} h:");
    println!(
        "  {:<6} {:>10} {:>8} {:>8} {:>8}",
        "layer", "succeeded", "empty", "failed", "dropped"
    );
    for l in ["l1", "l2", "l3"] {
        let n = |s: &str| {
            summary["items"]
                .as_array()
                .into_iter()
                .flatten()
                .filter(|i| i["layer"] == l && i["status"] == s)
                .filter_map(|i| i["count"].as_u64())
                .sum::<u64>()
        };
        println!(
            "  {:<6} {:>10} {:>8} {:>8} {:>8}",
            l,
            n("succeeded"),
            n("empty"),
            n("failed"),
            n("dropped")
        );
    }
    let items = recent["items"].as_array().cloned().unwrap_or_default();
    println!("\nLatest {status} runs ({}):", items.len());
    for i in &items {
        println!(
            "  {}  {}  {}  {} ms  session {}",
            when(i["started_at_ms"].as_i64().unwrap_or(0)),
            i["layer"].as_str().unwrap_or("-"),
            i["model"].as_str().unwrap_or("-"),
            i["latency_ms"].as_i64().unwrap_or(0),
            i["session_id"].as_str().unwrap_or("-"),
        );
        if let Some(err) = i["error_message"].as_str() {
            println!("      {}", one_line(err, 200));
        }
    }
    Ok(())
}

pub async fn dead_letters(args: impl Iterator<Item = String>) -> Result<()> {
    let mut a = Args::new(args);
    if a.flag("--help") || a.flag("-h") {
        println!("{DEAD_LETTERS_HELP}");
        return Ok(());
    }
    let redrive = a.flag("redrive");
    a.flag("list");
    let json_out = a.flag("--json");
    let kind = a.value("--kind")?;
    let limit = a.number("--limit")?;
    let department = a.value("--department")?.unwrap_or_else(|| "default".into());
    a.finish()?;

    let core = Core::new(department)?;
    let mut body = json!({});
    if let Some(kind) = &kind {
        body["kind"] = json!(kind);
    }
    if redrive {
        body["limit"] = json!(limit.unwrap_or(10));
        let data = core.call("pipeline/dead-letter/redrive", body).await?;
        println!(
            "Requeued {} dead letter(s).",
            data["requeued"].as_u64().unwrap_or(0)
        );
        return Ok(());
    }

    body["limit"] = json!(limit.unwrap_or(50));
    let data = core.call("pipeline/dead-letter/list", body).await?;
    if json_out {
        println!("{}", serde_json::to_string_pretty(&data["items"])?);
        return Ok(());
    }
    let items = data["items"].as_array().cloned().unwrap_or_default();
    println!("{} dead letter(s), newest first:", items.len());
    for d in &items {
        let job = d["job"].as_object().and_then(|o| o.values().next());
        let subject = job
            .and_then(|j| {
                j["session_id"]
                    .as_str()
                    .or(j["task_id"].as_str())
                    .map(str::to_owned)
                    .or_else(|| j.get("profile").map(|p| p.to_string()))
            })
            .unwrap_or_default();
        println!(
            "  {}  {:<5}  attempts {}  {}",
            when(d["dead_at_ms"].as_i64().unwrap_or(0)),
            d["kind"].as_str().unwrap_or("-"),
            d["attempt"].as_u64().unwrap_or(0) + 1,
            subject,
        );
        if let Some(err) = d["error"].as_str() {
            println!("      {}", one_line(err, 200));
        }
    }
    Ok(())
}
