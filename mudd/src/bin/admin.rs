//! mudd_admin - offline administration tool
//!
//! Sets global access levels. Run it while mudd is stopped: it writes the
//! database directly, outside Raft and the server's caches.

use std::path::PathBuf;

use anyhow::Result;
use clap::{Parser, Subcommand};

use mudd::admin::{set_global_level, GlobalLevel};

/// Mudcroft offline administration
#[derive(Parser, Debug)]
#[command(
    name = "mudd_admin",
    version,
    about = "Offline Mudcroft administration"
)]
struct Args {
    /// Path to the SQLite database file (made by mudd_init)
    #[arg(short, long)]
    database: PathBuf,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Set an account's global level: admin, owner, or none
    SetLevel {
        /// Account username
        username: String,
        /// admin, owner or none
        level: GlobalLevel,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    match args.command {
        Command::SetLevel { username, level } => {
            set_global_level(&args.database, &username, level).await?;
            println!("{}: global level set to {:?}", username, level);
        }
    }
    Ok(())
}
