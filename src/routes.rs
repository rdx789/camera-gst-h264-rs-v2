use axum::{
    extract::{ws::WebSocketUpgrade, State},
    http::StatusCode,
    response::IntoResponse,
    routing::get,
    Router,
};
use std::sync::atomic::Ordering;
use tower_http::{cors::{AllowOrigin, CorsLayer}, services::ServeDir};
use tracing::info;
use uuid::Uuid;

use crate::config::Config;
use crate::types::AppState;

/// Health check — returns `"ok"`.
pub async fn health() -> &'static str {
    "ok"
}

/// Prometheus metrics — renders the current snapshot.
pub async fn metrics_handler(State(state): State<AppState>) -> String {
    state.prometheus_handle.render()
}

/// WebSocket upgrade handler.
///
/// Atomically checks and reserves a peer slot before upgrading so concurrent
/// arrivals cannot all pass the limit check before any of them are inserted
/// into the peers map (TOCTOU race in the previous `peers.len() >= max_peers`
/// check).
pub async fn ws_handler(
    ws: WebSocketUpgrade,
    State(state): State<AppState>,
) -> axum::response::Response {
    // fetch_update returns Ok(old_value) if the predicate returned Some,
    // or Err(current_value) if the predicate returned None (limit reached).
    let reserved = state.peer_count.fetch_update(
        Ordering::SeqCst,
        Ordering::SeqCst,
        |n| if n < state.max_peers { Some(n + 1) } else { None },
    );

    if reserved.is_err() {
        tracing::warn!("peer limit ({}) reached, rejecting connection", state.max_peers);
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    }

    let peer_id = Uuid::new_v4().to_string();
    info!("new peer: {peer_id}");

    ws.on_upgrade(move |socket| crate::peer::peer_task(socket, state, peer_id))
}

/// Assemble the HTTP router with all routes and CORS.
pub fn build_router(state: AppState, config: &Config) -> Router {
    let allowed_origins: Vec<axum::http::HeaderValue> = config
        .allowed_origins
        .iter()
        .map(|s| s.parse().expect("invalid origin in ALLOWED_ORIGINS"))
        .collect();

    Router::new()
        .route("/ws",      get(ws_handler))
        .route("/health",  get(health))
        .route("/metrics", get(metrics_handler))
        .nest_service("/", ServeDir::new("static"))
        .layer(
            CorsLayer::new()
                .allow_origin(AllowOrigin::list(allowed_origins))
                .allow_methods([axum::http::Method::GET]),
        )
        .with_state(state)
}
