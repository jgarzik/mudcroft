//! Timer and delayed execution system
//!
//! Provides:
//! - call_out: One-shot delayed callbacks
//! - heartbeat: Periodic callbacks for NPCs/objects
//! - Persistence: Timers survive server restart

use std::collections::BTreeMap;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;
use tokio::sync::RwLock;
use tracing::{debug, warn};

use crate::raft::RaftWriter;

/// A one-shot timer that fires after a delay
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Timer {
    /// Unique timer ID
    pub id: String,
    /// Universe this timer belongs to
    pub universe_id: String,
    /// Object that owns this timer
    pub object_id: String,
    /// Method to call when timer fires
    pub method: String,
    /// Unix timestamp (ms) when timer should fire
    pub fire_at: i64,
    /// Arguments to pass to method (JSON)
    pub args: Option<String>,
    /// Failed tries to fire it so far (stored: see TimerManager::retry_or_drop)
    pub attempts: i64,
}

impl Timer {
    /// Create a new timer
    pub fn new(
        universe_id: &str,
        object_id: &str,
        method: &str,
        delay_ms: u64,
        args: Option<String>,
    ) -> Self {
        let now = chrono::Utc::now().timestamp_millis();
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            universe_id: universe_id.to_string(),
            object_id: object_id.to_string(),
            method: method.to_string(),
            fire_at: now
                .saturating_add(delay_ms.clamp(MIN_TIMER_DELAY_MS, MAX_TIMER_DELAY_MS) as i64),
            args,
            attempts: 0,
        }
    }

    /// Check if timer is due to fire
    pub fn is_due(&self) -> bool {
        let now = chrono::Utc::now().timestamp_millis();
        now >= self.fire_at
    }

    /// Time remaining until fire (0 if already due)
    pub fn time_remaining_ms(&self) -> u64 {
        let now = chrono::Utc::now().timestamp_millis();
        if now >= self.fire_at {
            0
        } else {
            (self.fire_at - now) as u64
        }
    }
}

/// A recurring heartbeat timer
#[derive(Debug, Clone)]
pub struct HeartBeat {
    /// Object that owns this heartbeat
    pub object_id: String,
    /// Universe this heartbeat belongs to
    pub universe_id: String,
    /// Interval between beats in milliseconds
    pub interval_ms: u64,
    /// When it was set (Unix ms, stored): beats fall at started_at + k *
    /// interval_ms, so a restart keeps the schedule
    pub started_at: i64,
    /// When the next beat is due (Unix ms; derived from started_at)
    pub next_due: i64,
    /// Method to call (defaults to "heart_beat")
    pub method: String,
}

impl HeartBeat {
    /// Create a new heartbeat, started now
    pub fn new(universe_id: &str, object_id: &str, interval_ms: u64) -> Self {
        Self::anchored(
            universe_id,
            object_id,
            interval_ms,
            chrono::Utc::now().timestamp_millis(),
        )
    }

    /// A stored heartbeat started at `started_at`
    pub fn anchored(universe_id: &str, object_id: &str, interval_ms: u64, started_at: i64) -> Self {
        let interval_ms = interval_ms.clamp(MIN_HEARTBEAT_MS, MAX_HEARTBEAT_MS);
        let now = chrono::Utc::now().timestamp_millis();
        Self {
            object_id: object_id.to_string(),
            universe_id: universe_id.to_string(),
            interval_ms,
            started_at,
            next_due: Self::next_after(started_at, interval_ms as i64, now),
            method: "heart_beat".to_string(),
        }
    }

    /// The first beat strictly after `now` of a heartbeat started at
    /// `started_at` (beats fall at started_at + k * interval_ms)
    pub fn next_after(started_at: i64, interval_ms: i64, now: i64) -> i64 {
        let interval_ms = interval_ms.max(1);
        if now < started_at {
            // Not started yet (or set by a node whose clock ran ahead): at
            // most one interval away
            return started_at.min(now.saturating_add(interval_ms));
        }
        let k = (now - started_at) / interval_ms + 1;
        started_at.saturating_add(k.saturating_mul(interval_ms))
    }

    /// Check if heartbeat is due to fire
    pub fn is_due(&self) -> bool {
        chrono::Utc::now().timestamp_millis() >= self.next_due
    }

    /// After a beat: the next on its schedule after now (a late server
    /// skips missed beats, never fires a burst)
    pub fn reset(&mut self) {
        self.interval_ms = self.interval_ms.clamp(MIN_HEARTBEAT_MS, MAX_HEARTBEAT_MS);
        self.next_due = Self::next_after(
            self.started_at,
            self.interval_ms as i64,
            chrono::Utc::now().timestamp_millis(),
        );
    }
}

/// Result of firing a timer
#[derive(Debug, Clone)]
pub struct TimerFired {
    pub object_id: String,
    pub universe_id: String,
    pub method: String,
    pub args: Option<String>,
}

/// Most pending one-shot timers a single object may have
pub const MAX_TIMERS_PER_OBJECT: usize = 32;

/// Shortest allowed heartbeat interval
pub const MIN_HEARTBEAT_MS: u64 = 100;
/// Longest heartbeat interval (one day)
pub const MAX_HEARTBEAT_MS: u64 = 86_400_000;
/// Shortest call_out delay: with a deadline-driven loop, a handler that
/// reschedules itself with no delay would otherwise spin
pub const MIN_TIMER_DELAY_MS: u64 = 100;
/// Longest call_out delay (30 days)
pub const MAX_TIMER_DELAY_MS: u64 = 30 * 86_400_000;
/// Tries to fire a call_out (that could not be saved or read) before it is
/// dropped
pub const MAX_FIRE_ATTEMPTS: i64 = 8;
/// First wait before trying a call_out again; doubles per attempt
pub const RETRY_MIN_MS: i64 = 1_000;
/// Longest wait between tries
pub const RETRY_MAX_MS: i64 = 300_000;

/// Timers. A call_out lives only in the `timers` table (no cache): the
/// table decides, so scheduling, cancelling and firing are each one
/// statement inside a run, serialized by the write lock. Heartbeats are a
/// cache of the `heartbeats` table (their next beat is derived from it).
pub struct TimerManager {
    /// Heartbeats keyed by (universe, object): a cache of the heartbeats
    /// table
    heartbeats: RwLock<BTreeMap<(String, String), HeartBeat>>,
    /// Database pool (call_outs are read from the table)
    pool: Option<SqlitePool>,
    /// Raft writer for consensus
    raft_writer: Option<Arc<RaftWriter>>,
    /// Wakes the timer loop when a new deadline may be earlier
    changed: tokio::sync::Notify,
}

impl std::fmt::Debug for TimerManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TimerManager")
            .field("pool", &self.pool.is_some())
            .field("raft_writer", &self.raft_writer.is_some())
            .finish()
    }
}

impl TimerManager {
    /// Create a new timer manager
    pub fn new(pool: Option<SqlitePool>, raft_writer: Option<Arc<RaftWriter>>) -> Self {
        Self {
            heartbeats: RwLock::new(BTreeMap::new()),
            pool,
            raft_writer,
            changed: tokio::sync::Notify::new(),
        }
    }

    /// Create a shared instance
    pub fn shared(pool: Option<SqlitePool>, raft_writer: Option<Arc<RaftWriter>>) -> Arc<Self> {
        Arc::new(Self::new(pool, raft_writer))
    }

    fn pool(&self) -> anyhow::Result<&SqlitePool> {
        self.pool
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("timers need a database"))
    }

    /// Run one write (through Raft when available, else directly: tests);
    /// inside a run it is part of the run's batch. Returns the rows it
    /// changed.
    async fn write(
        &self,
        sql: &'static str,
        params: Vec<serde_json::Value>,
    ) -> anyhow::Result<u64> {
        if let Some(ref raft_writer) = self.raft_writer {
            return Ok(raft_writer.execute(sql, params).await?.rows_affected);
        }
        let mut query = sqlx::query(sql);
        for param in params {
            query = match param {
                serde_json::Value::String(v) => query.bind(v),
                serde_json::Value::Number(n) => query.bind(n.as_i64()),
                serde_json::Value::Null => query.bind(Option::<String>::None),
                other => query.bind(other.to_string()),
            };
        }
        Ok(query.execute(self.pool()?).await?.rows_affected())
    }

    /// Schedule a call_out (game.call_out): its row is written within the
    /// caller's run, and the timer loop sees it once that run commits.
    /// None if the object already has [`MAX_TIMERS_PER_OBJECT`] pending.
    pub async fn schedule(self: &Arc<Self>, timer: Timer) -> anyhow::Result<Option<String>> {
        // Count under the write lock, so two runs cannot both pass the cap
        if let Some(run) = crate::db::run::current() {
            run.lock_writes().await?;
        }
        let pending: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM timers WHERE universe_id = ? AND object_id = ?",
        )
        .bind(&timer.universe_id)
        .bind(&timer.object_id)
        .fetch_one(&mut *crate::db::run::reader(self.pool()?).await?)
        .await?;
        if pending >= MAX_TIMERS_PER_OBJECT as i64 {
            warn!(
                "Object {} already has {} pending timers; refusing call_out",
                timer.object_id, pending
            );
            return Ok(None);
        }
        self.write(
            "INSERT INTO timers (id, universe_id, object_id, method, fire_at, args) VALUES (?, ?, ?, ?, ?, ?)",
            vec![
                serde_json::json!(&timer.id),
                serde_json::json!(&timer.universe_id),
                serde_json::json!(&timer.object_id),
                serde_json::json!(&timer.method),
                serde_json::json!(timer.fire_at),
                serde_json::json!(&timer.args),
            ],
        )
        .await?;
        // The loop may need to wake sooner, once the row is there to see
        let me = self.clone();
        crate::db::run::after_commit(move || async move { me.changed.notify_one() }).await;
        Ok(Some(timer.id))
    }

    /// Cancel one of an object's own call_outs (game.remove_call_out):
    /// true only if its row was there to delete. Under the write lock, so
    /// against a firing call_out (whose run deleted its row first) the
    /// answer is false: too late, as in LPMud.
    pub async fn cancel(
        &self,
        timer_id: &str,
        universe_id: &str,
        object_id: &str,
    ) -> anyhow::Result<bool> {
        let rows = self
            .write(
                "DELETE FROM timers WHERE id = ? AND universe_id = ? AND object_id = ?",
                vec![
                    serde_json::json!(timer_id),
                    serde_json::json!(universe_id),
                    serde_json::json!(object_id),
                ],
            )
            .await?;
        Ok(rows == 1)
    }

    /// The statement that claims a due call_out for firing: the run that
    /// deletes its row (one row) is the one that runs it
    pub fn consume_statement(timer_id: &str) -> (&'static str, Vec<serde_json::Value>) {
        (
            "DELETE FROM timers WHERE id = ?",
            vec![serde_json::json!(timer_id)],
        )
    }

    /// Whether a call_out's row exists (read through the current run, so
    /// under its write lock it is the truth)
    pub async fn is_stored(&self, timer_id: &str) -> anyhow::Result<bool> {
        let found: Option<i64> = sqlx::query_scalar("SELECT 1 FROM timers WHERE id = ?")
            .bind(timer_id)
            .fetch_optional(&mut *crate::db::run::reader(self.pool()?).await?)
            .await?;
        Ok(found.is_some())
    }

    /// Call_outs due at `now`, earliest first (then in the order made)
    pub async fn due_call_outs(&self, now: i64, limit: i64) -> anyhow::Result<Vec<Timer>> {
        type Row = (String, String, String, String, i64, Option<String>, i64);
        let rows: Vec<Row> = sqlx::query_as(
            "SELECT id, universe_id, object_id, method, fire_at, args, attempts FROM timers
             WHERE fire_at <= ? ORDER BY fire_at, rowid LIMIT ?",
        )
        .bind(now)
        .bind(limit)
        .fetch_all(self.pool()?)
        .await?;
        Ok(rows
            .into_iter()
            .map(
                |(id, universe_id, object_id, method, fire_at, args, attempts)| Timer {
                    id,
                    universe_id,
                    object_id,
                    method,
                    fire_at,
                    args,
                    attempts,
                },
            )
            .collect())
    }

    /// A call_out could not be fired (nothing was saved, or what it read
    /// failed): try it again after a wait that doubles per attempt, counted
    /// on its row, or drop it after [`MAX_FIRE_ATTEMPTS`]. No failure needs
    /// classifying as passing or lasting: a passing one succeeds on a later
    /// try, a lasting one is dropped in bounded time. True if dropped.
    pub async fn retry_or_drop(&self, timer: &Timer) -> anyhow::Result<bool> {
        if timer.attempts + 1 >= MAX_FIRE_ATTEMPTS {
            let (sql, params) = Self::consume_statement(&timer.id);
            self.write(sql, params).await?;
            return Ok(true);
        }
        let wait = RETRY_MIN_MS
            .saturating_mul(1i64 << timer.attempts.clamp(0, 20))
            .min(RETRY_MAX_MS);
        let at = chrono::Utc::now().timestamp_millis().saturating_add(wait);
        self.write(
            "UPDATE timers SET attempts = attempts + 1, fire_at = ? WHERE id = ?",
            vec![serde_json::json!(at), serde_json::json!(timer.id)],
        )
        .await?;
        Ok(false)
    }

    /// Set a heartbeat for an object (interval clamped to
    /// [`MIN_HEARTBEAT_MS`]): stored first (one write; beats themselves
    /// write nothing), cached only once stored
    pub async fn set_heartbeat(&self, mut heartbeat: HeartBeat) -> anyhow::Result<()> {
        heartbeat.reset(); // clamps the interval too
        self.write(
            "INSERT INTO heartbeats (universe_id, object_id, interval_ms, started_at) VALUES (?, ?, ?, ?)
             ON CONFLICT (universe_id, object_id) DO UPDATE SET interval_ms = excluded.interval_ms,
               started_at = excluded.started_at",
            vec![
                serde_json::json!(&heartbeat.universe_id),
                serde_json::json!(&heartbeat.object_id),
                serde_json::json!(heartbeat.interval_ms),
                serde_json::json!(heartbeat.started_at),
            ],
        )
        .await?;
        let key = (heartbeat.universe_id.clone(), heartbeat.object_id.clone());
        self.heartbeats.write().await.insert(key, heartbeat);
        self.changed.notify_one();
        Ok(())
    }

    /// Remove the heartbeat for an object in a universe; true if it had one
    pub async fn remove_heartbeat(&self, universe_id: &str, object_id: &str) -> bool {
        let key = (universe_id.to_string(), object_id.to_string());
        if !self.heartbeats.read().await.contains_key(&key) {
            return false;
        }
        if let Err(e) = self
            .write(
                "DELETE FROM heartbeats WHERE universe_id = ? AND object_id = ?",
                vec![serde_json::json!(universe_id), serde_json::json!(object_id)],
            )
            .await
        {
            warn!("Failed to delete heartbeat of {}: {}", object_id, e);
            return false;
        }
        self.heartbeats.write().await.remove(&key).is_some()
    }

    /// The earliest call_out or heartbeat deadline (Unix ms), if any
    pub async fn next_deadline(&self) -> anyhow::Result<Option<i64>> {
        let call_out: Option<i64> = sqlx::query_scalar("SELECT MIN(fire_at) FROM timers")
            .fetch_one(self.pool()?)
            .await?;
        let beat = self
            .heartbeats
            .read()
            .await
            .values()
            .map(|h| h.next_due)
            .min();
        Ok(call_out.into_iter().chain(beat).min())
    }

    /// Resolves when a deadline may have become earlier (a call_out
    /// committed, a heartbeat set)
    pub async fn changed(&self) {
        self.changed.notified().await
    }

    /// Due heartbeats, each re-armed to its next beat (beats write nothing)
    pub async fn tick_heartbeats(&self) -> Vec<TimerFired> {
        let mut fired = Vec::new();
        let mut heartbeats = self.heartbeats.write().await;
        for hb in heartbeats.values_mut() {
            if hb.is_due() {
                fired.push(TimerFired {
                    object_id: hb.object_id.clone(),
                    universe_id: hb.universe_id.clone(),
                    method: hb.method.clone(),
                    args: None,
                });
                hb.reset();
            }
        }
        fired
    }

    /// Load the heartbeats cache from its table (on becoming primary, and
    /// to rebuild it); call_outs have no cache
    pub async fn load_from_db(&self) -> anyhow::Result<()> {
        let Some(ref pool) = self.pool else {
            return Ok(());
        };
        let rows: Vec<(String, String, i64, i64)> = sqlx::query_as(
            "SELECT universe_id, object_id, interval_ms, started_at FROM heartbeats",
        )
        .fetch_all(&mut *crate::db::run::reader(pool).await?)
        .await?;
        // A heartbeat already scheduled keeps its next beat (a rebuild after
        // a failed run must not refire one that just beat)
        let mut heartbeats = self.heartbeats.write().await;
        let mut old = std::mem::take(&mut *heartbeats);
        for (universe_id, object_id, interval_ms, started_at) in rows {
            let interval_ms = interval_ms.max(0) as u64;
            let key = (universe_id, object_id);
            let hb = match old.remove(&key) {
                Some(hb) if hb.interval_ms == interval_ms && hb.started_at == started_at => hb,
                _ => HeartBeat::anchored(&key.0, &key.1, interval_ms, started_at),
            };
            heartbeats.insert(key, hb);
        }
        debug!("Loaded {} heartbeats from database", heartbeats.len());
        drop(heartbeats);
        self.changed.notify_one();
        Ok(())
    }

    /// Get count of active heartbeats
    pub async fn heartbeat_count(&self) -> usize {
        self.heartbeats.read().await.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A manager over a test database with universes a and b and objects
    /// /clock and /other in each
    async fn manager() -> (Arc<TimerManager>, SqlitePool) {
        let pool = crate::db::test_utils::test_pool().await;
        sqlx::query(
            "INSERT INTO accounts (id, username, password_hash, access_level, created_at)
             VALUES ('acct', 'acct', 'x', 'player', '2026-01-01')",
        )
        .execute(&pool)
        .await
        .unwrap();
        for u in ["a", "b", "u"] {
            sqlx::query(
                "INSERT INTO universes (id, name, owner_id, config, created_at)
                 VALUES (?, ?, 'acct', '{}', '2026-01-01')",
            )
            .bind(u)
            .bind(u)
            .execute(&pool)
            .await
            .unwrap();
            for o in ["/clock", "/other"] {
                sqlx::query(
                    "INSERT INTO objects (id, universe_id, class, properties, created_at, updated_at)
                     VALUES (?, ?, 'item', '{}', '2026-01-01', '2026-01-01')",
                )
                .bind(o)
                .bind(u)
                .execute(&pool)
                .await
                .unwrap();
            }
        }
        (TimerManager::shared(Some(pool.clone()), None), pool)
    }

    async fn stored(pool: &SqlitePool) -> i64 {
        sqlx::query_scalar("SELECT COUNT(*) FROM timers")
            .fetch_one(pool)
            .await
            .unwrap()
    }

    /// A call_out can be cancelled only through its own object (and
    /// universe), and only once: the table decides
    #[tokio::test]
    async fn test_cancel_is_owned_and_once() {
        let (mgr, pool) = manager().await;
        let id = mgr
            .schedule(Timer::new("a", "/clock", "tick", 60_000, None))
            .await
            .unwrap()
            .unwrap();
        assert!(!mgr.cancel(&id, "a", "/other").await.unwrap());
        assert!(!mgr.cancel(&id, "b", "/clock").await.unwrap());
        assert_eq!(stored(&pool).await, 1);
        assert!(mgr.cancel(&id, "a", "/clock").await.unwrap());
        assert!(!mgr.cancel(&id, "a", "/clock").await.unwrap(), "only once");
        assert_eq!(stored(&pool).await, 0);
    }

    /// Each object (per universe) may have at most MAX_TIMERS_PER_OBJECT
    /// pending
    #[tokio::test]
    async fn test_cap_and_cleanup_are_per_object() {
        let (mgr, pool) = manager().await;
        for _ in 0..MAX_TIMERS_PER_OBJECT {
            assert!(
                mgr.schedule(Timer::new("a", "/clock", "tick", 60_000, None))
                    .await
                    .unwrap()
                    .is_some()
            );
        }
        assert!(
            mgr.schedule(Timer::new("a", "/clock", "tick", 60_000, None))
                .await
                .unwrap()
                .is_none(),
            "capped"
        );
        assert!(
            mgr.schedule(Timer::new("b", "/clock", "tick", 60_000, None))
                .await
                .unwrap()
                .is_some(),
            "universe b's object has its own cap"
        );
        assert_eq!(stored(&pool).await, MAX_TIMERS_PER_OBJECT as i64 + 1);
    }

    /// Due call_outs come earliest first and are only listed (claiming one
    /// is the firing run's delete); the loop sleeps until the earliest
    /// deadline of either kind
    #[tokio::test]
    async fn test_due_and_next_deadline() {
        let (mgr, pool) = manager().await;
        assert_eq!(mgr.next_deadline().await.unwrap(), None);
        let now = chrono::Utc::now().timestamp_millis();
        for (id, at) in [
            ("late", now - 100),
            ("early", now - 500),
            ("future", now + 60_000),
        ] {
            sqlx::query(
                "INSERT INTO timers (id, universe_id, object_id, method, fire_at) VALUES (?, 'u', '/clock', 'm', ?)",
            )
            .bind(id)
            .bind(at)
            .execute(&pool)
            .await
            .unwrap();
        }
        let due: Vec<String> = mgr
            .due_call_outs(now, 10)
            .await
            .unwrap()
            .into_iter()
            .map(|t| t.id)
            .collect();
        assert_eq!(due, ["early", "late"]);
        assert_eq!(stored(&pool).await, 3, "listing claims nothing");
        assert_eq!(mgr.next_deadline().await.unwrap(), Some(now - 500));
        let (sql, params) = TimerManager::consume_statement("early");
        assert_eq!(mgr.write(sql, params.clone()).await.unwrap(), 1, "claimed");
        assert_eq!(mgr.write(sql, params).await.unwrap(), 0, "only once");
    }

    /// A call_out that could not be fired is retried after a wait that
    /// doubles per attempt (counted on its row), and dropped after
    /// MAX_FIRE_ATTEMPTS: no failure has to be classified as passing or not
    #[tokio::test]
    async fn test_retry_backs_off_then_drops() {
        let (mgr, pool) = manager().await;
        let id = mgr
            .schedule(Timer::new("u", "/clock", "tick", 0, None))
            .await
            .unwrap()
            .unwrap();
        let mut last_wait = 0;
        for attempt in 1..MAX_FIRE_ATTEMPTS {
            let timer = mgr.due_call_outs(i64::MAX, 1).await.unwrap().pop().unwrap();
            assert_eq!(timer.attempts, attempt - 1);
            let now = chrono::Utc::now().timestamp_millis();
            assert!(!mgr.retry_or_drop(&timer).await.unwrap(), "retried");
            let (attempts, fire_at): (i64, i64) =
                sqlx::query_as("SELECT attempts, fire_at FROM timers WHERE id = ?")
                    .bind(&id)
                    .fetch_one(&pool)
                    .await
                    .unwrap();
            assert_eq!(attempts, attempt);
            let wait = fire_at - now;
            assert!(
                wait > last_wait || wait >= RETRY_MAX_MS - 50,
                "{wait} after {last_wait}"
            );
            last_wait = wait;
        }
        let timer = mgr.due_call_outs(i64::MAX, 1).await.unwrap().pop().unwrap();
        assert!(mgr.retry_or_drop(&timer).await.unwrap(), "dropped at last");
        assert_eq!(stored(&pool).await, 0);
    }

    /// Heartbeats are per universe
    #[tokio::test]
    async fn test_heartbeats_are_per_universe() {
        let (mgr, _pool) = manager().await;
        mgr.set_heartbeat(HeartBeat::new("a", "/clock", 1000))
            .await
            .unwrap();
        mgr.set_heartbeat(HeartBeat::new("b", "/clock", 1000))
            .await
            .unwrap();
        assert_eq!(mgr.heartbeat_count().await, 2, "one heartbeat per universe");
        assert!(mgr.remove_heartbeat("b", "/clock").await);
        assert!(!mgr.remove_heartbeat("b", "/clock").await);
        assert_eq!(mgr.heartbeat_count().await, 1);
    }

    #[test]
    fn test_timer_creation() {
        let timer = Timer::new("universe1", "obj1", "on_timer", 1000, None);
        assert_eq!(timer.object_id, "obj1");
        assert_eq!(timer.method, "on_timer");
        assert!(!timer.is_due()); // Not due yet
    }

    #[test]
    fn test_timer_due() {
        let mut timer = Timer::new("universe1", "obj1", "on_timer", 0, None);
        timer.fire_at = chrono::Utc::now().timestamp_millis() - 100; // In the past
        assert!(timer.is_due());
    }

    #[test]
    fn test_heartbeat_creation() {
        let hb = HeartBeat::new("universe1", "obj1", 1000);
        assert_eq!(hb.object_id, "obj1");
        assert_eq!(hb.interval_ms, 1000);
        assert!(!hb.is_due()); // Just created, not due
    }

    /// Beats fall at started_at + k * interval: the next is the first such
    /// time after now (missed ones are skipped, never fired in a burst)
    #[test]
    fn test_heartbeat_phase() {
        let now = 1_000_000;
        assert_eq!(HeartBeat::next_after(now - 2_500, 1_000, now), now + 500);
        assert_eq!(
            HeartBeat::next_after(now - 3_000, 1_000, now),
            now + 1_000,
            "strictly after"
        );
        assert_eq!(
            HeartBeat::next_after(now + 300, 1_000, now),
            now + 300,
            "not started yet"
        );
        // Started in the future by a node whose clock ran ahead: no more
        // than one interval away here
        assert_eq!(
            HeartBeat::next_after(now + 7_200_000, 1_000, now),
            now + 1_000
        );
    }

    /// A restart (a new manager loading the table) keeps the schedule: the
    /// next beat is not pushed a whole interval away
    #[tokio::test]
    async fn test_heartbeat_phase_survives_restart() {
        let pool = crate::db::test_utils::test_pool().await;
        for sql in [
            "INSERT INTO accounts (id, username, password_hash, access_level, created_at)
             VALUES ('acct', 'acct', 'x', 'player', '2026-01-01')",
            "INSERT INTO universes (id, name, owner_id, config, created_at)
             VALUES ('u', 'u', 'acct', '{}', '2026-01-01')",
            "INSERT INTO objects (id, universe_id, class, properties, created_at, updated_at)
             VALUES ('/clock', 'u', 'item', '{}', '2026-01-01', '2026-01-01')",
        ] {
            sqlx::query(sql).execute(&pool).await.unwrap();
        }
        let day = MAX_HEARTBEAT_MS as i64;
        let started = chrono::Utc::now().timestamp_millis() - day + 60_000;
        sqlx::query("INSERT INTO heartbeats (universe_id, object_id, interval_ms, started_at) VALUES ('u', '/clock', ?, ?)")
            .bind(day)
            .bind(started)
            .execute(&pool)
            .await
            .unwrap();
        let mgr = TimerManager::new(Some(pool), None);
        mgr.load_from_db().await.unwrap();
        let next = mgr.next_deadline().await.unwrap().unwrap();
        assert_eq!(
            next,
            started + day,
            "a daily beat due in a minute stays due in a minute"
        );
    }

    /// Huge intervals and delays are clamped, never wrapped into the past
    /// (which would fire them in a tight loop)
    #[test]
    fn test_times_are_clamped() {
        let now = chrono::Utc::now().timestamp_millis();
        let hb = HeartBeat::new("u", "/o", u64::MAX);
        assert_eq!(hb.interval_ms, MAX_HEARTBEAT_MS);
        assert!(hb.next_due > now);
        let timer = Timer::new("u", "/o", "m", u64::MAX, None);
        assert!(timer.fire_at > now && timer.fire_at <= now + MAX_TIMER_DELAY_MS as i64 + 1_000);
        assert!(!timer.is_due());

        // A zero delay is at least the minimum: a handler that reschedules
        // itself cannot spin
        let soon = Timer::new("u", "/o", "m", 0, None);
        assert!(soon.fire_at >= now + MIN_TIMER_DELAY_MS as i64);
    }
}
