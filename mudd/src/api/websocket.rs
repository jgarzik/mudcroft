//! WebSocket handler for real-time player connections

use std::collections::BTreeMap;

use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        Query, State,
    },
    response::IntoResponse,
};
use serde::{Deserialize, Serialize};
use tokio::sync::{mpsc, RwLock};
use tracing::{info, warn};

use super::scripting::{self, ScriptContext};
use super::AppState;
use crate::auth::accounts::Account;
use crate::combat::engine::{attack_mods, attack_statements, resolve_attack, Combatant};
use crate::combat::DamageType;
use crate::images::generate_room_image;
use crate::permissions::AccessLevel;
use crate::theme::DEFAULT_THEME_ID;
use crate::universe::validate_universe_id;

/// A connected player session
#[derive(Debug)]
pub struct PlayerSession {
    pub player_id: String,
    pub account_id: String,
    pub universe_id: String,
    pub room_id: Option<String>,
    pub sender: mpsc::Sender<ServerMessage>,
    /// Unique per WebSocket connection, so a replaced connection's cleanup
    /// never removes its replacement
    pub conn_id: u64,
}

/// Sessions are keyed by (universe, player): player ids are per universe,
/// and one account may play several universes at once
type SessionKey = (String, String);

fn key(universe_id: &str, player_id: &str) -> SessionKey {
    (universe_id.to_string(), player_id.to_string())
}

/// Grace period for reconnection (prevents inventory drop on brief disconnects)
pub(crate) const RECONNECT_GRACE_PERIOD: std::time::Duration = std::time::Duration::from_secs(5);

/// Connection manager for all active WebSocket connections
#[derive(Default)]
pub struct ConnectionManager {
    sessions: RwLock<BTreeMap<SessionKey, PlayerSession>>,
    /// Pending disconnect tasks that can be cancelled on reconnect
    /// Scheduled disconnects: (generation, cancel signal)
    pending_disconnects: RwLock<BTreeMap<SessionKey, (u64, tokio::sync::oneshot::Sender<()>)>>,
    next_conn_id: std::sync::atomic::AtomicU64,
}

impl ConnectionManager {
    pub fn new() -> Self {
        Self::default()
    }

    /// A fresh connection id
    pub fn next_conn_id(&self) -> u64 {
        self.next_conn_id
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    }

    /// Cancel any pending disconnect for this player (called on reconnect)
    pub async fn cancel_pending_disconnect(&self, universe_id: &str, player_id: &str) -> bool {
        let pending = self
            .pending_disconnects
            .write()
            .await
            .remove(&key(universe_id, player_id));
        if let Some((_, cancel_tx)) = pending {
            // Send cancel signal - ignore result since receiver may be dropped
            let _ = cancel_tx.send(());
            info!("Cancelled pending disconnect for player {}", player_id);
            true
        } else {
            false
        }
    }

    /// Schedule a delayed disconnect (grace period for reconnection).
    /// Returns the cancel signal and the disconnect's generation, for
    /// `claim_expired_disconnect`.
    pub async fn schedule_disconnect(
        &self,
        universe_id: &str,
        player_id: &str,
    ) -> (tokio::sync::oneshot::Receiver<()>, u64) {
        let (cancel_tx, cancel_rx) = tokio::sync::oneshot::channel();
        let gen = self.next_conn_id();
        self.pending_disconnects
            .write()
            .await
            .insert(key(universe_id, player_id), (gen, cancel_tx));
        (cancel_rx, gen)
    }

    /// When a grace period expires: claim the disconnect for handling. True
    /// only if it is still the pending one (`gen`) and the player has no
    /// session, checked under the sessions lock so a reconnect that raced
    /// past the cancel is never treated as gone.
    /// Start handling the disconnect of a player whose server was lost
    /// (recorded online, not connected here): records a pending disconnect
    /// (a reconnect cancels it, as during a grace period) and returns its
    /// generation, for claim_expired_disconnect right before acting. None
    /// if they have a live session or a disconnect already pending here,
    /// which then stays in charge.
    pub async fn begin_orphaned_disconnect(
        &self,
        universe_id: &str,
        player_id: &str,
    ) -> Option<u64> {
        let k = key(universe_id, player_id);
        let sessions = self.sessions.read().await;
        let mut pending = self.pending_disconnects.write().await;
        if sessions.contains_key(&k) || pending.contains_key(&k) {
            return None;
        }
        let gen = self.next_conn_id();
        let (cancel_tx, _cancel_rx) = tokio::sync::oneshot::channel();
        pending.insert(k, (gen, cancel_tx));
        Some(gen)
    }

    pub async fn claim_expired_disconnect(
        &self,
        universe_id: &str,
        player_id: &str,
        gen: u64,
    ) -> bool {
        let k = key(universe_id, player_id);
        let sessions = self.sessions.read().await;
        let mut pending = self.pending_disconnects.write().await;
        if !pending.get(&k).is_some_and(|(g, _)| *g == gen) {
            return false;
        }
        pending.remove(&k);
        !sessions.contains_key(&k)
    }

    /// Register a player session. A previous connection for the same
    /// (universe, player) is replaced: it is sent `Replaced`, then its sender
    /// is dropped, which ends that connection's loop after delivering it.
    pub async fn register(&self, session: PlayerSession) {
        let k = key(&session.universe_id, &session.player_id);
        if let Some(old) = self.sessions.write().await.insert(k, session) {
            let _ = old.sender.try_send(ServerMessage::Replaced {
                message: "You connected from another window; this one is now disconnected."
                    .to_string(),
            });
        }
    }

    /// End every session (this node stopped being the primary): dropping
    /// their senders ends their loops. Players reconnect, through this node
    /// as a gateway or to the new primary.
    pub async fn close_all(&self) {
        self.sessions.write().await.clear();
    }

    /// Remove a player session, but only if it is still `conn_id`'s.
    /// Returns false if a newer connection had replaced it.
    pub async fn unregister(&self, universe_id: &str, player_id: &str, conn_id: u64) -> bool {
        let mut sessions = self.sessions.write().await;
        let k = key(universe_id, player_id);
        if sessions.get(&k).is_some_and(|s| s.conn_id == conn_id) {
            sessions.remove(&k);
            return true;
        }
        false
    }

    /// Get a player's sender channel
    pub async fn get_sender(
        &self,
        universe_id: &str,
        player_id: &str,
    ) -> Option<mpsc::Sender<ServerMessage>> {
        self.sessions
            .read()
            .await
            .get(&key(universe_id, player_id))
            .map(|s| s.sender.clone())
    }

    /// Send a message to a specific player.
    ///
    /// Never blocks: if the player's queue is full (a client that stopped
    /// reading) the message is dropped, so one slow client cannot stall
    /// other players or the player's own command handler.
    pub async fn send_to_player(&self, universe_id: &str, player_id: &str, msg: ServerMessage) {
        if let Some(sender) = self.get_sender(universe_id, player_id).await {
            try_deliver(player_id, &sender, msg);
        }
    }

    /// Broadcast a message to all players in a room
    pub async fn broadcast_room(&self, universe_id: &str, room_id: &str, msg: ServerMessage) {
        self.broadcast_room_except(universe_id, room_id, None, msg)
            .await;
    }

    /// Broadcast to all players in a room except `exclude_id` (if given)
    pub async fn broadcast_room_except(
        &self,
        universe_id: &str,
        room_id: &str,
        exclude_id: Option<&str>,
        msg: ServerMessage,
    ) {
        // Copy the recipients out so the lock is not held while sending
        let recipients: Vec<(String, mpsc::Sender<ServerMessage>)> = self
            .sessions
            .read()
            .await
            .values()
            .filter(|s| {
                s.universe_id == universe_id
                    && s.room_id.as_deref() == Some(room_id)
                    && Some(s.player_id.as_str()) != exclude_id
            })
            .map(|s| (s.player_id.clone(), s.sender.clone()))
            .collect();
        for (player_id, sender) in recipients {
            try_deliver(&player_id, &sender, msg.clone());
        }
    }

    /// (player_id, room_id) for every connected player in the universe
    /// that is in a room
    pub async fn player_rooms(&self, universe_id: &str) -> Vec<(String, String)> {
        self.sessions
            .read()
            .await
            .values()
            .filter(|s| s.universe_id == universe_id)
            .filter_map(|s| s.room_id.clone().map(|r| (s.player_id.clone(), r)))
            .collect()
    }

    /// Update player's room
    pub async fn update_room(&self, universe_id: &str, player_id: &str, room_id: Option<String>) {
        if let Some(session) = self
            .sessions
            .write()
            .await
            .get_mut(&key(universe_id, player_id))
        {
            session.room_id = room_id;
        }
    }

    /// Get player's current room ID
    pub async fn get_room_id(&self, universe_id: &str, player_id: &str) -> Option<String> {
        self.sessions
            .read()
            .await
            .get(&key(universe_id, player_id))
            .and_then(|s| s.room_id.clone())
    }

    /// The account of a connected player, or None if not connected
    pub async fn account_id(&self, universe_id: &str, player_id: &str) -> Option<String> {
        self.sessions
            .read()
            .await
            .get(&key(universe_id, player_id))
            .map(|s| s.account_id.clone())
    }
}

/// Queue a message for one client without waiting; drop it if the client's
/// queue is full or closed
fn try_deliver(player_id: &str, sender: &mpsc::Sender<ServerMessage>, msg: ServerMessage) {
    match sender.try_send(msg) {
        Ok(()) => {}
        Err(mpsc::error::TrySendError::Full(_)) => {
            warn!("Dropping message for player {}: queue full", player_id)
        }
        Err(mpsc::error::TrySendError::Closed(_)) => {
            warn!(
                "Failed to send message to player {}: disconnected",
                player_id
            )
        }
    }
}

/// Per-connection outgoing queue size. Generous so a burst of script output
/// fits; a client that stops reading only loses its own messages.
const CLIENT_QUEUE_SIZE: usize = 256;

/// Messages sent from server to client
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type")]
pub enum ServerMessage {
    /// Welcome message on connect
    #[serde(rename = "welcome")]
    Welcome { player_id: String, theme_id: String },
    /// Text output to display
    #[serde(rename = "output")]
    Output { text: String },
    /// Ask the player to confirm a payment; answered with a `confirm`
    /// client message (or by typing yes / no)
    #[serde(rename = "confirm")]
    Confirm {
        id: String,
        text: String,
        amount: i64,
    },
    /// Room description
    #[serde(rename = "room")]
    Room {
        name: String,
        description: String,
        exits: Vec<String>,
        contents: Vec<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        image_hash: Option<String>,
    },
    /// Error message
    #[serde(rename = "error")]
    Error { message: String },
    /// Command echo (for confirmation)
    #[serde(rename = "echo")]
    Echo { command: String },
    /// This connection was replaced by a newer one for the same player and
    /// is about to close; clients must not reconnect automatically
    #[serde(rename = "replaced")]
    Replaced { message: String },
    /// A command sent with an id has been saved (or had been already)
    #[serde(rename = "done")]
    Done { id: String },
    /// No reply: the command already said everything it had to (e.g. a verb
    /// handler that only used game.send). Never sent to clients.
    #[serde(skip)]
    Silent,
}

/// Messages sent from client to server
#[derive(Debug, Deserialize)]
#[serde(tag = "type")]
pub enum ClientMessage {
    /// Player command input
    #[serde(rename = "command")]
    Command {
        text: String,
        /// Optional, up to 64 characters: answered with `done` once saved,
        /// and never run twice (a gateway sets it to retry after failover)
        #[serde(default)]
        id: Option<String>,
    },
    /// Ping to keep connection alive
    #[serde(rename = "ping")]
    Ping,
    /// Answer to a payment prompt
    #[serde(rename = "confirm")]
    Confirm { id: String, accept: bool },
}

/// WebSocket query parameters
#[derive(Debug, Deserialize)]
pub struct WsParams {
    pub token: Option<String>,
    pub universe: Option<String>,
}

/// Check a connection's universe and token: the account and the normalized
/// universe id, or the HTTP status and reason to refuse it with
async fn admit(
    state: &AppState,
    universe: Option<&str>,
    token: Option<&str>,
) -> Result<(Account, String), (axum::http::StatusCode, String)> {
    use axum::http::StatusCode;
    let universe_id = match universe {
        Some(id) => validate_universe_id(id).map_err(|e| {
            (
                StatusCode::BAD_REQUEST,
                format!("Invalid universe ID: {}", e),
            )
        })?,
        None => {
            return Err((
                StatusCode::BAD_REQUEST,
                "Universe ID required in query param: ?universe=<id>".to_string(),
            ))
        }
    };
    match state.object_store.universe_exists(&universe_id).await {
        Ok(true) => {}
        Ok(false) => {
            return Err((
                StatusCode::NOT_FOUND,
                format!("Universe not found: {}", universe_id),
            ))
        }
        Err(e) => {
            warn!("Error checking universe: {}", e);
            return Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                "Failed to verify universe".to_string(),
            ));
        }
    }
    // A valid token is required: there are no guests
    let account = match token {
        Some(token) => state.accounts().validate_token(token).await.ok().flatten(),
        None => None,
    };
    let Some(account) = account else {
        return Err((
            StatusCode::UNAUTHORIZED,
            "A valid token is required: ?token=<token>".to_string(),
        ));
    };
    Ok((account, universe_id))
}

/// Handle WebSocket upgrade with optional token authentication
pub async fn ws_handler(
    ws: WebSocketUpgrade,
    Query(params): Query<WsParams>,
    State(state): State<AppState>,
) -> impl IntoResponse {
    // A node that is not the primary is a gateway: the session runs on the
    // primary, and this node relays it (and follows a failover)
    if state.raft_writer.cluster().is_some() && !state.primary_active() {
        // Just elected: give the node a moment to rebuild its caches
        let activating = state.raft_writer.is_leader()
            && state
                .wait_primary_active(std::time::Duration::from_secs(5))
                .await;
        if !activating {
            if state.raft_writer.is_leader() {
                return (
                    axum::http::StatusCode::SERVICE_UNAVAILABLE,
                    "The primary is starting; try again",
                )
                    .into_response();
            }
            return super::gateway::ws_upgrade(ws, params, state).await;
        }
    }
    match admit(&state, params.universe.as_deref(), params.token.as_deref()).await {
        Ok((account, universe_id)) => ws
            .on_upgrade(move |socket| {
                handle_socket(
                    SessionIo::Local(Box::new(socket)),
                    state,
                    account,
                    universe_id,
                )
            })
            .into_response(),
        Err((status, reason)) => (status, reason).into_response(),
    }
}

/// A game session forwarded by a gateway (runs on the primary): the same
/// checks as a direct connection, then the same session
pub async fn serve_remote_session(
    state: AppState,
    open: crate::raft::transport::GameOpen,
    mut stream: crate::raft::transport::GameStream,
) {
    if !state
        .wait_primary_active(std::time::Duration::from_secs(5))
        .await
    {
        // Not (yet) the primary: the gateway retries quietly
        stream.end(crate::raft::transport::NOT_PRIMARY).await;
        return;
    }
    match admit(&state, Some(&open.universe), Some(&open.token)).await {
        Ok((account, universe_id)) => {
            handle_socket(SessionIo::Remote(stream), state, account, universe_id).await
        }
        Err((_, reason)) => {
            let msg = ServerMessage::Error { message: reason };
            if let Ok(json) = serde_json::to_string(&msg) {
                stream.send(&json).await;
            }
            stream.end("refused").await;
        }
    }
}

/// A player's connection: a local WebSocket, or a session a gateway relays
/// over the cluster port
pub enum SessionIo {
    Local(Box<WebSocket>),
    Remote(crate::raft::transport::GameStream),
}

impl SessionIo {
    /// Send a message's JSON; false when the connection is gone
    async fn send(&mut self, text: String) -> bool {
        match self {
            SessionIo::Local(ws) => ws.send(Message::Text(text.into())).await.is_ok(),
            SessionIo::Remote(stream) => stream.send(&text).await,
        }
    }

    /// The next message: Some(Some(text)), Some(None) for anything to
    /// ignore, None once the connection is closed. Cancel-safe.
    async fn recv(&mut self) -> Option<Option<String>> {
        use crate::raft::transport::GameFrame;
        match self {
            SessionIo::Local(ws) => match ws.recv().await {
                Some(Ok(Message::Text(text))) => Some(Some(text.to_string())),
                Some(Ok(Message::Close(_))) | None => None,
                _ => Some(None),
            },
            SessionIo::Remote(stream) => match stream.recv().await {
                Some(GameFrame::Text(text)) => Some(Some(text)),
                Some(GameFrame::End(_)) | None => None,
            },
        }
    }

    /// The session ends on purpose (a gateway must not take it for a
    /// failover)
    async fn finish(&mut self) {
        if let SessionIo::Remote(stream) = self {
            stream.end("session ended").await;
        }
    }
}

/// Handle an individual WebSocket connection
async fn handle_socket(
    mut socket: SessionIo,
    state: AppState,
    account: Account,
    universe_id: String,
) {
    // Create message channel for this connection
    let (tx, mut rx) = mpsc::channel::<ServerMessage>(CLIENT_QUEUE_SIZE);

    let access_level = state
        .permissions
        .get_access_level(&universe_id, &account.id)
        .await;
    let account_id = account.id.clone();

    // The account's persistent player object in this universe
    let player_id = crate::player::PlayerManager::player_path(&account_id);
    let player_id_clone = player_id.clone();

    // Cancel any pending disconnect for this player (reconnection within grace period)
    let was_reconnect = state
        .connections
        .cancel_pending_disconnect(&universe_id, &player_id)
        .await;
    if was_reconnect {
        info!("Player {} reconnected within grace period", player_id);
    }

    // Ensure the player object exists and find where it spawns
    let spawn_room_id = {
        let name = account.username.as_str();
        match state
            .player_manager
            .get_or_create_player(&account_id, &universe_id, name)
            .await
        {
            Ok(player) => {
                info!(
                    "WebSocket connected: {} ({}) universe={} access={:?}",
                    player_id, name, universe_id, access_level
                );
                // Get spawn location from player manager
                state
                    .player_manager
                    .get_spawn_location(&player, &universe_id)
                    .await
                    .ok()
                    .flatten()
            }
            Err(e) => {
                warn!("Failed to create player object: {}", e);
                None
            }
        }
    };

    // Create session (replacing any older connection for this player)
    let conn_id = state.connections.next_conn_id();
    let session = PlayerSession {
        player_id: player_id.clone(),
        account_id: account_id.clone(),
        universe_id: universe_id.clone(),
        room_id: spawn_room_id.clone(),
        sender: tx,
        conn_id,
    };

    state.connections.register(session).await;
    // Recorded, so the next primary can handle this player's disconnect if
    // the server this session is on dies
    let online_since = match state
        .player_manager
        .mark_online(&universe_id, &player_id)
        .await
    {
        Ok(since) => since,
        Err(e) => {
            warn!("recording {} as online failed: {}", player_id, e);
            String::new()
        }
    };

    // Send welcome message with theme
    // TODO: Get theme_id from universe config
    let theme_id = crate::theme::DEFAULT_THEME_ID.to_string();
    let welcome = ServerMessage::Welcome {
        player_id: player_id.clone(),
        theme_id,
    };
    if let Ok(json) = serde_json::to_string(&welcome) {
        let _ = socket.send(json).await;
    }

    // Move player to spawn room and send room description
    if let Some(room_id) = spawn_room_id {
        // Sync player's location to DB
        if let Err(e) = state
            .player_manager
            .move_player(&universe_id, &player_id, &room_id)
            .await
        {
            warn!("Failed to sync player location to DB: {}", e);
        }

        // Send initial room description
        if let Some(room_msg) =
            build_room_message(&state, &universe_id, &room_id, Some(&account_id)).await
        {
            if let Ok(json) = serde_json::to_string(&room_msg) {
                let _ = socket.send(json).await;
            }
        }

        // Arriving in the world counts as entering the spawn room
        run_hook(
            &state,
            &universe_id,
            Hook::new(
                &room_id,
                Some(&room_id),
                "on_enter",
                &player_id,
                serde_json::json!({ "from": null }),
            ),
        )
        .await;
    } else {
        // No spawn location - player stays nowhere
        let msg = ServerMessage::Output {
            text: "Universe not initialized. Wizards: use 'setportal' command.".to_string(),
        };
        if let Ok(json) = serde_json::to_string(&msg) {
            let _ = socket.send(json).await;
        }
    }

    // Main loop: handle incoming messages and outgoing messages
    loop {
        tokio::select! {
            // Handle outgoing messages from our channel. The channel closes
            // when a newer connection for this player replaces us.
            msg = rx.recv() => {
                let Some(msg) = msg else { break };
                if let Ok(json) = serde_json::to_string(&msg) {
                    if !socket.send(json).await {
                        break;
                    }
                }
            }
            // Handle incoming messages from WebSocket
            result = socket.recv() => {
                match result {
                    Some(Some(text)) => {
                        if let Ok(client_msg) = serde_json::from_str::<ClientMessage>(&text) {
                            // Read the level per command: a promotion or
                            // demotion applies to this open session at once
                            let access_level = state.permissions.get_access_level(&universe_id, &account_id).await;
                            handle_client_message(&state, &universe_id, &player_id, &account_id, access_level, client_msg).await;
                        }
                    }
                    None => break,
                    Some(None) => {}
                }
            }
        }
    }
    // Ended here on purpose, unless this node stepped down: then a gateway
    // relaying the session must take it for a failover and move it on
    if state.primary_active() {
        socket.finish().await;
    }

    // Unregister session immediately (stops message delivery). A connection
    // that was replaced by a newer one leaves the new session alone, and
    // the player is still connected: no disconnect handling.
    let replaced = !state
        .connections
        .unregister(&universe_id, &player_id_clone, conn_id)
        .await;
    info!("WebSocket disconnected: {}", player_id_clone);

    // Grace period before handling disconnect, so a reconnect does not drop
    // inventory
    if !replaced {
        let (cancel_rx, gen) = state
            .connections
            .schedule_disconnect(&universe_id, &player_id_clone)
            .await;

        // Clone what we need for the spawned task
        let state_clone = state.clone();
        let player_id_for_task = player_id_clone.clone();
        let universe_for_task = universe_id.clone();
        let since_for_task = online_since.clone();

        tokio::spawn(async move {
            // Wait for grace period or cancellation
            tokio::select! {
                _ = tokio::time::sleep(RECONNECT_GRACE_PERIOD) => {
                    // Grace period expired: handle the disconnect, unless
                    // the player is connected again
                    if !state_clone.connections.claim_expired_disconnect(&universe_for_task, &player_id_for_task, gen).await {
                        info!("Disconnect handling skipped for {} (connected again)", player_id_for_task);
                        return;
                    }
                    if let Err(e) = state_clone.player_manager.finish_disconnect(&universe_for_task, &player_id_for_task, &since_for_task).await {
                        warn!("Error handling player disconnect: {}", e);
                    }
                    info!("Disconnect handling completed for {} (grace period expired)", player_id_for_task);
                }
                _ = cancel_rx => {
                    // Reconnection cancelled the disconnect - do nothing
                    info!("Disconnect handling cancelled for {} (reconnected)", player_id_for_task);
                }
            }
        });
    }
}

/// Handle a message from the client
async fn handle_client_message(
    state: &AppState,
    universe_id: &str,
    player_id: &str,
    account_id: &str,
    access_level: AccessLevel,
    msg: ClientMessage,
) {
    match msg {
        ClientMessage::Command { text, id } => {
            info!("Player {} command: {}", player_id, text);
            let id = id.filter(|id| !id.is_empty() && id.len() <= 64);
            // Already saved (a retry after a failover): not run again
            if let Some(ref id) = id {
                if command_applied(state, universe_id, id).await {
                    state
                        .connections
                        .send_to_player(
                            universe_id,
                            player_id,
                            ServerMessage::Done { id: id.clone() },
                        )
                        .await;
                    return;
                }
            }

            // Echo the command back
            state
                .connections
                .send_to_player(
                    universe_id,
                    player_id,
                    ServerMessage::Echo {
                        command: text.clone(),
                    },
                )
                .await;

            // Parse and execute the command; hooks it triggers that should
            // follow the reply (e.g. on_enter after the room description)
            // run once the reply is queued. The whole command, hooks
            // included, is one change (db::run): one Raft entry.
            let saved = super::scripting::in_run_with(
                state,
                command_marker(universe_id, id.as_deref()),
                async {
                    let mut deferred = Vec::new();
                    let response = execute_command(
                        state,
                        universe_id,
                        player_id,
                        account_id,
                        access_level,
                        &text,
                        &mut deferred,
                    )
                    .await;
                    if !matches!(response, ServerMessage::Silent) {
                        state
                            .connections
                            .send_to_player(universe_id, player_id, response)
                            .await;
                    }
                    for hook in deferred {
                        run_hook(state, universe_id, hook).await;
                    }
                    Ok::<(), String>(())
                },
            )
            .await;
            if let (Ok(()), Some(id)) = (&saved, id) {
                state
                    .connections
                    .send_to_player(universe_id, player_id, ServerMessage::Done { id })
                    .await;
            }
            if let Err(e) = saved {
                warn!("command by {} not saved: {}", player_id, e);
                state
                    .connections
                    .send_to_player(
                        universe_id,
                        player_id,
                        ServerMessage::Error {
                            message: "Your last command could not be saved.".to_string(),
                        },
                    )
                    .await;
            }
        }
        ClientMessage::Ping => {
            // Just keep the connection alive, no response needed
        }
        ClientMessage::Confirm { id, accept } => {
            let reply = super::payments::answer(
                state,
                universe_id,
                account_id,
                player_id,
                Some(&id),
                accept,
            )
            .await
            .unwrap_or_else(|| ServerMessage::Error {
                message: "There is no such payment request (it may have expired).".to_string(),
            });
            state
                .connections
                .send_to_player(universe_id, player_id, reply)
                .await;
        }
    }
}

/// How long a command id is remembered (a retry comes within seconds)
const COMMAND_ID_TTL_MS: i64 = 10 * 60 * 1000;

/// Whether a command with this id was saved already
async fn command_applied(state: &AppState, universe_id: &str, id: &str) -> bool {
    sqlx::query_scalar::<_, i64>(
        "SELECT 1 FROM applied_commands WHERE universe_id = ? AND command_id = ?",
    )
    .bind(universe_id)
    .bind(id)
    .fetch_optional(state.db.pool())
    .await
    .ok()
    .flatten()
    .is_some()
}

/// Statements leading a command's batch when it has an id: the id is
/// recorded with it (a duplicate fails the batch), and ids past their
/// deadline go
fn command_marker(universe_id: &str, id: Option<&str>) -> Vec<crate::db::run::Statement> {
    let Some(id) = id else {
        return Vec::new();
    };
    let now = crate::combat::now_ms() as i64;
    vec![
        (
            "DELETE FROM applied_commands WHERE universe_id = ? AND expires_at < ?".to_string(),
            vec![universe_id.into(), now.into()],
        ),
        (
            "INSERT INTO applied_commands (universe_id, command_id, expires_at) VALUES (?, ?, ?)"
                .to_string(),
            vec![
                universe_id.into(),
                id.into(),
                (now + COMMAND_ID_TTL_MS).into(),
            ],
        ),
    ]
}

/// Everything after a command's verb, verbatim apart from surrounding
/// whitespace (for commands whose argument is free text, like eval)
fn args_after_verb(command: &str) -> &str {
    command
        .trim()
        .split_once(char::is_whitespace)
        .map(|(_, rest)| rest.trim_start())
        .unwrap_or("")
}

/// Name shown to other players: the player object's `name`, else "Someone"
async fn display_name(state: &AppState, universe_id: &str, player_id: &str) -> String {
    match state.object_store.get(universe_id, player_id).await {
        Ok(Some(obj)) => obj
            .properties
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("Someone")
            .to_string(),
        _ => "Someone".to_string(),
    }
}

/// Build a Room message from a room object
/// Also triggers background image generation if room has no image and Venice is configured
pub(crate) async fn build_room_message(
    state: &AppState,
    universe_id: &str,
    room_id: &str,
    account_id: Option<&str>,
) -> Option<ServerMessage> {
    // Get room object
    let room = state.object_store.get(universe_id, room_id).await.ok()??;

    // Get room contents
    let contents = state
        .object_store
        .get_contents(universe_id, room_id)
        .await
        .unwrap_or_default();

    // Extract room properties
    let name = room
        .properties
        .get("name")
        .and_then(|v| v.as_str())
        .unwrap_or("Unknown Room")
        .to_string();

    let description = room
        .properties
        .get("description")
        .and_then(|v| v.as_str())
        .unwrap_or("You see nothing special.")
        .to_string();

    // Extract exits (keys of the exits object)
    let exits: Vec<String> = room
        .properties
        .get("exits")
        .and_then(|v| v.as_object())
        .map(|obj| obj.keys().cloned().collect())
        .unwrap_or_default();

    // Extract content names (filter out players for now, just show items)
    let content_names: Vec<String> = contents
        .iter()
        .filter_map(|obj| {
            obj.properties
                .get("name")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
        })
        .collect();

    // Get image hash if present
    let image_hash = room
        .properties
        .get("image_hash")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    // Trigger background image generation if room has no image and Venice is configured
    // (at most one job per room: viewers arriving meanwhile start none)
    let job = (crate::images::room_needs_image(&room)
        && state.venice.is_configured()
        && account_id.is_some())
    .then(|| state.image_jobs.try_start(universe_id, room_id))
    .flatten();
    if let Some(job) = job {
        if let Some(acct_id) = account_id {
            let venice = state.venice.clone();
            let image_store = state.image_store.clone();
            let object_store = state.object_store.clone();
            let themes = state.themes.clone();
            let room_id_owned = room_id.to_string();
            let universe_owned = universe_id.to_string();
            let account_id_owned = acct_id.to_string();

            // Spawn background task for image generation
            tokio::spawn(async move {
                // Held until the job ends, however it ends
                let _job = job;
                let theme = themes.get(DEFAULT_THEME_ID);
                match generate_room_image(
                    &venice,
                    &image_store,
                    &object_store,
                    &universe_owned,
                    &room_id_owned,
                    &theme,
                    &account_id_owned,
                )
                .await
                {
                    Ok(hash) => {
                        info!(
                            "Background image generation complete for room {}: {}",
                            room_id_owned, hash
                        );
                    }
                    Err(e) => {
                        warn!(
                            "Background image generation failed for room {}: {}",
                            room_id_owned, e
                        );
                    }
                }
            });
        }
    }

    Some(ServerMessage::Room {
        name,
        description,
        exits,
        contents: content_names,
        image_hash,
    })
}

/// An object hook to run on behalf of a player
struct Hook {
    /// Object whose attached code handles the hook
    object_id: String,
    /// Room the hook happens in (room context for the script)
    room_id: Option<String>,
    /// Handler name, e.g. "on_enter"
    method: &'static str,
    /// Handler argument table (object_id and actor_id are always set)
    args: serde_json::Value,
}

impl Hook {
    fn new(
        object_id: &str,
        room_id: Option<&str>,
        method: &'static str,
        actor_id: &str,
        mut args: serde_json::Value,
    ) -> Self {
        if let Some(map) = args.as_object_mut() {
            map.insert("object_id".into(), object_id.into());
            map.insert("actor_id".into(), actor_id.into());
            // Room hooks name what moved; for a player's own move that is
            // the player (Lua moves pass it explicitly, see scripting.rs)
            if matches!(method, "on_enter" | "on_leave") && !map.contains_key("moved_id") {
                map.insert("moved_id".into(), actor_id.into());
            }
            if let Some(room) = room_id {
                map.insert("room_id".into(), room.into());
            }
        }
        Self {
            object_id: object_id.to_string(),
            room_id: room_id.map(str::to_string),
            method,
            args,
        }
    }
}

/// Run an object hook triggered by a player. The hook runs as the
/// object's owner (see scripting::invoke_handler); errors are logged.
async fn run_hook(state: &AppState, universe_id: &str, hook: Hook) {
    if let Err(e) = scripting::invoke_handler(
        state,
        universe_id,
        &hook.object_id,
        hook.room_id,
        hook.method,
        hook.args,
    )
    .await
    {
        warn!("{} hook on {} failed: {}", hook.method, hook.object_id, e);
    }
}

/// Parse and execute a player command
async fn execute_command(
    state: &AppState,
    universe_id: &str,
    player_id: &str,
    account_id: &str,
    access_level: AccessLevel,
    command: &str,
    deferred: &mut Vec<Hook>,
) -> ServerMessage {
    let parts: Vec<&str> = command.split_whitespace().collect();

    if parts.is_empty() {
        return ServerMessage::Output {
            text: "What?".to_string(),
        };
    }

    let verb = parts[0].to_lowercase();

    match verb.as_str() {
        "look" | "l" => {
            // Get player's current room
            if let Some(room_id) = state.connections.get_room_id(universe_id, player_id).await {
                if let Some(room_msg) =
                    build_room_message(state, universe_id, &room_id, Some(account_id)).await
                {
                    return room_msg;
                }
            }
            ServerMessage::Output {
                text: "You are nowhere.".to_string(),
            }
        }
        "north" | "n" | "south" | "s" | "east" | "e" | "west" | "w" | "up" | "u" | "down" | "d" => {
            // Normalize direction
            let direction = match verb.as_str() {
                "n" => "north",
                "s" => "south",
                "e" => "east",
                "w" => "west",
                "u" => "up",
                "d" => "down",
                other => other,
            };

            // Get player's current room
            let current_room_id = match state.connections.get_room_id(universe_id, player_id).await
            {
                Some(id) => id,
                None => {
                    return ServerMessage::Output {
                        text: "You are nowhere.".to_string(),
                    };
                }
            };

            // Check if exit exists
            let dest_room_id = match state
                .object_store
                .get_exit(universe_id, &current_room_id, direction)
                .await
            {
                Ok(Some(dest)) => dest,
                _ => {
                    return ServerMessage::Output {
                        text: "You can't go that way.".to_string(),
                    };
                }
            };

            // Leaving hook runs before the move
            run_hook(
                state,
                universe_id,
                Hook::new(
                    &current_room_id,
                    Some(&current_room_id),
                    "on_leave",
                    player_id,
                    serde_json::json!({ "to": dest_room_id, "direction": direction }),
                ),
            )
            .await;

            // The hook may have moved the player (a trapdoor): its move
            // stands, and this walk is over (the player was shown the room)
            if state.connections.get_room_id(universe_id, player_id).await
                != Some(current_room_id.clone())
            {
                return ServerMessage::Silent;
            }

            // Update player's room in memory
            state
                .connections
                .update_room(universe_id, player_id, Some(dest_room_id.clone()))
                .await;

            // Sync to database
            if let Err(e) = state
                .player_manager
                .move_player(universe_id, player_id, &dest_room_id)
                .await
            {
                warn!("Failed to sync player location to DB: {}", e);
            }

            // Entering hook runs after the new room description is sent
            deferred.push(Hook::new(
                &dest_room_id,
                Some(&dest_room_id),
                "on_enter",
                player_id,
                serde_json::json!({ "from": current_room_id, "direction": direction }),
            ));

            // Return new room description
            if let Some(room_msg) =
                build_room_message(state, universe_id, &dest_room_id, Some(account_id)).await
            {
                return room_msg;
            }

            ServerMessage::Output {
                text: "You move but find yourself nowhere.".to_string(),
            }
        }
        "say" => {
            let message = parts[1..].join(" ");
            if message.is_empty() {
                return ServerMessage::Error {
                    message: "Say what?".to_string(),
                };
            }
            if let Some(room_id) = state.connections.get_room_id(universe_id, player_id).await {
                let speaker = display_name(state, universe_id, player_id).await;
                state
                    .connections
                    .broadcast_room_except(
                        universe_id,
                        &room_id,
                        Some(player_id),
                        ServerMessage::Output {
                            text: format!("{} says: {}", speaker, message),
                        },
                    )
                    .await;
            }
            ServerMessage::Output {
                text: format!("You say: {}", message),
            }
        }
        "help" => {
            let mut text = String::from(
                "Commands: look, north/south/east/west/up/down, say <message>, \
                 get/take <item>, drop <item>, inventory/i, attack/kill <target>, help. \
                 Objects may offer their own verbs (e.g. 'pull lever').",
            );
            if access_level >= AccessLevel::Builder {
                text.push_str(
                    "\nBuilder: create <type> <path> \"<name>\" [\"<description>\"] [key=value ...]",
                );
            }
            if access_level >= AccessLevel::Wizard {
                text.push_str("\nWizard: eval <lua>, goto <room_id>, setportal [room_id]");
            }
            ServerMessage::Output { text }
        }
        "get" | "take" => {
            // Get item name from args
            let item_name = if parts.len() > 1 {
                parts[1..].join(" ")
            } else {
                return ServerMessage::Error {
                    message: "Take what?".to_string(),
                };
            };

            // Execute Commands.take() via Lua
            // Arguments are passed as values, never spliced into the source
            execute_lua_with_args(
                state,
                universe_id,
                account_id,
                "local pid, item = ...; return Commands.take(pid, item).message",
                vec![player_id.to_string(), item_name],
            )
            .await
        }
        "drop" => {
            // Get item name from args
            let item_name = if parts.len() > 1 {
                parts[1..].join(" ")
            } else {
                return ServerMessage::Error {
                    message: "Drop what?".to_string(),
                };
            };

            // Execute Commands.drop() via Lua
            // Arguments are passed as values, never spliced into the source
            execute_lua_with_args(
                state,
                universe_id,
                account_id,
                "local pid, item = ...; return Commands.drop(pid, item).message",
                vec![player_id.to_string(), item_name],
            )
            .await
        }
        "inventory" | "inv" | "i" => {
            // Execute Commands.inventory() via Lua
            execute_lua_with_args(
                state,
                universe_id,
                account_id,
                "local pid = ...; return Commands.inventory(pid).message",
                vec![player_id.to_string()],
            )
            .await
        }
        "goto" => {
            // Wizard+ only
            if access_level < AccessLevel::Wizard {
                return ServerMessage::Error {
                    message: "Permission denied: wizard+ required for goto".to_string(),
                };
            }

            // Get room_id from args
            let room_id = if parts.len() > 1 {
                parts[1].to_string()
            } else {
                return ServerMessage::Error {
                    message: "Usage: goto <room_id>".to_string(),
                };
            };

            // Verify room exists (in this universe)
            match state.object_store.get(universe_id, &room_id).await {
                Ok(Some(room)) if room.class == "room" => {
                    let from = state.connections.get_room_id(universe_id, player_id).await;
                    if let Some(ref from_id) = from {
                        run_hook(
                            state,
                            universe_id,
                            Hook::new(
                                from_id,
                                Some(from_id),
                                "on_leave",
                                player_id,
                                serde_json::json!({ "to": room_id }),
                            ),
                        )
                        .await;
                        // A hook that moved the player wins over the goto
                        if state.connections.get_room_id(universe_id, player_id).await
                            != Some(from_id.clone())
                        {
                            return ServerMessage::Silent;
                        }
                    }

                    // Update player's room, in the session and the player
                    // object, which get/drop and respawn use
                    state
                        .connections
                        .update_room(universe_id, player_id, Some(room_id.clone()))
                        .await;
                    if let Err(e) = state
                        .player_manager
                        .move_player(universe_id, player_id, &room_id)
                        .await
                    {
                        warn!("Failed to sync player location to DB: {}", e);
                    }
                    deferred.push(Hook::new(
                        &room_id,
                        Some(&room_id),
                        "on_enter",
                        player_id,
                        serde_json::json!({ "from": from }),
                    ));

                    // Return room description
                    if let Some(room_msg) =
                        build_room_message(state, universe_id, &room_id, Some(account_id)).await
                    {
                        return room_msg;
                    }
                    ServerMessage::Output {
                        text: "Teleported but room has no description.".to_string(),
                    }
                }
                Ok(Some(_)) => ServerMessage::Error {
                    message: format!("Object {} is not a room.", room_id),
                },
                Ok(None) => ServerMessage::Error {
                    message: format!("Room not found: {}", room_id),
                },
                Err(e) => ServerMessage::Error {
                    message: format!("Error looking up room: {}", e),
                },
            }
        }
        "setportal" => {
            // Wizard+ only
            if access_level < AccessLevel::Wizard {
                return ServerMessage::Error {
                    message: "Permission denied: wizard+ required for setportal".to_string(),
                };
            }

            // Get room_id from args or current room
            let room_id = if parts.len() > 1 {
                parts[1].to_string()
            } else {
                match state.connections.get_room_id(universe_id, player_id).await {
                    Some(id) => id,
                    None => {
                        return ServerMessage::Error {
                            message:
                                "You are nowhere. Use 'setportal <room_id>' to specify a room."
                                    .to_string(),
                        };
                    }
                }
            };

            // Verify room exists (in this universe)
            match state.object_store.get(universe_id, &room_id).await {
                Ok(Some(room)) if room.class == "room" => {
                    // Set portal
                    if let Err(e) = state.object_store.set_portal(universe_id, &room_id).await {
                        return ServerMessage::Error {
                            message: format!("Failed to set portal: {}", e),
                        };
                    }

                    let room_name = room
                        .properties
                        .get("name")
                        .and_then(|v| v.as_str())
                        .unwrap_or("Unknown Room");

                    ServerMessage::Output {
                        text: format!("Portal set to: {} ({})", room_name, room_id),
                    }
                }
                Ok(Some(_)) => ServerMessage::Error {
                    message: format!("Object {} is not a room.", room_id),
                },
                Ok(None) => ServerMessage::Error {
                    message: format!("Room not found: {}", room_id),
                },
                Err(e) => ServerMessage::Error {
                    message: format!("Error looking up room: {}", e),
                },
            }
        }
        "eval" => {
            // Wizard+ only
            if access_level < AccessLevel::Wizard {
                return ServerMessage::Error {
                    message: "Permission denied: wizard+ required for eval".to_string(),
                };
            }

            let code = args_after_verb(command);
            if code.is_empty() {
                return ServerMessage::Error {
                    message: "Usage: eval <lua code>".to_string(),
                };
            }

            // Execute Lua code
            execute_lua(state, universe_id, account_id, code).await
        }
        "attack" | "kill" => {
            // Get target name from args
            let target_name = if parts.len() > 1 {
                parts[1..].join(" ").to_lowercase()
            } else {
                return ServerMessage::Error {
                    message: "Attack what?".to_string(),
                };
            };

            // Get player's current room
            let room_id = match state.connections.get_room_id(universe_id, player_id).await {
                Some(id) => id,
                None => {
                    return ServerMessage::Error {
                        message: "You are nowhere.".to_string(),
                    };
                }
            };

            // Find target in room
            let contents = state
                .object_store
                .get_contents(universe_id, &room_id)
                .await
                .unwrap_or_default();

            // Never yourself; an exact name match beats a partial one
            let name_of = |obj: &crate::objects::Object| {
                obj.properties
                    .get("name")
                    .and_then(|v| v.as_str())
                    .map(|n| n.to_lowercase())
            };
            let others: Vec<_> = contents.iter().filter(|o| o.id != player_id).collect();
            let target = others
                .iter()
                .find(|o| name_of(o).as_deref() == Some(target_name.as_str()))
                .or_else(|| {
                    others.iter().find(|o| {
                        name_of(o)
                            .map(|n| n.contains(&target_name))
                            .unwrap_or(false)
                    })
                })
                .copied();

            let target = match target {
                Some(t) => t,
                None => {
                    return ServerMessage::Error {
                        message: format!("You don't see '{}' here.", target_name),
                    };
                }
            };

            let target_id = target.id.clone();
            let target_display_name = target
                .properties
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or("something")
                .to_string();

            // Check if target is attackable (NPC or PvP-flagged player)
            // Any npc, including subclasses defined with define_class
            let is_npc = target.class == "monster"
                || state
                    .classes
                    .read()
                    .await
                    .is_a(universe_id, &target.class, "npc");
            if !is_npc {
                return ServerMessage::Error {
                    message: format!("You can't attack {}.", target_display_name),
                };
            }

            // Serialize with the effect scheduler and effect writes, and
            // read the target again under the lock: HP must not be computed
            // from a stale row
            let _writes = state.effects.write_guard().await;
            let Ok(Some(target)) = state.object_store.get(universe_id, &target_id).await else {
                return ServerMessage::Error {
                    message: format!("You don't see {} here.", target_display_name),
                };
            };
            let target = &target;
            // It may have died or left since it was picked (another
            // attacker got there first): nothing to fight
            let alive = target
                .properties
                .get("hp")
                .and_then(|v| v.as_i64())
                .is_none_or(|hp| hp > 0);
            if target.parent_id.as_deref() != Some(room_id.as_str()) {
                return ServerMessage::Error {
                    message: format!("You don't see {} here.", target_display_name),
                };
            }
            if !alive {
                return ServerMessage::Error {
                    message: format!("{} is already dead.", target_display_name),
                };
            }

            // Stats come from the object rows, the single source of truth
            let Ok(Some(attacker_obj)) = state.object_store.get(universe_id, player_id).await
            else {
                return ServerMessage::Error {
                    message: "You have no body to fight with.".to_string(),
                };
            };
            let attacker = Combatant::from_object(&attacker_obj, 100);
            let defender = Combatant::from_object(target, 10);

            // Status effects: a stunned or paralyzed attacker cannot act;
            // others change to-hit and damage
            let now = crate::combat::now_ms();
            let attacker_effects = state.effects.get(universe_id, player_id).await;
            let target_effects = state.effects.get(universe_id, &target_id).await;
            if !attacker_effects.can_act(now) {
                return ServerMessage::Error {
                    message: "You are stunned and cannot act.".to_string(),
                };
            }
            let mods = attack_mods(&attacker_effects, &target_effects, now);

            // Basic 1d6 physical damage, doubled on a critical
            let d20 = crate::combat::roll_d20();
            let damage_roll = {
                use rand::RngExt;
                rand::rng().random_range(1..=6)
            };
            let outcome = resolve_attack(
                &attacker,
                &defender,
                &mods,
                d20,
                damage_roll,
                DamageType::Physical,
                now,
            );

            // The round is one atomic batch: new HP, and removal on death
            match attack_statements(universe_id, &target_id, &outcome) {
                Ok(statements) if !statements.is_empty() => {
                    if let Err(e) = state.object_store.execute_batch(statements).await {
                        return ServerMessage::Error {
                            message: format!("Attack failed: {}", e),
                        };
                    }
                }
                Ok(_) => {}
                Err(e) => {
                    return ServerMessage::Error {
                        message: format!("Attack failed: {}", e),
                    };
                }
            }
            if outcome.killed {
                state.effects.cache_clear(universe_id, &target_id).await;
            }
            if let Some(ref bleed) = outcome.inflicts {
                state
                    .effects
                    .cache_add(universe_id, &target_id, bleed.clone())
                    .await;
            }
            let attack_result = &outcome.result;

            // Build result message
            let mut messages = Vec::new();

            if attack_result.hit {
                let damage = attack_result
                    .damage
                    .as_ref()
                    .map(|d| d.final_damage)
                    .unwrap_or(0);
                deferred.push(Hook::new(
                    &target_id,
                    Some(&room_id),
                    "on_damage",
                    player_id,
                    serde_json::json!({ "attacker_id": player_id, "damage": damage }),
                ));
                if attack_result.critical {
                    messages.push(format!(
                        "CRITICAL HIT! You strike {} for {} damage!",
                        target_display_name, damage
                    ));
                    if outcome.inflicts.is_some() {
                        messages.push(format!("{} is bleeding!", target_display_name));
                    }
                } else {
                    messages.push(format!(
                        "You hit {} for {} damage.",
                        target_display_name, damage
                    ));
                }

                if outcome.killed {
                    messages.push(format!("{} is slain!", target_display_name));
                    deferred.push(Hook::new(
                        &target_id,
                        Some(&room_id),
                        "on_death",
                        player_id,
                        serde_json::json!({ "killer_id": player_id }),
                    ));
                } else {
                    messages.push(format!(
                        "{} has {} HP remaining.",
                        target_display_name, outcome.target_hp
                    ));
                }
            } else if attack_result.fumble {
                messages.push(format!(
                    "You fumble your attack against {}!",
                    target_display_name
                ));
            } else {
                messages.push(format!(
                    "You miss {} (rolled {} vs AC {}).",
                    target_display_name, attack_result.attack_total, attack_result.target_ac
                ));
            }

            // Onlookers get a third-person summary; the attacker gets the
            // detailed result as this command's reply
            let attacker_name = display_name(state, universe_id, player_id).await;
            let mut seen = format!("{} attacks {}.", attacker_name, target_display_name);
            if outcome.killed {
                seen.push_str(&format!(" {} is slain!", target_display_name));
            }
            state
                .connections
                .broadcast_room_except(
                    universe_id,
                    &room_id,
                    Some(player_id),
                    ServerMessage::Output { text: seen },
                )
                .await;

            ServerMessage::Output {
                text: messages.join("\n"),
            }
        }
        "create" => {
            // Builder+ only
            if access_level < AccessLevel::Builder {
                return ServerMessage::Error {
                    message: "Permission denied: builder+ required for create".to_string(),
                };
            }

            let args = args_after_verb(command);
            if args.is_empty() {
                return ServerMessage::Error {
                    message:
                        "Usage: create <type> <path> \"<name>\" [\"<description>\"] [key=value ...]"
                            .to_string(),
                };
            };

            // Execute create command
            execute_create_command(state, universe_id, account_id, access_level, args).await
        }
        // Typed answers to the latest payment prompt (text clients); with
        // none pending they are ordinary words for custom verbs
        "yes" | "no" if parts.len() == 1 => {
            match super::payments::answer(
                state,
                universe_id,
                account_id,
                player_id,
                None,
                verb == "yes",
            )
            .await
            {
                Some(reply) => reply,
                None => match dispatch_custom_verb(
                    state,
                    universe_id,
                    player_id,
                    account_id,
                    access_level,
                    &verb,
                    "",
                )
                .await
                {
                    Some(reply) => reply,
                    None => ServerMessage::Output {
                        text: format!("Unknown command: {}", verb),
                    },
                },
            }
        }
        _ => {
            let args = parts[1..].join(" ");
            match dispatch_custom_verb(
                state,
                universe_id,
                player_id,
                account_id,
                access_level,
                &verb,
                &args,
            )
            .await
            {
                Some(reply) => reply,
                None => ServerMessage::Output {
                    text: format!("Unknown command: {}", verb),
                },
            }
        }
    }
}

/// Run a verb registered with game.add_action, if one is in scope.
///
/// Candidates, in order: actions registered for the player's room, then
/// object actions on inventory items, on objects in the room, and on the
/// room itself. When the player names a target ("pull lever"), a candidate
/// whose object name contains it wins; otherwise the first candidate does.
/// The handler runs as the object's owner and gets {verb, args, actor_id,
/// object_id, room_id}; a string result becomes the reply.
async fn dispatch_custom_verb(
    state: &AppState,
    universe_id: &str,
    player_id: &str,
    player_account: &str,
    access_level: AccessLevel,
    verb: &str,
    args: &str,
) -> Option<ServerMessage> {
    let room_id = state.connections.get_room_id(universe_id, player_id).await;

    let mut nearby: Vec<String> = Vec::new();
    if let Ok(items) = state
        .object_store
        .get_contents(universe_id, player_id)
        .await
    {
        nearby.extend(items.into_iter().map(|o| o.id));
    }
    if let Some(ref room) = room_id {
        if let Ok(objs) = state.object_store.get_contents(universe_id, room).await {
            nearby.extend(objs.into_iter().map(|o| o.id));
        }
        nearby.push(room.clone());
    }
    // Room actions only count when their object is actually nearby
    let mut candidates = Vec::new();
    if let Some(ref room) = room_id {
        candidates.extend(
            state
                .actions
                .get_room_actions(universe_id, room, verb)
                .await
                .into_iter()
                .filter(|a| nearby.contains(&a.object_id)),
        );
    }
    for object_id in &nearby {
        if let Some(action) = state
            .actions
            .get_object_action(universe_id, object_id, verb)
            .await
        {
            candidates.push(action);
        }
    }
    // Never dispatch to private handlers or engine hooks
    candidates.retain(|a| !crate::lua::is_private_handler(&a.method));
    if candidates.is_empty() {
        return None;
    }

    // Prefer the candidate the player named: an exact name first, else one
    // whose name the words contain (or that contains them)
    let wanted = args.trim().to_lowercase();
    let mut exact = Vec::new();
    let mut fuzzy = Vec::new();
    if !wanted.is_empty() {
        for action in &candidates {
            if let Ok(Some(obj)) = state.object_store.get(universe_id, &action.object_id).await {
                let name = obj
                    .properties
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_lowercase();
                if name.is_empty() {
                    continue;
                }
                if name == wanted {
                    exact.push(action.clone());
                } else if wanted.contains(&name) || name.contains(&wanted) {
                    fuzzy.push(action.clone());
                }
            }
        }
    }
    let objects = |list: &[crate::lua::Action]| {
        let mut ids: Vec<&str> = list.iter().map(|a| a.object_id.as_str()).collect();
        ids.sort_unstable();
        ids.dedup();
        ids.len()
    };
    // The player's wallet goes only to an object they clearly picked: the
    // only one offering the verb, their exact name for it, or the single
    // match. Otherwise another builder's look-alike could capture the verb
    // and bill them; the handler still runs, with no paying player.
    let (chosen, picked) = if objects(&candidates) == 1 {
        (candidates[0].clone(), true)
    } else if objects(&exact) == 1 {
        (exact[0].clone(), true)
    } else if exact.is_empty() && objects(&fuzzy) == 1 {
        (fuzzy[0].clone(), true)
    } else {
        let first = exact.first().or(fuzzy.first()).unwrap_or(&candidates[0]);
        (first.clone(), false)
    };

    let handler_args = serde_json::json!({
        "verb": verb,
        "args": args,
        "actor_id": player_id,
        "object_id": chosen.object_id,
        "room_id": room_id,
    });
    // The player chose to run this verb: they pay for what it spends
    let result = scripting::invoke_verb_handler(
        state,
        universe_id,
        &chosen.object_id,
        room_id,
        &chosen.method,
        handler_args,
        picked.then_some(player_account),
    )
    .await;
    Some(match result {
        // nil or "" means the handler already said everything (game.send)
        Ok(Some(text)) if text != "nil" && !text.is_empty() => ServerMessage::Output { text },
        Ok(_) => ServerMessage::Silent,
        Err(message) => {
            warn!(
                "{} handler on {} failed: {}",
                verb, chosen.object_id, message
            );
            // Script internals are for builders' eyes only
            if access_level >= AccessLevel::Wizard {
                ServerMessage::Error { message }
            } else {
                ServerMessage::Error {
                    message: "Something went wrong.".to_string(),
                }
            }
        }
    })
}

/// Execute Lua code in sandbox with game API
async fn execute_lua(
    state: &AppState,
    universe_id: &str,
    account_id: &str,
    code: &str,
) -> ServerMessage {
    execute_lua_with_args(state, universe_id, account_id, code, Vec::new()).await
}

/// Execute a Lua chunk with `args` available as its varargs (`...`),
/// as the player's account, in the player's universe
async fn execute_lua_with_args(
    state: &AppState,
    universe_id: &str,
    account_id: &str,
    code: &str,
    args: Vec<String>,
) -> ServerMessage {
    let ctx = ScriptContext {
        universe_id: universe_id.to_string(),
        user_id: Some(account_id.to_string()),
        room_id: None,
        object_id: None,
        payer: Some(account_id.to_string()),
    };
    match scripting::run_lua(state, &ctx, code, args, scripting::eval_config()).await {
        Ok(text) => ServerMessage::Output { text },
        Err(message) => ServerMessage::Error { message },
    }
}

/// Execute create command for builders
/// Syntax: <type> <path> "<name>" ["<description>"] [key=value ...]
async fn execute_create_command(
    state: &AppState,
    universe_id: &str,
    account_id: &str,
    access_level: AccessLevel,
    args: &str,
) -> ServerMessage {
    // Parse create arguments
    let parsed = match parse_create_args(args) {
        Ok(p) => p,
        Err(e) => {
            return ServerMessage::Error {
                message: format!("Parse error: {}", e),
            };
        }
    };

    // Check permission to create at this path
    let user_ctx = state
        .permissions
        .get_user_context(account_id, universe_id)
        .await;

    // Override access level from session (in case it's newer than cached)
    let user_ctx = crate::permissions::UserContext {
        access_level,
        ..user_ctx
    };

    let perm_result = state
        .permissions
        .check_create_permission(&user_ctx, &parsed.path);

    if !perm_result.is_allowed() {
        return ServerMessage::Error {
            message: format!(
                "Permission denied: no access to create at path {}",
                parsed.path
            ),
        };
    }

    // Create the object
    let mut obj = match crate::objects::Object::new_with_owner(
        &parsed.path,
        universe_id,
        &parsed.class,
        account_id,
    ) {
        Ok(o) => o,
        Err(e) => {
            return ServerMessage::Error {
                message: format!("Invalid path: {}", e),
            };
        }
    };

    // Set name property
    obj.set_property("name", serde_json::json!(parsed.name));

    // Set description if provided
    if let Some(desc) = parsed.description {
        obj.set_property("description", serde_json::json!(desc));
    }

    // Set parent_id if provided
    if let Some(parent) = parsed.parent_id {
        obj.parent_id = Some(parent);
    }

    // Set additional properties
    for (key, value) in parsed.properties {
        obj.set_property(&key, value);
    }

    // Store the object
    if let Err(e) = state.object_store.create(&obj).await {
        return ServerMessage::Error {
            message: format!("Failed to create object: {}", e),
        };
    }

    ServerMessage::Output {
        text: format!(
            "Created {} '{}' at {}",
            parsed.class, parsed.name, parsed.path
        ),
    }
}

/// Parsed create command arguments
struct CreateParams {
    class: String,
    path: String,
    name: String,
    description: Option<String>,
    parent_id: Option<String>,
    properties: std::collections::HashMap<String, serde_json::Value>,
}

/// Parse create command arguments
/// Syntax: <type> <path> "<name>" ["<description>"] [key=value ...]
fn parse_create_args(args: &str) -> Result<CreateParams, String> {
    let mut chars = args.chars().peekable();
    let mut tokens: Vec<String> = Vec::new();

    // Helper to skip whitespace
    fn skip_whitespace(chars: &mut std::iter::Peekable<std::str::Chars>) {
        while chars.peek().is_some_and(|c| c.is_whitespace()) {
            chars.next();
        }
    }

    // Tokenize: words and quoted strings
    while chars.peek().is_some() {
        skip_whitespace(&mut chars);
        if chars.peek().is_none() {
            break;
        }

        if chars.peek() == Some(&'"') {
            // Quoted string
            chars.next(); // consume opening quote
            let mut s = String::new();
            while let Some(&c) = chars.peek() {
                if c == '"' {
                    chars.next(); // consume closing quote
                    break;
                }
                if c == '\\' {
                    chars.next();
                    if let Some(&escaped) = chars.peek() {
                        s.push(escaped);
                        chars.next();
                    }
                } else {
                    s.push(c);
                    chars.next();
                }
            }
            tokens.push(s);
        } else {
            // Unquoted word
            let mut word = String::new();
            while let Some(&c) = chars.peek() {
                if c.is_whitespace() {
                    break;
                }
                word.push(c);
                chars.next();
            }
            tokens.push(word);
        }
    }

    // Need at least: type, path, name
    if tokens.len() < 3 {
        return Err(
            "Usage: create <type> <path> \"<name>\" [\"<description>\"] [key=value ...]"
                .to_string(),
        );
    }

    let class = tokens[0].clone();
    let path = tokens[1].clone();
    let name = tokens[2].clone();

    let mut description = None;
    let mut parent_id = None;
    let mut properties = std::collections::HashMap::new();
    let mut idx = 3;

    // Check for optional description (next token without '=')
    if idx < tokens.len() && !tokens[idx].contains('=') {
        description = Some(tokens[idx].clone());
        idx += 1;
    }

    // Parse key=value pairs
    while idx < tokens.len() {
        let token = &tokens[idx];
        if let Some(eq_pos) = token.find('=') {
            let key = &token[..eq_pos];
            let val_str = &token[eq_pos + 1..];

            // Parse value - try numeric, then boolean, then string
            let value: serde_json::Value = if let Ok(n) = val_str.parse::<i64>() {
                serde_json::json!(n)
            } else if let Ok(n) = val_str.parse::<f64>() {
                serde_json::json!(n)
            } else if val_str == "true" {
                serde_json::json!(true)
            } else if val_str == "false" {
                serde_json::json!(false)
            } else {
                serde_json::json!(val_str)
            };

            if key == "parent" {
                parent_id = Some(val_str.to_string());
            } else {
                properties.insert(key.to_string(), value);
            }
        }
        idx += 1;
    }

    Ok(CreateParams {
        class,
        path,
        name,
        description,
        parent_id,
        properties,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session(
        player_id: &str,
        room: &str,
        cap: usize,
    ) -> (PlayerSession, mpsc::Receiver<ServerMessage>) {
        let (tx, rx) = mpsc::channel(cap);
        (
            PlayerSession {
                player_id: player_id.to_string(),
                account_id: String::new(),
                universe_id: "u".to_string(),
                room_id: Some(room.to_string()),
                sender: tx,
                conn_id: 0,
            },
            rx,
        )
    }

    fn out(text: &str) -> ServerMessage {
        ServerMessage::Output {
            text: text.to_string(),
        }
    }

    /// A client that stops reading must not block senders or the session
    /// lock: messages to a full queue are dropped.
    #[tokio::test]
    async fn test_full_client_queue_does_not_block() {
        let conns = ConnectionManager::new();
        let (stuck, _stuck_rx) = session("stuck", "r", 1);
        let (ok, mut ok_rx) = session("ok", "r", 8);
        conns.register(stuck).await;
        conns.register(ok).await;

        let deliver = async {
            for i in 0..5 {
                conns
                    .broadcast_room_except("u", "r", None, out(&format!("b{i}")))
                    .await;
                conns.send_to_player("u", "stuck", out("direct")).await;
            }
            // Writers still get the lock
            conns.update_room("u", "ok", Some("r2".to_string())).await;
        };
        tokio::time::timeout(std::time::Duration::from_secs(2), deliver)
            .await
            .expect("delivery must not block on a full client queue");

        assert!(matches!(ok_rx.recv().await, Some(ServerMessage::Output { text }) if text == "b0"));
    }

    /// Sessions are per (universe, player); a replaced connection's
    /// unregister leaves the replacement in place
    #[tokio::test]
    async fn test_sessions_keyed_by_universe_and_connection() {
        let conns = ConnectionManager::new();
        let (mut a, _a_rx) = session("p", "r", 4);
        a.universe_id = "one".into();
        a.conn_id = 1;
        let (mut b, mut b_rx) = session("p", "r", 4);
        b.universe_id = "two".into();
        b.conn_id = 2;
        conns.register(a).await;
        conns.register(b).await;

        // A replacement connection in universe "one"
        let (mut a2, mut a2_rx) = session("p", "r", 4);
        a2.universe_id = "one".into();
        a2.conn_id = 3;
        conns.register(a2).await;
        conns.unregister("one", "p", 1).await; // the stale connection's cleanup

        conns.send_to_player("one", "p", out("to one")).await;
        conns.send_to_player("two", "p", out("to two")).await;
        assert!(matches!(a2_rx.try_recv(), Ok(ServerMessage::Output { text }) if text == "to one"));
        assert!(matches!(b_rx.try_recv(), Ok(ServerMessage::Output { text }) if text == "to two"));
    }

    /// A stale connection's unregister reports that it removed nothing
    #[tokio::test]
    async fn test_unregister_reports_replacement() {
        let conns = ConnectionManager::new();
        let (mut a, _a_rx) = session("p", "r", 4);
        a.universe_id = "u".into();
        a.conn_id = 1;
        conns.register(a).await;
        let (mut a2, _a2_rx) = session("p", "r", 4);
        a2.universe_id = "u".into();
        a2.conn_id = 2;
        conns.register(a2).await;
        assert!(
            !conns.unregister("u", "p", 1).await,
            "replaced: nothing removed"
        );
        assert!(conns.unregister("u", "p", 2).await);
    }

    /// An expired disconnect is not handled for a player who is connected
    /// again, even if the reconnect raced past the cancel
    #[tokio::test]
    async fn test_expired_disconnect_skips_connected_player() {
        let conns = ConnectionManager::new();
        let (_rx, gen) = conns.schedule_disconnect("u", "p").await;
        // A new connection registers without having cancelled anything
        let (mut s, _s_rx) = session("p", "r", 4);
        s.universe_id = "u".into();
        conns.register(s).await;
        assert!(!conns.claim_expired_disconnect("u", "p", gen).await);

        // With nobody connected, the matching disconnect is claimed once
        let (_rx, gen) = conns.schedule_disconnect("u", "q").await;
        assert!(conns.claim_expired_disconnect("u", "q", gen).await);
        assert!(!conns.claim_expired_disconnect("u", "q", gen).await);

        // A newer scheduled disconnect is not claimed by an older one
        let (_rx, old) = conns.schedule_disconnect("u", "r").await;
        let (_rx2, new) = conns.schedule_disconnect("u", "r").await;
        assert!(!conns.claim_expired_disconnect("u", "r", old).await);
        assert!(conns.claim_expired_disconnect("u", "r", new).await);
    }
}
