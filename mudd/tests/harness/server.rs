//! TestServer - True end-to-end test harness
//!
//! Spawns the actual mudd binary on a random port with on-disk SQLite database.
//! Uses a temporary directory for each test instance to ensure isolation
//! while exercising the complete server binary including CLI parsing.

#![allow(dead_code)]

use std::fs::File;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
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
    /// Server process; killed on drop. Declared before `_temp_dir` so the
    /// process is gone before its directory is removed.
    child: ChildGuard,
    world: Option<TestWorld>,
    /// Path to the server log file (inside the temp dir)
    pub log_path: PathBuf,
    /// mudd binary and extra arguments, for restart()
    mudd_path: PathBuf,
    extra_args: Vec<String>,
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
        Self::start_with_args(create_world, &[]).await
    }

    /// Start a test server passing extra command-line arguments to mudd
    pub async fn start_with_args(create_world: bool, extra_args: &[&str]) -> Result<Self> {
        // Create temp directory for this test instance
        let temp_dir = TempDir::new()?;
        let db_path = temp_dir.path().join("test.db");

        // Find the binary paths
        let mudd_path = find_binary_path("mudd")?;
        let init_path = find_binary_path("mudd_init")?;

        // Find the core mudlib directory (lib/ relative to mudd crate root)
        let lib_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("lib");

        // Run mudd_init to create the database with core mudlib.
        // Output goes to a log file: piped-but-unread output can block a child.
        let init_log = temp_dir.path().join("mudd_init.log");
        let init_out = File::create(&init_log)?;
        let init_child = die_with_parent(&mut Command::new(&init_path))
            .arg("--database")
            .arg(db_path.to_string_lossy().as_ref())
            .arg("--lib-dir")
            .arg(lib_dir.to_string_lossy().as_ref())
            .env("MUDD_ADMIN_USERNAME", "test_admin")
            .env("MUDD_ADMIN_PASSWORD", "testpass123")
            .stdin(Stdio::null())
            .stdout(Stdio::from(init_out.try_clone()?))
            .stderr(Stdio::from(init_out))
            .spawn()
            .map_err(|e| anyhow::anyhow!("Failed to run mudd_init at {:?}: {}", init_path, e))?;
        let init_status = ChildGuard::new(init_child).wait()?;

        if !init_status.success() {
            anyhow::bail!(
                "mudd_init failed with exit code {:?}:\n{}",
                init_status.code(),
                log_tail(&init_log, 40)
            );
        }

        let extra_args: Vec<String> = extra_args.iter().map(|a| a.to_string()).collect();
        let log_path = temp_dir.path().join("mudd.log");
        let client = Client::builder().timeout(Duration::from_secs(5)).build()?;
        // Pick a free port and start; if another process took the port in the
        // meantime, the server exits and we try again with a new one
        let mut attempt = 0;
        let (addr, child) = loop {
            attempt += 1;
            let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
            let addr = listener.local_addr()?;
            drop(listener);
            match spawn_mudd(
                &client,
                &mudd_path,
                addr,
                &db_path,
                &extra_args,
                &log_path,
                None,
            )
            .await
            {
                Ok(child) => break (addr, child),
                Err(e) if attempt < 3 && e.to_string().contains("exited during startup") => {
                    continue
                }
                Err(e) => return Err(e),
            }
        };

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
            log_path,
            mudd_path,
            extra_args,
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

    /// Stop the server process and start it again on the same database,
    /// port and arguments (simulates a server restart)
    pub async fn restart(&mut self) -> Result<()> {
        self.stop();
        self.start_again().await
    }

    /// Send mudd SIGTERM (as `docker stop` or systemd would) and wait up to
    /// `timeout` for it to exit; its exit status, or None if still running
    pub fn terminate(&mut self, timeout: Duration) -> Option<std::process::ExitStatus> {
        self.child.terminate(timeout).expect("signal mudd")
    }

    /// Stop the server process (the database stays; see start_again)
    pub fn stop(&mut self) {
        self.child.kill();
    }

    /// Start the server again after stop(), on the same database, port and
    /// arguments
    pub async fn start_again(&mut self) -> Result<()> {
        self.child = spawn_mudd(
            &self.client,
            &self.mudd_path,
            self.addr,
            &self.db_path,
            &self.extra_args,
            &self.log_path,
            None,
        )
        .await?;
        Ok(())
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

    /// Connect as a specific role (creates account if needed)
    pub async fn connect_as(&self, role: Role) -> Result<TestClient> {
        TestClient::connect(self, role).await
    }

    /// Connect as a role to another universe on this server
    pub async fn connect_in(&self, role: Role, universe_id: &str) -> Result<TestClient> {
        TestClient::connect_in(self, role, universe_id).await
    }

    /// Create another universe with one portal room, owned by `owner_id`
    pub async fn create_universe(&self, universe_id: &str, owner_id: &str) -> Result<()> {
        sqlx::query(
            "INSERT INTO universes (id, name, owner_id, config)
             SELECT ?, ?, ?, config FROM universes WHERE id = ?",
        )
        .bind(universe_id)
        .bind(universe_id)
        .bind(owner_id)
        .bind(self.universe_id())
        .execute(self.pool())
        .await?;
        let mut hall = mudd::objects::Object::new("/rooms/hall", universe_id, "room")?;
        hall.set_property("name", serde_json::json!("Hall"));
        self.world().store().create(&hall).await?;
        self.world()
            .store()
            .set_portal(universe_id, "/rooms/hall")
            .await?;
        Ok(())
    }

    /// Connect multiple clients with specified roles
    pub async fn connect_clients(&self, roles: &[Role]) -> Result<Vec<TestClient>> {
        let mut clients = Vec::with_capacity(roles.len());
        for role in roles {
            clients.push(self.connect_as(role.clone()).await?);
        }
        Ok(clients)
    }

    /// Register a fresh player account and connect to the WebSocket
    /// endpoint with its token. Returns a raw WsClient without automatic
    /// welcome message handling.
    pub async fn connect_ws(&self) -> Result<RawWsClient> {
        let username = format!("raw-{}", &uuid::Uuid::new_v4().simple().to_string()[..12]);
        let resp: serde_json::Value = self
            .post(
                "/auth/register",
                &serde_json::json!({"username": username, "password": "test123"}),
            )
            .await?
            .json()
            .await?;
        let token = resp["token"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("register failed: {}", resp))?;
        let (ws_stream, _) =
            tokio_tungstenite::connect_async(&self.ws_url_with_token(token)).await?;
        let (write, read) = futures_util::StreamExt::split(ws_stream);
        Ok(RawWsClient { write, read })
    }
}

/// Find a binary path by name (e.g., "mudd" or "mudd_init")
/// Uses the same build profile as the test: debug for `cargo test`, release for `cargo test --release`
pub(super) fn find_binary_path(name: &str) -> Result<PathBuf> {
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
        // Kill the server before the temp dir (and its log) is removed
        self.child.kill();
        // A failing test loses the temp dir, so surface the server log now
        if std::thread::panicking() {
            eprintln!(
                "---- mudd log tail ({}) ----\n{}",
                self.addr,
                log_tail(&self.log_path, 60)
            );
        }
    }
}

/// Spawn mudd and wait until it answers /health. The child is guarded
/// immediately, so any early return (startup timeout, exit) kills it.
/// Output is appended to `log_path`.
pub(super) async fn spawn_mudd(
    client: &Client,
    mudd_path: &Path,
    addr: SocketAddr,
    db_path: &Path,
    extra_args: &[String],
    log_path: &Path,
    current_dir: Option<&Path>,
) -> Result<ChildGuard> {
    let log_out = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_path)?;
    let mut command = Command::new(mudd_path);
    if let Some(dir) = current_dir {
        // run_script reads scripts/ relative to the working directory
        command.current_dir(dir);
    }
    let mut child = ChildGuard::new(
        die_with_parent(&mut command)
            .arg("--bind")
            .arg(addr.to_string())
            .arg("--database")
            .arg(db_path.to_string_lossy().as_ref())
            .args(extra_args)
            .env("RUST_LOG", "info")
            .stdin(Stdio::null())
            .stdout(Stdio::from(log_out.try_clone()?))
            .stderr(Stdio::from(log_out))
            .spawn()
            .map_err(|e| {
                anyhow::anyhow!("Failed to spawn mudd binary at {:?}: {}", mudd_path, e)
            })?,
    );

    // Poll until the server answers. mudd binds only after Raft leader
    // election (up to 10s), which is slow when many test servers start at
    // once, so allow generous time but stop early if it exits.
    let deadline = std::time::Instant::now() + STARTUP_TIMEOUT;
    // Ports are picked by bind-and-release, so another test's server can
    // grab ours first. Answering /health is not enough: our own log must
    // show that *this* process is listening on the address.
    let listening = format!("mudd listening on {}", addr);
    loop {
        if client
            .get(format!("http://{}/health", addr))
            .send()
            .await
            .is_ok()
            && std::fs::read_to_string(log_path)
                .map(|log| log.contains(&listening))
                .unwrap_or(false)
        {
            return Ok(child);
        }
        if let Some(status) = child.try_wait()? {
            anyhow::bail!(
                "Server exited during startup ({}). Log tail:\n{}",
                status,
                log_tail(log_path, 40)
            );
        }
        if std::time::Instant::now() > deadline {
            anyhow::bail!(
                "Server failed to start within {:?}. Log tail:\n{}",
                STARTUP_TIMEOUT,
                log_tail(log_path, 40)
            );
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// How long to wait for a test server to answer /health
const STARTUP_TIMEOUT: Duration = Duration::from_secs(20);

/// Owns a child process and kills it when dropped, so no exit path
/// (early return, `?`, panic) can leak a running server.
pub(super) struct ChildGuard(Option<Child>);

impl ChildGuard {
    pub(super) fn new(child: Child) -> Self {
        Self(Some(child))
    }

    /// Wait for the process to exit normally
    pub(super) fn wait(mut self) -> std::io::Result<std::process::ExitStatus> {
        let mut child = self.0.take().expect("child already taken");
        child.wait()
    }

    /// Exit status if the process has already exited
    pub(super) fn try_wait(&mut self) -> std::io::Result<Option<std::process::ExitStatus>> {
        match self.0.as_mut() {
            Some(child) => child.try_wait(),
            None => Ok(None),
        }
    }

    /// Send SIGTERM and wait up to `timeout` for the process to exit;
    /// its exit status, or None if it is still running
    fn terminate(
        &mut self,
        timeout: Duration,
    ) -> std::io::Result<Option<std::process::ExitStatus>> {
        let Some(child) = self.0.as_mut() else {
            return Ok(None);
        };
        // SAFETY: kill(2) on our own child's pid
        if unsafe { libc::kill(child.id() as libc::pid_t, libc::SIGTERM) } == -1 {
            return Err(std::io::Error::last_os_error());
        }
        let deadline = std::time::Instant::now() + timeout;
        while std::time::Instant::now() < deadline {
            if let Some(status) = child.try_wait()? {
                self.0 = None;
                return Ok(Some(status));
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        Ok(None)
    }

    /// Kill and reap the process (idempotent)
    pub(super) fn kill(&mut self) {
        if let Some(mut child) = self.0.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

impl Drop for ChildGuard {
    fn drop(&mut self) {
        self.kill();
    }
}

/// Ask the kernel to SIGKILL the child if the test process dies without
/// running destructors (Ctrl-C, timeout kill). Linux only; elsewhere the
/// `ChildGuard` drop is the only cleanup.
pub(super) fn die_with_parent(cmd: &mut Command) -> &mut Command {
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::process::CommandExt;
        // SAFETY: prctl is async-signal-safe and touches no parent state.
        unsafe {
            cmd.pre_exec(|| {
                if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
    }
    cmd
}

/// Last `lines` lines of a log file (best effort)
pub(super) fn log_tail(path: &Path, lines: usize) -> String {
    match std::fs::read_to_string(path) {
        Ok(text) => {
            let all: Vec<&str> = text.lines().collect();
            all[all.len().saturating_sub(lines)..].join("\n")
        }
        Err(e) => format!("<could not read {}: {}>", path.display(), e),
    }
}

// Re-export MuddTest as an alias for backward compatibility
pub type MuddTest = TestServer;
