//! mudd_init - One-time database initialization tool
//!
//! Creates a fresh game server database with admin account.

use std::collections::HashMap;
use std::path::PathBuf;

use anyhow::{bail, Result};
use clap::Parser;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

/// HemiMUD database initialization tool
#[derive(Parser, Debug)]
#[command(
    name = "mudd_init",
    version,
    about = "Initialize a new HemiMUD database"
)]
struct Args {
    /// Path to SQLite database file to create (must not exist)
    #[arg(short, long)]
    database: PathBuf,

    /// Lua library files to store (can be specified multiple times)
    #[arg(long = "lib")]
    libs: Vec<PathBuf>,
}

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize tracing
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "mudd=info".into()),
        )
        .with(tracing_subscriber::fmt::layer())
        .init();

    // Parse CLI arguments
    let args = Args::parse();

    // Read admin credentials from environment
    let admin_username = std::env::var("MUDD_ADMIN_USERNAME")
        .map_err(|_| anyhow::anyhow!("MUDD_ADMIN_USERNAME environment variable is required"))?;

    let admin_password = std::env::var("MUDD_ADMIN_PASSWORD")
        .map_err(|_| anyhow::anyhow!("MUDD_ADMIN_PASSWORD environment variable is required"))?;

    // Load Lua library files
    let mut libs = HashMap::new();
    for lib_path in &args.libs {
        if !lib_path.exists() {
            bail!("Library file not found: {}", lib_path.display());
        }

        let name = lib_path
            .file_stem()
            .and_then(|s| s.to_str())
            .ok_or_else(|| anyhow::anyhow!("Invalid library filename: {}", lib_path.display()))?
            .to_string();

        let source = std::fs::read_to_string(lib_path)
            .map_err(|e| anyhow::anyhow!("Failed to read {}: {}", lib_path.display(), e))?;

        libs.insert(name, source);
    }

    // Initialize the database
    mudd::init::init_database(&args.database, &admin_username, &admin_password, libs).await?;

    Ok(())
}
