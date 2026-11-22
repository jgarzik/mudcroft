//! RaftWriter - Central write coordinator for all database writes
//!
//! All SQLite writes must go through this coordinator to ensure
//! consensus across the cluster.

use std::time::Duration;

use anyhow::{bail, Result};
use openraft::async_runtime::WatchReceiver;
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

        let (raft, _read_pool) = create_raft_node(
            node_id,
            pool,
            network_config,
            true,
            Some(db_path.to_string()),
        )
        .await?;

        // Initialize as a single-node cluster the first time only: on a
        // database this node has served before, the Raft state (vote, log)
        // is already persisted and initialize() is refused.
        if raft.is_initialized().await? {
            info!("Raft state found; resuming existing single-node cluster");
        } else {
            let mut members = std::collections::BTreeMap::new();
            members.insert(node_id, BasicNode::default());
            raft.initialize(members).await?;
        }

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

        let (raft, _read_pool) = create_raft_node(
            node_id,
            pool,
            network_config,
            false,
            Some(db_path.to_string()),
        )
        .await?;

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
    /// The statements are one log entry, applied in one transaction: if any
    /// fails, none are applied and an error is returned.
    pub async fn execute_batch(
        &self,
        statements: Vec<(String, Vec<serde_json::Value>)>,
    ) -> Result<()> {
        let response = self.raft.client_write(Request::batch(statements)).await?;
        if !response.data.success {
            bail!(
                "Batch failed: {}",
                response.data.error.as_deref().unwrap_or("unknown error")
            );
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

            let metrics = self.raft.metrics().borrow_watched().clone();

            if let Some(leader) = metrics.current_leader {
                info!("Raft leader elected: node {}", leader);
                return Ok(());
            }

            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    /// Resolves when the Raft core stops: a fatal error (e.g. the state
    /// machine could not apply a committed entry) or a shutdown. Returns
    /// the reason. After this every write fails, so the server must stop.
    pub async fn stopped(&self) -> String {
        let mut metrics = self.raft.metrics();
        loop {
            if let Err(fatal) = &metrics.borrow_watched().running_state {
                return fatal.to_string();
            }
            if metrics.changed().await.is_err() {
                return "Raft core exited".to_string();
            }
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
        let metrics = self.raft.metrics().borrow_watched().clone();
        metrics.current_leader == Some(self.node_id)
    }

    /// Get the current leader's node ID (if known)
    pub fn current_leader(&self) -> Option<NodeId> {
        self.raft.metrics().borrow_watched().current_leader
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A private database path for the Raft config (never a shared fixed
    /// path: parallel tests would collide)
    fn test_db_path() -> (tempfile::TempDir, String) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.db").to_string_lossy().to_string();
        (dir, path)
    }
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
                leader_node INTEGER NOT NULL DEFAULT 0,
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
        let (_dir, db_path) = test_db_path();
        let writer = RaftWriter::single_node(pool, 1, 19000, &db_path).await;
        assert!(writer.is_ok());
    }

    #[tokio::test]
    async fn test_single_node_write() {
        let pool = test_pool_with_raft_tables().await;
        let (_dir, db_path) = test_db_path();
        let writer = RaftWriter::single_node(pool, 1, 19001, &db_path)
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
        let (_dir, db_path) = test_db_path();
        let writer = RaftWriter::single_node(pool, 1, 19002, &db_path)
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

    /// Concurrent writes (which openraft batches into shared appends and
    /// applies) all commit, each with its own response
    #[tokio::test]
    async fn test_concurrent_writes() {
        let pool = test_pool_with_raft_tables().await;
        let (_dir, db_path) = test_db_path();
        let writer = std::sync::Arc::new(
            RaftWriter::single_node(pool.clone(), 1, 19003, &db_path)
                .await
                .unwrap(),
        );
        writer
            .wait_for_leader(Duration::from_secs(5))
            .await
            .unwrap();

        let tasks: Vec<_> = (0..50)
            .map(|i| {
                let writer = writer.clone();
                tokio::spawn(async move {
                    writer
                        .execute(
                            "INSERT INTO test_data (id, value) VALUES (?, ?)",
                            vec![serde_json::json!(format!("id-{}", i)), serde_json::json!(i)],
                        )
                        .await
                })
            })
            .collect();
        for task in tasks {
            let response = task.await.unwrap().unwrap();
            assert_eq!(response.rows_affected, 1);
        }
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM test_data")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(count, 50);
    }

    /// stopped() resolves once the Raft core is gone
    #[tokio::test]
    async fn test_stopped_resolves_when_core_stops() {
        let pool = test_pool_with_raft_tables().await;
        let (_dir, db_path) = test_db_path();
        let writer = RaftWriter::single_node(pool, 1, 19004, &db_path)
            .await
            .unwrap();
        writer
            .wait_for_leader(Duration::from_secs(5))
            .await
            .unwrap();
        writer.raft().shutdown().await.unwrap();
        let reason = tokio::time::timeout(Duration::from_secs(5), writer.stopped())
            .await
            .expect("stopped() should resolve after shutdown");
        assert!(!reason.is_empty());
    }
}
