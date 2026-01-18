//! WebSocket handler for real-time player connections

use std::collections::BTreeMap;

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
use crate::combat::DamageType;
use crate::images::generate_room_image;
use crate::lua::{GameApi, Sandbox, SandboxConfig};
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

/// Connection manager for all active WebSocket connections
#[derive(Default)]
pub struct ConnectionManager {
    sessions: RwLock<BTreeMap<String, PlayerSession>>,
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

    /// Get player's universe ID
    pub async fn get_universe_id(&self, player_id: &str) -> Option<String> {
        self.sessions
            .read()
            .await
            .get(player_id)
            .map(|s| s.universe_id.clone())
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
            "WebSocket connected: {} ({}) universe={} access={:?}",
            player_id, name, universe_id, access_level
        );
    } else {
        info!(
            "WebSocket connected: {} (guest) universe={}",
            player_id, universe_id
        );
    }

    // Create session
    let session = PlayerSession {
        player_id: player_id.clone(),
        account_id: account_id.clone(),
        universe_id: universe_id.clone(),
        room_id: None,
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

    // Spawn player at portal room (if set)
    if let Ok(Some(portal_id)) = state.object_store.get_portal(&universe_id).await {
        state
            .connections
            .update_room(&player_id, Some(portal_id.clone()))
            .await;

        // Send initial room description
        let acct_ref = if account_id.is_empty() {
            None
        } else {
            Some(account_id.as_str())
        };
        if let Some(room_msg) = build_room_message(&state, &portal_id, acct_ref).await {
            if let Ok(json) = serde_json::to_string(&room_msg) {
                let _ = socket.send(Message::Text(json.into())).await;
            }
        }
    } else {
        // No portal set - player stays nowhere
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

            // Update player's room
            state
                .connections
                .update_room(player_id, Some(dest_room_id.clone()))
                .await;

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
            ServerMessage::Output {
                text: format!("You say: {}", message),
            }
        }
        "help" => ServerMessage::Output {
            text: "Commands: look, north/south/east/west, say <message>, get/take <item>, drop <item>, inventory/i, attack <target>, eval <lua>, goto <room_id>, setportal [room_id], help"
                .to_string(),
        },
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
            let code = format!(
                r#"local r = Commands.take("{}", "{}"); return r.message"#,
                player_id.replace('"', r#"\""#),
                item_name.replace('"', r#"\""#)
            );
            execute_lua(state, player_id, account_id, &code).await
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
            let code = format!(
                r#"local r = Commands.drop("{}", "{}"); return r.message"#,
                player_id.replace('"', r#"\""#),
                item_name.replace('"', r#"\""#)
            );
            execute_lua(state, player_id, account_id, &code).await
        }
        "inventory" | "inv" | "i" => {
            // Execute Commands.inventory() via Lua
            let code = format!(
                r#"local r = Commands.inventory("{}"); return r.message"#,
                player_id.replace('"', r#"\""#)
            );
            execute_lua(state, player_id, account_id, &code).await
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

            // Verify room exists
            match state.object_store.get(&room_id).await {
                Ok(Some(room)) if room.class == "room" => {
                    // Update player's room
                    state
                        .connections
                        .update_room(player_id, Some(room_id.clone()))
                        .await;

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
                            message: "You are nowhere. Use 'setportal <room_id>' to specify a room."
                                .to_string(),
                        };
                    }
                }
            };

            // Verify room exists
            match state.object_store.get(&room_id).await {
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

            let target = contents.iter().find(|obj| {
                obj.properties
                    .get("name")
                    .and_then(|v| v.as_str())
                    .map(|n| n.to_lowercase().contains(&target_name))
                    .unwrap_or(false)
            });

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
            let is_npc = target.class == "npc" || target.class == "monster";
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
                        state.connections.get_universe_id(player_id).await.as_deref(),
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
                        state.connections.get_universe_id(player_id).await.as_deref(),
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

            // Perform attack (basic 1d6 physical damage)
            let attack_result = match state
                .combat
                .attack(player_id, &target_id, 6, DamageType::Physical)
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
                let damage = attack_result.damage.as_ref().map(|d| d.final_damage).unwrap_or(0);
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

            // Broadcast combat message to room
            let combat_msg = ServerMessage::Output {
                text: messages.join("\n"),
            };
            state
                .connections
                .broadcast_room(&room_id, combat_msg.clone())
                .await;

            combat_msg
        }
        "create" => {
            // Builder+ only
            if access_level < AccessLevel::Builder {
                return ServerMessage::Error {
                    message: "Permission denied: builder+ required for create".to_string(),
                };
            }

            // Get the arguments after "create "
            let args = if command.len() > 7 {
                &command[7..]
            } else {
                return ServerMessage::Error {
                    message: "Usage: create <type> <path> \"<name>\" [\"<description>\"] [key=value ...]".to_string(),
                };
            };

            // Execute create command
            execute_create_command(state, player_id, account_id, access_level, args).await
        }
        _ => ServerMessage::Output {
            text: format!("Unknown command: {}", verb),
        },
    }
}

/// Execute Lua code in sandbox with game API
async fn execute_lua(
    state: &AppState,
    player_id: &str,
    account_id: &str,
    code: &str,
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

    // Pre-load universe libraries (before creating sandbox)
    let lib_codes = match load_universe_lib_codes(state, &universe_id).await {
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
        state.image_store.clone(),
        &universe_id,
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
        while chars.peek().map_or(false, |c| c.is_whitespace()) {
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
