//! Raft consensus for multi-node replication
//!
//! Uses OpenRaft for distributed consensus:
//! - Statement-based SQL replication
//! - Leader election
//! - Snapshot transfer for new nodes
//!
//! # Architecture
//!
//! This module implements "Raft as WAL" - the Raft log provides durability
//! instead of SQLite's WAL. All writes go through Raft consensus before
//! being applied to the database.

pub mod config;
pub mod network;
pub mod storage;
pub mod types;
pub mod writer;

// Re-exports
pub use config::{create_openraft_config, RaftNodeConfig};
pub use network::{NetworkConfig, RaftNetworkFactoryImpl};
pub use storage::{LogStore, SharedPool, StateMachine};
pub use types::{NodeId, Request, Response, TypeConfig};
pub use writer::RaftWriter;

use std::sync::Arc;

use openraft::Raft;
use sqlx::sqlite::SqlitePool;
use tokio::sync::RwLock;
use tracing::info;

/// Type alias for our Raft instance
pub type GameRaft = Raft<TypeConfig, StateMachine>;

/// Create a new Raft node with all components
///
/// Returns the Raft instance and a clone of the pool for read operations.
/// Writes must go through the Raft instance; reads can go directly to the pool.
pub async fn create_raft_node(
    node_id: NodeId,
    pool: SqlitePool,
    network_config: NetworkConfig,
    single_node: bool,
    db_path: Option<String>,
) -> anyhow::Result<(GameRaft, SqlitePool)> {
    info!("Creating Raft node {}", node_id);

    let config = create_openraft_config(single_node);
    let read_pool = pool.clone();
    let shared: SharedPool = Arc::new(RwLock::new(pool));
    let log_store = LogStore::new(shared.clone());
    // The path places snapshot scratch files next to the database
    let state_machine = StateMachine::new(shared, db_path).await?;
    let network = RaftNetworkFactoryImpl::new(network_config);

    let raft = Raft::new(node_id, config, network, log_store, state_machine).await?;

    info!("Raft node {} created successfully", node_id);

    Ok((raft, read_pool))
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

    #[test]
    fn test_raft_config_single() {
        let config = RaftNodeConfig::single(1, 9000);
        assert_eq!(config.node_id, 1);
        assert_eq!(config.raft_port, 9000);
        assert!(config.peers.contains_key(&1));
    }

    #[test]
    fn test_parse_peers() {
        let peers = RaftNodeConfig::parse_peers("1=127.0.0.1:9000,2=127.0.0.1:9001");
        assert_eq!(peers.len(), 2);
        assert_eq!(peers.get(&1), Some(&("127.0.0.1".to_string(), 9000)));
        assert_eq!(peers.get(&2), Some(&("127.0.0.1".to_string(), 9001)));
    }

    #[tokio::test]
    async fn test_state_machine_creation() {
        let pool: SharedPool = Arc::new(RwLock::new(test_pool().await));
        assert!(StateMachine::new(pool, None).await.is_ok());
    }
}
