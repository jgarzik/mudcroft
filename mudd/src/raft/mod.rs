//! Raft consensus for multi-node replication
//!
//! Uses OpenRaft for distributed consensus:
//! - Statement-based SQL replication
//! - Leader election
//! - Snapshot transfer for new nodes
//! - Cluster transport over TLS ([`transport`])
//!
//! # Architecture
//!
//! This module implements "Raft as WAL" - the Raft log provides durability
//! instead of SQLite's WAL. All writes go through Raft consensus before
//! being applied to the database.

pub mod config;
pub mod storage;
pub mod transport;
pub mod types;
pub mod writer;

// Re-exports
pub use config::create_openraft_config;
pub use storage::{LogStore, SharedPool, StateMachine};
pub use transport::{ClusterFiles, ClusterNetwork, ClusterSecurity};
pub use types::{NodeId, Request, Response, TypeConfig};
pub use writer::{ClusterSetup, RaftWriter};

use std::sync::Arc;

use openraft::Raft;
use sqlx::sqlite::SqlitePool;
use tokio::sync::RwLock;
use tracing::info;

/// Type alias for our Raft instance
pub type GameRaft = Raft<TypeConfig, StateMachine>;

/// Create a new Raft node with all components (writes go through the Raft
/// instance; reads go directly to the pool)
pub async fn create_raft_node(
    node_id: NodeId,
    pool: SqlitePool,
    network: ClusterNetwork,
    db_path: Option<String>,
) -> anyhow::Result<GameRaft> {
    info!("Creating Raft node {}", node_id);

    let config = create_openraft_config();
    let shared: SharedPool = Arc::new(RwLock::new(pool));
    let log_store = LogStore::new(shared.clone());
    // The path places snapshot scratch files next to the database
    let state_machine = StateMachine::new(shared, db_path).await?;

    let raft = Raft::new(node_id, config, network, log_store, state_machine).await?;
    info!("Raft node {} created successfully", node_id);
    Ok(raft)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::test_utils::test_pool;

    #[test]
    fn test_request_creation() {
        let req = Request::new(
            "INSERT INTO test VALUES (?)",
            vec![serde_json::json!("hello")],
        );
        assert_eq!(req.statements.len(), 1);
        assert_eq!(req.statements[0].sql, "INSERT INTO test VALUES (?)");
        assert_eq!(req.statements[0].params.len(), 1);
    }

    #[test]
    fn test_response_ok() {
        let resp = Response::ok(5);
        assert!(resp.success);
        assert_eq!(resp.rows_affected, 5);
    }

    #[test]
    fn test_response_error() {
        let resp = Response::error("fail");
        assert!(!resp.success);
        assert_eq!(resp.rows_affected, 0);
    }

    #[tokio::test]
    async fn test_state_machine_creation() {
        let pool: SharedPool = Arc::new(RwLock::new(test_pool().await));
        assert!(StateMachine::new(pool, None).await.is_ok());
    }
}
