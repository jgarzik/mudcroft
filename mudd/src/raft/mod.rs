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
pub mod snapshot;
pub mod state_machine;
pub mod storage;
pub mod types;

// Note: log_storage.rs is kept for reference but not compiled
// It contains the original attempt to implement RaftLogStorage
// which is a sealed trait in OpenRaft 0.9. Use storage.rs instead.

// Re-exports
pub use config::{create_openraft_config, RaftNodeConfig};
pub use network::{NetworkConfig, RaftNetworkFactoryImpl};
pub use snapshot::SnapshotStore;
pub use state_machine::SnapshotData;
pub use storage::CombinedStorage;
pub use types::{NodeId, Request, Response, TypeConfig};

use openraft::storage::Adaptor;
use openraft::Raft;
use sqlx::sqlite::SqlitePool;
use tracing::info;

/// Type alias for the storage adaptor
pub type StorageAdaptor = Adaptor<TypeConfig, CombinedStorage>;

/// Type alias for our Raft instance
pub type GameRaft = Raft<TypeConfig>;

/// Create a new Raft node with all components
///
/// Returns the Raft instance and a clone of the pool for read operations.
/// Writes must go through the Raft instance; reads can go directly to the pool.
pub async fn create_raft_node(
    node_id: NodeId,
    pool: SqlitePool,
    network_config: NetworkConfig,
) -> anyhow::Result<(GameRaft, SqlitePool)> {
    info!("Creating Raft node {}", node_id);

    let config = create_openraft_config();
    let read_pool = pool.clone();
    let storage = CombinedStorage::new(pool).await?;
    let network = RaftNetworkFactoryImpl::new(network_config);

    // Wrap storage with Adaptor to satisfy sealed traits
    let (log_store, state_machine) = Adaptor::new(storage);

    // Create Raft instance
    let raft = Raft::new(node_id, config, network, log_store, state_machine).await?;

    info!("Raft node {} created successfully", node_id);

    Ok((raft, read_pool))
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::sqlite::SqlitePoolOptions;

    async fn test_pool() -> SqlitePool {
        SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap()
    }

    #[test]
    fn test_request_creation() {
        let req = Request::new(
            "INSERT INTO test VALUES (?)",
            vec![serde_json::json!("hello")],
        );
        assert_eq!(req.sql, "INSERT INTO test VALUES (?)");
        assert_eq!(req.params.len(), 1);
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
    async fn test_combined_storage_creation() {
        let pool = test_pool().await;
        let storage = CombinedStorage::new(pool).await;
        assert!(storage.is_ok());
    }
}
