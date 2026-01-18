//! mudd_init - Database initialization and upgrade tool
//!
//! Creates a new database or upgrades an existing one (idempotent).
//! Admin credentials required only for new database creation.

use std::collections::HashMap;
use std::path::PathBuf;

use anyhow::{bail, Result};
use clap::Parser;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

/// HemiMUD database initialization and upgrade tool
#[derive(Parser, Debug)]
#[command(
    name = "mudd_init",
    version,
    about = "Initialize or upgrade a HemiMUD database"
)]
struct Args {
    /// Path to SQLite database file (creates new or upgrades existing)
    #[arg(short, long)]
    database: PathBuf,

    /// Force fresh initialization by deleting existing database
    #[arg(long, short)]
    force: bool,

    /// Core mudlib directory containing *.lua files (default: lib/)
    #[arg(long = "lib-dir")]
    lib_dir: Option<PathBuf>,

    /// Additional Lua library files to store (can be specified multiple times)
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

    // Handle --force: delete existing database
    if args.force && args.database.exists() {
        tracing::info!("Removing existing database at {}", args.database.display());
        std::fs::remove_file(&args.database)?;
    }

    // Read admin credentials from environment (only required for new DB)
    let db_exists = args.database.exists();
    let admin_username = std::env::var("MUDD_ADMIN_USERNAME").ok();
    let admin_password = std::env::var("MUDD_ADMIN_PASSWORD").ok();

    // Validate credentials are provided for new database
    if !db_exists && admin_username.is_none() {
        bail!("MUDD_ADMIN_USERNAME environment variable required for new database");
    }
    if !db_exists && admin_password.is_none() {
        bail!("MUDD_ADMIN_PASSWORD environment variable required for new database");
    }

    // Load additional Lua library files
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

    // Initialize or upgrade the database
    mudd::init::init_database(
        &args.database,
        admin_username.as_deref(),
        admin_password.as_deref(),
        libs,
        args.lib_dir.as_deref(),
    )
    .await?;

    Ok(())
}
