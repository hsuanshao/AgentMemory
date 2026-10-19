//! `memory-server`: the MemoryCore gateway binary.

use anyhow::{Context, Result};
use tracing::info;
use tracing_subscriber::EnvFilter;

use memory_server::config::Config;
use memory_server::wiring;

#[tokio::main]
async fn main() -> Result<()> {
    if std::env::args().nth(1).as_deref() == Some("healthcheck") {
        return healthcheck();
    }
    let json_logs = std::env::var("LOG_FORMAT").is_ok_and(|f| f == "json");
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("info,tower_http=warn"));
    if json_logs {
        tracing_subscriber::fmt()
            .with_env_filter(filter)
            .json()
            .init();
    } else {
        tracing_subscriber::fmt().with_env_filter(filter).init();
    }

    if std::env::args().nth(1).as_deref() == Some("migrate-storage") {
        return memory_server::migrate_storage::run(std::env::args().skip(2)).await;
    }

    let config = Config::load()?;
    let app = wiring::build(&config).await?;
    let addr = format!("{}:{}", config.server.host, config.server.port);
    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .with_context(|| format!("binding {addr}"))?;
    info!(
        %addr,
        auth = config.server.api_key.is_some(),
        llm = app.state.health.llm_configured,
        embedding = app.state.health.embedding_enabled,
        "memory-core listening"
    );

    memory_gateway::serve(listener, app.state.clone(), shutdown_signal()).await?;
    for task in app.background {
        task.abort();
    }
    info!("memory-core stopped");
    Ok(())
}

async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let terminate = async {
        if let Ok(mut s) = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        {
            s.recv().await;
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();
    tokio::select! { _ = ctrl_c => {}, _ = terminate => {} }
}

/// `memory-server healthcheck`: exits 0 when the local `/health` answers 200.
/// Used by container health checks (no curl in the runtime image).
fn healthcheck() -> Result<()> {
    use std::io::{Read, Write};
    let port = std::env::var("AGENT_MEMORY_PORT").unwrap_or_else(|_| "8420".into());
    let mut stream = std::net::TcpStream::connect(format!("127.0.0.1:{port}"))?;
    stream.set_read_timeout(Some(std::time::Duration::from_secs(3)))?;
    stream.write_all(b"GET /health HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")?;
    let mut response = String::new();
    stream.read_to_string(&mut response)?;
    anyhow::ensure!(
        response.starts_with("HTTP/1.1 200"),
        "unhealthy: {}",
        response.lines().next().unwrap_or("")
    );
    Ok(())
}
