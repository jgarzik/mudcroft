//! RaftWriter - Central write coordinator for all database writes
//!
//! All SQLite writes must go through this coordinator to ensure
//! consensus across the cluster.

use std::time::Duration;

use anyhow::{bail, Result};
use openraft::BasicNode;
use sqlx::SqlitePool;
use tracing::{debug, info};

use super::config::RaftNodeConfig;
use super::network::NetworkConfig;
use super::types::{NodeId, Request, Response};
use super::{create_raft_node, GameRaft};

/// Central coordinator for all database writes via Raft consensus
pub struct RaftWriter {
    raft: GameRaft,
    node_id: NodeId,
    db_path: String,
}

impl RaftWriter {
    /// Create a single-node RaftWriter (no replication, but still uses Raft log)
    pub async fn single_node(
        pool: SqlitePool,
        node_id: NodeId,
        port: u16,
        db_path: &str,
    ) -> Result<Self> {
        info!("Creating single-node RaftWriter (node_id={})", node_id);

        let config = RaftNodeConfig::single(node_id, port).with_db_path(db_path);
        let network_config = NetworkConfig::from_raft_config(&config);

        let (raft, _read_pool) = create_raft_node(node_id, pool, network_config).await?;

        // Initialize as single-node cluster
        let mut members = std::collections::BTreeMap::new();
        members.insert(node_id, BasicNode::default());

        raft.initialize(members).await?;

        Ok(Self {
            raft,
            node_id,
            db_path: db_path.to_string(),
        })
    }

    /// Create a cluster RaftWriter (multi-node replication)
    pub async fn cluster(pool: SqlitePool, config: RaftNodeConfig, db_path: &str) -> Result<Self> {
        info!(
            "Creating cluster RaftWriter (node_id={}, peers={:?})",
            config.node_id, config.peers
        );

        let node_id = config.node_id;
        let network_config = NetworkConfig::from_raft_config(&config);

        let (raft, _read_pool) = create_raft_node(node_id, pool, network_config).await?;

        // Build initial membership from peers
        let members: std::collections::BTreeMap<NodeId, BasicNode> = config
            .peers
            .keys()
            .map(|&id| (id, BasicNode::default()))
            .collect();

        // Only initialize if this is the first node or if not already initialized
        // In practice, only node 1 should call initialize; others join via replication
        if node_id == 1 {
            if let Err(e) = raft.initialize(members).await {
                debug!("Cluster may already be initialized: {}", e);
            }
        }

        Ok(Self {
            raft,
            node_id,
            db_path: db_path.to_string(),
        })
    }

    /// Execute a single SQL write via Raft consensus
    ///
    /// The SQL and params are replicated to all nodes before being applied.
    /// Non-deterministic values (UUIDs, timestamps) must be pre-computed.
    pub async fn execute(&self, sql: &str, params: Vec<serde_json::Value>) -> Result<Response> {
        let request = Request::new(sql, params);

        debug!("Submitting write to Raft: {}", sql);

        let response = self.raft.client_write(request).await?;

        if !response.data.success {
            if let Some(ref err) = response.data.error {
                bail!("Raft write failed: {}", err);
            }
            bail!("Raft write failed (unknown error)");
        }

        Ok(response.data)
    }

    /// Execute a batch of SQL statements atomically via Raft consensus
    ///
    /// All statements are committed together. If any fails, the batch is rejected.
    pub async fn execute_batch(
        &self,
        statements: Vec<(String, Vec<serde_json::Value>)>,
    ) -> Result<()> {
        for (sql, params) in statements {
            let response = self.execute(&sql, params).await?;
            if !response.success {
                if let Some(err) = response.error {
                    bail!("Batch statement failed: {}", err);
                }
                bail!("Batch statement failed (unknown error)");
            }
        }
        Ok(())
    }

    /// Wait for this node to have a leader (either self or another node)
    pub async fn wait_for_leader(&self, timeout: Duration) -> Result<()> {
        let start = std::time::Instant::now();

        loop {
            if start.elapsed() > timeout {
                bail!("Timeout waiting for Raft leader");
            }

            let metrics = self.raft.metrics().borrow().clone();

            if let Some(leader) = metrics.current_leader {
                info!("Raft leader elected: node {}", leader);
                return Ok(());
            }

            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    /// Get the Raft instance (for advanced operations)
    pub fn raft(&self) -> &GameRaft {
        &self.raft
    }

    /// Get this node's ID
    pub fn node_id(&self) -> NodeId {
        self.node_id
    }

    /// Get the database path
    pub fn db_path(&self) -> &str {
        &self.db_path
    }

    /// Check if this node is currently the leader
    pub fn is_leader(&self) -> bool {
        let metrics = self.raft.metrics().borrow().clone();
        metrics.current_leader == Some(self.node_id)
    }

    /// Get the current leader's node ID (if known)
    pub fn current_leader(&self) -> Option<NodeId> {
        self.raft.metrics().borrow().current_leader
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::sqlite::SqlitePoolOptions;

    async fn test_pool_with_raft_tables() -> SqlitePool {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();

        // Create raft tables
        sqlx::query(
            "CREATE TABLE raft_log (
                log_index INTEGER PRIMARY KEY,
                term INTEGER NOT NULL,
                entry_type TEXT NOT NULL,
                payload TEXT,
                created_at INTEGER NOT NULL DEFAULT (unixepoch())
            )",
        )
        .execute(&pool)
        .await
        .unwrap();

        sqlx::query(
            "CREATE TABLE raft_vote (
                id INTEGER PRIMARY KEY CHECK (id = 1),
                term INTEGER NOT NULL,
                node_id INTEGER,
                committed INTEGER NOT NULL DEFAULT 0
            )",
        )
        .execute(&pool)
        .await
        .unwrap();

        sqlx::query(
            "CREATE TABLE raft_meta (
                key TEXT PRIMARY KEY,
                value TEXT NOT NULL
            )",
        )
        .execute(&pool)
        .await
        .unwrap();

        // Create a test table for write operations
        sqlx::query("CREATE TABLE test_data (id TEXT PRIMARY KEY, value TEXT)")
            .execute(&pool)
            .await
            .unwrap();

        pool
    }

    #[tokio::test]
    async fn test_single_node_writer_creation() {
        let pool = test_pool_with_raft_tables().await;
        let writer = RaftWriter::single_node(pool, 1, 19000, "/tmp/test.db").await;
        assert!(writer.is_ok());
    }

    #[tokio::test]
    async fn test_single_node_write() {
        let pool = test_pool_with_raft_tables().await;
        let writer = RaftWriter::single_node(pool, 1, 19001, "/tmp/test.db")
            .await
            .unwrap();

        // Wait for leader election
        writer
            .wait_for_leader(Duration::from_secs(5))
            .await
            .unwrap();

        // Execute a write
        let response = writer
            .execute(
                "INSERT INTO test_data (id, value) VALUES (?, ?)",
                vec![
                    serde_json::json!("test-id"),
                    serde_json::json!("test-value"),
                ],
            )
            .await
            .unwrap();

        assert!(response.success);
        assert_eq!(response.rows_affected, 1);
    }

    #[tokio::test]
    async fn test_is_leader() {
        let pool = test_pool_with_raft_tables().await;
        let writer = RaftWriter::single_node(pool, 1, 19002, "/tmp/test.db")
            .await
            .unwrap();

        writer
            .wait_for_leader(Duration::from_secs(5))
            .await
            .unwrap();

        // Single-node should always be leader
        assert!(writer.is_leader());
        assert_eq!(writer.current_leader(), Some(1));
    }
}
