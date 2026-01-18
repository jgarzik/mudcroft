//! Timer and delayed execution system
//!
//! Provides:
//! - call_out: One-shot delayed callbacks
//! - heartbeat: Periodic callbacks for NPCs/objects
//! - Persistence: Timers survive server restart

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

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
            fire_at: now + delay_ms as i64,
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
    /// Last time the heartbeat fired
    pub last_fired: Instant,
    /// Method to call (defaults to "heart_beat")
    pub method: String,
}

impl HeartBeat {
    /// Create a new heartbeat
    pub fn new(universe_id: &str, object_id: &str, interval_ms: u64) -> Self {
        Self {
            object_id: object_id.to_string(),
            universe_id: universe_id.to_string(),
            interval_ms,
            last_fired: Instant::now(),
            method: "heart_beat".to_string(),
        }
    }

    /// Check if heartbeat is due to fire
    pub fn is_due(&self) -> bool {
        self.last_fired.elapsed() >= Duration::from_millis(self.interval_ms)
    }

    /// Reset the heartbeat timer after firing
    pub fn reset(&mut self) {
        self.last_fired = Instant::now();
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

/// Timer manager for a universe
pub struct TimerManager {
    /// One-shot timers (in memory)
    timers: RwLock<HashMap<String, Timer>>,
    /// Heartbeats (in memory only, not persisted)
    heartbeats: RwLock<HashMap<String, HeartBeat>>,
    /// Database pool for persistence
    pool: Option<SqlitePool>,
    /// Raft writer for consensus
    raft_writer: Option<Arc<RaftWriter>>,
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
            timers: RwLock::new(HashMap::new()),
            heartbeats: RwLock::new(HashMap::new()),
            pool,
            raft_writer,
        }
    }

    /// Create a shared instance
    pub fn shared(pool: Option<SqlitePool>, raft_writer: Option<Arc<RaftWriter>>) -> Arc<Self> {
        Arc::new(Self::new(pool, raft_writer))
    }

    /// Add a one-shot timer
    pub async fn add_timer(&self, timer: Timer) -> String {
        let id = timer.id.clone();

        // Persist via Raft if available, otherwise direct SQL
        if let Some(ref raft_writer) = self.raft_writer {
            if let Err(e) = self.persist_timer(&timer, raft_writer).await {
                warn!("Failed to persist timer: {}", e);
            }
        } else if let Some(ref pool) = self.pool {
            // Direct SQL fallback (for tests)
            if let Err(e) = sqlx::query(
                "INSERT OR REPLACE INTO timers (id, universe_id, object_id, method, fire_at, args) VALUES (?, ?, ?, ?, ?, ?)",
            )
            .bind(&timer.id)
            .bind(&timer.universe_id)
            .bind(&timer.object_id)
            .bind(&timer.method)
            .bind(timer.fire_at)
            .bind(&timer.args)
            .execute(pool)
            .await
            {
                warn!("Failed to persist timer: {}", e);
            }
        }

        self.timers.write().await.insert(id.clone(), timer);
        id
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

    /// Remove all timers for an object
    pub async fn remove_timers_for_object(&self, object_id: &str) {
        let mut timers = self.timers.write().await;
        let ids_to_remove: Vec<String> = timers
            .values()
            .filter(|t| t.object_id == object_id)
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

    /// Set a heartbeat for an object
    pub async fn set_heartbeat(&self, heartbeat: HeartBeat) {
        self.heartbeats
            .write()
            .await
            .insert(heartbeat.object_id.clone(), heartbeat);
    }

    /// Remove heartbeat for an object
    pub async fn remove_heartbeat(&self, object_id: &str) -> bool {
        self.heartbeats.write().await.remove(object_id).is_some()
    }

    /// Check for due timers and heartbeats, returning fired callbacks
    pub async fn tick(&self) -> Vec<TimerFired> {
        let mut fired = Vec::new();

        // Check one-shot timers
        let due_timers: Vec<Timer> = {
            let timers = self.timers.read().await;
            timers.values().filter(|t| t.is_due()).cloned().collect()
        };

        for timer in due_timers {
            fired.push(TimerFired {
                object_id: timer.object_id.clone(),
                universe_id: timer.universe_id.clone(),
                method: timer.method.clone(),
                args: timer.args.clone(),
            });

            // Remove fired timer
            self.remove_timer(&timer.id).await;
        }

        // Check heartbeats
        {
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
                .fetch_all(pool)
                .await?;

        let mut timers = self.timers.write().await;
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
        Ok(())
    }

    /// Persist a timer to database via Raft
    async fn persist_timer(&self, timer: &Timer, raft_writer: &RaftWriter) -> anyhow::Result<()> {
        raft_writer
            .execute(
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
        let id = manager.add_timer(timer).await;

        assert_eq!(manager.timer_count().await, 1);

        let removed = manager.remove_timer(&id).await;
        assert!(removed);
        assert_eq!(manager.timer_count().await, 0);
    }

    #[tokio::test]
    async fn test_heartbeat_add_remove() {
        let manager = TimerManager::new(None, None);

        let hb = HeartBeat::new("u1", "obj1", 100);
        manager.set_heartbeat(hb).await;

        assert_eq!(manager.heartbeat_count().await, 1);

        let removed = manager.remove_heartbeat("obj1").await;
        assert!(removed);
        assert_eq!(manager.heartbeat_count().await, 0);
    }

    #[tokio::test]
    async fn test_tick_fires_due_timers() {
        let manager = TimerManager::new(None, None);

        // Create a timer that's already due
        let mut timer = Timer::new("u1", "obj1", "on_fire", 0, Some("test_arg".to_string()));
        timer.fire_at = chrono::Utc::now().timestamp_millis() - 100;
        manager.add_timer(timer).await;

        let fired = manager.tick().await;
        assert_eq!(fired.len(), 1);
        assert_eq!(fired[0].object_id, "obj1");
        assert_eq!(fired[0].method, "on_fire");
        assert_eq!(fired[0].args, Some("test_arg".to_string()));

        // Timer should be removed after firing
        assert_eq!(manager.timer_count().await, 0);
    }

    #[tokio::test]
    async fn test_remove_timers_for_object() {
        let manager = TimerManager::new(None, None);

        manager
            .add_timer(Timer::new("u1", "obj1", "m1", 10000, None))
            .await;
        manager
            .add_timer(Timer::new("u1", "obj1", "m2", 10000, None))
            .await;
        manager
            .add_timer(Timer::new("u1", "obj2", "m1", 10000, None))
            .await;

        assert_eq!(manager.timer_count().await, 3);

        manager.remove_timers_for_object("obj1").await;
        assert_eq!(manager.timer_count().await, 1); // Only obj2's timer remains
    }
}
