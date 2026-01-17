//! mudd - HemiMUD server daemon

use anyhow::Result;
use mudd::{Config, Server};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

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

    // Load config (TODO: from file/env)
    let config = Config::default();

    // Create and run server
    let server = Server::new(config).await?;
    server.run().await?;

    Ok(())
}
