//! TestClient - Role-based authenticated WebSocket client for testing
//!
//! Supports Player, Builder, Wizard, and Admin roles with automatic
//! account creation and authentication (there are no guests).

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
    /// Get the username for this role
    pub fn username(&self) -> &str {
        match self {
            Role::Player { username }
            | Role::Builder { username, .. }
            | Role::Wizard { username }
            | Role::Admin { username } => username,
        }
    }

    /// Get the access level string for this role
    pub fn access_level(&self) -> &'static str {
        match self {
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
    account_id: String,
    auth_token: String,
    player_id: Option<String>,
    /// Messages already read from the socket but not yet returned
    pending: std::collections::VecDeque<Value>,
}

impl TestClient {
    /// Connect and authenticate as the specified role
    pub async fn connect(server: &TestServer, role: Role) -> Result<Self> {
        Self::connect_in(server, role, server.universe_id()).await
    }

    /// Connect to a given universe as the specified role; a builder or
    /// wizard role is made a member of that universe at that level
    pub async fn connect_in(server: &TestServer, role: Role, universe_id: &str) -> Result<Self> {
        // Register account if needed
        let (account_id, auth_token) = {
            {
                let username = role.username();
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

                // Builders and wizards are members of the test universe;
                // admins are global
                match &role {
                    Role::Builder { .. } | Role::Wizard { .. } => {
                        sqlx::query(
                            "INSERT OR REPLACE INTO universe_members (universe_id, account_id, level)
                             VALUES (?, ?, ?)",
                        )
                        .bind(universe_id)
                        .bind(&account_id)
                        .bind(role.access_level())
                        .execute(server.pool())
                        .await?;
                    }
                    Role::Admin { .. } => {
                        let account_service =
                            mudd::auth::accounts::AccountService::new(server.pool().clone());
                        account_service
                            .set_access_level(&account_id, role.access_level())
                            .await?;
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

                (account_id, token)
            }
        };

        let ws_url = format!(
            "ws://{}/ws?token={}&universe={}",
            server.addr, auth_token, universe_id
        );

        let (ws_stream, _) = connect_async(&ws_url).await?;
        let (write, read) = ws_stream.split();

        let mut client = Self {
            role,
            write,
            read,
            account_id,
            auth_token,
            player_id: None,
            pending: std::collections::VecDeque::new(),
        };

        // Receive and process welcome message
        let welcome = client.recv_json_timeout(Duration::from_secs(5)).await?;
        if welcome["type"] == "welcome" {
            client.player_id = welcome["player_id"].as_str().map(|s| s.to_string());

            // The server always follows the welcome with the spawn room (or
            // a "not initialized" notice). Wait for it here and queue it, so
            // a later drain() or expect() sees it deterministically instead
            // of racing its arrival.
            let spawn = client.recv_json_timeout(Duration::from_secs(5)).await?;
            client.pending.push_back(spawn);
        }

        Ok(client)
    }

    /// Get the role this client is connected as
    pub fn role(&self) -> &Role {
        &self.role
    }

    /// Get the account ID
    pub fn account_id(&self) -> &str {
        &self.account_id
    }

    /// Get the auth token
    pub fn auth_token(&self) -> &str {
        &self.auth_token
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
        if let Some(msg) = self.pending.pop_front() {
            return Ok(msg);
        }
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

    /// Wait for an `output` message whose text contains `needle`
    /// (skipping other messages). Returns the full text.
    pub async fn expect_output_containing(&mut self, needle: &str) -> Result<String> {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        loop {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                bail!("Timeout waiting for output containing {:?}", needle);
            }
            let msg = self.recv_json_timeout(remaining).await?;
            if msg["type"] == "output" {
                let text = msg["text"].as_str().unwrap_or_default();
                if text.contains(needle) {
                    return Ok(text.to_string());
                }
            }
        }
    }

    /// Texts of all `output` messages received within `window`
    pub async fn outputs_within(&mut self, window: Duration) -> Vec<String> {
        let deadline = tokio::time::Instant::now() + window;
        let mut texts = Vec::new();
        loop {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                return texts;
            }
            match self.recv_json_timeout(remaining).await {
                Ok(msg) if msg["type"] == "output" => {
                    texts.push(msg["text"].as_str().unwrap_or_default().to_string())
                }
                Ok(_) => {}
                Err(_) => return texts,
            }
        }
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
