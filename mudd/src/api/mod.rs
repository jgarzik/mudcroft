//! HTTP API module - REST endpoints and WebSocket

mod auth;
mod universe;
mod websocket;

use std::sync::Arc;

use axum::{extract::State, http::StatusCode, response::IntoResponse, routing::get, Json, Router};
use serde::Serialize;
use tokio::sync::RwLock;

use crate::credits::CreditManager;
use crate::db::Database;
use crate::lua::{ActionRegistry, MessageQueue};
use crate::objects::{ClassRegistry, ObjectStore};
use crate::permissions::PermissionManager;
use crate::timers::TimerManager;
use crate::venice::VeniceClient;
pub use websocket::{ConnectionManager, PlayerSession, ServerMessage};

/// Shared application state
#[derive(Clone)]
pub struct AppState {
    pub db: Arc<Database>,
    pub connections: Arc<ConnectionManager>,
    pub object_store: Arc<ObjectStore>,
    pub classes: Arc<RwLock<ClassRegistry>>,
    pub actions: Arc<ActionRegistry>,
    pub messages: Arc<MessageQueue>,
    pub permissions: Arc<PermissionManager>,
    pub timers: Arc<TimerManager>,
    pub credits: Arc<CreditManager>,
    pub venice: Arc<VeniceClient>,
}

/// Build the API router
pub async fn router(db: Arc<Database>) -> Router {
    let connections = Arc::new(ConnectionManager::new());
    let object_store = Arc::new(ObjectStore::new(db.pool().clone()));
    let mut class_registry = ClassRegistry::with_db(db.pool().clone());
    let actions = Arc::new(ActionRegistry::new());
    let messages = Arc::new(MessageQueue::new());
    let permissions = Arc::new(PermissionManager::with_db(db.pool().clone()));
    let timers = Arc::new(TimerManager::new(Some(db.pool().clone())));
    let credits = Arc::new(CreditManager::new(Some(db.pool().clone())));
    let venice = Arc::new(VeniceClient::new()); // No API key

    // Load persisted data on startup
    if let Err(e) = timers.load_from_db().await {
        tracing::warn!("Failed to load timers from database: {}", e);
    }
    if let Err(e) = permissions.load_builder_regions().await {
        tracing::warn!("Failed to load builder regions from database: {}", e);
    }
    if let Err(e) = class_registry.load_from_db().await {
        tracing::warn!("Failed to load classes from database: {}", e);
    }

    let classes = Arc::new(RwLock::new(class_registry));

    let state = AppState {
        db,
        connections,
        object_store,
        classes,
        actions,
        messages,
        permissions,
        timers,
        credits,
        venice,
    };

    Router::new()
        .route("/health", get(health_check))
        .route("/", get(root))
        .route("/ws", get(websocket::ws_handler))
        .merge(auth::router())
        .merge(universe::router())
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
