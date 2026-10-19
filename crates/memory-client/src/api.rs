//! HTTP calls to Core's user-scoped data plane. Only the user key is sent;
//! the client never holds the instance API key.

use std::time::Duration;

use serde_json::{json, Value};

use crate::config::Config;

#[derive(Debug)]
pub enum ApiError {
    /// Worth retrying later: network failure, 5xx, 429, or an auth problem
    /// the user fixes with `amem login`.
    Retryable(String),
    /// The request itself is bad; retrying cannot help.
    Rejected(String),
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Retryable(m) | Self::Rejected(m) => f.write_str(m),
        }
    }
}

pub struct Api {
    cfg: Config,
    http: reqwest::blocking::Client,
}

impl Api {
    pub fn new(cfg: Config, timeout: Duration) -> Result<Self, String> {
        if let Some(missing) = cfg.missing().first() {
            return Err(format!(
                "not configured: {missing} is empty (run `amem login`)"
            ));
        }
        let http = reqwest::blocking::Client::builder()
            .timeout(timeout)
            .build()
            .map_err(|e| e.to_string())?;
        Ok(Self { cfg, http })
    }

    pub fn config(&self) -> &Config {
        &self.cfg
    }

    /// Adds the configured team and agent to `body`.
    pub fn scoped(&self, mut body: Value) -> Value {
        body["team_id"] = json!(self.cfg.team_id);
        body["agent_id"] = json!(self.cfg.agent_id);
        body
    }

    /// `POST /v3/<path>` → the envelope's `data`.
    pub fn post(&self, path: &str, body: &Value) -> Result<Value, ApiError> {
        let res = self
            .http
            .post(format!("{}/v3/{path}", self.cfg.server_url))
            .header("x-memory-user-key", &self.cfg.user_key)
            .header("x-memory-service-id", &self.cfg.service_id)
            .json(body)
            .send()
            .map_err(|e| ApiError::Retryable(format!("{path}: {e}")))?;
        let status = res.status().as_u16();
        let envelope: Value = res.json().unwrap_or(Value::Null);
        if (200..300).contains(&status) && envelope["code"] == 0 {
            return Ok(envelope["data"].clone());
        }
        let message = format!(
            "{path}: HTTP {status} {}",
            envelope["message"].as_str().unwrap_or("(no message)")
        );
        match status {
            401 | 403 | 408 | 429 | 500..=599 => Err(ApiError::Retryable(message)),
            _ => Err(ApiError::Rejected(message)),
        }
    }

    pub fn recall(&self, query: &str, max_results: Option<u64>) -> Result<Value, ApiError> {
        let mut body = self.scoped(json!({"query": query}));
        if let Some(n) = max_results {
            body["max_results"] = json!(n);
        }
        self.post("recall", &body)
    }
}

/// The context block a host injects before the model runs ("" if nothing).
pub fn recall_context(data: &Value) -> String {
    ["prepend_context", "append_system_context"]
        .iter()
        .filter_map(|k| data[*k].as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn context_joins_both_blocks() {
        let data = json!({"prepend_context": "<relevant-memories>a</relevant-memories>", "append_system_context": " <user-persona>p</user-persona> "});
        assert_eq!(
            recall_context(&data),
            "<relevant-memories>a</relevant-memories>\n\n<user-persona>p</user-persona>"
        );
        assert_eq!(recall_context(&json!({"prepend_context": null})), "");
    }
}
