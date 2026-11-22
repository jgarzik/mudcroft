//! Raft storage on SQLite (openraft v2 storage traits)
//!
//! - [`LogStore`]: the log (`raft_log`), the vote (`raft_vote`) and the last
//!   purged log id (`raft_meta` `last_purged`).
//! - [`StateMachine`]: applies committed SQL requests to the game tables and
//!   records its position (`raft_meta` `last_applied`, `last_membership`) in
//!   the same transaction, so every entry is applied exactly once even across
//!   a crash.
//!
//! Both share one pool, which a snapshot install replaces.

use std::io;
use std::io::Cursor;
use std::ops::{Bound, RangeBounds};
use std::sync::Arc;

use futures_util::{Stream, TryStreamExt};
use openraft::alias::{EntryOf, LogIdOf, SnapshotMetaOf, SnapshotOf, StoredMembershipOf, VoteOf};
use openraft::impls::leader_id_adv::LeaderId;
use openraft::storage::{EntryResponder, IOFlushed, RaftLogStorage, RaftStateMachine};
use openraft::{EntryPayload, LogState, OptionalSend, RaftLogReader, RaftSnapshotBuilder};
use serde::{Deserialize, Serialize};
use sqlx::sqlite::{SqliteConnection, SqlitePool};
use tokio::sync::RwLock;
use tracing::{debug, error};

use super::types::{Request, Response, TypeConfig};

type LogId = LogIdOf<TypeConfig>;
type Vote = VoteOf<TypeConfig>;
type Entry = EntryOf<TypeConfig>;
type StoredMembership = StoredMembershipOf<TypeConfig>;
type SnapshotMeta = SnapshotMetaOf<TypeConfig>;
type Snapshot = SnapshotOf<TypeConfig, Cursor<Vec<u8>>>;

/// The database pool, shared by the log store and the state machine (a
/// snapshot install replaces it)
pub type SharedPool = Arc<RwLock<SqlitePool>>;

/// A log id from its stored parts: (term, leader node, index)
fn log_id(term: i64, leader_node: i64, index: i64) -> LogId {
    LogId::new(
        LeaderId {
            term: term as u64,
            node_id: leader_node as u64,
        },
        index as u64,
    )
}

async fn read_meta(pool: &SqlitePool, key: &str) -> io::Result<Option<String>> {
    sqlx::query_scalar("SELECT value FROM raft_meta WHERE key = ?")
        .bind(key)
        .fetch_optional(pool)
        .await
        .map_err(io::Error::other)
}

async fn write_meta(
    conn: &mut SqliteConnection,
    key: &str,
    value: &impl Serialize,
) -> io::Result<()> {
    let json = serde_json::to_string(value).map_err(io::Error::other)?;
    sqlx::query("INSERT OR REPLACE INTO raft_meta (key, value) VALUES (?, ?)")
        .bind(key)
        .bind(json)
        .execute(conn)
        .await
        .map_err(io::Error::other)?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Log store
// ---------------------------------------------------------------------------

/// The Raft log, vote and purge point
#[derive(Clone)]
pub struct LogStore {
    pool: SharedPool,
}

impl LogStore {
    /// A log store over the raft tables (created by mudd_init)
    pub fn new(pool: SharedPool) -> Self {
        Self { pool }
    }

    fn row_to_entry(
        index: i64,
        term: i64,
        leader_node: i64,
        entry_type: &str,
        payload: Option<&str>,
    ) -> io::Result<Entry> {
        let payload = match entry_type {
            "blank" => EntryPayload::Blank,
            "normal" => EntryPayload::Normal(
                serde_json::from_str(payload.unwrap_or("{}")).map_err(io::Error::other)?,
            ),
            "membership" => EntryPayload::Membership(
                serde_json::from_str(payload.unwrap_or("{}")).map_err(io::Error::other)?,
            ),
            other => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("unknown entry type: {}", other),
                ))
            }
        };
        Ok(Entry {
            log_id: log_id(term, leader_node, index),
            payload,
        })
    }
}

impl RaftLogReader<TypeConfig> for LogStore {
    async fn try_get_log_entries<RB: RangeBounds<u64> + Clone + std::fmt::Debug + OptionalSend>(
        &mut self,
        range: RB,
    ) -> io::Result<Vec<Entry>> {
        let start = match range.start_bound() {
            Bound::Included(&n) => n as i64,
            Bound::Excluded(&n) => (n + 1) as i64,
            Bound::Unbounded => 0,
        };
        let end = match range.end_bound() {
            Bound::Included(&n) => (n + 1) as i64,
            Bound::Excluded(&n) => n as i64,
            Bound::Unbounded => i64::MAX,
        };

        let pool = self.pool.read().await;
        let rows: Vec<(i64, i64, i64, String, Option<String>)> = sqlx::query_as(
            "SELECT log_index, term, leader_node, entry_type, payload FROM raft_log
             WHERE log_index >= ? AND log_index < ? ORDER BY log_index",
        )
        .bind(start)
        .bind(end)
        .fetch_all(&*pool)
        .await
        .map_err(io::Error::other)?;

        rows.into_iter()
            .map(|(index, term, leader, etype, payload)| {
                Self::row_to_entry(index, term, leader, &etype, payload.as_deref())
            })
            .collect()
    }

    async fn read_vote(&mut self) -> io::Result<Option<Vote>> {
        let pool = self.pool.read().await;
        let row: Option<(i64, Option<i64>, i64)> =
            sqlx::query_as("SELECT term, node_id, committed FROM raft_vote WHERE id = 1")
                .fetch_optional(&*pool)
                .await
                .map_err(io::Error::other)?;
        Ok(row.map(|(term, node_id, committed)| Vote {
            leader_id: LeaderId {
                term: term as u64,
                node_id: node_id.unwrap_or(0) as u64,
            },
            committed: committed != 0,
        }))
    }
}

impl RaftLogStorage<TypeConfig> for LogStore {
    type LogReader = Self;

    async fn get_log_state(&mut self) -> io::Result<LogState<TypeConfig>> {
        let pool = self.pool.read().await;
        let last_purged: Option<LogId> = match read_meta(&pool, "last_purged").await? {
            Some(json) => serde_json::from_str(&json).map_err(io::Error::other)?,
            None => None,
        };
        let last_log: Option<(i64, i64, i64)> = sqlx::query_as(
            "SELECT log_index, term, leader_node FROM raft_log ORDER BY log_index DESC LIMIT 1",
        )
        .fetch_optional(&*pool)
        .await
        .map_err(io::Error::other)?;

        // An empty log after purging still has a last log id: the purged one
        let last_log_id = last_log
            .map(|(index, term, leader)| log_id(term, leader, index))
            .or(last_purged);

        Ok(LogState {
            last_purged_log_id: last_purged,
            last_log_id,
        })
    }

    async fn get_log_reader(&mut self) -> Self::LogReader {
        self.clone()
    }

    async fn save_vote(&mut self, vote: &Vote) -> io::Result<()> {
        let pool = self.pool.read().await;
        sqlx::query(
            "INSERT OR REPLACE INTO raft_vote (id, term, node_id, committed) VALUES (1, ?, ?, ?)",
        )
        .bind(vote.leader_id.term as i64)
        .bind(vote.leader_id.node_id as i64)
        .bind(vote.committed as i64)
        .execute(&*pool)
        .await
        .map_err(io::Error::other)?;
        Ok(())
    }

    async fn append<I>(&mut self, entries: I, callback: IOFlushed<TypeConfig>) -> io::Result<()>
    where
        I: IntoIterator<Item = Entry> + OptionalSend,
        I::IntoIter: OptionalSend,
    {
        // openraft hands over every entry it has ready at once (concurrent
        // client writes batched together): one transaction for all of them
        let pool = self.pool.read().await;
        let mut tx = pool.begin().await.map_err(io::Error::other)?;
        for entry in entries {
            let (entry_type, payload) = match &entry.payload {
                EntryPayload::Blank => ("blank", None),
                EntryPayload::Normal(req) => (
                    "normal",
                    Some(serde_json::to_string(req).map_err(io::Error::other)?),
                ),
                EntryPayload::Membership(m) => (
                    "membership",
                    Some(serde_json::to_string(m).map_err(io::Error::other)?),
                ),
            };
            sqlx::query(
                "INSERT OR REPLACE INTO raft_log (log_index, term, leader_node, entry_type, payload)
                 VALUES (?, ?, ?, ?, ?)",
            )
            .bind(entry.log_id.index as i64)
            .bind(entry.log_id.leader_id.term as i64)
            .bind(entry.log_id.leader_id.node_id as i64)
            .bind(entry_type)
            .bind(payload)
            .execute(&mut *tx)
            .await
            .map_err(io::Error::other)?;
        }
        tx.commit().await.map_err(io::Error::other)?;
        callback.io_completed(Ok(()));
        Ok(())
    }

    async fn truncate_after(&mut self, last_log_id: Option<LogId>) -> io::Result<()> {
        let keep_through = last_log_id.map(|id| id.index as i64).unwrap_or(-1);
        let pool = self.pool.read().await;
        sqlx::query("DELETE FROM raft_log WHERE log_index > ?")
            .bind(keep_through)
            .execute(&*pool)
            .await
            .map_err(io::Error::other)?;
        Ok(())
    }

    async fn purge(&mut self, log_id: LogId) -> io::Result<()> {
        let pool = self.pool.read().await;
        let mut tx = pool.begin().await.map_err(io::Error::other)?;
        sqlx::query("DELETE FROM raft_log WHERE log_index <= ?")
            .bind(log_id.index as i64)
            .execute(&mut *tx)
            .await
            .map_err(io::Error::other)?;
        write_meta(&mut tx, "last_purged", &Some(log_id)).await?;
        tx.commit().await.map_err(io::Error::other)?;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// State machine
// ---------------------------------------------------------------------------

/// What a snapshot carries
#[derive(Debug, Serialize, Deserialize)]
struct SnapshotPayload {
    last_applied_log: Option<LogId>,
    last_membership: StoredMembership,
    /// The database file, when the state machine knows its path
    db_snapshot: Vec<u8>,
}

struct SmState {
    last_applied: Option<LogId>,
    membership: StoredMembership,
    current_snapshot: Option<(SnapshotMeta, Vec<u8>)>,
}

/// Applies committed SQL requests to the database
#[derive(Clone)]
pub struct StateMachine {
    pool: SharedPool,
    db_path: Option<String>,
    state: Arc<RwLock<SmState>>,
}

impl StateMachine {
    /// A state machine resuming from the position recorded in `raft_meta`
    pub async fn new(pool: SharedPool, db_path: Option<String>) -> io::Result<Self> {
        let (last_applied, membership) = {
            let p = pool.read().await;
            let last_applied: Option<LogId> = match read_meta(&p, "last_applied").await? {
                Some(json) => serde_json::from_str(&json).map_err(io::Error::other)?,
                None => None,
            };
            // Without the persisted membership a restarted node has no
            // voters and can never elect a leader
            let membership: StoredMembership = match read_meta(&p, "last_membership").await? {
                Some(json) => serde_json::from_str(&json).map_err(io::Error::other)?,
                None => StoredMembership::default(),
            };
            (last_applied, membership)
        };
        Ok(Self {
            pool,
            db_path,
            state: Arc::new(RwLock::new(SmState {
                last_applied,
                membership,
                current_snapshot: None,
            })),
        })
    }

    /// Apply one request's statements inside a savepoint: all or nothing.
    /// A failing statement rolls back only this request; an error is
    /// returned only if the transaction itself is broken.
    async fn apply_request(conn: &mut SqliteConnection, request: &Request) -> io::Result<Response> {
        sqlx::query("SAVEPOINT entry")
            .execute(&mut *conn)
            .await
            .map_err(io::Error::other)?;
        let mut rows = 0;
        for statement in &request.statements {
            debug!("Executing SQL: {}", statement.sql);
            // Requests are built only from SQL literals in this crate, with
            // every value bound as a parameter (see RaftWriter::execute)
            let query = sqlx::query(sqlx::AssertSqlSafe(statement.sql.as_str()));
            match bind_params(query, &statement.params)
                .execute(&mut *conn)
                .await
            {
                Ok(result) => rows += result.rows_affected(),
                Err(e) => {
                    error!("SQL execution failed (rolling back the request): {}", e);
                    sqlx::query("ROLLBACK TO entry")
                        .execute(&mut *conn)
                        .await
                        .map_err(io::Error::other)?;
                    sqlx::query("RELEASE entry")
                        .execute(&mut *conn)
                        .await
                        .map_err(io::Error::other)?;
                    return Ok(Response::error(e.to_string()));
                }
            }
        }
        sqlx::query("RELEASE entry")
            .execute(&mut *conn)
            .await
            .map_err(io::Error::other)?;
        Ok(Response::ok(rows))
    }
}

/// Bind JSON parameters to a query. Strings prefixed `blob:` carry base64
/// binary data.
fn bind_params<'q>(
    mut query: sqlx::query::Query<'q, sqlx::Sqlite, sqlx::sqlite::SqliteArguments>,
    params: &[serde_json::Value],
) -> sqlx::query::Query<'q, sqlx::Sqlite, sqlx::sqlite::SqliteArguments> {
    use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
    for param in params {
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
            serde_json::Value::String(s) => {
                if let Some(b64_data) = s.strip_prefix("blob:") {
                    match BASE64.decode(b64_data) {
                        Ok(bytes) => query.bind(bytes),
                        Err(e) => {
                            error!("Failed to decode base64 blob: {}", e);
                            query.bind(s.as_str())
                        }
                    }
                } else {
                    query.bind(s.as_str())
                }
            }
            _ => query.bind(param.to_string()),
        };
    }
    query
}

impl RaftSnapshotBuilder<TypeConfig> for StateMachine {
    type SnapshotData = Cursor<Vec<u8>>;

    async fn build_snapshot(&mut self) -> io::Result<Snapshot> {
        let (last_applied, membership) = {
            let state = self.state.read().await;
            (state.last_applied, state.membership.clone())
        };

        // The database file, if the path is known
        let db_snapshot = if let Some(ref db_path) = self.db_path {
            let pool = self.pool.read().await;
            sqlx::query("PRAGMA wal_checkpoint(TRUNCATE)")
                .execute(&*pool)
                .await
                .map_err(io::Error::other)?;
            drop(pool);
            tokio::fs::read(db_path).await?
        } else {
            vec![]
        };

        let bytes = serde_json::to_vec(&SnapshotPayload {
            last_applied_log: last_applied,
            last_membership: membership.clone(),
            db_snapshot,
        })
        .map_err(io::Error::other)?;

        let meta = SnapshotMeta {
            last_log_id: last_applied,
            last_membership: membership,
        };
        self.state.write().await.current_snapshot = Some((meta.clone(), bytes.clone()));
        debug!("Built snapshot at {:?}", meta.last_log_id);

        Ok(Snapshot {
            meta,
            snapshot: Cursor::new(bytes),
        })
    }
}

impl RaftStateMachine<TypeConfig> for StateMachine {
    type SnapshotData = Cursor<Vec<u8>>;
    type SnapshotBuilder = Self;

    async fn applied_state(&mut self) -> io::Result<(Option<LogId>, StoredMembership)> {
        let state = self.state.read().await;
        Ok((state.last_applied, state.membership.clone()))
    }

    /// Apply everything openraft has ready as one transaction (group commit):
    /// each request in its own savepoint, plus the new position, then answer
    /// the clients once it is durable
    async fn apply<Strm>(&mut self, mut entries: Strm) -> io::Result<()>
    where
        Strm: Stream<Item = Result<EntryResponder<TypeConfig>, io::Error>> + Unpin + OptionalSend,
    {
        let mut batch = Vec::new();
        while let Some(item) = entries.try_next().await? {
            batch.push(item);
        }
        if batch.is_empty() {
            return Ok(());
        }

        let (mut last_applied, mut membership) = {
            let state = self.state.read().await;
            (state.last_applied, state.membership.clone())
        };
        let mut membership_changed = false;
        let mut replies = Vec::with_capacity(batch.len());

        let pool = self.pool.read().await;
        let mut tx = pool.begin().await.map_err(io::Error::other)?;
        for (entry, responder) in batch {
            last_applied = Some(entry.log_id);
            let response = match entry.payload {
                EntryPayload::Blank => Response::ok(0),
                EntryPayload::Normal(request) => Self::apply_request(&mut tx, &request).await?,
                EntryPayload::Membership(m) => {
                    membership = StoredMembership::new(Some(entry.log_id), m);
                    membership_changed = true;
                    Response::ok(0)
                }
            };
            replies.push((responder, response));
        }
        write_meta(&mut tx, "last_applied", &last_applied).await?;
        if membership_changed {
            write_meta(&mut tx, "last_membership", &membership).await?;
        }
        tx.commit().await.map_err(io::Error::other)?;

        // No checkpoint here: in WAL mode a committed transaction is visible
        // to every connection at once, and SQLite checkpoints on its own
        drop(pool);

        {
            let mut state = self.state.write().await;
            state.last_applied = last_applied;
            state.membership = membership;
        }
        for (responder, response) in replies {
            if let Some(responder) = responder {
                responder.send(response);
            }
        }
        Ok(())
    }

    async fn get_snapshot_builder(&mut self) -> Self::SnapshotBuilder {
        self.clone()
    }

    async fn install_snapshot(
        &mut self,
        meta: &SnapshotMeta,
        snapshot: Cursor<Vec<u8>>,
    ) -> io::Result<()> {
        use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
        use std::str::FromStr;

        let data = snapshot.into_inner();
        let payload: SnapshotPayload = serde_json::from_slice(&data).map_err(io::Error::other)?;

        // A snapshot is only usable with its database file: recording the
        // position without the data would skip every entry it covers
        let Some(db_path) = self.db_path.clone() else {
            return Err(io::Error::other(
                "cannot install a snapshot: this state machine has no database path",
            ));
        };
        if payload.db_snapshot.is_empty() {
            return Err(io::Error::other(
                "cannot install a snapshot that carries no database",
            ));
        }

        debug!(
            "Installing snapshot database ({} bytes)",
            payload.db_snapshot.len()
        );
        // NOTE: this closes the shared pool, including clones the
        // application reads through (see docs/roadmap.md, Snapshots)
        self.pool.read().await.close().await;
        tokio::fs::write(&db_path, &payload.db_snapshot).await?;
        let options = SqliteConnectOptions::from_str(&format!("sqlite:{}?mode=rw", db_path))
            .map_err(io::Error::other)?
            .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal)
            .foreign_keys(true);
        let new_pool = SqlitePoolOptions::new()
            .max_connections(10)
            .connect_with(options)
            .await
            .map_err(io::Error::other)?;
        *self.pool.write().await = new_pool;

        // Persist the position so a restart resumes from it
        {
            let pool = self.pool.read().await;
            let mut tx = pool.begin().await.map_err(io::Error::other)?;
            write_meta(&mut tx, "last_applied", &payload.last_applied_log).await?;
            write_meta(&mut tx, "last_membership", &payload.last_membership).await?;
            tx.commit().await.map_err(io::Error::other)?;
        }

        let mut state = self.state.write().await;
        state.last_applied = payload.last_applied_log;
        state.membership = payload.last_membership;
        state.current_snapshot = Some((meta.clone(), data));
        debug!("Installed snapshot at {:?}", meta.last_log_id);
        Ok(())
    }

    async fn get_current_snapshot(&mut self) -> io::Result<Option<Snapshot>> {
        let state = self.state.read().await;
        Ok(state.current_snapshot.clone().map(|(meta, data)| Snapshot {
            meta,
            snapshot: Cursor::new(data),
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::test_utils::test_pool;

    fn shared(pool: SqlitePool) -> SharedPool {
        Arc::new(RwLock::new(pool))
    }

    fn normal(index: u64, request: Request) -> Entry {
        Entry {
            log_id: log_id(1, 1, index as i64),
            payload: EntryPayload::Normal(request),
        }
    }

    async fn apply(sm: &mut StateMachine, entries: Vec<Entry>) {
        let stream =
            futures_util::stream::iter(entries.into_iter().map(|e| Ok::<_, io::Error>((e, None))));
        sm.apply(stream).await.unwrap();
    }

    #[tokio::test]
    async fn test_vote_persistence() {
        let mut store = LogStore::new(shared(test_pool().await));
        let vote = Vote {
            leader_id: LeaderId {
                term: 5,
                node_id: 2,
            },
            committed: true,
        };
        store.save_vote(&vote).await.unwrap();
        let loaded = store.read_vote().await.unwrap().expect("vote");
        assert_eq!(loaded.leader_id.term, 5);
        assert_eq!(loaded.leader_id.node_id, 2);
        assert!(loaded.committed);
    }

    #[tokio::test]
    async fn test_log_append_read_truncate_purge() {
        let mut store = LogStore::new(shared(test_pool().await));
        let entries: Vec<Entry> = (1..=4)
            .map(|i| normal(i, Request::simple("SELECT 1")))
            .collect();
        store.append(entries, IOFlushed::noop()).await.unwrap();
        assert_eq!(store.try_get_log_entries(1..5).await.unwrap().len(), 4);

        store.truncate_after(Some(log_id(1, 1, 2))).await.unwrap();
        let state = store.get_log_state().await.unwrap();
        assert_eq!(state.last_log_id.unwrap().index, 2);

        store.purge(log_id(1, 1, 2)).await.unwrap();
        let state = store.get_log_state().await.unwrap();
        assert_eq!(state.last_purged_log_id.unwrap().index, 2);
        assert_eq!(
            state.last_log_id.unwrap().index,
            2,
            "the purged id is the last"
        );
        assert!(store.try_get_log_entries(0..10).await.unwrap().is_empty());
    }

    /// Each request is atomic: a failing one leaves no rows behind and does
    /// not disturb the others applied in the same batch, and the position
    /// covers the whole batch and survives a restart
    #[tokio::test]
    async fn test_apply_batch_is_atomic_per_request() {
        let pool = test_pool().await;
        sqlx::query("CREATE TABLE batch_t (v INTEGER NOT NULL)")
            .execute(&pool)
            .await
            .unwrap();
        let mut sm = StateMachine::new(shared(pool.clone()), None).await.unwrap();

        apply(
            &mut sm,
            vec![
                normal(
                    1,
                    Request::batch(vec![
                        ("INSERT INTO batch_t VALUES (1)".into(), vec![]),
                        ("INSERT INTO batch_t VALUES (2)".into(), vec![]),
                    ]),
                ),
                normal(
                    2,
                    Request::batch(vec![
                        ("INSERT INTO batch_t VALUES (3)".into(), vec![]),
                        ("INSERT INTO batch_t VALUES (NULL)".into(), vec![]),
                    ]),
                ),
                normal(3, Request::simple("INSERT INTO batch_t VALUES (4)")),
            ],
        )
        .await;

        let values: Vec<i64> = sqlx::query_scalar("SELECT v FROM batch_t ORDER BY v")
            .fetch_all(&pool)
            .await
            .unwrap();
        assert_eq!(values, vec![1, 2, 4], "the failed request left no rows");

        let mut resumed = StateMachine::new(shared(pool), None).await.unwrap();
        let (last_applied, _) = resumed.applied_state().await.unwrap();
        assert_eq!(last_applied.unwrap().index, 3);
    }

    /// A snapshot without the database file cannot be installed: recording
    /// its position would skip every entry it covers with none of their data
    #[tokio::test]
    async fn test_install_refuses_snapshot_without_data() {
        let pool = test_pool().await;
        let mut leader = StateMachine::new(shared(pool.clone()), None).await.unwrap();
        apply(&mut leader, vec![normal(7, Request::simple("SELECT 1"))]).await;
        let snapshot = leader
            .get_snapshot_builder()
            .await
            .build_snapshot()
            .await
            .unwrap();

        let mut follower = StateMachine::new(shared(test_pool().await), None)
            .await
            .unwrap();
        assert!(follower
            .install_snapshot(&snapshot.meta, snapshot.snapshot)
            .await
            .is_err());
        let (last_applied, _) = follower.applied_state().await.unwrap();
        assert!(last_applied.is_none(), "nothing recorded");
    }
}
