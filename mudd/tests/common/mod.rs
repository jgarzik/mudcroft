//! Common test utilities - MuddTest harness for end-to-end testing

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use futures_util::{SinkExt, StreamExt};
use mudd::{Config, Server};
use reqwest::Client;
use tokio::task::JoinHandle;
use tokio_tungstenite::{connect_async, tungstenite::Message};

/// Test harness that spawns a real mudd server on a random port
pub struct MuddTest {
    pub addr: SocketAddr,
    pub client: Client,
    server: Arc<Server>,
    _handle: JoinHandle<()>,
}

impl MuddTest {
    /// Start a new test server instance
    pub async fn start() -> Result<Self> {
        // Find a random available port
        let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
        let addr = listener.local_addr()?;
        drop(listener);

        let config = Config {
            bind_addr: addr,
            db_path: None, // In-memory for tests
        };

        let server = Arc::new(Server::new(config).await?);
        let server_clone = server.clone();

        // Spawn the server in a background task
        let handle = tokio::spawn(async move {
            if let Err(e) = server_clone.run().await {
                eprintln!("Server error: {}", e);
            }
        });

        // Wait for server to be ready
        let client = Client::builder().timeout(Duration::from_secs(5)).build()?;

        // Poll until server is ready (max 2 seconds)
        let mut ready = false;
        for _ in 0..20 {
            tokio::time::sleep(Duration::from_millis(100)).await;
            if client
                .get(format!("http://{}/health", addr))
                .send()
                .await
                .is_ok()
            {
                ready = true;
                break;
            }
        }

        if !ready {
            panic!("Server failed to start within 2 seconds");
        }

        Ok(Self {
            addr,
            client,
            server,
            _handle: handle,
        })
    }

    /// Get the base URL for the server
    pub fn base_url(&self) -> String {
        format!("http://{}", self.addr)
    }

    /// Make a GET request
    pub async fn get(&self, path: &str) -> Result<reqwest::Response> {
        Ok(self
            .client
            .get(format!("{}{}", self.base_url(), path))
            .send()
            .await?)
    }

    /// Make a POST request with JSON body
    pub async fn post<T: serde::Serialize + ?Sized>(
        &self,
        path: &str,
        body: &T,
    ) -> Result<reqwest::Response> {
        Ok(self
            .client
            .post(format!("{}{}", self.base_url(), path))
            .json(body)
            .send()
            .await?)
    }

    /// Get direct access to the database for test setup/assertions
    pub fn db(&self) -> Arc<mudd::db::Database> {
        self.server.db()
    }

    /// Shutdown the server gracefully
    pub fn shutdown(&self) {
        self.server.shutdown();
    }

    /// Create a test account and return its ID
    pub async fn create_test_account(&self, username: &str) -> Result<String> {
        let id = uuid::Uuid::new_v4().to_string();
        sqlx::query("INSERT INTO accounts (id, username) VALUES (?, ?)")
            .bind(&id)
            .bind(username)
            .execute(self.db().pool())
            .await?;
        Ok(id)
    }

    /// Create a test universe and return its ID
    pub async fn create_test_universe(&self, name: &str, owner_id: &str) -> Result<String> {
        let id = uuid::Uuid::new_v4().to_string();
        sqlx::query("INSERT INTO universes (id, name, owner_id) VALUES (?, ?, ?)")
            .bind(&id)
            .bind(name)
            .bind(owner_id)
            .execute(self.db().pool())
            .await?;
        Ok(id)
    }

    /// Get the WebSocket URL for the server
    pub fn ws_url(&self) -> String {
        format!("ws://{}/ws", self.addr)
    }

    /// Connect to the WebSocket endpoint and return a test client
    pub async fn connect_ws(&self) -> Result<WsClient> {
        let (ws_stream, _) = connect_async(&self.ws_url()).await?;
        let (write, read) = ws_stream.split();
        Ok(WsClient { write, read })
    }
}

/// WebSocket client for testing
pub struct WsClient {
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
}

impl WsClient {
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
    pub async fn recv_json(&mut self) -> Result<serde_json::Value> {
        loop {
            match self.read.next().await {
                Some(Ok(Message::Text(text))) => {
                    return Ok(serde_json::from_str(&text)?);
                }
                Some(Ok(Message::Close(_))) | None => {
                    anyhow::bail!("WebSocket closed");
                }
                _ => continue, // Skip binary/ping/pong frames
            }
        }
    }

    /// Receive with timeout
    pub async fn recv_json_timeout(&mut self, timeout: Duration) -> Result<serde_json::Value> {
        match tokio::time::timeout(timeout, self.recv_json()).await {
            Ok(result) => result,
            Err(_) => anyhow::bail!("Timeout waiting for WebSocket message"),
        }
    }

    /// Close the connection
    pub async fn close(&mut self) -> Result<()> {
        self.write.close().await?;
        Ok(())
    }
}

impl Drop for MuddTest {
    fn drop(&mut self) {
        self.server.shutdown();
    }
}
