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

    /// Database file path (uses in-memory if not specified)
    #[arg(short, long)]
    database: Option<String>,
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
    };

    // Create and run server
    let server = Server::new(config).await?;
    server.run().await?;

    Ok(())
}
