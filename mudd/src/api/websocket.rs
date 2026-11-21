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
use crate::auth::accounts::{Account, AccountService};
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
    pub access_level: AccessLevel,
    pub sender: mpsc::Sender<ServerMessage>,
}

/// Grace period for reconnection (prevents inventory drop on brief disconnects)
const RECONNECT_GRACE_PERIOD: std::time::Duration = std::time::Duration::from_secs(5);

/// Connection manager for all active WebSocket connections
pub struct ConnectionManager {
    sessions: RwLock<BTreeMap<String, PlayerSession>>,
    /// Pending disconnect tasks that can be cancelled on reconnect
    pending_disconnects: RwLock<BTreeMap<String, tokio::sync::oneshot::Sender<()>>>,
}

impl Default for ConnectionManager {
    fn default() -> Self {
        Self {
            sessions: RwLock::new(BTreeMap::new()),
            pending_disconnects: RwLock::new(BTreeMap::new()),
        }
    }
}

impl ConnectionManager {
    pub fn new() -> Self {
        Self::default()
    }

    /// Cancel any pending disconnect for this player (called on reconnect)
    pub async fn cancel_pending_disconnect(&self, player_id: &str) -> bool {
        if let Some(cancel_tx) = self.pending_disconnects.write().await.remove(player_id) {
            // Send cancel signal - ignore result since receiver may be dropped
            let _ = cancel_tx.send(());
            info!("Cancelled pending disconnect for player {}", player_id);
            true
        } else {
            false
        }
    }

    /// Schedule a delayed disconnect (grace period for reconnection)
    pub async fn schedule_disconnect(
        &self,
        player_id: String,
    ) -> tokio::sync::oneshot::Receiver<()> {
        let (cancel_tx, cancel_rx) = tokio::sync::oneshot::channel();
        self.pending_disconnects
            .write()
            .await
            .insert(player_id, cancel_tx);
        cancel_rx
    }

    /// Remove pending disconnect entry after grace period expires
    pub async fn clear_pending_disconnect(&self, player_id: &str) {
        self.pending_disconnects.write().await.remove(player_id);
    }

    /// Register a new player session
    pub async fn register(&self, session: PlayerSession) {
        let player_id = session.player_id.clone();
        self.sessions.write().await.insert(player_id, session);
    }

    /// Remove a player session
    pub async fn unregister(&self, player_id: &str) {
        self.sessions.write().await.remove(player_id);
    }

    /// Get a player's sender channel
    pub async fn get_sender(&self, player_id: &str) -> Option<mpsc::Sender<ServerMessage>> {
        self.sessions
            .read()
            .await
            .get(player_id)
            .map(|s| s.sender.clone())
    }

    /// Send a message to a specific player.
    ///
    /// Never blocks: if the player's queue is full (a client that stopped
    /// reading) the message is dropped, so one slow client cannot stall
    /// other players or the player's own command handler.
    pub async fn send_to_player(&self, player_id: &str, msg: ServerMessage) {
        if let Some(sender) = self.get_sender(player_id).await {
            try_deliver(player_id, &sender, msg);
        }
    }

    /// Broadcast a message to all players in a room
    pub async fn broadcast_room(&self, room_id: &str, msg: ServerMessage) {
        self.broadcast_room_except(room_id, None, msg).await;
    }

    /// Broadcast to all players in a room except `exclude_id` (if given)
    pub async fn broadcast_room_except(
        &self,
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
                s.room_id.as_deref() == Some(room_id) && Some(s.player_id.as_str()) != exclude_id
            })
            .map(|s| (s.player_id.clone(), s.sender.clone()))
            .collect();
        for (player_id, sender) in recipients {
            try_deliver(&player_id, &sender, msg.clone());
        }
    }

    /// (player_id, room_id) for every connected player that is in a room
    pub async fn player_rooms(&self) -> Vec<(String, String)> {
        self.sessions
            .read()
            .await
            .values()
            .filter_map(|s| s.room_id.clone().map(|r| (s.player_id.clone(), r)))
            .collect()
    }

    /// Update player's room
    pub async fn update_room(&self, player_id: &str, room_id: Option<String>) {
        if let Some(session) = self.sessions.write().await.get_mut(player_id) {
            session.room_id = room_id;
        }
    }

    /// Get player's current room ID
    pub async fn get_room_id(&self, player_id: &str) -> Option<String> {
        self.sessions
            .read()
            .await
            .get(player_id)
            .and_then(|s| s.room_id.clone())
    }

    /// Get player's universe ID
    pub async fn get_universe_id(&self, player_id: &str) -> Option<String> {
        self.sessions
            .read()
            .await
            .get(player_id)
            .map(|s| s.universe_id.clone())
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
    Command { text: String },
    /// Ping to keep connection alive
    #[serde(rename = "ping")]
    Ping,
}

/// WebSocket query parameters
#[derive(Debug, Deserialize)]
pub struct WsParams {
    pub token: Option<String>,
    pub universe: Option<String>,
}

/// Handle WebSocket upgrade with optional token authentication
pub async fn ws_handler(
    ws: WebSocketUpgrade,
    Query(params): Query<WsParams>,
    State(state): State<AppState>,
) -> impl IntoResponse {
    // Validate universe ID (required)
    let universe_id = match &params.universe {
        Some(id) => match validate_universe_id(id) {
            Ok(validated) => validated,
            Err(e) => {
                return (
                    axum::http::StatusCode::BAD_REQUEST,
                    format!("Invalid universe ID: {}", e),
                )
                    .into_response();
            }
        },
        None => {
            return (
                axum::http::StatusCode::BAD_REQUEST,
                "Universe ID required in query param: ?universe=<id>",
            )
                .into_response();
        }
    };

    // Verify universe exists
    match state.object_store.universe_exists(&universe_id).await {
        Ok(true) => {}
        Ok(false) => {
            return (
                axum::http::StatusCode::NOT_FOUND,
                format!("Universe not found: {}", universe_id),
            )
                .into_response();
        }
        Err(e) => {
            warn!("Error checking universe: {}", e);
            return (
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                "Failed to verify universe",
            )
                .into_response();
        }
    }

    // Validate token if provided
    let account = if let Some(token) = params.token {
        let service = AccountService::new(state.db.pool().clone());
        service.validate_token(&token).await.ok().flatten()
    } else {
        None
    };

    ws.on_upgrade(move |socket| handle_socket(socket, state, account, universe_id))
}

/// Handle an individual WebSocket connection
async fn handle_socket(
    mut socket: WebSocket,
    state: AppState,
    account: Option<Account>,
    universe_id: String,
) {
    // Create message channel for this connection
    let (tx, mut rx) = mpsc::channel::<ServerMessage>(CLIENT_QUEUE_SIZE);

    let (account_id, username, access_level) = match &account {
        Some(acc) => {
            let access = acc.access_level.parse().unwrap_or(AccessLevel::Player);
            (acc.id.clone(), Some(acc.username.clone()), access)
        }
        None => (String::new(), None, AccessLevel::Player),
    };

    // Generate player ID:
    // - Authenticated users: use persistent path /players/<account_id>
    // - Guests: use random UUID (ephemeral, no persistence)
    let player_id = if !account_id.is_empty() {
        crate::player::PlayerManager::player_path(&account_id)
    } else {
        uuid::Uuid::new_v4().to_string()
    };
    let player_id_clone = player_id.clone();

    // Cancel any pending disconnect for this player (reconnection within grace period)
    if !account_id.is_empty() {
        let was_reconnect = state
            .connections
            .cancel_pending_disconnect(&player_id)
            .await;
        if was_reconnect {
            info!("Player {} reconnected within grace period", player_id);
        }
    }

    // For authenticated users, ensure persistent player object exists
    let spawn_room_id = if !account_id.is_empty() {
        let name = username.as_deref().unwrap_or("Unknown");
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
    } else {
        info!(
            "WebSocket connected: {} (guest) universe={}",
            player_id, universe_id
        );
        // Guests spawn at portal
        state
            .object_store
            .get_portal(&universe_id)
            .await
            .ok()
            .flatten()
    };

    // Create session
    let session = PlayerSession {
        player_id: player_id.clone(),
        account_id: account_id.clone(),
        universe_id: universe_id.clone(),
        room_id: spawn_room_id.clone(),
        access_level,
        sender: tx,
    };

    state.connections.register(session).await;

    // Send welcome message with theme
    // TODO: Get theme_id from universe config
    let theme_id = crate::theme::DEFAULT_THEME_ID.to_string();
    let welcome = ServerMessage::Welcome {
        player_id: player_id.clone(),
        theme_id,
    };
    if let Ok(json) = serde_json::to_string(&welcome) {
        let _ = socket.send(Message::Text(json.into())).await;
    }

    // Move player to spawn room and send room description
    if let Some(room_id) = spawn_room_id {
        // Sync player's location to DB for authenticated users
        if !account_id.is_empty() {
            if let Err(e) = state.player_manager.move_player(&player_id, &room_id).await {
                warn!("Failed to sync player location to DB: {}", e);
            }
        }

        // Send initial room description
        let acct_ref = if account_id.is_empty() {
            None
        } else {
            Some(account_id.as_str())
        };
        if let Some(room_msg) = build_room_message(&state, &room_id, acct_ref).await {
            if let Ok(json) = serde_json::to_string(&room_msg) {
                let _ = socket.send(Message::Text(json.into())).await;
            }
        }

        // Arriving in the world counts as entering the spawn room
        run_hook(
            &state,
            &player_id,
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
            let _ = socket.send(Message::Text(json.into())).await;
        }
    }

    // Main loop: handle incoming messages and outgoing messages
    loop {
        tokio::select! {
            // Handle outgoing messages from our channel
            Some(msg) = rx.recv() => {
                if let Ok(json) = serde_json::to_string(&msg) {
                    if socket.send(Message::Text(json.into())).await.is_err() {
                        break;
                    }
                }
            }
            // Handle incoming messages from WebSocket
            result = socket.recv() => {
                match result {
                    Some(Ok(Message::Text(text))) => {
                        if let Ok(client_msg) = serde_json::from_str::<ClientMessage>(&text) {
                            handle_client_message(&state, &player_id, &account_id, access_level, client_msg).await;
                        }
                    }
                    Some(Ok(Message::Close(_))) | None => break,
                    _ => {}
                }
            }
        }
    }

    // Unregister session immediately (stops message delivery)
    state.connections.unregister(&player_id_clone).await;
    info!("WebSocket disconnected: {}", player_id_clone);

    // For authenticated users, use grace period before handling disconnect
    // This allows reconnection without dropping inventory
    if !account_id.is_empty() {
        let cancel_rx = state
            .connections
            .schedule_disconnect(player_id_clone.clone())
            .await;

        // Clone what we need for the spawned task
        let state_clone = state.clone();
        let player_id_for_task = player_id_clone.clone();

        tokio::spawn(async move {
            // Wait for grace period or cancellation
            tokio::select! {
                _ = tokio::time::sleep(RECONNECT_GRACE_PERIOD) => {
                    // Grace period expired - execute disconnect handling
                    state_clone.connections.clear_pending_disconnect(&player_id_for_task).await;
                    if let Err(e) = state_clone.player_manager.handle_disconnect(&player_id_for_task).await {
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
    player_id: &str,
    account_id: &str,
    access_level: AccessLevel,
    msg: ClientMessage,
) {
    match msg {
        ClientMessage::Command { text } => {
            info!("Player {} command: {}", player_id, text);

            // Echo the command back
            state
                .connections
                .send_to_player(
                    player_id,
                    ServerMessage::Echo {
                        command: text.clone(),
                    },
                )
                .await;

            // Parse and execute the command; hooks it triggers that should
            // follow the reply (e.g. on_enter after the room description)
            // run once the reply is queued
            let mut deferred = Vec::new();
            let response = execute_command(
                state,
                player_id,
                account_id,
                access_level,
                &text,
                &mut deferred,
            )
            .await;
            if !matches!(response, ServerMessage::Silent) {
                state.connections.send_to_player(player_id, response).await;
            }
            for hook in deferred {
                run_hook(state, player_id, hook).await;
            }
        }
        ClientMessage::Ping => {
            // Just keep the connection alive, no response needed
        }
    }
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
async fn display_name(state: &AppState, player_id: &str) -> String {
    match state.object_store.get(player_id).await {
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
async fn build_room_message(
    state: &AppState,
    room_id: &str,
    account_id: Option<&str>,
) -> Option<ServerMessage> {
    // Get room object
    let room = state.object_store.get(room_id).await.ok()??;

    // Get room contents
    let contents = state
        .object_store
        .get_contents(room_id)
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
    if image_hash.is_none() && state.venice.is_configured() {
        if let Some(acct_id) = account_id {
            let venice = state.venice.clone();
            let image_store = state.image_store.clone();
            let object_store = state.object_store.clone();
            let themes = state.themes.clone();
            let room_id_owned = room_id.to_string();
            let account_id_owned = acct_id.to_string();

            // Spawn background task for image generation
            tokio::spawn(async move {
                let theme = themes.get(DEFAULT_THEME_ID);
                match generate_room_image(
                    &venice,
                    &image_store,
                    &object_store,
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
async fn run_hook(state: &AppState, player_id: &str, hook: Hook) {
    let Some(universe_id) = state.connections.get_universe_id(player_id).await else {
        return;
    };
    if let Err(e) = scripting::invoke_handler(
        state,
        &universe_id,
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
            if let Some(room_id) = state.connections.get_room_id(player_id).await {
                let acct_ref = if account_id.is_empty() {
                    None
                } else {
                    Some(account_id)
                };
                if let Some(room_msg) = build_room_message(state, &room_id, acct_ref).await {
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
            let current_room_id = match state.connections.get_room_id(player_id).await {
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
                .get_exit(&current_room_id, direction)
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
                player_id,
                Hook::new(
                    &current_room_id,
                    Some(&current_room_id),
                    "on_leave",
                    player_id,
                    serde_json::json!({ "to": dest_room_id, "direction": direction }),
                ),
            )
            .await;

            // Update player's room in memory
            state
                .connections
                .update_room(player_id, Some(dest_room_id.clone()))
                .await;

            // Sync to database for authenticated users
            if !account_id.is_empty() {
                if let Err(e) = state
                    .player_manager
                    .move_player(player_id, &dest_room_id)
                    .await
                {
                    warn!("Failed to sync player location to DB: {}", e);
                }
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
            let acct_ref = if account_id.is_empty() {
                None
            } else {
                Some(account_id)
            };
            if let Some(room_msg) = build_room_message(state, &dest_room_id, acct_ref).await {
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
            if let Some(room_id) = state.connections.get_room_id(player_id).await {
                let speaker = display_name(state, player_id).await;
                state
                    .connections
                    .broadcast_room_except(
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
                player_id,
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
                player_id,
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
                player_id,
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

            let session_universe = state.connections.get_universe_id(player_id).await;

            // Verify room exists (in this universe)
            match state.object_store.get(&room_id).await {
                Ok(Some(room)) if Some(&room.universe_id) != session_universe.as_ref() => {
                    ServerMessage::Error {
                        message: format!("Room not found: {}", room_id),
                    }
                }
                Ok(Some(room)) if room.class == "room" => {
                    let from = state.connections.get_room_id(player_id).await;
                    if let Some(ref from_id) = from {
                        run_hook(
                            state,
                            player_id,
                            Hook::new(
                                from_id,
                                Some(from_id),
                                "on_leave",
                                player_id,
                                serde_json::json!({ "to": room_id }),
                            ),
                        )
                        .await;
                    }

                    // Update player's room, in the session and (for accounts)
                    // the player object, which get/drop and respawn use
                    state
                        .connections
                        .update_room(player_id, Some(room_id.clone()))
                        .await;
                    if !account_id.is_empty() {
                        if let Err(e) = state.player_manager.move_player(player_id, &room_id).await
                        {
                            warn!("Failed to sync player location to DB: {}", e);
                        }
                    }
                    deferred.push(Hook::new(
                        &room_id,
                        Some(&room_id),
                        "on_enter",
                        player_id,
                        serde_json::json!({ "from": from }),
                    ));

                    // Return room description
                    let acct_ref = if account_id.is_empty() {
                        None
                    } else {
                        Some(account_id)
                    };
                    if let Some(room_msg) = build_room_message(state, &room_id, acct_ref).await {
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

            // Get universe from session
            let universe_id = match state.connections.get_universe_id(player_id).await {
                Some(id) => id,
                None => {
                    return ServerMessage::Error {
                        message: "Session error: no universe".to_string(),
                    };
                }
            };

            // Get room_id from args or current room
            let room_id = if parts.len() > 1 {
                parts[1].to_string()
            } else {
                match state.connections.get_room_id(player_id).await {
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
            match state.object_store.get(&room_id).await {
                Ok(Some(room)) if room.universe_id != universe_id => ServerMessage::Error {
                    message: format!("Room not found: {}", room_id),
                },
                Ok(Some(room)) if room.class == "room" => {
                    // Set portal
                    if let Err(e) = state.object_store.set_portal(&universe_id, &room_id).await {
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
            execute_lua(state, player_id, account_id, code).await
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
            let room_id = match state.connections.get_room_id(player_id).await {
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
                .get_contents(&room_id)
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
            let is_npc =
                target.class == "monster" || state.classes.read().await.is_a(&target.class, "npc");
            if !is_npc {
                return ServerMessage::Error {
                    message: format!("You can't attack {}.", target_display_name),
                };
            }

            // Get target HP (initialize combat state if needed)
            let target_hp = target
                .properties
                .get("hp")
                .and_then(|v| v.as_i64())
                .unwrap_or(10) as i32;
            let target_max_hp = target
                .properties
                .get("max_hp")
                .and_then(|v| v.as_i64())
                .unwrap_or(target_hp as i64) as i32;

            // Initialize combat states if needed
            if state.combat.get_state(&target_id).await.is_none() {
                state
                    .combat
                    .init_entity_with_universe(
                        &target_id,
                        state
                            .connections
                            .get_universe_id(player_id)
                            .await
                            .as_deref(),
                        target_max_hp,
                    )
                    .await;
                // Set current HP to match object property
                if let Some(mut combat_state) = state.combat.get_state(&target_id).await {
                    combat_state.hp = target_hp;
                    state.combat.update_state(&target_id, combat_state).await;
                }
            }
            if state.combat.get_state(player_id).await.is_none() {
                state
                    .combat
                    .init_entity_with_universe(
                        player_id,
                        state
                            .connections
                            .get_universe_id(player_id)
                            .await
                            .as_deref(),
                        100, // Default player HP
                    )
                    .await;
            }

            // Initiate combat
            if let Err(e) = state.combat.initiate(player_id, &target_id).await {
                return ServerMessage::Error {
                    message: format!("Combat error: {}", e),
                };
            }

            // Perform attack (basic 1d6 physical damage; the combat manager
            // takes the rolled amount and doubles it on a critical)
            let damage_roll = {
                use rand::Rng;
                rand::rng().random_range(1..=6)
            };
            let attack_result = match state
                .combat
                .attack(player_id, &target_id, damage_roll, DamageType::Physical)
                .await
            {
                Ok(result) => result,
                Err(e) => {
                    return ServerMessage::Error {
                        message: format!("Attack failed: {}", e),
                    };
                }
            };

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
                } else {
                    messages.push(format!(
                        "You hit {} for {} damage.",
                        target_display_name, damage
                    ));
                }

                // Check if target is dead
                if state.combat.is_dead(&target_id).await {
                    messages.push(format!("{} is slain!", target_display_name));
                    deferred.push(Hook::new(
                        &target_id,
                        Some(&room_id),
                        "on_death",
                        player_id,
                        serde_json::json!({ "killer_id": player_id }),
                    ));

                    // End combat and remove target from room
                    state.combat.end_combat(player_id).await;
                    state.combat.remove_entity(&target_id).await;

                    // Remove NPC from room (set parent_id to None)
                    if let Ok(Some(mut dead_target)) = state.object_store.get(&target_id).await {
                        dead_target.parent_id = None;
                        let _ = state.object_store.update(&dead_target).await;
                    }
                } else {
                    // Show remaining HP
                    if let Some(target_state) = state.combat.get_state(&target_id).await {
                        messages.push(format!(
                            "{} has {} HP remaining.",
                            target_display_name, target_state.hp
                        ));
                    }
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
            let attacker_name = display_name(state, player_id).await;
            let mut seen = format!("{} attacks {}.", attacker_name, target_display_name);
            if state.combat.get_state(&target_id).await.is_none() {
                seen.push_str(&format!(" {} is slain!", target_display_name));
            }
            state
                .connections
                .broadcast_room_except(
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
            execute_create_command(state, player_id, account_id, access_level, args).await
        }
        _ => {
            let args = parts[1..].join(" ");
            match dispatch_custom_verb(state, player_id, access_level, &verb, &args).await {
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
    player_id: &str,
    access_level: AccessLevel,
    verb: &str,
    args: &str,
) -> Option<ServerMessage> {
    let room_id = state.connections.get_room_id(player_id).await;

    let mut nearby: Vec<String> = Vec::new();
    // Only objects of this universe (an inventory may hold items carried in
    // from elsewhere; their code cannot run here)
    let universe = state.connections.get_universe_id(player_id).await?;
    if let Ok(items) = state.object_store.get_contents(player_id).await {
        nearby.extend(
            items
                .into_iter()
                .filter(|o| o.universe_id == universe)
                .map(|o| o.id),
        );
    }
    if let Some(ref room) = room_id {
        if let Ok(objs) = state.object_store.get_contents(room).await {
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
                .get_room_actions(room, verb)
                .await
                .into_iter()
                .filter(|a| nearby.contains(&a.object_id)),
        );
    }
    for object_id in &nearby {
        if let Some(action) = state.actions.get_object_action(object_id, verb).await {
            candidates.push(action);
        }
    }
    // Never dispatch to private handlers or engine hooks
    candidates.retain(|a| !crate::lua::is_private_handler(&a.method));
    if candidates.is_empty() {
        return None;
    }

    // Prefer the candidate the player named
    let wanted = args.to_lowercase();
    let mut chosen = candidates[0].clone();
    if !wanted.is_empty() {
        for action in &candidates {
            if let Ok(Some(obj)) = state.object_store.get(&action.object_id).await {
                let name = obj
                    .properties
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_lowercase();
                if !name.is_empty() && (wanted.contains(&name) || name.contains(&wanted)) {
                    chosen = action.clone();
                    break;
                }
            }
        }
    }

    let universe_id = state.connections.get_universe_id(player_id).await?;
    let handler_args = serde_json::json!({
        "verb": verb,
        "args": args,
        "actor_id": player_id,
        "object_id": chosen.object_id,
        "room_id": room_id,
    });
    let result = scripting::invoke_handler(
        state,
        &universe_id,
        &chosen.object_id,
        room_id,
        &chosen.method,
        handler_args,
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
    player_id: &str,
    account_id: &str,
    code: &str,
) -> ServerMessage {
    execute_lua_with_args(state, player_id, account_id, code, Vec::new()).await
}

/// Execute a Lua chunk with `args` available as its varargs (`...`),
/// as the player's account, in the player's universe
async fn execute_lua_with_args(
    state: &AppState,
    player_id: &str,
    account_id: &str,
    code: &str,
    args: Vec<String>,
) -> ServerMessage {
    let Some(universe_id) = state.connections.get_universe_id(player_id).await else {
        return ServerMessage::Error {
            message: "Session error: no universe".to_string(),
        };
    };
    let ctx = ScriptContext {
        universe_id,
        user_id: Some(account_id.to_string()),
        room_id: None,
        object_id: None,
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
    player_id: &str,
    account_id: &str,
    access_level: AccessLevel,
    args: &str,
) -> ServerMessage {
    // Get universe from session
    let universe_id = match state.connections.get_universe_id(player_id).await {
        Some(id) => id,
        None => {
            return ServerMessage::Error {
                message: "Session error: no universe".to_string(),
            };
        }
    };

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
        .get_user_context(account_id, &universe_id)
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
        &universe_id,
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
                access_level: AccessLevel::Player,
                sender: tx,
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
                    .broadcast_room_except("r", None, out(&format!("b{i}")))
                    .await;
                conns.send_to_player("stuck", out("direct")).await;
            }
            // Writers still get the lock
            conns.update_room("ok", Some("r2".to_string())).await;
        };
        tokio::time::timeout(std::time::Duration::from_secs(2), deliver)
            .await
            .expect("delivery must not block on a full client queue");

        assert!(matches!(ok_rx.recv().await, Some(ServerMessage::Output { text }) if text == "b0"));
    }
}
