//! HTTP API module - REST endpoints and WebSocket

mod websocket;

use std::sync::Arc;

use axum::{
    extract::State,
    http::StatusCode,
    response::IntoResponse,
    routing::get,
    Json, Router,
};
use serde::Serialize;

use crate::db::Database;
pub use websocket::{ConnectionManager, PlayerSession, ServerMessage};

/// Shared application state
#[derive(Clone)]
pub struct AppState {
    pub db: Arc<Database>,
    pub connections: Arc<ConnectionManager>,
}

/// Build the API router
pub fn router(db: Arc<Database>) -> Router {
    let connections = Arc::new(ConnectionManager::new());
    let state = AppState { db, connections };

    Router::new()
        .route("/health", get(health_check))
        .route("/", get(root))
        .route("/ws", get(websocket::ws_handler))
        .with_state(state)
}

/// Root endpoint
async fn root() -> impl IntoResponse {
    Json(RootResponse {
        name: "mudd",
        version: env!("CARGO_PKG_VERSION"),
    })
}

#[derive(Serialize)]
struct RootResponse {
    name: &'static str,
    version: &'static str,
}

/// Health check endpoint
async fn health_check(State(state): State<AppState>) -> impl IntoResponse {
    match state.db.health_check().await {
        Ok(()) => (
            StatusCode::OK,
            Json(HealthResponse {
                status: "healthy",
                database: "ok",
            }),
        ),
        Err(_) => (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(HealthResponse {
                status: "unhealthy",
                database: "error",
            }),
        ),
    }
}

#[derive(Serialize)]
struct HealthResponse {
    status: &'static str,
    database: &'static str,
}
