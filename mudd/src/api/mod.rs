//! HTTP API module - REST endpoints and WebSocket

mod auth;
mod combat;
mod devices;
mod effect_scheduler;
mod equipment;
mod gateway;
mod images;
pub mod payments;
mod primary;
mod ratelimit;
pub mod scripting;
mod universe;
mod websocket;

use std::sync::Arc;

use axum::{extract::State, http::StatusCode, response::IntoResponse, routing::get, Json, Router};
use serde::Serialize;
use tokio::sync::RwLock;

use crate::auth::accounts::AccountService;
use crate::credits::CreditManager;
use crate::db::Database;
use crate::images::ImageStore;
use crate::lua::ActionRegistry;
use crate::objects::{ClassRegistry, ObjectStore};
use crate::permissions::PermissionManager;
use crate::player::PlayerManager;
use crate::raft::RaftWriter;
use crate::theme::ThemeRegistry;
use crate::timers::TimerManager;
use crate::venice::VeniceClient;
pub use websocket::{ConnectionManager, PlayerSession, ServerMessage};

/// Shared application state
#[derive(Clone)]
pub struct AppState {
    pub db: Arc<Database>,
    pub raft_writer: Arc<RaftWriter>,
    pub connections: Arc<ConnectionManager>,
    pub object_store: Arc<ObjectStore>,
    pub classes: Arc<RwLock<ClassRegistry>>,
    pub actions: Arc<ActionRegistry>,
    pub permissions: Arc<PermissionManager>,
    pub player_manager: Arc<PlayerManager>,
    pub timers: Arc<TimerManager>,
    pub credits: Arc<CreditManager>,
    pub venice: Arc<VeniceClient>,
    pub image_store: Arc<ImageStore>,
    pub themes: Arc<ThemeRegistry>,
    /// Login/registration limits: per client IP (twice the configured
    /// rate) and per username (login; the configured rate)
    pub auth_by_ip: Arc<ratelimit::RateLimiter>,
    pub auth_by_user: Arc<ratelimit::RateLimiter>,
    /// Failed logins per username across all clients (ten times the
    /// configured rate): beyond it attempts for the name go one at a time,
    /// a second apart (the owner still gets in, queued)
    pub auth_by_user_global: Arc<ratelimit::RateLimiter>,
    /// The hard cap across all clients (a hundred times the rate): refused
    pub auth_by_user_cap: Arc<ratelimit::RateLimiter>,
    /// Device tokens (in memory): logins presenting a valid one skip the
    /// slow per-username queue
    pub devices: Arc<devices::DeviceTokens>,
    /// Serializes the slowed attempts per username
    pub auth_user_locks: Arc<ratelimit::KeyedLocks>,
    /// True while this node is the active primary: leader, caches rebuilt,
    /// schedulers running (set by api/primary.rs). Game traffic is served
    /// locally only then.
    pub primary: tokio::sync::watch::Receiver<bool>,
    /// Room images being generated (one job per room)
    pub image_jobs: crate::images::ImageJobs,
    /// Status effects: a cache of the active_effects table
    pub effects: Arc<crate::combat::EffectRegistry>,
    /// Wakes the combat scheduler when a fight or deadline is added
    pub combat_changed: Arc<tokio::sync::Notify>,
}

impl AppState {
    /// Whether this node is the active primary
    pub fn primary_active(&self) -> bool {
        *self.primary.borrow()
    }

    /// Wait up to `limit` for this node to be the active primary (a node
    /// that has just been elected is still rebuilding its caches)
    pub async fn wait_primary_active(&self, limit: std::time::Duration) -> bool {
        let mut primary = self.primary.clone();
        let waited = tokio::time::timeout(limit, primary.wait_for(|active| *active)).await;
        matches!(waited, Ok(Ok(_)))
    }

    /// The account service, writing through Raft
    pub fn accounts(&self) -> AccountService {
        AccountService::new(self.db.pool().clone(), self.raft_writer.clone())
    }
}

/// Build the API router. With `static_dir`, also serve the built web client
/// from it: files by path, and index.html for `/` and any other path no API
/// route claims (client-side routes).
pub async fn router(
    db: Arc<Database>,
    raft_writer: Arc<RaftWriter>,
    static_dir: Option<std::path::PathBuf>,
    auth_limit: u32,
) -> Router {
    // Before serving: the first unknown-user login must not pay for this
    let _ = tokio::task::spawn_blocking(crate::auth::accounts::prepare_login_timing).await;
    let connections = Arc::new(ConnectionManager::new());
    let object_store = Arc::new(ObjectStore::new(
        db.pool().clone(),
        Some(raft_writer.clone()),
    ));
    let class_registry = ClassRegistry::with_db(db.pool().clone(), raft_writer.clone());
    let actions = Arc::new(ActionRegistry::with_db(
        db.pool().clone(),
        raft_writer.clone(),
    ));
    let permissions = Arc::new(PermissionManager::with_db(
        db.pool().clone(),
        Some(raft_writer.clone()),
    ));
    let player_manager = Arc::new(PlayerManager::new(object_store.clone()));
    let timers = Arc::new(TimerManager::new(
        Some(db.pool().clone()),
        Some(raft_writer.clone()),
    ));
    let credits = Arc::new(CreditManager::new(
        Some(db.pool().clone()),
        Some(raft_writer.clone()),
    ));
    let venice = Arc::new(VeniceClient::new());
    let image_store = Arc::new(ImageStore::new(db.pool().clone(), raft_writer.clone()));
    let themes = Arc::new(ThemeRegistry::new());

    // The caches are built from the tables when this node becomes primary
    // (api/primary.rs)
    let effects = crate::combat::EffectRegistry::shared();

    let classes = Arc::new(RwLock::new(class_registry));

    let (primary_tx, primary_rx) = tokio::sync::watch::channel(false);
    let state = AppState {
        db,
        raft_writer,
        connections,
        object_store,
        classes,
        actions,
        permissions,
        player_manager,
        timers,
        credits,
        venice,
        image_store,
        themes,
        image_jobs: crate::images::ImageJobs::new(),
        primary: primary_rx,
        auth_by_ip: Arc::new(ratelimit::RateLimiter::new(auth_limit.saturating_mul(2))),
        auth_by_user: Arc::new(ratelimit::RateLimiter::new(auth_limit)),
        auth_by_user_global: Arc::new(ratelimit::RateLimiter::new(auth_limit.saturating_mul(10))),
        auth_by_user_cap: Arc::new(ratelimit::RateLimiter::new(auth_limit.saturating_mul(100))),
        auth_user_locks: Arc::new(ratelimit::KeyedLocks::default()),
        devices: Arc::new(devices::DeviceTokens::default()),
        effects,
        combat_changed: Arc::new(tokio::sync::Notify::new()),
    };

    // Timers, heartbeats and status effects run on the primary only. A node
    // that is already the leader (always, for a single node) waits for its
    // caches before serving, so no request races their first load.
    let leader_at_start = state.raft_writer.is_leader();
    tokio::spawn(primary::run(state.clone(), primary_tx));
    if leader_at_start
        && !state
            .wait_primary_active(std::time::Duration::from_secs(30))
            .await
    {
        tracing::warn!("primary not ready after 30 s; serving anyway");
    }

    let api = Router::new()
        .route("/health", get(health_check))
        .route("/ws", get(websocket::ws_handler))
        .nest("/images", images::router())
        // On a gateway these are forwarded to the primary
        .merge(
            auth::router()
                .merge(universe::router())
                .route_layer(axum::middleware::from_fn_with_state(
                    state.clone(),
                    gateway::forward_to_primary,
                ))
                // Outermost: before a gateway forwards anything
                .route_layer(axum::middleware::from_fn_with_state(
                    state.clone(),
                    ratelimit::limit_auth_by_ip,
                )),
        );

    let app = match static_dir {
        Some(dir) => {
            let index = tower_http::services::ServeFile::new(dir.join("index.html"));
            let files = tower_http::services::ServeDir::new(dir).fallback(index);
            api.fallback_service(files).with_state(state.clone())
        }
        None => api.route("/", get(root)).with_state(state.clone()),
    };
    let app = with_web_layers(app);
    gateway::install_hooks(&state, &app);
    app
}

/// What a front-end proxy would otherwise add: gzip for text (images and
/// WebSocket upgrades pass through) and browser security headers
fn with_web_layers(app: Router) -> Router {
    use axum::http::{header, HeaderValue};
    use tower_http::set_header::SetResponseHeaderLayer;
    app.layer(tower_http::compression::CompressionLayer::new())
        .layer(SetResponseHeaderLayer::if_not_present(
            header::X_CONTENT_TYPE_OPTIONS,
            HeaderValue::from_static("nosniff"),
        ))
        .layer(SetResponseHeaderLayer::if_not_present(
            header::X_FRAME_OPTIONS,
            HeaderValue::from_static("SAMEORIGIN"),
        ))
        .layer(SetResponseHeaderLayer::if_not_present(
            header::REFERRER_POLICY,
            HeaderValue::from_static("strict-origin-when-cross-origin"),
        ))
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
