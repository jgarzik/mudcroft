//! mudd_admin - administration tool
//!
//! `set-level` sets global access levels: run it while mudd is stopped (it
//! writes the database directly, outside Raft and the server's caches).
//! `cluster` inspects and changes a running cluster over its cluster port.

use std::path::PathBuf;

use anyhow::Result;
use clap::{Parser, Subcommand};

use mudd::admin::{set_global_level, GlobalLevel};
use mudd::raft::transport::{PeerClient, RemoveReply};
use mudd::raft::ClusterSecurity;

/// Mudcroft offline administration
#[derive(Parser, Debug)]
#[command(name = "mudd_admin", version, about = "Mudcroft administration")]
struct Args {
    /// Path to the SQLite database file (made by mudd_init; for set-level)
    #[arg(short, long)]
    database: Option<PathBuf>,

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
    /// Inspect or change a running cluster through any member
    Cluster {
        /// A member's cluster port (host:port)
        #[arg(long)]
        node: String,
        /// The cluster CA certificate (PEM)
        #[arg(long)]
        cluster_ca: PathBuf,
        /// File holding the cluster's shared secret
        #[arg(long)]
        cluster_secret_file: PathBuf,
        #[command(subcommand)]
        action: ClusterAction,
    },
}

#[derive(Subcommand, Debug)]
enum ClusterAction {
    /// Members, roles, leader, log and apply positions (JSON)
    Status,
    /// Remove a node (it is demoted first if a voter; the last voter cannot
    /// be removed). The removed node refuses to start again.
    Remove {
        /// The node's id (see status)
        node_id: u64,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    match args.command {
        Command::SetLevel { username, level } => {
            let Some(database) = args.database else {
                anyhow::bail!("set-level needs --database");
            };
            set_global_level(&database, &username, level).await?;
            println!("{}: global level set to {:?}", username, level);
        }
        Command::Cluster {
            node,
            cluster_ca,
            cluster_secret_file,
            action,
        } => {
            let security = std::sync::Arc::new(ClusterSecurity::client_only(
                &cluster_ca,
                &cluster_secret_file,
            )?);
            match action {
                ClusterAction::Status => {
                    let status = PeerClient::new(node, security)
                        .status()
                        .await
                        .map_err(|e| anyhow::anyhow!(e))?;
                    println!("{}", serde_json::to_string_pretty(&status)?);
                }
                ClusterAction::Remove { node_id } => {
                    // Follow "not the leader" answers to the leader
                    let mut target = node;
                    for _ in 0..5 {
                        let reply = PeerClient::new(target.clone(), security.clone())
                            .remove(node_id)
                            .await
                            .map_err(|e| anyhow::anyhow!(e))?;
                        match reply {
                            RemoveReply::Removed => {
                                println!("node {} removed", node_id);
                                return Ok(());
                            }
                            RemoveReply::NotLeader {
                                leader: Some(leader),
                            } => target = leader,
                            RemoveReply::NotLeader { leader: None } => {
                                anyhow::bail!("the cluster has no leader right now; try again")
                            }
                            RemoveReply::Refused(why) => anyhow::bail!("refused: {}", why),
                        }
                    }
                    anyhow::bail!("could not reach the leader");
                }
            }
        }
    }
    Ok(())
}
