//! mudd - HemiMUD server daemon

use std::net::SocketAddr;

use anyhow::Result;
use clap::Parser;
use mudd::{Config, Server};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

/// HemiMUD server daemon
#[derive(Parser, Debug)]
#[command(name = "mudd", version, about)]
struct Args {
    /// Address to bind to
    #[arg(short, long, default_value = "127.0.0.1:8080")]
    bind: SocketAddr,

    /// Database file path (required - must be pre-initialized with mudd_init)
    #[arg(short, long)]
    database: String,

    /// Raft node ID (unique within cluster)
    #[arg(long, default_value = "1")]
    node_id: u64,

    /// Raft port for inter-node communication
    #[arg(long, default_value = "9000")]
    raft_port: u16,

    /// Cluster peers (format: "1=host1:9000,2=host2:9001"). Omit for single-node.
    #[arg(long)]
    peers: Option<String>,
}

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize tracing
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "mudd=info,tower_http=debug".into()),
        )
        .with(tracing_subscriber::fmt::layer())
        .init();

    // Parse CLI arguments
    let args = Args::parse();

    // Build config from CLI args
    let config = Config {
        bind_addr: args.bind,
        db_path: args.database,
        node_id: args.node_id,
        raft_port: args.raft_port,
        peers: args.peers,
    };

    // Create and run server
    let server = Server::new(config).await?;
    server.run().await?;

    Ok(())
}
