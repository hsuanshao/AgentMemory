//! Interface adapters: the v3 HTTP API over the application use cases.

pub mod authz;
pub mod client;
pub mod context;
pub mod envelope;
pub mod guard;
pub mod handlers;
pub mod router;
pub mod state;

pub use router::build_router;
pub use state::{AppState, GatewaySettings, HealthFacts, Services, SharedState};

/// Serves the API on `listener` until `shutdown` resolves. Peer addresses
/// reach the handlers, for throttling credential failures per client.
pub async fn serve(
    listener: tokio::net::TcpListener,
    state: SharedState,
    shutdown: impl std::future::Future<Output = ()> + Send + 'static,
) -> std::io::Result<()> {
    axum::serve(
        listener,
        build_router(state).into_make_service_with_connect_info::<std::net::SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown)
    .await
}
