//! Raft state machine - executes replicated SQL commands
//!
//! The state machine receives committed log entries from Raft and
//! applies them to the SQLite database.

use std::io;

use openraft::{BasicNode, LogId, StorageError, StorageIOError, StoredMembership};
use serde::{Deserialize, Serialize};
use sqlx::sqlite::SqlitePool;
use tokio::sync::RwLock;
use tracing::{debug, error, warn};

use super::types::{NodeId, Request, Response};

/// Snapshot data format
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SnapshotData {
    /// Last applied log ID
    pub last_applied_log: Option<LogId<NodeId>>,
    /// Current membership configuration
    pub last_membership: StoredMembership<NodeId, BasicNode>,
    /// Serialized database state (for now, just metadata)
    /// In production, this would be the SQLite database file
    pub db_snapshot: Vec<u8>,
}

/// Game state machine that executes SQL against SQLite
pub struct GameStateMachine {
    /// Database connection pool
    pool: SqlitePool,
    /// Path to database file (for snapshots)
    db_path: Option<String>,
    /// Last applied log ID
    last_applied_log: RwLock<Option<LogId<NodeId>>>,
    /// Current membership configuration
    last_membership: RwLock<StoredMembership<NodeId, BasicNode>>,
}

impl GameStateMachine {
    /// Create a new state machine
    pub fn new(pool: SqlitePool) -> Self {
        Self {
            pool,
            db_path: None,
            last_applied_log: RwLock::new(None),
            last_membership: RwLock::new(StoredMembership::default()),
        }
    }

    /// Create with a database path (for file-based snapshots)
    pub fn with_db_path(pool: SqlitePool, db_path: impl Into<String>) -> Self {
        Self {
            pool,
            db_path: Some(db_path.into()),
            last_applied_log: RwLock::new(None),
            last_membership: RwLock::new(StoredMembership::default()),
        }
    }

    /// Execute a SQL statement against the database
    async fn execute_sql(&self, request: &Request) -> Response {
        debug!("Executing SQL: {}", request.sql);

        // Build the query with parameters
        let mut query = sqlx::query(&request.sql);

        // Bind JSON parameters
        for param in &request.params {
            query = match param {
                serde_json::Value::Null => query.bind(Option::<String>::None),
                serde_json::Value::Bool(b) => query.bind(*b),
                serde_json::Value::Number(n) => {
                    if let Some(i) = n.as_i64() {
                        query.bind(i)
                    } else if let Some(f) = n.as_f64() {
                        query.bind(f)
                    } else {
                        query.bind(n.to_string())
                    }
                }
                serde_json::Value::String(s) => query.bind(s.clone()),
                serde_json::Value::Array(_) | serde_json::Value::Object(_) => {
                    // Serialize complex types as JSON strings
                    query.bind(param.to_string())
                }
            };
        }

        match query.execute(&self.pool).await {
            Ok(result) => {
                let rows = result.rows_affected();
                debug!("SQL executed successfully, {} rows affected", rows);
                Response::ok(rows)
            }
            Err(e) => {
                error!("SQL execution failed: {}", e);
                Response::error(e.to_string())
            }
        }
    }

    /// Get the database pool
    pub fn pool(&self) -> &SqlitePool {
        &self.pool
    }

    /// Create a snapshot of the current state
    pub async fn create_snapshot_data(&self) -> Result<SnapshotData, StorageError<NodeId>> {
        let last_applied = self.last_applied_log.read().await.clone();
        let membership = self.last_membership.read().await.clone();

        // For file-based databases, we'd checkpoint and copy the file
        // For in-memory, we serialize key tables
        let db_snapshot = if let Some(ref path) = self.db_path {
            // Checkpoint WAL first
            sqlx::query("PRAGMA wal_checkpoint(TRUNCATE)")
                .execute(&self.pool)
                .await
                .map_err(|e| {
                    StorageIOError::read_snapshot(None, &io::Error::new(io::ErrorKind::Other, e))
                })?;

            // Read the database file
            tokio::fs::read(path)
                .await
                .map_err(|e| StorageIOError::read_snapshot(None, &e))?
        } else {
            // In-memory database - serialize key state
            // This is a simplified approach; production would need full state
            vec![]
        };

        Ok(SnapshotData {
            last_applied_log: last_applied,
            last_membership: membership,
            db_snapshot,
        })
    }

    /// Restore from a snapshot
    pub async fn restore_snapshot(&self, data: SnapshotData) -> Result<(), StorageError<NodeId>> {
        // Update metadata
        *self.last_applied_log.write().await = data.last_applied_log;
        *self.last_membership.write().await = data.last_membership;

        // For file-based databases, restore the file
        if let Some(ref path) = self.db_path {
            if !data.db_snapshot.is_empty() {
                // Close existing connections (SQLite will reopen)
                // Write the snapshot data
                tokio::fs::write(path, &data.db_snapshot)
                    .await
                    .map_err(|e| StorageIOError::write_snapshot(None, &e))?;

                warn!("Database restored from snapshot - connections may need refresh");
            }
        }

        debug!("Restored snapshot at log {:?}", data.last_applied_log);
        Ok(())
    }
}

// Note: RaftStateMachine and RaftSnapshotBuilder are sealed traits in OpenRaft 0.9.
// The actual state machine is implemented in storage.rs via CombinedStorage + Adaptor pattern.

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::sqlite::SqlitePoolOptions;

    async fn test_pool() -> SqlitePool {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();

        // Create a test table
        sqlx::query("CREATE TABLE test (id INTEGER PRIMARY KEY, value TEXT)")
            .execute(&pool)
            .await
            .unwrap();

        pool
    }

    #[tokio::test]
    async fn test_execute_sql_insert() {
        let pool = test_pool().await;
        let sm = GameStateMachine::new(pool);

        let request = Request::new(
            "INSERT INTO test (id, value) VALUES (?, ?)",
            vec![serde_json::json!(1), serde_json::json!("hello")],
        );

        let response = sm.execute_sql(&request).await;
        assert!(response.success);
        assert_eq!(response.rows_affected, 1);
    }

    #[tokio::test]
    async fn test_execute_sql_update() {
        let pool = test_pool().await;
        let sm = GameStateMachine::new(pool.clone());

        // Insert first
        sqlx::query("INSERT INTO test (id, value) VALUES (1, 'old')")
            .execute(&pool)
            .await
            .unwrap();

        // Update via state machine
        let request = Request::new(
            "UPDATE test SET value = ? WHERE id = ?",
            vec![serde_json::json!("new"), serde_json::json!(1)],
        );

        let response = sm.execute_sql(&request).await;
        assert!(response.success);
        assert_eq!(response.rows_affected, 1);

        // Verify
        let row: (String,) = sqlx::query_as("SELECT value FROM test WHERE id = 1")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(row.0, "new");
    }

    #[tokio::test]
    async fn test_execute_sql_error() {
        let pool = test_pool().await;
        let sm = GameStateMachine::new(pool);

        // Invalid SQL
        let request = Request::simple("SELECT * FROM nonexistent_table");
        let response = sm.execute_sql(&request).await;
        assert!(!response.success);
        assert!(response.error.is_some());
    }

    // Note: apply_entries test removed - state machine functionality is now
    // tested via CombinedStorage in storage.rs

    #[tokio::test]
    async fn test_snapshot_roundtrip() {
        let pool = test_pool().await;
        let sm = GameStateMachine::new(pool.clone());

        // Insert some data
        sqlx::query("INSERT INTO test VALUES (1, 'test')")
            .execute(&pool)
            .await
            .unwrap();

        // Create snapshot
        let snapshot_data = sm.create_snapshot_data().await.unwrap();
        assert!(snapshot_data.last_applied_log.is_none()); // Not applied via Raft

        // Restore (in-memory, so just metadata)
        sm.restore_snapshot(snapshot_data).await.unwrap();
    }
}
