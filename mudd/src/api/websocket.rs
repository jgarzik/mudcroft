//! WebSocket handler for real-time player connections

use std::collections::HashMap;

use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        State,
    },
    response::IntoResponse,
};
use serde::{Deserialize, Serialize};
use tokio::sync::{mpsc, RwLock};
use tracing::{info, warn};

use super::AppState;

/// A connected player session
#[derive(Debug)]
pub struct PlayerSession {
    pub player_id: String,
    pub account_id: String,
    pub universe_id: String,
    pub room_id: Option<String>,
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
}

/// Messages sent from server to client
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type")]
pub enum ServerMessage {
    /// Welcome message on connect
    #[serde(rename = "welcome")]
    Welcome { player_id: String },
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

/// Handle WebSocket upgrade
pub async fn ws_handler(
    ws: WebSocketUpgrade,
    State(state): State<AppState>,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_socket(socket, state))
}

/// Handle an individual WebSocket connection
async fn handle_socket(mut socket: WebSocket, state: AppState) {
    // Create message channel for this connection
    let (tx, mut rx) = mpsc::channel::<ServerMessage>(32);

    // Generate temporary player ID (would normally come from auth)
    let player_id = uuid::Uuid::new_v4().to_string();
    let player_id_clone = player_id.clone();

    info!("WebSocket connected: {}", player_id);

    // Create session
    let session = PlayerSession {
        player_id: player_id.clone(),
        account_id: String::new(), // TODO: from auth
        universe_id: String::new(), // TODO: from selection
        room_id: None,
        sender: tx,
    };

    state.connections.register(session).await;

    // Send welcome message
    let welcome = ServerMessage::Welcome {
        player_id: player_id.clone(),
    };
    if let Ok(json) = serde_json::to_string(&welcome) {
        let _ = socket.send(Message::Text(json.into())).await;
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
                            handle_client_message(&state, &player_id, client_msg).await;
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
async fn handle_client_message(state: &AppState, player_id: &str, msg: ClientMessage) {
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
            let response = execute_command(state, player_id, &text).await;
            state.connections.send_to_player(player_id, response).await;
        }
        ClientMessage::Ping => {
            // Just keep the connection alive, no response needed
        }
    }
}

/// Parse and execute a player command
async fn execute_command(_state: &AppState, _player_id: &str, command: &str) -> ServerMessage {
    let parts: Vec<&str> = command.split_whitespace().collect();

    if parts.is_empty() {
        return ServerMessage::Output {
            text: "What?".to_string(),
        };
    }

    let verb = parts[0].to_lowercase();

    match verb.as_str() {
        "look" | "l" => ServerMessage::Output {
            text: "You see nothing special.".to_string(),
        },
        "north" | "n" | "south" | "s" | "east" | "e" | "west" | "w" | "up" | "u" | "down" | "d" => {
            // TODO: Actually move the player
            ServerMessage::Output {
                text: format!("You go {}.", verb),
            }
        }
        "say" => {
            let message = parts[1..].join(" ");
            ServerMessage::Output {
                text: format!("You say: {}", message),
            }
        }
        "help" => ServerMessage::Output {
            text: "Commands: look, north/south/east/west, say <message>, help".to_string(),
        },
        _ => ServerMessage::Output {
            text: format!("Unknown command: {}", verb),
        },
    }
}
