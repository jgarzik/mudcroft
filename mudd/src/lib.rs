//! mudd - HemiMUD server daemon
//!
//! A modern MUD engine with Lua scripting, Raft replication, and AI integration.

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
use raft::{RaftNodeConfig, RaftWriter};

/// Server configuration
#[derive(Debug, Clone)]
pub struct Config {
    pub bind_addr: SocketAddr,
    /// Path to pre-initialized database (created by mudd_init)
    pub db_path: String,
    /// Raft node ID (unique within cluster)
    pub node_id: u64,
    /// Raft port for inter-node communication
    pub raft_port: u16,
    /// Cluster peers (format: "1=host1:9000,2=host2:9001"). None = single-node.
    pub peers: Option<String>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            bind_addr: "127.0.0.1:8080".parse().unwrap(),
            db_path: String::new(),
            node_id: 1,
            raft_port: 9000,
            peers: None,
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
        let db = Database::open(&config.db_path).await?;
        let db = Arc::new(db);

        // Initialize RaftWriter
        let raft_writer = if let Some(ref peers_str) = config.peers {
            // Multi-node cluster mode
            let peers = RaftNodeConfig::parse_peers(peers_str);
            let raft_config = RaftNodeConfig::cluster(config.node_id, config.raft_port, peers)
                .with_db_path(&config.db_path);
            RaftWriter::cluster(db.pool().clone(), raft_config, &config.db_path).await?
        } else {
            // Single-node mode
            RaftWriter::single_node(
                db.pool().clone(),
                config.node_id,
                config.raft_port,
                &config.db_path,
            )
            .await?
        };

        // Wait for leader election
        raft_writer.wait_for_leader(Duration::from_secs(10)).await?;

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
        api::router(self.db.clone(), self.raft_writer.clone()).await
    }

    /// Run the server until shutdown
    pub async fn run(&self) -> Result<()> {
        let listener = TcpListener::bind(self.config.bind_addr).await?;
        let local_addr = listener.local_addr()?;
        info!("mudd listening on {}", local_addr);

        let router = self.router().await;
        let mut shutdown_rx = self.shutdown_rx.clone();

        axum::serve(listener, router)
            .with_graceful_shutdown(async move {
                shutdown_rx.changed().await.ok();
            })
            .await?;

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
