//! Combined Raft storage implementation using v1 RaftStorage trait
//!
//! OpenRaft 0.9 uses sealed v2 traits (RaftLogStorage, RaftStateMachine).
//! The proper approach is to implement v1 RaftStorage and wrap with Adaptor.

#![allow(clippy::type_complexity)]
#![allow(clippy::result_large_err)]

use std::io;
use std::io::Cursor;

use openraft::storage::{RaftSnapshotBuilder, RaftStorage, Snapshot};
use openraft::{
    BasicNode, Entry, EntryPayload, LogId, LogState, OptionalSend, RaftLogReader, SnapshotMeta,
    StorageError, StorageIOError, StoredMembership, Vote,
};
use sqlx::sqlite::SqlitePool;
use tokio::sync::RwLock;
use tracing::{debug, error};

use super::state_machine::SnapshotData;
use super::types::{NodeId, Request, Response, TypeConfig};

/// Combined Raft storage implementing v1 RaftStorage trait
pub struct CombinedStorage {
    pool: SqlitePool,
    vote: RwLock<Option<Vote<NodeId>>>,
    last_purged: RwLock<Option<LogId<NodeId>>>,
    last_applied: RwLock<Option<LogId<NodeId>>>,
    membership: RwLock<StoredMembership<NodeId, BasicNode>>,
    current_snapshot: RwLock<Option<(SnapshotMeta<NodeId, BasicNode>, Vec<u8>)>>,
}

impl CombinedStorage {
    /// Create a new CombinedStorage
    /// Requires raft tables (raft_log, raft_vote, raft_meta) to already exist.
    /// Tables are created by mudd_init via Database migrations.
    pub async fn new(pool: SqlitePool) -> Result<Self, StorageError<NodeId>> {
        let storage = Self {
            pool,
            vote: RwLock::new(None),
            last_purged: RwLock::new(None),
            last_applied: RwLock::new(None),
            membership: RwLock::new(StoredMembership::default()),
            current_snapshot: RwLock::new(None),
        };
        storage.load_state().await?;
        Ok(storage)
    }

    pub fn pool(&self) -> &SqlitePool {
        &self.pool
    }

    async fn load_state(&self) -> Result<(), StorageError<NodeId>> {
        if let Some((term, node_id, committed)) = sqlx::query_as::<_, (i64, Option<i64>, i64)>(
            "SELECT term, node_id, committed FROM raft_vote WHERE id = 1",
        )
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| StorageIOError::read_vote(&io::Error::other(e)))?
        {
            *self.vote.write().await = Some(Vote {
                leader_id: openraft::LeaderId {
                    term: term as u64,
                    node_id: node_id.unwrap_or(0) as u64,
                },
                committed: committed != 0,
            });
        }

        if let Some((value,)) =
            sqlx::query_as::<_, (String,)>("SELECT value FROM raft_meta WHERE key = 'last_purged'")
                .fetch_optional(&self.pool)
                .await
                .map_err(|e| StorageIOError::read(&io::Error::other(e)))?
        {
            if let Ok(log_id) = serde_json::from_str(&value) {
                *self.last_purged.write().await = Some(log_id);
            }
        }

        if let Some((value,)) =
            sqlx::query_as::<_, (String,)>("SELECT value FROM raft_meta WHERE key = 'last_applied'")
                .fetch_optional(&self.pool)
                .await
                .map_err(|e| StorageIOError::read(&io::Error::other(e)))?
        {
            if let Ok(log_id) = serde_json::from_str(&value) {
                *self.last_applied.write().await = Some(log_id);
            }
        }

        Ok(())
    }

    fn row_to_entry(
        log_index: i64,
        term: i64,
        entry_type: &str,
        payload: Option<&str>,
    ) -> Result<Entry<TypeConfig>, StorageError<NodeId>> {
        let log_id = LogId {
            leader_id: openraft::LeaderId {
                term: term as u64,
                node_id: 0,
            },
            index: log_index as u64,
        };

        let entry_payload = match entry_type {
            "blank" => EntryPayload::Blank,
            "normal" => {
                let request: Request = serde_json::from_str(payload.unwrap_or("{}"))
                    .map_err(|e| StorageIOError::read_logs(&io::Error::other(e)))?;
                EntryPayload::Normal(request)
            }
            "membership" => {
                let membership = serde_json::from_str(payload.unwrap_or("{}"))
                    .map_err(|e| StorageIOError::read_logs(&io::Error::other(e)))?;
                EntryPayload::Membership(membership)
            }
            other => {
                return Err(StorageIOError::read_logs(&io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("unknown entry type: {}", other),
                ))
                .into())
            }
        };

        Ok(Entry {
            log_id,
            payload: entry_payload,
        })
    }

    async fn execute_sql(&self, request: &Request) -> Response {
        debug!("Executing SQL: {}", request.sql);

        let mut query = sqlx::query(&request.sql);
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
                _ => query.bind(param.to_string()),
            };
        }

        match query.execute(&self.pool).await {
            Ok(result) => Response::ok(result.rows_affected()),
            Err(e) => {
                error!("SQL execution failed: {}", e);
                Response::error(e.to_string())
            }
        }
    }
}

// Implement RaftLogReader (not sealed)
impl RaftLogReader<TypeConfig> for CombinedStorage {
    async fn try_get_log_entries<
        RB: std::ops::RangeBounds<u64> + Clone + std::fmt::Debug + OptionalSend,
    >(
        &mut self,
        range: RB,
    ) -> Result<Vec<Entry<TypeConfig>>, StorageError<NodeId>> {
        let start = match range.start_bound() {
            std::ops::Bound::Included(&n) => n as i64,
            std::ops::Bound::Excluded(&n) => (n + 1) as i64,
            std::ops::Bound::Unbounded => 0,
        };
        let end = match range.end_bound() {
            std::ops::Bound::Included(&n) => (n + 1) as i64,
            std::ops::Bound::Excluded(&n) => n as i64,
            std::ops::Bound::Unbounded => i64::MAX,
        };

        let rows: Vec<(i64, i64, String, Option<String>)> = sqlx::query_as(
            "SELECT log_index, term, entry_type, payload FROM raft_log
             WHERE log_index >= ? AND log_index < ? ORDER BY log_index",
        )
        .bind(start)
        .bind(end)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| StorageIOError::read_logs(&io::Error::other(e)))?;

        rows.into_iter()
            .map(|(idx, term, etype, payload)| {
                Self::row_to_entry(idx, term, &etype, payload.as_deref())
            })
            .collect()
    }
}

// Implement RaftSnapshotBuilder (not sealed)
impl RaftSnapshotBuilder<TypeConfig> for CombinedStorage {
    async fn build_snapshot(&mut self) -> Result<Snapshot<TypeConfig>, StorageError<NodeId>> {
        let last_applied = *self.last_applied.read().await;
        let membership = self.membership.read().await.clone();

        let data = SnapshotData {
            last_applied_log: last_applied,
            last_membership: membership.clone(),
            db_snapshot: vec![],
        };

        let bytes = serde_json::to_vec(&data)
            .map_err(|e| StorageIOError::read_snapshot(None, &io::Error::other(e)))?;

        let snapshot_id = last_applied
            .map(|id| format!("{}-{}", id.leader_id.term, id.index))
            .unwrap_or_else(|| "0-0".to_string());

        let meta = SnapshotMeta {
            last_log_id: last_applied,
            last_membership: membership,
            snapshot_id,
        };

        // Store snapshot
        *self.current_snapshot.write().await = Some((meta.clone(), bytes.clone()));

        Ok(Snapshot {
            meta,
            snapshot: Box::new(Cursor::new(bytes)),
        })
    }
}

// Implement RaftStorage (v1 combined trait)
impl RaftStorage<TypeConfig> for CombinedStorage {
    type LogReader = Self;
    type SnapshotBuilder = Self;

    async fn get_log_state(&mut self) -> Result<LogState<TypeConfig>, StorageError<NodeId>> {
        let last_purged = *self.last_purged.read().await;

        let last_log: Option<(i64, i64)> =
            sqlx::query_as("SELECT log_index, term FROM raft_log ORDER BY log_index DESC LIMIT 1")
                .fetch_optional(&self.pool)
                .await
                .map_err(|e| StorageIOError::read_logs(&io::Error::other(e)))?;

        let last_log_id = last_log.map(|(index, term)| LogId {
            leader_id: openraft::LeaderId {
                term: term as u64,
                node_id: 0,
            },
            index: index as u64,
        });

        Ok(LogState {
            last_purged_log_id: last_purged,
            last_log_id,
        })
    }

    async fn save_vote(&mut self, vote: &Vote<NodeId>) -> Result<(), StorageError<NodeId>> {
        sqlx::query(
            "INSERT OR REPLACE INTO raft_vote (id, term, node_id, committed) VALUES (1, ?, ?, ?)",
        )
        .bind(vote.leader_id.term as i64)
        .bind(vote.leader_id.node_id as i64)
        .bind(if vote.committed { 1i64 } else { 0i64 })
        .execute(&self.pool)
        .await
        .map_err(|e| StorageIOError::write_vote(&io::Error::other(e)))?;

        *self.vote.write().await = Some(*vote);
        Ok(())
    }

    async fn read_vote(&mut self) -> Result<Option<Vote<NodeId>>, StorageError<NodeId>> {
        Ok(*self.vote.read().await)
    }

    async fn get_log_reader(&mut self) -> Self::LogReader {
        CombinedStorage {
            pool: self.pool.clone(),
            vote: RwLock::new(*self.vote.read().await),
            last_purged: RwLock::new(*self.last_purged.read().await),
            last_applied: RwLock::new(*self.last_applied.read().await),
            membership: RwLock::new(self.membership.read().await.clone()),
            current_snapshot: RwLock::new(self.current_snapshot.read().await.clone()),
        }
    }

    async fn append_to_log<I>(&mut self, entries: I) -> Result<(), StorageError<NodeId>>
    where
        I: IntoIterator<Item = Entry<TypeConfig>> + Send,
    {
        let entries: Vec<_> = entries.into_iter().collect();
        if entries.is_empty() {
            return Ok(());
        }

        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| StorageIOError::write_logs(&io::Error::other(e)))?;

        for entry in &entries {
            let (entry_type, payload) = match &entry.payload {
                EntryPayload::Blank => ("blank", None),
                EntryPayload::Normal(req) => {
                    let json = serde_json::to_string(req)
                        .map_err(|e| StorageIOError::write_logs(&io::Error::other(e)))?;
                    ("normal", Some(json))
                }
                EntryPayload::Membership(m) => {
                    let json = serde_json::to_string(m)
                        .map_err(|e| StorageIOError::write_logs(&io::Error::other(e)))?;
                    ("membership", Some(json))
                }
            };

            sqlx::query(
                "INSERT OR REPLACE INTO raft_log (log_index, term, entry_type, payload) VALUES (?, ?, ?, ?)",
            )
            .bind(entry.log_id.index as i64)
            .bind(entry.log_id.leader_id.term as i64)
            .bind(entry_type)
            .bind(payload)
            .execute(&mut *tx)
            .await
            .map_err(|e| StorageIOError::write_logs(&io::Error::other(e)))?;
        }

        tx.commit()
            .await
            .map_err(|e| StorageIOError::write_logs(&io::Error::other(e)))?;

        Ok(())
    }

    async fn delete_conflict_logs_since(
        &mut self,
        log_id: LogId<NodeId>,
    ) -> Result<(), StorageError<NodeId>> {
        sqlx::query("DELETE FROM raft_log WHERE log_index >= ?")
            .bind(log_id.index as i64)
            .execute(&self.pool)
            .await
            .map_err(|e| StorageIOError::write_logs(&io::Error::other(e)))?;
        Ok(())
    }

    async fn purge_logs_upto(&mut self, log_id: LogId<NodeId>) -> Result<(), StorageError<NodeId>> {
        sqlx::query("DELETE FROM raft_log WHERE log_index <= ?")
            .bind(log_id.index as i64)
            .execute(&self.pool)
            .await
            .map_err(|e| StorageIOError::write_logs(&io::Error::other(e)))?;

        let json = serde_json::to_string(&log_id)
            .map_err(|e| StorageIOError::write_logs(&io::Error::other(e)))?;
        sqlx::query("INSERT OR REPLACE INTO raft_meta (key, value) VALUES ('last_purged', ?)")
            .bind(&json)
            .execute(&self.pool)
            .await
            .map_err(|e| StorageIOError::write_logs(&io::Error::other(e)))?;

        *self.last_purged.write().await = Some(log_id);
        Ok(())
    }

    async fn last_applied_state(
        &mut self,
    ) -> Result<(Option<LogId<NodeId>>, StoredMembership<NodeId, BasicNode>), StorageError<NodeId>>
    {
        Ok((
            *self.last_applied.read().await,
            self.membership.read().await.clone(),
        ))
    }

    async fn apply_to_state_machine(
        &mut self,
        entries: &[Entry<TypeConfig>],
    ) -> Result<Vec<Response>, StorageError<NodeId>> {
        let mut results = Vec::new();

        for entry in entries {
            *self.last_applied.write().await = Some(entry.log_id);

            match &entry.payload {
                EntryPayload::Blank => results.push(Response::ok(0)),
                EntryPayload::Normal(request) => {
                    let response = self.execute_sql(request).await;
                    results.push(response);
                }
                EntryPayload::Membership(membership) => {
                    *self.membership.write().await =
                        StoredMembership::new(Some(entry.log_id), membership.clone());
                    results.push(Response::ok(0));
                }
            }
        }

        // Persist last_applied
        if let Some(last) = self.last_applied.read().await.as_ref() {
            let json = serde_json::to_string(last)
                .map_err(|e| StorageIOError::write(&io::Error::other(e)))?;
            sqlx::query("INSERT OR REPLACE INTO raft_meta (key, value) VALUES ('last_applied', ?)")
                .bind(&json)
                .execute(&self.pool)
                .await
                .map_err(|e| StorageIOError::write(&io::Error::other(e)))?;
        }

        Ok(results)
    }

    async fn get_snapshot_builder(&mut self) -> Self::SnapshotBuilder {
        CombinedStorage {
            pool: self.pool.clone(),
            vote: RwLock::new(*self.vote.read().await),
            last_purged: RwLock::new(*self.last_purged.read().await),
            last_applied: RwLock::new(*self.last_applied.read().await),
            membership: RwLock::new(self.membership.read().await.clone()),
            current_snapshot: RwLock::new(self.current_snapshot.read().await.clone()),
        }
    }

    async fn begin_receiving_snapshot(
        &mut self,
    ) -> Result<Box<Cursor<Vec<u8>>>, StorageError<NodeId>> {
        Ok(Box::new(Cursor::new(Vec::new())))
    }

    async fn install_snapshot(
        &mut self,
        meta: &SnapshotMeta<NodeId, BasicNode>,
        snapshot: Box<Cursor<Vec<u8>>>,
    ) -> Result<(), StorageError<NodeId>> {
        let data = snapshot.into_inner();

        // Parse snapshot data
        let snapshot_data: SnapshotData = serde_json::from_slice(&data)
            .map_err(|e| StorageIOError::read_snapshot(None, &io::Error::other(e)))?;

        // Apply snapshot state
        *self.last_applied.write().await = snapshot_data.last_applied_log;
        *self.membership.write().await = snapshot_data.last_membership;
        *self.current_snapshot.write().await = Some((meta.clone(), data));

        debug!("Installed snapshot at {:?}", meta.last_log_id);
        Ok(())
    }

    async fn get_current_snapshot(
        &mut self,
    ) -> Result<Option<Snapshot<TypeConfig>>, StorageError<NodeId>> {
        let snapshot = self.current_snapshot.read().await.clone();
        Ok(snapshot.map(|(meta, data)| Snapshot {
            meta,
            snapshot: Box::new(Cursor::new(data)),
        }))
    }
}

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

        // Create raft tables (normally done by mudd_init)
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

        sqlx::query("CREATE INDEX idx_raft_log_term ON raft_log(term)")
            .execute(&pool)
            .await
            .unwrap();

        pool
    }

    #[tokio::test]
    async fn test_storage_creation() {
        let pool = test_pool().await;
        let storage = CombinedStorage::new(pool).await;
        assert!(storage.is_ok());
    }

    #[tokio::test]
    async fn test_vote_persistence() {
        let pool = test_pool().await;
        let mut storage = CombinedStorage::new(pool).await.unwrap();

        let vote = Vote {
            leader_id: openraft::LeaderId {
                term: 5,
                node_id: 2,
            },
            committed: true,
        };

        storage.save_vote(&vote).await.unwrap();
        let loaded = storage.read_vote().await.unwrap();
        assert!(loaded.is_some());
        assert_eq!(loaded.unwrap().leader_id.term, 5);
    }

    #[tokio::test]
    async fn test_log_append_and_read() {
        let pool = test_pool().await;
        let mut storage = CombinedStorage::new(pool).await.unwrap();

        let entries = vec![Entry {
            log_id: LogId {
                leader_id: openraft::LeaderId {
                    term: 1,
                    node_id: 1,
                },
                index: 1,
            },
            payload: EntryPayload::Normal(Request::simple("SELECT 1")),
        }];

        storage.append_to_log(entries).await.unwrap();

        let read = storage.try_get_log_entries(1..2).await.unwrap();
        assert_eq!(read.len(), 1);
    }
}
