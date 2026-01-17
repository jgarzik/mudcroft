//! mudd - HemiMUD server daemon
//!
//! A modern MUD engine with Lua scripting, Raft replication, and AI integration.

pub mod api;
pub mod combat;
pub mod db;
pub mod lua;
pub mod objects;
pub mod permissions;

use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::Result;
use axum::Router;
use tokio::net::TcpListener;
use tokio::sync::watch;
use tracing::info;

use db::Database;

/// Server configuration
#[derive(Debug, Clone)]
pub struct Config {
    pub bind_addr: SocketAddr,
    pub db_path: Option<String>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            bind_addr: "127.0.0.1:8080".parse().unwrap(),
            db_path: None, // None = in-memory
        }
    }
}

/// The mudd server instance
pub struct Server {
    config: Config,
    db: Arc<Database>,
    shutdown_tx: watch::Sender<bool>,
    shutdown_rx: watch::Receiver<bool>,
}

impl Server {
    /// Create a new server instance
    pub async fn new(config: Config) -> Result<Self> {
        let db = Database::new(config.db_path.as_deref()).await?;
        let (shutdown_tx, shutdown_rx) = watch::channel(false);

        Ok(Self {
            config,
            db: Arc::new(db),
            shutdown_tx,
            shutdown_rx,
        })
    }

    /// Get the database handle
    pub fn db(&self) -> Arc<Database> {
        self.db.clone()
    }

    /// Build the router
    fn router(&self) -> Router {
        api::router(self.db.clone())
    }

    /// Run the server until shutdown
    pub async fn run(&self) -> Result<()> {
        let listener = TcpListener::bind(self.config.bind_addr).await?;
        let local_addr = listener.local_addr()?;
        info!("mudd listening on {}", local_addr);

        let router = self.router();
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
