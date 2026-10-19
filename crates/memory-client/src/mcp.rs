//! Minimal stdio MCP server (newline-delimited JSON-RPC) exposing one tool,
//! `memory_recall`, so the agent can search memory on purpose, not only via
//! the automatic recall hook.

use std::io::{BufRead, Write};
use std::time::Duration;

use serde_json::{json, Value};

use crate::api::{recall_context, Api};
use crate::config::Config;

const DEFAULT_PROTOCOL: &str = "2025-06-18";

fn tools() -> Value {
    json!([{
        "name": "memory_recall",
        "description": "Search long-term memory (facts, preferences, past decisions, the user's persona) \
                        shared across sessions and machines. Use it when earlier context would help.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "query": {"type": "string", "description": "What to look for, in natural language or keywords"},
                "max_results": {"type": "integer", "minimum": 1, "maximum": 20}
            },
            "required": ["query"]
        }
    }])
}

fn call_tool(params: &Value) -> Value {
    let text_result = |text: String, is_error: bool| json!({"content": [{"type": "text", "text": text}], "isError": is_error});
    if params["name"] != "memory_recall" {
        return text_result(format!("unknown tool {}", params["name"]), true);
    }
    let args = &params["arguments"];
    let query = args["query"].as_str().unwrap_or_default();
    let max = args["max_results"].as_u64().map(|n| n.clamp(1, 20));
    let result = Config::load()
        .and_then(|c| Api::new(c, Duration::from_secs(15)))
        .and_then(|api| api.recall(query, max).map_err(|e| e.to_string()));
    match result {
        Ok(data) => {
            let context = recall_context(&data);
            text_result(
                if context.is_empty() {
                    "No relevant memories.".into()
                } else {
                    context
                },
                false,
            )
        }
        Err(e) => text_result(e, true),
    }
}

/// Answers one request; `None` for notifications.
pub fn handle(request: &Value) -> Option<Value> {
    let id = request.get("id")?.clone();
    let params = &request["params"];
    let result = match request["method"].as_str().unwrap_or_default() {
        "initialize" => json!({
            "protocolVersion": params["protocolVersion"].as_str().unwrap_or(DEFAULT_PROTOCOL),
            "capabilities": {"tools": {}},
            "serverInfo": {"name": "agent-memory", "version": env!("CARGO_PKG_VERSION")}
        }),
        "ping" => json!({}),
        "tools/list" => json!({"tools": tools()}),
        "tools/call" => call_tool(params),
        other => {
            return Some(json!({"jsonrpc": "2.0", "id": id,
                "error": {"code": -32601, "message": format!("method not found: {other}")}}))
        }
    };
    Some(json!({"jsonrpc": "2.0", "id": id, "result": result}))
}

pub fn serve() {
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();
    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        if line.trim().is_empty() {
            continue;
        }
        let reply = match serde_json::from_str::<Value>(&line) {
            Ok(request) => handle(&request),
            Err(_) => Some(
                json!({"jsonrpc": "2.0", "id": null, "error": {"code": -32700, "message": "parse error"}}),
            ),
        };
        if let Some(reply) = reply {
            let _ = writeln!(stdout, "{reply}");
            let _ = stdout.flush();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn handshake_and_listing() {
        let init = handle(&json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-03-26"}})).unwrap();
        assert_eq!(init["result"]["protocolVersion"], "2025-03-26");
        assert!(handle(&json!({"jsonrpc":"2.0","method":"notifications/initialized"})).is_none());
        let list = handle(&json!({"jsonrpc":"2.0","id":2,"method":"tools/list"})).unwrap();
        assert_eq!(list["result"]["tools"][0]["name"], "memory_recall");
        let unknown = handle(&json!({"jsonrpc":"2.0","id":3,"method":"resources/list"})).unwrap();
        assert_eq!(unknown["error"]["code"], -32601);
    }
}
