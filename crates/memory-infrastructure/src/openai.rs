//! OpenAI-compatible chat-completion and embedding clients.

use std::time::Duration;

use async_trait::async_trait;
use memory_application::ports::system::{
    ChatMessage, EmbeddingProvider, LlmClient, LLM_TOOL_REQUEST,
};
use memory_domain::error::{DomainError, DomainResult, ErrorKind};
use serde::Deserialize;
use serde_json::json;

#[derive(Debug, Clone)]
pub struct LlmSettings {
    pub base_url: String,
    pub api_key: String,
    pub model: String,
    pub max_tokens: u32,
    pub timeout: Duration,
}

pub struct OpenAiLlm {
    http: reqwest::Client,
    settings: LlmSettings,
}

impl OpenAiLlm {
    pub fn new(settings: LlmSettings) -> Self {
        let http = reqwest::Client::builder()
            .timeout(settings.timeout)
            .build()
            .expect("reqwest client");
        Self { http, settings }
    }
}

fn llm_unavailable(detail: impl Into<String>) -> DomainError {
    DomainError::new(ErrorKind::Unavailable, "LLM_UNAVAILABLE", detail)
}

#[derive(Deserialize)]
struct ChatResponse {
    choices: Vec<ChatChoice>,
}

#[derive(Deserialize)]
struct ChatChoice {
    message: ResponseMessage,
    #[serde(default)]
    finish_reason: Option<String>,
}

/// An assistant message as returned: `content` is null when the model only
/// called a tool.
#[derive(Deserialize)]
struct ResponseMessage {
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    tool_calls: Option<Vec<serde_json::Value>>,
    #[serde(default)]
    function_call: Option<serde_json::Value>,
}

#[async_trait]
impl LlmClient for OpenAiLlm {
    fn is_configured(&self) -> bool {
        !self.settings.api_key.is_empty() && !self.settings.base_url.is_empty()
    }

    fn model(&self) -> &str {
        &self.settings.model
    }

    async fn complete(&self, messages: &[ChatMessage]) -> DomainResult<String> {
        if !self.is_configured() {
            return Err(llm_unavailable("LLM is not configured"));
        }
        let url = format!(
            "{}/chat/completions",
            self.settings.base_url.trim_end_matches('/')
        );
        let body = json!({
            "model": self.settings.model,
            "messages": messages,
            "max_tokens": self.settings.max_tokens,
            "temperature": 0.2,
        });
        let response = self
            .http
            .post(url)
            .bearer_auth(&self.settings.api_key)
            .json(&body)
            .send()
            .await
            .map_err(|e| llm_unavailable(format!("LLM request failed: {e}")))?;
        let status = response.status();
        if !status.is_success() {
            let text = response.text().await.unwrap_or_default();
            let snippet: String = text.chars().take(300).collect();
            return Err(llm_unavailable(format!("LLM returned {status}: {snippet}")));
        }
        let parsed: ChatResponse = response
            .json()
            .await
            .map_err(|e| llm_unavailable(format!("bad LLM response: {e}")))?;
        parsed
            .choices
            .into_iter()
            .next()
            .ok_or_else(|| llm_unavailable("LLM returned no choices"))
            .and_then(answer)
    }
}

/// Markers some chat templates use when a model writes a tool call as text.
const TOOL_CALL_MARKUP: &[(&str, &str)] = &[
    ("<tool_call>", "</tool_call>"),
    ("<|tool_call|>", "<|/tool_call|>"),
    ("<function_call>", "</function_call>"),
];

/// The answer in a choice. We never send `tools`, so a tool call means the
/// model tried to act instead of answering (#37): that fails with
/// `LLM_TOOL_REQUEST` and is not retried. Only an explicit tool call counts,
/// and only when nothing else was written: an answer next to one is kept,
/// with the tool-call markup removed.
fn answer(choice: ChatChoice) -> DomainResult<String> {
    let content = choice.message.content.unwrap_or_default();
    let remainder = without_tool_markup(&content);
    let called_tool = matches!(
        choice.finish_reason.as_deref(),
        Some("tool_calls" | "function_call")
    ) || choice
        .message
        .tool_calls
        .as_ref()
        .is_some_and(|c| !c.is_empty())
        || choice.message.function_call.is_some()
        || remainder.len() < content.trim().len();
    if !remainder.is_empty() {
        // Without the markup: the steps parse from the first `{` or `[`, so a
        // tool call left in front of the answer would break the parse.
        return Ok(remainder);
    }
    if called_tool {
        return Err(DomainError::new(
            ErrorKind::Unprocessable,
            LLM_TOOL_REQUEST,
            format!(
                "the model returned a tool call instead of an answer (finish_reason: {})",
                choice.finish_reason.as_deref().unwrap_or("none")
            ),
        ));
    }
    Err(llm_unavailable("LLM returned no content"))
}

/// The text with every tool-call markup block removed, trimmed. An unclosed
/// block runs to the end.
fn without_tool_markup(text: &str) -> String {
    let mut rest = text.to_owned();
    for (open, close) in TOOL_CALL_MARKUP {
        while let Some(start) = rest.find(open) {
            let end = rest[start..]
                .find(close)
                .map_or(rest.len(), |i| start + i + close.len());
            rest.replace_range(start..end, "");
        }
    }
    rest.trim().to_owned()
}

#[derive(Debug, Clone)]
pub struct EmbeddingSettings {
    pub base_url: String,
    pub api_key: String,
    pub model: String,
    pub dimensions: usize,
    /// Per request.
    pub timeout: Duration,
    /// Requests in flight at once; more wait their turn. A burst of L1
    /// writes then queues here instead of overloading the service and
    /// timing out.
    pub max_concurrency: usize,
}

const MAX_BATCH: usize = 256;
const MAX_INPUT_CHARS: usize = 5000;

pub struct OpenAiEmbedding {
    http: reqwest::Client,
    permits: tokio::sync::Semaphore,
    settings: EmbeddingSettings,
}

impl OpenAiEmbedding {
    pub fn new(settings: EmbeddingSettings) -> Self {
        let http = reqwest::Client::builder()
            .timeout(settings.timeout)
            .build()
            .expect("reqwest client");
        let permits = tokio::sync::Semaphore::new(settings.max_concurrency.max(1));
        Self {
            http,
            permits,
            settings,
        }
    }

    async fn embed_batch(&self, batch: &[String]) -> DomainResult<Vec<Vec<f32>>> {
        #[derive(Deserialize)]
        struct Item {
            index: usize,
            embedding: Vec<f32>,
        }
        #[derive(Deserialize)]
        struct Response {
            data: Vec<Item>,
        }
        let input: Vec<String> = batch
            .iter()
            .map(|t| t.chars().take(MAX_INPUT_CHARS).collect())
            .collect();
        let url = format!(
            "{}/embeddings",
            self.settings.base_url.trim_end_matches('/')
        );
        let response = self
            .http
            .post(url)
            .bearer_auth(&self.settings.api_key)
            .json(&json!({"input": input, "model": self.settings.model, "dimensions": self.settings.dimensions}))
            .send()
            .await
            .map_err(|e| DomainError::unavailable(format!("embedding request failed: {e}")))?;
        let status = response.status();
        if !status.is_success() {
            let detail = format!("embedding service returned {status}");
            // The input was refused (too long, malformed, …): retrying the
            // same text will not help, but the service is up. Anything else
            // (5xx, 429, auth) is the service's state, not the input's.
            return Err(match status.as_u16() {
                400 | 413 | 422 => DomainError::invalid(detail),
                _ => DomainError::unavailable(detail),
            });
        }
        let mut parsed: Response = response
            .json()
            .await
            .map_err(|e| DomainError::unavailable(format!("bad embedding response: {e}")))?;
        parsed.data.sort_by_key(|d| d.index);
        Ok(parsed
            .data
            .into_iter()
            .map(|d| normalise(d.embedding))
            .collect())
    }
}

/// Replaces non-finite values and L2-normalises.
fn normalise(mut v: Vec<f32>) -> Vec<f32> {
    for x in v.iter_mut() {
        if !x.is_finite() {
            *x = 0.0;
        }
    }
    let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > 1e-10 {
        v.iter_mut().for_each(|x| *x /= norm);
    }
    v
}

#[async_trait]
impl EmbeddingProvider for OpenAiEmbedding {
    fn dimensions(&self) -> Option<usize> {
        Some(self.settings.dimensions)
    }

    async fn embed(&self, texts: &[String]) -> DomainResult<Vec<Vec<f32>>> {
        let mut out = Vec::with_capacity(texts.len());
        for batch in texts.chunks(MAX_BATCH) {
            // The semaphore is never closed.
            let _permit = self.permits.acquire().await.expect("open semaphore");
            out.extend(self.embed_batch(batch).await?);
        }
        Ok(out)
    }
}

/// Embedding disabled: recall uses BM25 only.
pub struct NoEmbedding;

#[async_trait]
impl EmbeddingProvider for NoEmbedding {
    fn dimensions(&self) -> Option<usize> {
        None
    }

    async fn embed(&self, _texts: &[String]) -> DomainResult<Vec<Vec<f32>>> {
        Ok(Vec::new())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn choice(raw: serde_json::Value) -> ChatChoice {
        serde_json::from_value(raw).unwrap()
    }

    /// An embedding service that answers every request with `status`.
    async fn embedding_service(status: u16) -> String {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            while let Ok((mut socket, _)) = listener.accept().await {
                let mut buf = [0u8; 4096];
                let _ = socket.read(&mut buf).await;
                let reply = format!(
                    "HTTP/1.1 {status} X\r\ncontent-length: 0\r\nconnection: close\r\n\r\n"
                );
                let _ = socket.write_all(reply.as_bytes()).await;
            }
        });
        format!("http://{addr}/v1")
    }

    #[tokio::test]
    async fn a_refused_input_is_invalid_and_a_failing_service_unavailable() {
        for (status, kind) in [
            (400, ErrorKind::InvalidInput),
            (413, ErrorKind::InvalidInput),
            (422, ErrorKind::InvalidInput),
            (429, ErrorKind::Unavailable),
            (500, ErrorKind::Unavailable),
            (503, ErrorKind::Unavailable),
        ] {
            let embedder = OpenAiEmbedding::new(EmbeddingSettings {
                base_url: embedding_service(status).await,
                api_key: "k".into(),
                model: "m".into(),
                dimensions: 2,
                timeout: Duration::from_secs(5),
                max_concurrency: 1,
            });
            let err = embedder.embed(&["text".into()]).await.unwrap_err();
            assert_eq!(err.kind, kind, "{status}: {err:?}");
        }
    }

    #[test]
    fn a_tool_call_without_an_answer_is_a_tool_request() {
        for raw in [
            json!({"message": {"content": null, "tool_calls": [{"id": "c1", "type": "function",
                "function": {"name": "bash", "arguments": "{\"cmd\":\"ls\"}"}}]},
                "finish_reason": "tool_calls"}),
            json!({"message": {"content": null, "function_call": {"name": "bash"}},
                "finish_reason": "function_call"}),
            json!({"message": {"content": "<tool_call>{\"name\": \"bash\"}</tool_call>"},
                "finish_reason": "stop"}),
            json!({"message": {"content": "  <|tool_call|>{\"name\": \"ls\"}"}}),
        ] {
            let err = answer(choice(raw.clone())).unwrap_err();
            assert_eq!(err.code, LLM_TOOL_REQUEST, "{raw}");
            assert!(
                memory_application::ports::pipeline::failure_is_deterministic(&err.to_string())
            );
        }
    }

    #[test]
    fn an_answer_is_kept_even_next_to_a_tool_call() {
        let plain = answer(choice(
            json!({"message": {"content": "[]"}, "finish_reason": "stop"}),
        ));
        assert_eq!(plain.unwrap(), "[]");
        let both = answer(choice(json!({
            "message": {"content": "<tool_call>{}</tool_call>\n[{\"scene_name\": \"x\"}]",
                "tool_calls": [{"id": "c1"}]},
            "finish_reason": "tool_calls"})));
        // Returned without the markup, so the step's parser reads the answer.
        let both = both.unwrap();
        assert_eq!(both, "[{\"scene_name\": \"x\"}]");
        let parsed: serde_json::Value =
            memory_application::pipeline::llm_json::parse_llm_json(&both, '[').unwrap();
        assert_eq!(parsed[0]["scene_name"], "x");
        // Mentioning tools is not calling one.
        let prose = answer(choice(
            json!({"message": {"content": "use the tool_calls field"}}),
        ));
        assert!(prose.is_ok());
    }

    #[test]
    fn an_empty_answer_stays_retryable() {
        let err = answer(choice(
            json!({"message": {"content": null}, "finish_reason": "length"}),
        ))
        .unwrap_err();
        assert_eq!(err.code, "LLM_UNAVAILABLE");
        let err = answer(choice(json!({"message": {"content": "   "}}))).unwrap_err();
        assert_eq!(err.code, "LLM_UNAVAILABLE");
    }

    #[test]
    fn normalise_produces_unit_vectors_and_scrubs_nan() {
        let v = normalise(vec![3.0, 4.0, f32::NAN]);
        assert!((v[0] - 0.6).abs() < 1e-6 && (v[1] - 0.8).abs() < 1e-6 && v[2] == 0.0);
    }
}
