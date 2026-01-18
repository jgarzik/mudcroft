//! TestServer - True end-to-end test harness
//!
//! Spawns the actual mudd binary on a random port with on-disk SQLite database.
//! Uses a temporary directory for each test instance to ensure isolation
//! while exercising the complete server binary including CLI parsing.

#![allow(dead_code)]

use std::net::SocketAddr;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use anyhow::Result;
use reqwest::Client;
use tempfile::TempDir;

use super::client::{Role, TestClient};
use super::world::TestWorld;

/// Test harness that spawns the actual mudd binary on a random port
/// Uses on-disk SQLite in a temp directory for realistic testing
pub struct TestServer {
    pub addr: SocketAddr,
    pub client: Client,
    child: Child,
    world: Option<TestWorld>,
    /// Temp directory for database and other files (cleaned up on drop)
    _temp_dir: TempDir,
    /// Path to the database file
    pub db_path: PathBuf,
    /// Database pool for direct access (test setup)
    db_pool: sqlx::SqlitePool,
}

impl TestServer {
    /// Start a new test server instance
    pub async fn start() -> Result<Self> {
        Self::start_with_world(true).await
    }

    /// Start a test server, optionally creating a test world
    pub async fn start_with_world(create_world: bool) -> Result<Self> {
        // Create temp directory for this test instance
        let temp_dir = TempDir::new()?;
        let db_path = temp_dir.path().join("test.db");

        // Find a random available port
        let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
        let addr = listener.local_addr()?;
        drop(listener);

        // Find the binary paths
        let mudd_path = find_binary_path("mudd")?;
        let init_path = find_binary_path("mudd_init")?;

        // Find the core mudlib directory (lib/ relative to mudd crate root)
        let lib_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("lib");

        // Run mudd_init to create the database with core mudlib
        let init_status = Command::new(&init_path)
            .arg("--database")
            .arg(db_path.to_string_lossy().as_ref())
            .arg("--lib-dir")
            .arg(lib_dir.to_string_lossy().as_ref())
            .env("MUDD_ADMIN_USERNAME", "test_admin")
            .env("MUDD_ADMIN_PASSWORD", "testpass123")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .status()
            .map_err(|e| anyhow::anyhow!("Failed to run mudd_init at {:?}: {}", init_path, e))?;

        if !init_status.success() {
            anyhow::bail!("mudd_init failed with exit code: {:?}", init_status.code());
        }

        // Spawn the server process (inherit stderr to see server logs)
        let child = Command::new(&mudd_path)
            .arg("--bind")
            .arg(addr.to_string())
            .arg("--database")
            .arg(db_path.to_string_lossy().as_ref())
            .env("RUST_LOG", "info")
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .map_err(|e| {
                anyhow::anyhow!("Failed to spawn mudd binary at {:?}: {}", mudd_path, e)
            })?;

        // Wait for server to be ready
        let client = Client::builder().timeout(Duration::from_secs(5)).build()?;

        // Poll until server is ready (max 5 seconds to handle resource contention)
        let mut ready = false;
        for _ in 0..50 {
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
            panic!("Server failed to start within 5 seconds");
        }

        // Open a separate database connection for test setup
        // Configure with foreign keys enabled to match server behavior
        use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
        use std::str::FromStr;

        let db_url = format!("sqlite:{}?mode=rwc", db_path.display());
        let options = SqliteConnectOptions::from_str(&db_url)?
            .foreign_keys(true)
            .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal);
        let db_pool = SqlitePoolOptions::new()
            .max_connections(5)
            .connect_with(options)
            .await?;

        let mut test_server = Self {
            addr,
            client,
            child,
            world: None,
            _temp_dir: temp_dir,
            db_path,
            db_pool,
        };

        // Create test world if requested
        if create_world {
            let world = TestWorld::create(&test_server).await?;
            test_server.world = Some(world);
        }

        Ok(test_server)
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

    /// Make an authenticated POST request
    pub async fn post_auth<T: serde::Serialize + ?Sized>(
        &self,
        path: &str,
        body: &T,
        token: &str,
    ) -> Result<reqwest::Response> {
        Ok(self
            .client
            .post(format!("{}{}", self.base_url(), path))
            .header("Authorization", format!("Bearer {}", token))
            .json(body)
            .send()
            .await?)
    }

    /// Get direct access to the database pool for test setup/assertions
    pub fn pool(&self) -> &sqlx::SqlitePool {
        &self.db_pool
    }

    /// Get the test world (panics if not created)
    pub fn world(&self) -> &TestWorld {
        self.world
            .as_ref()
            .expect("TestWorld not created - use start() instead of start_with_world(false)")
    }

    /// Create a test account and return its ID
    pub async fn create_test_account(&self, username: &str) -> Result<String> {
        let id = uuid::Uuid::new_v4().to_string();
        sqlx::query("INSERT INTO accounts (id, username) VALUES (?, ?)")
            .bind(&id)
            .bind(username)
            .execute(&self.db_pool)
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
            .execute(&self.db_pool)
            .await?;
        Ok(id)
    }

    /// Get the universe ID (panics if world not created)
    pub fn universe_id(&self) -> &str {
        &self.world().universe_id
    }

    /// Get the WebSocket URL for the server (includes universe)
    pub fn ws_url(&self) -> String {
        format!("ws://{}/ws?universe={}", self.addr, self.universe_id())
    }

    /// Get the WebSocket URL with auth token (includes universe)
    pub fn ws_url_with_token(&self, token: &str) -> String {
        format!(
            "ws://{}/ws?token={}&universe={}",
            self.addr,
            token,
            self.universe_id()
        )
    }

    /// Connect a guest (unauthenticated) client
    pub async fn connect_guest(&self) -> Result<TestClient> {
        TestClient::connect(self, Role::Guest).await
    }

    /// Connect as a specific role (creates account if needed)
    pub async fn connect_as(&self, role: Role) -> Result<TestClient> {
        TestClient::connect(self, role).await
    }

    /// Connect multiple clients with specified roles
    pub async fn connect_clients(&self, roles: &[Role]) -> Result<Vec<TestClient>> {
        let mut clients = Vec::with_capacity(roles.len());
        for role in roles {
            clients.push(self.connect_as(role.clone()).await?);
        }
        Ok(clients)
    }

    /// Connect to the WebSocket endpoint (backward compatibility)
    /// Returns a raw WsClient without automatic welcome message handling
    pub async fn connect_ws(&self) -> Result<RawWsClient> {
        let (ws_stream, _) = tokio_tungstenite::connect_async(&self.ws_url()).await?;
        let (write, read) = futures_util::StreamExt::split(ws_stream);
        Ok(RawWsClient { write, read })
    }
}

/// Find a binary path by name (e.g., "mudd" or "mudd_init")
/// Uses the same build profile as the test: debug for `cargo test`, release for `cargo test --release`
fn find_binary_path(name: &str) -> Result<PathBuf> {
    // Match the build profile of the test runner
    #[cfg(debug_assertions)]
    let candidates = [
        // Debug build (matches `cargo test`)
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(format!("target/debug/{}", name)),
        // Workspace root debug
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(format!("../target/debug/{}", name)),
    ];

    #[cfg(not(debug_assertions))]
    let candidates = [
        // Release build (matches `cargo test --release`)
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(format!("target/release/{}", name)),
        // Workspace root release
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(format!("../target/release/{}", name)),
    ];

    for path in &candidates {
        if path.exists() {
            return Ok(path.clone());
        }
    }

    #[cfg(debug_assertions)]
    let build_cmd = "cargo build";
    #[cfg(not(debug_assertions))]
    let build_cmd = "cargo build --release";

    anyhow::bail!(
        "Could not find {} binary. Run '{}' first. Searched: {:?}",
        name,
        build_cmd,
        candidates
    )
}

/// Raw WebSocket client for backward compatibility with existing tests
/// Does not consume the welcome message automatically
pub struct RawWsClient {
    write: futures_util::stream::SplitSink<
        tokio_tungstenite::WebSocketStream<
            tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
        >,
        tokio_tungstenite::tungstenite::Message,
    >,
    read: futures_util::stream::SplitStream<
        tokio_tungstenite::WebSocketStream<
            tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
        >,
    >,
}

impl RawWsClient {
    /// Send a command message
    pub async fn send_command(&mut self, text: &str) -> Result<()> {
        use futures_util::SinkExt;
        use tokio_tungstenite::tungstenite::Message;

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
        use futures_util::SinkExt;
        use tokio_tungstenite::tungstenite::Message;

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
        use futures_util::StreamExt;
        use tokio_tungstenite::tungstenite::Message;

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
        use futures_util::SinkExt;
        self.write.close().await?;
        Ok(())
    }
}

impl Drop for TestServer {
    fn drop(&mut self) {
        // Kill the server process
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

// Re-export MuddTest as an alias for backward compatibility
pub type MuddTest = TestServer;
