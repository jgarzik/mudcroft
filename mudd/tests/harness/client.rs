//! TestClient - Role-based authenticated WebSocket client for testing
//!
//! Supports Guest, Player, Builder, Wizard, and Admin roles with
//! automatic account creation and authentication.

#![allow(dead_code)]

use std::time::Duration;

use anyhow::{bail, Result};
use futures_util::{SinkExt, StreamExt};
use serde_json::Value;
use tokio_tungstenite::{connect_async, tungstenite::Message};

use super::server::TestServer;

/// Role for test client authentication
#[derive(Debug, Clone)]
pub enum Role {
    /// Unauthenticated guest connection
    Guest,
    /// Regular player (creates account with password "test123")
    Player { username: String },
    /// Builder with assigned regions (creates account with password "test123")
    Builder {
        username: String,
        regions: Vec<String>,
    },
    /// Wizard-level access (creates account with password "test123")
    Wizard { username: String },
    /// Admin-level access (creates account with password "test123")
    Admin { username: String },
}

impl Role {
    /// Get the username for this role, if any
    pub fn username(&self) -> Option<&str> {
        match self {
            Role::Guest => None,
            Role::Player { username } => Some(username),
            Role::Builder { username, .. } => Some(username),
            Role::Wizard { username } => Some(username),
            Role::Admin { username } => Some(username),
        }
    }

    /// Get the access level string for this role
    pub fn access_level(&self) -> &'static str {
        match self {
            Role::Guest => "guest",
            Role::Player { .. } => "player",
            Role::Builder { .. } => "builder",
            Role::Wizard { .. } => "wizard",
            Role::Admin { .. } => "admin",
        }
    }

    /// Default test password for all roles
    pub fn password() -> &'static str {
        "test123"
    }
}

/// WebSocket test client with role-based authentication
pub struct TestClient {
    role: Role,
    write: futures_util::stream::SplitSink<
        tokio_tungstenite::WebSocketStream<
            tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
        >,
        Message,
    >,
    read: futures_util::stream::SplitStream<
        tokio_tungstenite::WebSocketStream<
            tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
        >,
    >,
    account_id: Option<String>,
    auth_token: Option<String>,
    player_id: Option<String>,
}

impl TestClient {
    /// Connect and authenticate as the specified role
    pub async fn connect(server: &TestServer, role: Role) -> Result<Self> {
        // Register account if needed
        let (account_id, auth_token) = match &role {
            Role::Guest => (None, None),
            _ => {
                let username = role.username().unwrap();
                let password = Role::password();

                // Try to register (may already exist)
                let reg_resp = server
                    .post(
                        "/auth/register",
                        &serde_json::json!({
                            "username": username,
                            "password": password
                        }),
                    )
                    .await?;

                let (account_id, token) = if reg_resp.status().is_success() {
                    let body: Value = reg_resp.json().await?;
                    (
                        body["account_id"].as_str().unwrap().to_string(),
                        body["token"].as_str().unwrap().to_string(),
                    )
                } else {
                    // Account exists, login instead
                    let login_resp = server
                        .post(
                            "/auth/login",
                            &serde_json::json!({
                                "username": username,
                                "password": password
                            }),
                        )
                        .await?;

                    if !login_resp.status().is_success() {
                        bail!("Failed to login as {}: {:?}", username, login_resp.status());
                    }

                    let body: Value = login_resp.json().await?;
                    (
                        body["account_id"].as_str().unwrap().to_string(),
                        body["token"].as_str().unwrap().to_string(),
                    )
                };

                // Set access level if not player
                match &role {
                    Role::Builder { .. } | Role::Wizard { .. } | Role::Admin { .. } => {
                        let level = role.access_level();
                        let account_service =
                            mudd::auth::accounts::AccountService::new(server.pool().clone());
                        account_service.set_access_level(&account_id, level).await?;
                    }
                    _ => {}
                }

                // Assign regions for builder
                if let Role::Builder { regions, .. } = &role {
                    for region in regions {
                        // TODO: Add to assigned_regions when that table exists
                        let _ = region;
                    }
                }

                (Some(account_id), Some(token))
            }
        };

        // Connect WebSocket with or without token
        let ws_url = match &auth_token {
            Some(token) => server.ws_url_with_token(token),
            None => server.ws_url(),
        };

        let (ws_stream, _) = connect_async(&ws_url).await?;
        let (write, read) = ws_stream.split();

        let mut client = Self {
            role,
            write,
            read,
            account_id,
            auth_token,
            player_id: None,
        };

        // Receive and process welcome message
        let welcome = client.recv_json_timeout(Duration::from_secs(5)).await?;
        if welcome["type"] == "welcome" {
            client.player_id = welcome["player_id"].as_str().map(|s| s.to_string());
        }

        Ok(client)
    }

    /// Get the role this client is connected as
    pub fn role(&self) -> &Role {
        &self.role
    }

    /// Get the account ID if authenticated
    pub fn account_id(&self) -> Option<&str> {
        self.account_id.as_deref()
    }

    /// Get the auth token if authenticated
    pub fn auth_token(&self) -> Option<&str> {
        self.auth_token.as_deref()
    }

    /// Get the player ID from the welcome message
    pub fn player_id(&self) -> Option<&str> {
        self.player_id.as_deref()
    }

    /// Send a game command
    pub async fn command(&mut self, text: &str) -> Result<()> {
        self.send_command(text).await
    }

    /// Send a command message
    pub async fn send_command(&mut self, text: &str) -> Result<()> {
        let msg = serde_json::json!({
            "type": "command",
            "text": text
        });
        self.write
            .send(Message::Text(msg.to_string().into()))
            .await?;
        Ok(())
    }

    /// Send a ping message
    pub async fn send_ping(&mut self) -> Result<()> {
        let msg = serde_json::json!({
            "type": "ping"
        });
        self.write
            .send(Message::Text(msg.to_string().into()))
            .await?;
        Ok(())
    }

    /// Receive the next message as JSON
    pub async fn recv_json(&mut self) -> Result<Value> {
        loop {
            match self.read.next().await {
                Some(Ok(Message::Text(text))) => {
                    return Ok(serde_json::from_str(&text)?);
                }
                Some(Ok(Message::Close(_))) | None => {
                    bail!("WebSocket closed");
                }
                _ => continue, // Skip binary/ping/pong frames
            }
        }
    }

    /// Receive with timeout
    pub async fn recv_json_timeout(&mut self, timeout: Duration) -> Result<Value> {
        match tokio::time::timeout(timeout, self.recv_json()).await {
            Ok(result) => result,
            Err(_) => bail!("Timeout waiting for WebSocket message"),
        }
    }

    /// Wait for a message of a specific type
    pub async fn expect(&mut self, msg_type: &str) -> Result<Value> {
        self.expect_timeout(msg_type, Duration::from_secs(5)).await
    }

    /// Wait for a message of a specific type with timeout
    pub async fn expect_timeout(&mut self, msg_type: &str, timeout: Duration) -> Result<Value> {
        let deadline = tokio::time::Instant::now() + timeout;

        loop {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                bail!("Timeout waiting for message type '{}'", msg_type);
            }

            let msg = self.recv_json_timeout(remaining).await?;
            if msg["type"] == msg_type {
                return Ok(msg);
            }
            // Continue waiting for the right type
        }
    }

    /// Wait for any message (returns next message regardless of type)
    pub async fn expect_any(&mut self) -> Result<Value> {
        self.recv_json_timeout(Duration::from_secs(5)).await
    }

    /// Wait for any message with custom timeout
    pub async fn expect_any_timeout(&mut self, timeout: Duration) -> Result<Value> {
        self.recv_json_timeout(timeout).await
    }

    /// Drain all pending messages (non-blocking)
    pub async fn drain(&mut self) -> Vec<Value> {
        let mut messages = Vec::new();
        while let Ok(Ok(msg)) =
            tokio::time::timeout(Duration::from_millis(50), self.recv_json()).await
        {
            messages.push(msg);
        }
        messages
    }

    /// Close the connection
    pub async fn close(&mut self) -> Result<()> {
        self.write.close().await?;
        Ok(())
    }
}
