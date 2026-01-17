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
#[derive(Debug)]
pub struct CreditManager {
    /// In-memory cache of balances: (universe_id, account_id) -> balance
    balances: RwLock<HashMap<(String, String), i64>>,
    /// Database pool for persistence
    pool: Option<SqlitePool>,
}

impl CreditManager {
    /// Create a new credit manager
    pub fn new(pool: Option<SqlitePool>) -> Self {
        Self {
            balances: RwLock::new(HashMap::new()),
            pool,
        }
    }

    /// Create a shared instance
    pub fn shared(pool: Option<SqlitePool>) -> Arc<Self> {
        Arc::new(Self::new(pool))
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

        // Load from DB if available
        if let Some(ref pool) = self.pool {
            if let Ok(balance) = self.load_balance(universe_id, account_id, pool).await {
                let mut balances = self.balances.write().await;
                balances.insert(key, balance);
                return balance;
            }
        }

        // Default to 0 if not found
        0
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

        // Get current balance
        let current = *balances.get(&key).unwrap_or(&0);
        if current < amount {
            debug!(
                "Insufficient credits: {} has {} but needs {}",
                account_id, current, amount
            );
            return false;
        }

        // Deduct
        let new_balance = current - amount;
        balances.insert(key, new_balance);

        // Persist to DB
        if let Some(ref pool) = self.pool {
            if let Err(e) = self
                .save_balance(universe_id, account_id, new_balance, pool)
                .await
            {
                warn!("Failed to persist credit balance: {}", e);
            }
            self.log_transaction(universe_id, account_id, -amount, reason, pool)
                .await;
        }

        debug!(
            "Deducted {} credits from {}: {} -> {} ({})",
            amount, account_id, current, new_balance, reason
        );
        true
    }

    /// Grant credits to a player (admin function)
    pub async fn grant(&self, universe_id: &str, account_id: &str, amount: i64, reason: &str) {
        if amount <= 0 {
            return;
        }

        let key = (universe_id.to_string(), account_id.to_string());
        let mut balances = self.balances.write().await;

        // Get current balance
        let current = *balances.get(&key).unwrap_or(&0);
        let new_balance = current + amount;
        balances.insert(key, new_balance);

        // Persist to DB
        if let Some(ref pool) = self.pool {
            if let Err(e) = self
                .save_balance(universe_id, account_id, new_balance, pool)
                .await
            {
                warn!("Failed to persist credit balance: {}", e);
            }
            self.log_transaction(universe_id, account_id, amount, reason, pool)
                .await;
        }

        debug!(
            "Granted {} credits to {}: {} -> {} ({})",
            amount, account_id, current, new_balance, reason
        );
    }

    /// Set balance directly (used for initialization)
    pub async fn set_balance(&self, universe_id: &str, account_id: &str, balance: i64) {
        let key = (universe_id.to_string(), account_id.to_string());
        self.balances.write().await.insert(key, balance);

        if let Some(ref pool) = self.pool {
            if let Err(e) = self
                .save_balance(universe_id, account_id, balance, pool)
                .await
            {
                warn!("Failed to persist credit balance: {}", e);
            }
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

    /// Save balance to database
    async fn save_balance(
        &self,
        universe_id: &str,
        account_id: &str,
        balance: i64,
        pool: &SqlitePool,
    ) -> anyhow::Result<()> {
        sqlx::query(
            r#"
            INSERT INTO credits (id, universe_id, player_id, balance)
            VALUES (?, ?, ?, ?)
            ON CONFLICT(universe_id, player_id) DO UPDATE SET balance = ?
            "#,
        )
        .bind(uuid::Uuid::new_v4().to_string())
        .bind(universe_id)
        .bind(account_id)
        .bind(balance)
        .bind(balance)
        .execute(pool)
        .await?;

        Ok(())
    }

    /// Log a transaction for auditing
    async fn log_transaction(
        &self,
        universe_id: &str,
        account_id: &str,
        amount: i64,
        reason: &str,
        pool: &SqlitePool,
    ) {
        // For now, just log to tracing - could add a transactions table later
        debug!(
            "Credit transaction: universe={}, account={}, amount={}, reason={}",
            universe_id, account_id, amount, reason
        );
        let _ = pool; // Silence unused warning
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_get_balance_default() {
        let manager = CreditManager::new(None);
        let balance = manager.get_balance("u1", "player1").await;
        assert_eq!(balance, 0);
    }

    #[tokio::test]
    async fn test_set_balance() {
        let manager = CreditManager::new(None);
        manager.set_balance("u1", "player1", 100).await;
        let balance = manager.get_balance("u1", "player1").await;
        assert_eq!(balance, 100);
    }

    #[tokio::test]
    async fn test_grant() {
        let manager = CreditManager::new(None);
        manager.grant("u1", "player1", 50, "admin grant").await;
        let balance = manager.get_balance("u1", "player1").await;
        assert_eq!(balance, 50);

        manager.grant("u1", "player1", 30, "bonus").await;
        let balance = manager.get_balance("u1", "player1").await;
        assert_eq!(balance, 80);
    }

    #[tokio::test]
    async fn test_deduct_success() {
        let manager = CreditManager::new(None);
        manager.set_balance("u1", "player1", 100).await;

        let result = manager.deduct("u1", "player1", 30, "purchase").await;
        assert!(result);

        let balance = manager.get_balance("u1", "player1").await;
        assert_eq!(balance, 70);
    }

    #[tokio::test]
    async fn test_deduct_insufficient() {
        let manager = CreditManager::new(None);
        manager.set_balance("u1", "player1", 20).await;

        let result = manager.deduct("u1", "player1", 50, "purchase").await;
        assert!(!result);

        // Balance unchanged
        let balance = manager.get_balance("u1", "player1").await;
        assert_eq!(balance, 20);
    }

    #[tokio::test]
    async fn test_deduct_invalid_amount() {
        let manager = CreditManager::new(None);
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
        let manager = CreditManager::new(None);

        manager.set_balance("u1", "player1", 100).await;
        manager.set_balance("u2", "player1", 200).await;

        assert_eq!(manager.get_balance("u1", "player1").await, 100);
        assert_eq!(manager.get_balance("u2", "player1").await, 200);
    }
}
