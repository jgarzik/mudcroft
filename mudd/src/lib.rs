//! mudd - Mudcroft server daemon
//!
//! A modern MUD engine with Lua scripting, Raft replication, and AI integration.

pub mod admin;
pub mod api;
pub mod auth;
pub mod combat;
pub mod credits;
pub mod db;
pub mod images;
pub mod init;
pub mod lua;
pub mod objects;
pub mod permissions;
pub mod player;
pub mod raft;
pub mod theme;
pub mod timers;
pub mod universe;
pub mod venice;

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use axum::Router;
use tokio::net::TcpListener;
use tokio::sync::watch;
use tracing::info;

use db::Database;
use raft::{ClusterFiles, ClusterSecurity, ClusterSetup, RaftWriter};

/// Cluster membership settings (see docs/cluster-design.md)
#[derive(Debug, Clone)]
pub struct ClusterConfig {
    /// Address the cluster port listens on
    pub listen: SocketAddr,
    /// Address peers reach this node at (`host:port`; not 0.0.0.0)
    pub advertise: String,
    /// Members to join through, for a new node created with
    /// `mudd_init --empty` (ignored once the node has joined)
    pub join: Vec<String>,
    /// TLS certificate, key, CA and shared secret files
    pub files: ClusterFiles,
}

/// Server configuration
#[derive(Debug, Clone)]
pub struct Config {
    pub bind_addr: SocketAddr,
    /// Path to pre-initialized database (created by mudd_init)
    pub db_path: String,
    /// Directory of the built web client to serve (None = API only)
    pub static_dir: Option<std::path::PathBuf>,
    /// Cluster settings (None = a single node)
    pub cluster: Option<ClusterConfig>,
    /// Login attempts a minute per username (twice that per client IP, for
    /// login and registration); 0 turns the limits off
    pub auth_limit: u32,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            bind_addr: "127.0.0.1:8080".parse().unwrap(),
            db_path: String::new(),
            static_dir: None,
            cluster: None,
            auth_limit: 30,
        }
    }
}

/// The mudd server instance
pub struct Server {
    config: Config,
    db: Arc<Database>,
    raft_writer: Arc<RaftWriter>,
    shutdown_tx: watch::Sender<bool>,
    shutdown_rx: watch::Receiver<bool>,
}

impl Server {
    /// Create a new server instance
    /// Requires a pre-initialized database (created by mudd_init)
    pub async fn new(config: Config) -> Result<Self> {
        if let Some(ref dir) = config.static_dir {
            if !dir.join("index.html").is_file() {
                anyhow::bail!(
                    "--static-dir {} has no index.html (build the client first)",
                    dir.display()
                );
            }
        }

        // A node joining a cluster starts empty (mudd_init --empty)
        let joining = config.cluster.as_ref().is_some_and(|c| !c.join.is_empty());
        let db = if joining {
            Database::open_joining(&config.db_path).await?
        } else {
            Database::open(&config.db_path).await?
        };
        let db = Arc::new(db);

        // Raft: a single node, or a cluster member serving the cluster port
        let (raft_writer, leader_wait) = match config.cluster {
            None => (
                RaftWriter::single_node(db.pool().clone(), &config.db_path).await?,
                Duration::from_secs(10),
            ),
            Some(ref cluster) => {
                if cluster.advertise.starts_with("0.0.0.0") {
                    anyhow::bail!("--advertise must be an address peers can reach, not 0.0.0.0");
                }
                let security = Arc::new(ClusterSecurity::load(&cluster.files)?);
                let listener = TcpListener::bind(cluster.listen).await?;
                info!("cluster port listening on {}", cluster.listen);
                let setup = ClusterSetup {
                    security,
                    listener,
                    advertise: cluster.advertise.clone(),
                    join: cluster.join.clone(),
                };
                (
                    RaftWriter::start(db.pool().clone(), &config.db_path, Some(setup)).await?,
                    // A new member receives a snapshot first
                    Duration::from_secs(120),
                )
            }
        };

        raft_writer.wait_for_leader(leader_wait).await?;

        let raft_writer = Arc::new(raft_writer);
        let (shutdown_tx, shutdown_rx) = watch::channel(false);

        Ok(Self {
            config,
            db,
            raft_writer,
            shutdown_tx,
            shutdown_rx,
        })
    }

    /// Get the database handle
    pub fn db(&self) -> Arc<Database> {
        self.db.clone()
    }

    /// Get the RaftWriter handle
    pub fn raft_writer(&self) -> Arc<RaftWriter> {
        self.raft_writer.clone()
    }

    /// Build the router
    async fn router(&self) -> Router {
        api::router(
            self.db.clone(),
            self.raft_writer.clone(),
            self.config.static_dir.clone(),
            self.config.auth_limit,
        )
        .await
    }

    /// Run the server until shutdown (`shutdown()`), or until `stop` turns
    /// true (see [`shutdown_on_signals`])
    pub async fn run(&self, mut stop: watch::Receiver<bool>) -> Result<()> {
        let listener = TcpListener::bind(self.config.bind_addr).await?;
        let local_addr = listener.local_addr()?;
        info!("mudd listening on {}", local_addr);

        let router = self.router().await;
        let mut shutdown_rx = self.shutdown_rx.clone();

        // Client addresses, for the per-IP login limit
        let app = router.into_make_service_with_connect_info::<SocketAddr>();
        let server = axum::serve(listener, app).with_graceful_shutdown(async move {
            tokio::select! {
                _ = shutdown_rx.changed() => {}
                _ = stop.wait_for(|stop| *stop) => {}
            }
        });

        // A stopped Raft core (e.g. a storage failure while applying) means
        // every write fails from then on: stop serving and exit with an
        // error so a supervisor restarts mudd, rather than limp along
        let raft_writer = self.raft_writer.clone();
        tokio::select! {
            result = server => result?,
            reason = raft_writer.stopped() => {
                tracing::error!("Raft stopped ({}); shutting down", reason);
                anyhow::bail!("Raft stopped: {}", reason);
            }
        }

        info!("mudd shutdown complete");
        Ok(())
    }

    /// Signal the server to shutdown
    pub fn shutdown(&self) {
        let _ = self.shutdown_tx.send(true);
    }

    /// Get the configured bind address
    pub fn bind_addr(&self) -> SocketAddr {
        self.config.bind_addr
    }
}

/// Handle SIGTERM (docker stop, systemd) and SIGINT (Ctrl-C) from now on:
/// the first asks for a graceful shutdown (the receiver turns true), a
/// second exits at once. Call it first thing, so a signal during startup
/// works too, also as PID 1, where the kernel ignores an unhandled SIGTERM.
pub fn shutdown_on_signals() -> watch::Receiver<bool> {
    let (tx, rx) = watch::channel(false);
    tokio::spawn(async move {
        let signal = termination_signal().await;
        info!("{}: shutting down", signal);
        let _ = tx.send(true);
        let signal = termination_signal().await;
        tracing::warn!("{} again: exiting now", signal);
        std::process::exit(130);
    });
    rx
}

/// Wait for SIGTERM or SIGINT
async fn termination_signal() -> &'static str {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        match signal(SignalKind::terminate()) {
            Ok(mut term) => tokio::select! {
                _ = term.recv() => "SIGTERM",
                _ = tokio::signal::ctrl_c() => "SIGINT",
            },
            Err(e) => {
                tracing::warn!("cannot handle SIGTERM: {}", e);
                let _ = tokio::signal::ctrl_c().await;
                "SIGINT"
            }
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
        "Ctrl-C"
    }
}
