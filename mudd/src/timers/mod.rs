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
    /// When the next beat is due (Unix ms; not stored: after a restart the
    /// next beat is one interval away)
    pub next_due: i64,
    /// Method to call (defaults to "heart_beat")
    pub method: String,
}

impl HeartBeat {
    /// Create a new heartbeat
    pub fn new(universe_id: &str, object_id: &str, interval_ms: u64) -> Self {
        let interval_ms = interval_ms.clamp(MIN_HEARTBEAT_MS, MAX_HEARTBEAT_MS);
        Self {
            object_id: object_id.to_string(),
            universe_id: universe_id.to_string(),
            interval_ms,
            next_due: chrono::Utc::now()
                .timestamp_millis()
                .saturating_add(interval_ms as i64),
            method: "heart_beat".to_string(),
        }
    }

    /// Check if heartbeat is due to fire
    pub fn is_due(&self) -> bool {
        chrono::Utc::now().timestamp_millis() >= self.next_due
    }

    /// After a beat: the next is one interval from now (a late server does
    /// not fire a burst of missed beats)
    pub fn reset(&mut self) {
        self.interval_ms = self.interval_ms.clamp(MIN_HEARTBEAT_MS, MAX_HEARTBEAT_MS);
        self.next_due = chrono::Utc::now()
            .timestamp_millis()
            .saturating_add(self.interval_ms as i64);
    }
}

/// Result of firing a timer
#[derive(Debug, Clone)]
pub struct TimerFired {
    /// The call_out's id (None for a heartbeat beat)
    pub timer_id: Option<String>,
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

/// Timer manager for a universe
pub struct TimerManager {
    /// One-shot timers (in memory)
    timers: RwLock<BTreeMap<String, Timer>>,
    /// Heartbeats keyed by (universe, object): a cache of the heartbeats
    /// table
    heartbeats: RwLock<BTreeMap<(String, String), HeartBeat>>,
    /// Database pool for persistence
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
            timers: RwLock::new(BTreeMap::new()),
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

    /// Run one write: through Raft when available, else directly (tests)
    async fn write(&self, sql: &'static str, params: Vec<serde_json::Value>) -> anyhow::Result<()> {
        if let Some(ref raft_writer) = self.raft_writer {
            raft_writer.execute(sql, params).await?;
        } else if let Some(ref pool) = self.pool {
            let mut query = sqlx::query(sql);
            for param in params {
                query = match param {
                    serde_json::Value::String(v) => query.bind(v),
                    serde_json::Value::Number(n) => query.bind(n.as_i64()),
                    serde_json::Value::Null => query.bind(Option::<String>::None),
                    other => query.bind(other.to_string()),
                };
            }
            query.execute(pool).await?;
        }
        Ok(())
    }

    /// Add a one-shot timer: stored first, cached only once stored
    pub async fn add_timer(&self, timer: Timer) -> anyhow::Result<String> {
        self.write(
            "INSERT OR REPLACE INTO timers (id, universe_id, object_id, method, fire_at, args) VALUES (?, ?, ?, ?, ?, ?)",
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
        let id = timer.id.clone();
        self.timers.write().await.insert(id.clone(), timer);
        self.changed.notify_one();
        Ok(id)
    }

    /// Add a one-shot timer unless its object already has
    /// [`MAX_TIMERS_PER_OBJECT`] pending. Returns the timer id if added.
    pub async fn try_add_timer(&self, timer: Timer) -> Option<String> {
        let pending = self
            .timers
            .read()
            .await
            .values()
            .filter(|t| t.universe_id == timer.universe_id && t.object_id == timer.object_id)
            .count();
        if pending >= MAX_TIMERS_PER_OBJECT {
            warn!(
                "Object {} already has {} pending timers; refusing call_out",
                timer.object_id, pending
            );
            return None;
        }
        match self.add_timer(timer).await {
            Ok(id) => Some(id),
            Err(e) => {
                warn!("Failed to store timer: {}", e);
                None
            }
        }
    }

    /// Remove a timer only if it belongs to `object_id` in `universe_id`
    /// (game.remove_call_out: an object may cancel only its own timers)
    pub async fn remove_timer_owned(
        &self,
        timer_id: &str,
        universe_id: &str,
        object_id: &str,
    ) -> bool {
        let owned = self
            .timers
            .read()
            .await
            .get(timer_id)
            .is_some_and(|t| t.universe_id == universe_id && t.object_id == object_id);
        owned && self.remove_timer(timer_id).await
    }

    /// Forget a settled call_out (cache only: its row is already deleted)
    pub async fn forget_timer(&self, timer_id: &str) {
        self.timers.write().await.remove(timer_id);
    }

    /// Try a call_out again in `delay_ms` (cache only: the stored fire_at
    /// is unchanged, so a restart fires it at once)
    pub async fn retry_later(&self, timer_id: &str, delay_ms: i64) {
        if let Some(t) = self.timers.write().await.get_mut(timer_id) {
            t.fire_at = chrono::Utc::now()
                .timestamp_millis()
                .saturating_add(delay_ms);
        }
    }

    /// Whether the call_out's row exists (the truth: a cached call_out
    /// whose row is gone was cancelled or consumed already)
    pub async fn is_stored(&self, timer_id: &str) -> anyhow::Result<bool> {
        let Some(ref pool) = self.pool else {
            return Ok(true);
        };
        let found: Option<i64> = sqlx::query_scalar("SELECT 1 FROM timers WHERE id = ?")
            .bind(timer_id)
            .fetch_optional(&mut *crate::db::run::reader(pool).await?)
            .await?;
        Ok(found.is_some())
    }

    /// The statement that consumes a fired call_out's row
    pub fn consume_statement(timer_id: &str) -> (&'static str, Vec<serde_json::Value>) {
        (
            "DELETE FROM timers WHERE id = ?",
            vec![serde_json::json!(timer_id)],
        )
    }

    /// Remove a timer by ID
    pub async fn remove_timer(&self, timer_id: &str) -> bool {
        // Remove from DB via Raft if available, otherwise direct SQL
        if let Some(ref raft_writer) = self.raft_writer {
            if let Err(e) = self.delete_timer_db(timer_id, raft_writer).await {
                warn!("Failed to delete timer from DB: {}", e);
            }
        } else if let Some(ref pool) = self.pool {
            // Direct SQL fallback (for tests)
            if let Err(e) = sqlx::query("DELETE FROM timers WHERE id = ?")
                .bind(timer_id)
                .execute(pool)
                .await
            {
                warn!("Failed to delete timer from DB: {}", e);
            }
        }

        self.timers.write().await.remove(timer_id).is_some()
    }

    /// Remove all timers for an object in a universe
    pub async fn remove_timers_for_object(&self, universe_id: &str, object_id: &str) {
        let mut timers = self.timers.write().await;
        let ids_to_remove: Vec<String> = timers
            .values()
            .filter(|t| t.universe_id == universe_id && t.object_id == object_id)
            .map(|t| t.id.clone())
            .collect();

        for id in &ids_to_remove {
            timers.remove(id);
            if let Some(ref raft_writer) = self.raft_writer {
                if let Err(e) = self.delete_timer_db(id, raft_writer).await {
                    warn!("Failed to delete timer from DB: {}", e);
                }
            } else if let Some(ref pool) = self.pool {
                // Direct SQL fallback (for tests)
                if let Err(e) = sqlx::query("DELETE FROM timers WHERE id = ?")
                    .bind(id)
                    .execute(pool)
                    .await
                {
                    warn!("Failed to delete timer from DB: {}", e);
                }
            }
        }
    }

    /// Set a heartbeat for an object (interval clamped to
    /// [`MIN_HEARTBEAT_MS`]): stored first (one write; beats themselves
    /// write nothing), cached only once stored
    pub async fn set_heartbeat(&self, mut heartbeat: HeartBeat) -> anyhow::Result<()> {
        heartbeat.reset(); // clamps the interval too
        self.write(
            "INSERT INTO heartbeats (universe_id, object_id, interval_ms) VALUES (?, ?, ?)
             ON CONFLICT (universe_id, object_id) DO UPDATE SET interval_ms = excluded.interval_ms",
            vec![
                serde_json::json!(&heartbeat.universe_id),
                serde_json::json!(&heartbeat.object_id),
                serde_json::json!(heartbeat.interval_ms),
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
    pub async fn next_deadline(&self) -> Option<i64> {
        let timers = self.timers.read().await.values().map(|t| t.fire_at).min();
        let beats = self
            .heartbeats
            .read()
            .await
            .values()
            .map(|h| h.next_due)
            .min();
        timers.into_iter().chain(beats).min()
    }

    /// Resolves when a timer or heartbeat is added (it may be due sooner)
    pub async fn changed(&self) {
        self.changed.notified().await
    }

    /// Due call_outs (earliest first) and heartbeats. A call_out is only
    /// reported: the timer loop deletes its row in the same change as its
    /// handler's writes and then forgets it ([`Self::forget_timer`]), so it
    /// is never lost between the two. Heartbeats are re-armed here (beats
    /// write nothing).
    pub async fn tick(&self) -> Vec<TimerFired> {
        let mut fired = Vec::new();

        let mut due_timers: Vec<Timer> = {
            let timers = self.timers.read().await;
            timers.values().filter(|t| t.is_due()).cloned().collect()
        };
        due_timers.sort_by_key(|t| t.fire_at);
        for timer in due_timers {
            fired.push(TimerFired {
                timer_id: Some(timer.id),
                object_id: timer.object_id,
                universe_id: timer.universe_id,
                method: timer.method,
                args: timer.args,
            });
        }

        // Check heartbeats
        {
            let mut heartbeats = self.heartbeats.write().await;
            for hb in heartbeats.values_mut() {
                if hb.is_due() {
                    fired.push(TimerFired {
                        timer_id: None,
                        object_id: hb.object_id.clone(),
                        universe_id: hb.universe_id.clone(),
                        method: hb.method.clone(),
                        args: None,
                    });
                    hb.reset();
                }
            }
        }

        fired
    }

    /// Load timers from database on startup
    pub async fn load_from_db(&self) -> anyhow::Result<()> {
        let Some(ref pool) = self.pool else {
            return Ok(());
        };

        let rows: Vec<(String, String, String, String, i64, Option<String>)> =
            sqlx::query_as("SELECT id, universe_id, object_id, method, fire_at, args FROM timers")
                .fetch_all(&mut *crate::db::run::reader(pool).await?)
                .await?;

        // Replaces the cache: also used to rebuild it on becoming primary
        let mut timers = self.timers.write().await;
        timers.clear();
        for (id, universe_id, object_id, method, fire_at, args) in rows {
            let timer = Timer {
                id: id.clone(),
                universe_id,
                object_id,
                method,
                fire_at,
                args,
            };
            timers.insert(id, timer);
        }

        debug!("Loaded {} timers from database", timers.len());
        drop(timers);

        // Heartbeats: the next beat is one interval from now
        let rows: Vec<(String, String, i64)> =
            sqlx::query_as("SELECT universe_id, object_id, interval_ms FROM heartbeats")
                .fetch_all(&mut *crate::db::run::reader(pool).await?)
                .await?;
        // A heartbeat already scheduled keeps its next beat (rebuilds after
        // a failed run must not shift every heartbeat)
        let mut heartbeats = self.heartbeats.write().await;
        let mut old = std::mem::take(&mut *heartbeats);
        for (universe_id, object_id, interval_ms) in rows {
            let interval_ms = interval_ms.max(0) as u64;
            let key = (universe_id, object_id);
            let hb = match old.remove(&key) {
                Some(hb) if hb.interval_ms == interval_ms => hb,
                _ => HeartBeat::new(&key.0, &key.1, interval_ms),
            };
            heartbeats.insert(key, hb);
        }
        debug!("Loaded {} heartbeats from database", heartbeats.len());
        drop(heartbeats);
        self.changed.notify_one();
        Ok(())
    }

    /// Delete a timer from database via Raft
    async fn delete_timer_db(
        &self,
        timer_id: &str,
        raft_writer: &RaftWriter,
    ) -> anyhow::Result<()> {
        raft_writer
            .execute(
                "DELETE FROM timers WHERE id = ?",
                vec![serde_json::json!(timer_id)],
            )
            .await?;
        Ok(())
    }

    /// Get count of active timers
    pub async fn timer_count(&self) -> usize {
        self.timers.read().await.len()
    }

    /// Get count of active heartbeats
    pub async fn heartbeat_count(&self) -> usize {
        self.heartbeats.read().await.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A timer can be cancelled only through its own object (and universe)
    #[tokio::test]
    async fn test_remove_timer_owned() {
        let mgr = TimerManager::new(None, None);
        let id = mgr
            .try_add_timer(Timer::new("a", "/clock", "tick", 60_000, None))
            .await
            .unwrap();
        assert!(!mgr.remove_timer_owned(&id, "a", "/other").await);
        assert!(!mgr.remove_timer_owned(&id, "b", "/clock").await);
        assert_eq!(mgr.timer_count().await, 1);
        assert!(mgr.remove_timer_owned(&id, "a", "/clock").await);
        assert_eq!(mgr.timer_count().await, 0);
    }

    /// The same object id in two universes has independent timers and
    /// heartbeats
    #[tokio::test]
    async fn test_timers_are_per_universe() {
        let mgr = TimerManager::new(None, None);
        mgr.set_heartbeat(HeartBeat::new("a", "/clock", 1000))
            .await
            .unwrap();
        mgr.set_heartbeat(HeartBeat::new("b", "/clock", 1000))
            .await
            .unwrap();
        assert_eq!(mgr.heartbeat_count().await, 2, "one heartbeat per universe");

        for _ in 0..MAX_TIMERS_PER_OBJECT {
            assert!(
                mgr.try_add_timer(Timer::new("a", "/clock", "tick", 60_000, None))
                    .await
                    .is_some()
            );
        }
        // Universe b's object has its own cap
        assert!(
            mgr.try_add_timer(Timer::new("b", "/clock", "tick", 60_000, None))
                .await
                .is_some()
        );

        // Removing b's object's timers and heartbeat leaves a's alone
        mgr.remove_timers_for_object("b", "/clock").await;
        assert!(mgr.remove_heartbeat("b", "/clock").await);
        assert_eq!(mgr.timer_count().await, MAX_TIMERS_PER_OBJECT);
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

    #[tokio::test]
    async fn test_timer_manager_add_remove() {
        let manager = TimerManager::new(None, None);

        let timer = Timer::new("u1", "obj1", "test_method", 10000, None);
        let id = manager.add_timer(timer).await.unwrap();

        assert_eq!(manager.timer_count().await, 1);

        let removed = manager.remove_timer(&id).await;
        assert!(removed);
        assert_eq!(manager.timer_count().await, 0);
    }

    #[tokio::test]
    async fn test_heartbeat_add_remove() {
        let manager = TimerManager::new(None, None);

        let hb = HeartBeat::new("u1", "obj1", 100);
        manager.set_heartbeat(hb).await.unwrap();

        assert_eq!(manager.heartbeat_count().await, 1);

        let removed = manager.remove_heartbeat("u1", "obj1").await;
        assert!(removed);
        assert_eq!(manager.heartbeat_count().await, 0);
    }

    #[tokio::test]
    async fn test_tick_fires_due_timers() {
        let manager = TimerManager::new(None, None);

        // Create a timer that's already due
        let mut timer = Timer::new("u1", "obj1", "on_fire", 0, Some("test_arg".to_string()));
        timer.fire_at = chrono::Utc::now().timestamp_millis() - 100;
        manager.add_timer(timer).await.unwrap();

        let fired = manager.tick().await;
        assert_eq!(fired.len(), 1);
        assert_eq!(fired[0].object_id, "obj1");
        assert_eq!(fired[0].method, "on_fire");
        assert_eq!(fired[0].args, Some("test_arg".to_string()));

        // Reported, not removed: the timer loop forgets it once settled
        assert_eq!(manager.timer_count().await, 1);
        let id = fired[0].timer_id.clone().unwrap();
        manager.retry_later(&id, 60_000).await;
        assert!(manager.tick().await.is_empty(), "retried later, not now");
        manager.forget_timer(&id).await;
        assert_eq!(manager.timer_count().await, 0);
    }

    /// tick() reports a due call_out without consuming it: the row stays
    /// until the timer loop deletes it in the same batch as the handler's
    /// writes, so a crash or failover in between loses nothing
    #[tokio::test]
    async fn test_tick_does_not_consume_call_outs() {
        let pool = crate::db::test_utils::test_pool().await;
        for sql in [
            "INSERT INTO accounts (id, username, password_hash, access_level, created_at)
             VALUES ('acct', 'acct', 'x', 'player', '2026-01-01')",
            "INSERT INTO universes (id, name, owner_id, config, created_at)
             VALUES ('u1', 'u1', 'acct', '{}', '2026-01-01')",
            "INSERT INTO objects (id, universe_id, class, properties, created_at, updated_at)
             VALUES ('obj1', 'u1', 'item', '{}', '2026-01-01', '2026-01-01')",
        ] {
            sqlx::query(sql).execute(&pool).await.unwrap();
        }
        let manager = TimerManager::new(Some(pool.clone()), None);
        let mut timer = Timer::new("u1", "obj1", "on_fire", 0, None);
        timer.fire_at = chrono::Utc::now().timestamp_millis() - 100;
        let id = manager.add_timer(timer).await.unwrap();

        let fired = manager.tick().await;
        assert_eq!(fired.len(), 1);
        assert_eq!(fired[0].timer_id.as_deref(), Some(id.as_str()));
        let rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM timers")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(
            rows, 1,
            "the stored call_out is consumed only with its handler"
        );
    }

    #[tokio::test]
    async fn test_remove_timers_for_object() {
        let manager = TimerManager::new(None, None);

        manager
            .add_timer(Timer::new("u1", "obj1", "m1", 10000, None))
            .await
            .unwrap();
        manager
            .add_timer(Timer::new("u1", "obj1", "m2", 10000, None))
            .await
            .unwrap();
        manager
            .add_timer(Timer::new("u1", "obj2", "m1", 10000, None))
            .await
            .unwrap();

        assert_eq!(manager.timer_count().await, 3);

        manager.remove_timers_for_object("u1", "obj1").await;
        assert_eq!(manager.timer_count().await, 1); // Only obj2's timer remains
    }

    /// The loop sleeps until the earliest deadline of either kind
    #[tokio::test]
    async fn test_next_deadline() {
        let mgr = TimerManager::new(None, None);
        assert_eq!(mgr.next_deadline().await, None);
        let id = mgr
            .try_add_timer(Timer::new("u", "/a", "m", 60_000, None))
            .await
            .unwrap();
        let fire_at = mgr.timers.read().await[&id].fire_at;
        assert_eq!(mgr.next_deadline().await, Some(fire_at));
        mgr.set_heartbeat(HeartBeat::new("u", "/b", 1_000))
            .await
            .unwrap();
        let beat = mgr.next_deadline().await.unwrap();
        assert!(beat < fire_at, "the heartbeat is due first");
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
