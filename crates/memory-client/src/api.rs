//! HTTP calls to Core's user-scoped data plane. Only the user key is sent;
//! the client never holds the server API key.

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
        // A redirect is not followed: it would take the user key along to
        // wherever it points.
        let http = reqwest::blocking::Client::builder()
            .timeout(timeout)
            .redirect(reqwest::redirect::Policy::none())
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
            .header("x-memory-department-id", &self.cfg.department_id)
            .json(body)
            .send()
            .map_err(|e| ApiError::Retryable(format!("{path}: {e}")))?;
        let status = res.status().as_u16();
        if res.status().is_redirection() {
            let to = res
                .headers()
                .get(reqwest::header::LOCATION)
                .and_then(|v| v.to_str().ok())
                .unwrap_or("elsewhere");
            return Err(ApiError::Retryable(format!(
                "{path}: HTTP {status}: the server redirects to {to}; set that address with `amem login` (redirects are not followed)"
            )));
        }
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

    /// A one-shot HTTP server answering every request with `response`;
    /// returns its address and whether a request arrived.
    fn serve_once(response: &'static str) -> (String, std::sync::mpsc::Receiver<String>) {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = format!("http://{}", listener.local_addr().unwrap());
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                let mut buf = [0u8; 4096];
                let n = stream.read(&mut buf).unwrap_or(0);
                let _ = tx.send(String::from_utf8_lossy(&buf[..n]).into_owned());
                let _ = stream.write_all(response.as_bytes());
            }
        });
        (addr, rx)
    }

    #[test]
    fn a_redirect_is_not_followed_with_the_key() {
        let (elsewhere, reached) = serve_once("HTTP/1.1 200 OK\r\ncontent-length: 2\r\n\r\n{}");
        let redirect: &'static str = Box::leak(
            format!("HTTP/1.1 307 Temporary Redirect\r\nlocation: {elsewhere}/v3/recall\r\ncontent-length: 0\r\n\r\n")
                .into_boxed_str(),
        );
        let (server, _) = serve_once(redirect);
        let api = Api::new(
            Config {
                server_url: server,
                department_id: "default".into(),
                user_key: "sk-mem-secret".into(),
                team_id: "t".into(),
                agent_id: "a".into(),
                ..Config::default()
            },
            Duration::from_secs(5),
        )
        .unwrap();
        let err = api.recall("q", None).unwrap_err();
        assert!(
            matches!(err, ApiError::Retryable(ref m) if m.contains("redirects to")),
            "{err}"
        );
        assert!(
            reached.recv_timeout(Duration::from_millis(300)).is_err(),
            "the key went to the redirect"
        );
    }

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
