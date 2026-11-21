//! Credit system for in-game currency
//!
//! Provides:
//! - Balance management (get, deduct, grant)
//! - Transaction logging for auditing
//! - Persistence in SQLite

use std::collections::HashMap;
use std::sync::Arc;

use sqlx::SqlitePool;
use tokio::sync::RwLock;
use tracing::{debug, warn};

use crate::raft::RaftWriter;

/// Credit balance for a player in a universe
#[derive(Debug, Clone)]
pub struct CreditBalance {
    /// Universe ID
    pub universe_id: String,
    /// Player account ID
    pub account_id: String,
    /// Current balance
    pub balance: i64,
}

/// Transaction record for auditing
#[derive(Debug, Clone)]
pub struct CreditTransaction {
    /// Universe ID
    pub universe_id: String,
    /// Player account ID
    pub account_id: String,
    /// Amount (positive = credit, negative = debit)
    pub amount: i64,
    /// Reason for transaction
    pub reason: String,
    /// Timestamp
    pub timestamp: chrono::DateTime<chrono::Utc>,
}

/// Credit manager for handling in-game currency
pub struct CreditManager {
    /// In-memory cache of balances: (universe_id, account_id) -> balance
    balances: RwLock<HashMap<(String, String), i64>>,
    /// Database pool for persistence
    pool: Option<SqlitePool>,
    /// Raft writer for consensus
    raft_writer: Option<Arc<RaftWriter>>,
}

impl std::fmt::Debug for CreditManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CreditManager")
            .field("pool", &self.pool.is_some())
            .field("raft_writer", &self.raft_writer.is_some())
            .finish()
    }
}

impl CreditManager {
    /// Create a new credit manager
    pub fn new(pool: Option<SqlitePool>, raft_writer: Option<Arc<RaftWriter>>) -> Self {
        Self {
            balances: RwLock::new(HashMap::new()),
            pool,
            raft_writer,
        }
    }

    /// Create a shared instance
    pub fn shared(pool: Option<SqlitePool>, raft_writer: Option<Arc<RaftWriter>>) -> Arc<Self> {
        Arc::new(Self::new(pool, raft_writer))
    }

    /// Get balance for a player in a universe
    pub async fn get_balance(&self, universe_id: &str, account_id: &str) -> i64 {
        let key = (universe_id.to_string(), account_id.to_string());

        // Check cache first
        {
            let balances = self.balances.read().await;
            if let Some(&balance) = balances.get(&key) {
                return balance;
            }
        }

        // Cache miss: load under the write lock, so a concurrent grant or
        // deduct cannot be overwritten by the stale value read here
        let mut balances = self.balances.write().await;
        match self
            .cached_or_stored(&balances, universe_id, account_id)
            .await
        {
            Ok(balance) => {
                balances.insert(key, balance);
                balance
            }
            Err(e) => {
                // Report 0 but do not cache it, so the next read retries
                warn!("Failed to load credit balance: {}", e);
                0
            }
        }
    }

    /// Deduct credits from a player's balance
    /// Returns true if successful, false if insufficient funds
    pub async fn deduct(
        &self,
        universe_id: &str,
        account_id: &str,
        amount: i64,
        reason: &str,
    ) -> bool {
        if amount <= 0 {
            return false;
        }

        let key = (universe_id.to_string(), account_id.to_string());
        let mut balances = self.balances.write().await;

        let current = match self
            .cached_or_stored(&balances, universe_id, account_id)
            .await
        {
            Ok(balance) => balance,
            Err(e) => {
                warn!("Failed to load credit balance: {}", e);
                return false;
            }
        };
        if current < amount {
            debug!(
                "Insufficient credits: {} has {} but needs {}",
                account_id, current, amount
            );
            return false;
        }

        let new_balance = current - amount;
        if let Err(e) = self
            .save_balance(universe_id, account_id, new_balance)
            .await
        {
            warn!("Failed to persist credit balance: {}", e);
            return false;
        }
        balances.insert(key, new_balance);

        debug!(
            "Deducted {} credits from {}: {} -> {} ({})",
            amount, account_id, current, new_balance, reason
        );
        true
    }

    /// Grant credits to a player (admin function)
    /// Returns false for a non-positive amount, an overflowing balance, or a
    /// failed load/persist; the balance is unchanged in those cases.
    pub async fn grant(
        &self,
        universe_id: &str,
        account_id: &str,
        amount: i64,
        reason: &str,
    ) -> bool {
        if amount <= 0 {
            return false;
        }

        let key = (universe_id.to_string(), account_id.to_string());
        let mut balances = self.balances.write().await;

        let current = match self
            .cached_or_stored(&balances, universe_id, account_id)
            .await
        {
            Ok(balance) => balance,
            Err(e) => {
                warn!("Failed to load credit balance: {}", e);
                return false;
            }
        };
        let Some(new_balance) = current.checked_add(amount) else {
            warn!("Credit grant to {} would overflow", account_id);
            return false;
        };
        if let Err(e) = self
            .save_balance(universe_id, account_id, new_balance)
            .await
        {
            warn!("Failed to persist credit balance: {}", e);
            return false;
        }
        balances.insert(key, new_balance);

        debug!(
            "Granted {} credits to {}: {} -> {} ({})",
            amount, account_id, current, new_balance, reason
        );
        true
    }

    /// Set balance directly (used for initialization)
    pub async fn set_balance(&self, universe_id: &str, account_id: &str, balance: i64) {
        let key = (universe_id.to_string(), account_id.to_string());
        self.balances.write().await.insert(key, balance);

        if let Err(e) = self.save_balance(universe_id, account_id, balance).await {
            warn!("Failed to persist credit balance: {}", e);
        }
    }

    /// Load balance from database
    async fn load_balance(
        &self,
        universe_id: &str,
        account_id: &str,
        pool: &SqlitePool,
    ) -> anyhow::Result<i64> {
        let row: Option<(i64,)> =
            sqlx::query_as("SELECT balance FROM credits WHERE universe_id = ? AND player_id = ?")
                .bind(universe_id)
                .bind(account_id)
                .fetch_optional(pool)
                .await?;

        Ok(row.map(|(b,)| b).unwrap_or(0))
    }

    /// Current balance: the cached value, or the stored one on a cache miss
    /// (e.g. after a restart). Callers hold the write lock across the
    /// read-modify-write, so the DB read cannot race another update.
    /// A failed DB read is an error, never a zero balance.
    async fn cached_or_stored(
        &self,
        balances: &HashMap<(String, String), i64>,
        universe_id: &str,
        account_id: &str,
    ) -> anyhow::Result<i64> {
        let key = (universe_id.to_string(), account_id.to_string());
        if let Some(&balance) = balances.get(&key) {
            return Ok(balance);
        }
        match self.pool {
            Some(ref pool) => self.load_balance(universe_id, account_id, pool).await,
            None => Ok(0),
        }
    }

    /// Persist a balance: via Raft when available, otherwise direct SQL
    async fn save_balance(
        &self,
        universe_id: &str,
        account_id: &str,
        balance: i64,
    ) -> anyhow::Result<()> {
        const UPSERT: &str = "INSERT INTO credits (id, universe_id, player_id, balance) VALUES (?, ?, ?, ?) ON CONFLICT(universe_id, player_id) DO UPDATE SET balance = ?";
        // Pre-compute UUID for deterministic replication
        let id = uuid::Uuid::new_v4().to_string();

        if let Some(ref raft_writer) = self.raft_writer {
            raft_writer
                .execute(
                    UPSERT,
                    vec![
                        serde_json::json!(&id),
                        serde_json::json!(universe_id),
                        serde_json::json!(account_id),
                        serde_json::json!(balance),
                        serde_json::json!(balance),
                    ],
                )
                .await?;
        } else if let Some(ref pool) = self.pool {
            sqlx::query(UPSERT)
                .bind(&id)
                .bind(universe_id)
                .bind(account_id)
                .bind(balance)
                .bind(balance)
                .execute(pool)
                .await?;
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Single-connection in-memory DB with a credits table holding one balance
    async fn pool_with_balance(balance: i64) -> SqlitePool {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::query(
            "CREATE TABLE credits (id TEXT PRIMARY KEY, universe_id TEXT NOT NULL,
             player_id TEXT NOT NULL, balance INTEGER NOT NULL DEFAULT 0,
             UNIQUE(universe_id, player_id))",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query("INSERT INTO credits VALUES ('c1', 'u1', 'player1', ?)")
            .bind(balance)
            .execute(&pool)
            .await
            .unwrap();
        pool
    }

    /// After a restart the cache is empty: grant must add to the stored
    /// balance, not overwrite it, and the result must persist.
    #[tokio::test]
    async fn test_grant_on_cold_cache_adds_to_stored_balance() {
        let pool = pool_with_balance(100).await;
        let manager = CreditManager::new(Some(pool.clone()), None);
        manager.grant("u1", "player1", 10, "test").await;
        assert_eq!(manager.get_balance("u1", "player1").await, 110);

        let restarted = CreditManager::new(Some(pool), None);
        assert_eq!(restarted.get_balance("u1", "player1").await, 110);
    }

    /// A failed DB read must not be treated as a zero balance (and written
    /// back over the real one)
    #[tokio::test]
    async fn test_failed_load_does_not_clobber_balance() {
        // No credits table: every load fails
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        let manager = CreditManager::new(Some(pool), None);
        assert!(!manager.grant("u1", "player1", 10, "test").await);
        assert!(!manager.deduct("u1", "player1", 1, "test").await);
        // Nothing cached from the failed reads
        assert!(manager.balances.read().await.is_empty());
    }

    #[tokio::test]
    async fn test_grant_rejects_invalid_and_overflowing_amounts() {
        let manager = CreditManager::new(None, None);
        assert!(!manager.grant("u1", "player1", 0, "test").await);
        assert!(!manager.grant("u1", "player1", -5, "test").await);
        assert!(manager.grant("u1", "player1", i64::MAX, "test").await);
        assert!(!manager.grant("u1", "player1", 1, "overflow").await);
        assert_eq!(manager.get_balance("u1", "player1").await, i64::MAX);
    }

    /// Deduct must see the stored balance on a cold cache
    #[tokio::test]
    async fn test_deduct_on_cold_cache_uses_stored_balance() {
        let pool = pool_with_balance(100).await;
        let manager = CreditManager::new(Some(pool.clone()), None);
        assert!(manager.deduct("u1", "player1", 30, "test").await);
        assert_eq!(manager.get_balance("u1", "player1").await, 70);

        let restarted = CreditManager::new(Some(pool), None);
        assert_eq!(restarted.get_balance("u1", "player1").await, 70);
    }

    #[tokio::test]
    async fn test_get_balance_default() {
        let manager = CreditManager::new(None, None);
        let balance = manager.get_balance("u1", "player1").await;
        assert_eq!(balance, 0);
    }

    #[tokio::test]
    async fn test_set_balance() {
        let manager = CreditManager::new(None, None);
        manager.set_balance("u1", "player1", 100).await;
        let balance = manager.get_balance("u1", "player1").await;
        assert_eq!(balance, 100);
    }

    #[tokio::test]
    async fn test_grant() {
        let manager = CreditManager::new(None, None);
        manager.grant("u1", "player1", 50, "admin grant").await;
        let balance = manager.get_balance("u1", "player1").await;
        assert_eq!(balance, 50);

        manager.grant("u1", "player1", 30, "bonus").await;
        let balance = manager.get_balance("u1", "player1").await;
        assert_eq!(balance, 80);
    }

    #[tokio::test]
    async fn test_deduct_success() {
        let manager = CreditManager::new(None, None);
        manager.set_balance("u1", "player1", 100).await;

        let result = manager.deduct("u1", "player1", 30, "purchase").await;
        assert!(result);

        let balance = manager.get_balance("u1", "player1").await;
        assert_eq!(balance, 70);
    }

    #[tokio::test]
    async fn test_deduct_insufficient() {
        let manager = CreditManager::new(None, None);
        manager.set_balance("u1", "player1", 20).await;

        let result = manager.deduct("u1", "player1", 50, "purchase").await;
        assert!(!result);

        // Balance unchanged
        let balance = manager.get_balance("u1", "player1").await;
        assert_eq!(balance, 20);
    }

    #[tokio::test]
    async fn test_deduct_invalid_amount() {
        let manager = CreditManager::new(None, None);
        manager.set_balance("u1", "player1", 100).await;

        // Zero amount
        let result = manager.deduct("u1", "player1", 0, "test").await;
        assert!(!result);

        // Negative amount
        let result = manager.deduct("u1", "player1", -10, "test").await;
        assert!(!result);

        // Balance unchanged
        let balance = manager.get_balance("u1", "player1").await;
        assert_eq!(balance, 100);
    }

    #[tokio::test]
    async fn test_multiple_universes() {
        let manager = CreditManager::new(None, None);

        manager.set_balance("u1", "player1", 100).await;
        manager.set_balance("u2", "player1", 200).await;

        assert_eq!(manager.get_balance("u1", "player1").await, 100);
        assert_eq!(manager.get_balance("u2", "player1").await, 200);
    }
}
