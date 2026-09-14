//! The health endpoint for liveness and readiness probes.

use axum::Router;
use axum::routing::get;
use tokio::net::TcpListener;

/// `GET /healthz` answers `200 ok`; every other path is `404`.
pub fn router() -> Router {
    Router::new().route("/healthz", get(|| async { "ok" }))
}

/// Serve health checks on `listener` until the task is dropped.
pub async fn serve(listener: TcpListener) -> std::io::Result<()> {
    axum::serve(listener, router()).await
}
