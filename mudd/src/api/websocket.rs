//! WebSocket handler for real-time player connections

use std::collections::HashMap;

use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        Query, State,
    },
    response::IntoResponse,
};
use mlua::Value;
use serde::{Deserialize, Serialize};
use tokio::sync::{mpsc, RwLock};
use tracing::{info, warn};

use super::AppState;
use crate::auth::accounts::{Account, AccountService};
use crate::lua::{GameApi, Sandbox, SandboxConfig};
use crate::permissions::AccessLevel;

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

/// Connection manager for all active WebSocket connections
#[derive(Default)]
pub struct ConnectionManager {
    sessions: RwLock<HashMap<String, PlayerSession>>,
}

impl ConnectionManager {
    pub fn new() -> Self {
        Self::default()
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

    /// Send a message to a specific player
    pub async fn send_to_player(&self, player_id: &str, msg: ServerMessage) {
        if let Some(sender) = self.get_sender(player_id).await {
            if sender.send(msg).await.is_err() {
                warn!("Failed to send message to player {}", player_id);
            }
        }
    }

    /// Broadcast a message to all players in a room
    pub async fn broadcast_room(&self, room_id: &str, msg: ServerMessage) {
        let sessions = self.sessions.read().await;
        for session in sessions.values() {
            if session.room_id.as_deref() == Some(room_id)
                && session.sender.send(msg.clone()).await.is_err()
            {
                warn!("Failed to broadcast to player {}", session.player_id);
            }
        }
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
}

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
}

/// Handle WebSocket upgrade with optional token authentication
pub async fn ws_handler(
    ws: WebSocketUpgrade,
    Query(params): Query<WsParams>,
    State(state): State<AppState>,
) -> impl IntoResponse {
    // Validate token if provided
    let account = if let Some(token) = params.token {
        let service = AccountService::new(state.db.pool().clone());
        service.validate_token(&token).await.ok().flatten()
    } else {
        None
    };

    ws.on_upgrade(move |socket| handle_socket(socket, state, account))
}

/// Handle an individual WebSocket connection
async fn handle_socket(mut socket: WebSocket, state: AppState, account: Option<Account>) {
    // Create message channel for this connection
    let (tx, mut rx) = mpsc::channel::<ServerMessage>(32);

    // Generate player ID (use account ID if authenticated, else random UUID)
    let player_id = uuid::Uuid::new_v4().to_string();
    let player_id_clone = player_id.clone();

    let (account_id, username, access_level) = match &account {
        Some(acc) => {
            let access = acc.access_level.parse().unwrap_or(AccessLevel::Player);
            (acc.id.clone(), Some(acc.username.clone()), access)
        }
        None => (String::new(), None, AccessLevel::Player),
    };

    if let Some(ref name) = username {
        info!(
            "WebSocket connected: {} ({}) access={:?}",
            player_id, name, access_level
        );
    } else {
        info!("WebSocket connected: {} (guest)", player_id);
    }

    // Create session
    let session = PlayerSession {
        player_id: player_id.clone(),
        account_id: account_id.clone(),
        universe_id: String::new(), // TODO: from selection
        room_id: None,
        access_level,
        sender: tx,
    };

    state.connections.register(session).await;

    // Send welcome message with theme
    // TODO: Get theme_id from universe config when universe selection is implemented
    let theme_id = crate::theme::DEFAULT_THEME_ID.to_string();
    let welcome = ServerMessage::Welcome {
        player_id: player_id.clone(),
        theme_id,
    };
    if let Ok(json) = serde_json::to_string(&welcome) {
        let _ = socket.send(Message::Text(json.into())).await;
    }

    // Spawn player in starting room
    let universe_id = "default"; // TODO: from universe selection
    if let Ok(rooms) = state.object_store.get_by_class(universe_id, "room").await {
        if let Some(first_room) = rooms.first() {
            let room_id = first_room.id.clone();
            state
                .connections
                .update_room(&player_id, Some(room_id.clone()))
                .await;

            // Send initial room description
            if let Some(room_msg) = build_room_message(&state, &room_id).await {
                if let Ok(json) = serde_json::to_string(&room_msg) {
                    let _ = socket.send(Message::Text(json.into())).await;
                }
            }
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

    // Clean up
    state.connections.unregister(&player_id_clone).await;
    info!("WebSocket disconnected: {}", player_id_clone);
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

            // Parse and execute the command
            let response = execute_command(state, player_id, account_id, access_level, &text).await;
            state.connections.send_to_player(player_id, response).await;
        }
        ClientMessage::Ping => {
            // Just keep the connection alive, no response needed
        }
    }
}

/// Build a Room message from a room object
async fn build_room_message(state: &AppState, room_id: &str) -> Option<ServerMessage> {
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

    Some(ServerMessage::Room {
        name,
        description,
        exits,
        contents: content_names,
        image_hash,
    })
}

/// Parse and execute a player command
async fn execute_command(
    state: &AppState,
    player_id: &str,
    account_id: &str,
    access_level: AccessLevel,
    command: &str,
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
                if let Some(room_msg) = build_room_message(state, &room_id).await {
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

            // Update player's room
            state
                .connections
                .update_room(player_id, Some(dest_room_id.clone()))
                .await;

            // Return new room description
            if let Some(room_msg) = build_room_message(state, &dest_room_id).await {
                return room_msg;
            }

            ServerMessage::Output {
                text: "You move but find yourself nowhere.".to_string(),
            }
        }
        "say" => {
            let message = parts[1..].join(" ");
            ServerMessage::Output {
                text: format!("You say: {}", message),
            }
        }
        "help" => ServerMessage::Output {
            text: "Commands: look, north/south/east/west, say <message>, eval <lua>, help"
                .to_string(),
        },
        "eval" => {
            // Wizard+ only
            if access_level < AccessLevel::Wizard {
                return ServerMessage::Error {
                    message: "Permission denied: wizard+ required for eval".to_string(),
                };
            }

            // Get the code after "eval "
            let code = if command.len() > 5 {
                &command[5..]
            } else {
                return ServerMessage::Error {
                    message: "Usage: eval <lua code>".to_string(),
                };
            };

            // Execute Lua code
            execute_lua(state, player_id, account_id, code).await
        }
        _ => ServerMessage::Output {
            text: format!("Unknown command: {}", verb),
        },
    }
}

/// Execute Lua code in sandbox with game API
async fn execute_lua(
    state: &AppState,
    _player_id: &str,
    account_id: &str,
    code: &str,
) -> ServerMessage {
    let universe_id = "default"; // TODO: universe from session

    // Pre-load universe libraries (before creating sandbox)
    let lib_codes = match load_universe_lib_codes(state, universe_id).await {
        Ok(codes) => codes,
        Err(e) => {
            return ServerMessage::Error {
                message: format!("Failed to load universe libs: {}", e),
            };
        }
    };

    // Create game API with all managers
    let mut game_api = GameApi::new(
        state.object_store.clone(),
        state.classes.clone(),
        state.actions.clone(),
        state.messages.clone(),
        state.permissions.clone(),
        state.timers.clone(),
        state.credits.clone(),
        state.venice.clone(),
        universe_id,
    );
    game_api.set_user_context(Some(account_id.to_string()));

    // Create sandbox with generous limits for wizards
    let config = SandboxConfig {
        max_instructions: 10_000_000, // 10M instructions
        timeout: std::time::Duration::from_secs(5),
        ..Default::default()
    };

    let mut sandbox = match Sandbox::new(config) {
        Ok(s) => s,
        Err(e) => {
            return ServerMessage::Error {
                message: format!("Failed to create sandbox: {}", e),
            };
        }
    };

    // Register game API
    if let Err(e) = game_api.register(sandbox.lua()) {
        return ServerMessage::Error {
            message: format!("Failed to register game API: {}", e),
        };
    }

    // Execute pre-loaded universe libraries
    for (lib_name, lib_code) in &lib_codes {
        let result: Result<(), _> = sandbox.execute(lib_code);
        if let Err(e) = result {
            return ServerMessage::Error {
                message: format!("Failed to execute library {}: {}", lib_name, e),
            };
        }
    }

    // Execute the code
    let result: Result<Value, _> = sandbox.execute(code);

    match result {
        Ok(value) => {
            let text = lua_value_to_string(&value);
            ServerMessage::Output { text }
        }
        Err(e) => ServerMessage::Error {
            message: format!("Lua error: {}", e),
        },
    }
}

/// Convert Lua value to displayable string
fn lua_value_to_string(value: &Value) -> String {
    match value {
        Value::Nil => "nil".to_string(),
        Value::Boolean(b) => b.to_string(),
        Value::Integer(i) => i.to_string(),
        Value::Number(n) => n.to_string(),
        Value::String(s) => s
            .to_str()
            .map(|s| s.to_string())
            .unwrap_or_else(|_| "<invalid utf8>".to_string()),
        Value::Table(_) => "[table]".to_string(),
        Value::Function(_) => "[function]".to_string(),
        Value::Thread(_) => "[thread]".to_string(),
        Value::UserData(_) => "[userdata]".to_string(),
        Value::LightUserData(_) => "[lightuserdata]".to_string(),
        Value::Error(e) => format!("[error: {}]", e),
        _ => "[unknown]".to_string(),
    }
}

/// Load universe library codes from database
/// Returns Vec of (lib_name, code) pairs, sorted by name for determinism
async fn load_universe_lib_codes(
    state: &AppState,
    universe_id: &str,
) -> Result<Vec<(String, String)>, String> {
    // Get universe config
    let universe = match state.object_store.get_universe(universe_id).await {
        Ok(Some(u)) => u,
        Ok(None) => return Ok(Vec::new()), // No universe = no libs to load
        Err(e) => return Err(format!("Failed to get universe: {}", e)),
    };

    // Get lib_hashes from config
    let lib_hashes = match universe.config.get("lib_hashes") {
        Some(hashes) => hashes,
        None => return Ok(Vec::new()), // No libs defined
    };

    // Parse lib_hashes as object
    let hashes_obj = match lib_hashes.as_object() {
        Some(obj) => obj,
        None => return Ok(Vec::new()), // Invalid format, skip
    };

    // Load each library in order (sorted for determinism)
    let mut lib_names: Vec<_> = hashes_obj.keys().collect();
    lib_names.sort();

    let mut lib_codes = Vec::new();

    for lib_name in lib_names {
        let hash = match hashes_obj.get(lib_name).and_then(|v| v.as_str()) {
            Some(h) => h,
            None => continue,
        };

        // Get code from code_store
        let code = match state.object_store.get_code(hash).await {
            Ok(Some(c)) => c,
            Ok(None) => {
                tracing::warn!(
                    "Library {} with hash {} not found in code_store",
                    lib_name,
                    hash
                );
                continue;
            }
            Err(e) => {
                tracing::warn!("Failed to load library {}: {}", lib_name, e);
                continue;
            }
        };

        lib_codes.push((lib_name.clone(), code));
    }

    Ok(lib_codes)
}
