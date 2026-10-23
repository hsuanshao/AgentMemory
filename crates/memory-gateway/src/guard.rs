//! Throttles credential guessing: every refused credential is counted per
//! client and written to the audit log; a client that keeps failing waits
//! longer for each refusal, then is refused outright for a while (see
//! `ThrottlePolicy`). Requests that succeed are not slowed.

use std::net::SocketAddr;

use axum::extract::{ConnectInfo, OriginalUri, Request, State};
use axum::http::{header, HeaderValue, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use memory_application::audit::{Attempt, Gate};
use memory_domain::error::{DomainError, ErrorKind};
use memory_domain::meta::digest_user_key;

use crate::client;
use crate::context::{Ctx, DEPARTMENT_ID, USER_KEY};
use crate::envelope::ApiError;
use crate::state::SharedState;

/// Set on a response that refused a credential without answering 401
/// (`meta/auth/verify` answers "not valid" with 200).
#[derive(Debug, Clone)]
pub struct CredentialRejected {
    pub credential: Option<String>,
}

/// Who sent the request, for handlers that record an audit event.
#[derive(Debug, Clone, Default)]
pub struct Origin {
    pub source: String,
    pub forwarded_for: Option<String>,
    pub user_agent: Option<String>,
}

impl Origin {
    pub fn attempt(&self, ctx: &Ctx, path: &str, credential: Option<String>) -> Attempt {
        Attempt {
            source: self.source.clone(),
            forwarded_for: self.forwarded_for.clone(),
            user_agent: self.user_agent.clone(),
            department: ctx.header(DEPARTMENT_ID),
            path: path.to_owned(),
            credential,
            request_id: ctx.request_id.clone(),
        }
    }
}

/// Names a user key without revealing it: the start of its digest.
pub fn describe_user_key(key: &str) -> String {
    let digest = digest_user_key(key);
    format!("user key {}", &digest[..digest.len().min(15)])
}

fn presented_credential(req: &Request) -> Option<String> {
    let headers = req.headers();
    if let Some(key) = headers.get(USER_KEY).and_then(|v| v.to_str().ok()) {
        return Some(describe_user_key(key.trim()));
    }
    headers
        .contains_key(header::AUTHORIZATION)
        .then(|| "server API key".to_owned())
}

fn header_text(req: &Request, name: impl header::AsHeaderName) -> Option<String> {
    req.headers()
        .get(name)
        .and_then(|v| v.to_str().ok())
        .map(|v| v.chars().take(256).collect())
}

pub async fn guard_credentials(
    State(s): State<SharedState>,
    ctx: Ctx,
    mut req: Request,
    next: Next,
) -> Response {
    // Only a server started by `serve` knows its peers; a router embedded
    // without connection info is not throttled.
    let Some(peer) = req
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|c| c.0.ip())
    else {
        return next.run(req).await;
    };
    let forwarded = header_text(&req, "x-forwarded-for");
    let client = client::resolve(&s.settings.trusted_proxies, peer, forwarded.as_deref()).await;
    let source = client.source();
    let guard = &s.services.guard;
    if let Gate::Locked { retry_after } = guard.gate(&source).await {
        let mut resp = ApiError::from_domain(
            &ctx.request_id,
            DomainError::new(
                ErrorKind::QuotaExceeded,
                "too_many_failures",
                "too many failed credentials from this address; try again later",
            ),
        )
        .into_response();
        if let Ok(v) = HeaderValue::from_str(&retry_after.as_secs().max(1).to_string()) {
            resp.headers_mut().insert(header::RETRY_AFTER, v);
        }
        return resp;
    }
    // One request id for the audit record and the handlers.
    if !req.headers().contains_key("x-request-id") {
        if let Ok(v) = HeaderValue::from_str(&ctx.request_id) {
            req.headers_mut().insert("x-request-id", v);
        }
    }
    let credential = presented_credential(&req);
    let path = req
        .extensions()
        .get::<OriginalUri>()
        .map_or_else(|| req.uri().path().to_owned(), |u| u.path().to_owned());
    let department = header_text(&req, DEPARTMENT_ID);
    let user_agent = header_text(&req, header::USER_AGENT);
    req.extensions_mut().insert(Origin {
        source: source.clone(),
        forwarded_for: client.forwarded_for.clone(),
        user_agent: user_agent.clone(),
    });

    let resp = next.run(req).await;

    let marked = resp.extensions().get::<CredentialRejected>().cloned();
    let refused = resp.status() == StatusCode::UNAUTHORIZED && credential.is_some();
    if marked.is_none() && !refused {
        return resp;
    }
    let delay = guard
        .rejected(Attempt {
            source,
            forwarded_for: client.forwarded_for,
            user_agent,
            department,
            path,
            credential: marked.and_then(|m| m.credential).or(credential),
            request_id: ctx.request_id.clone(),
        })
        .await;
    if !delay.is_zero() {
        tokio::time::sleep(delay).await;
    }
    resp
}
