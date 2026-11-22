//! Credits: per-universe balances held by accounts or objects
//!
//! The `credits` table is the only copy of a balance (no cache). Every
//! change is a relative update inside one atomic batch, and the table's
//! `CHECK (balance >= 0)` rejects an overdraft, so a spend or transfer is
//! all-or-nothing with no read-modify-write anywhere.

use std::sync::Arc;

use serde_json::{json, Value};
use sqlx::SqlitePool;

use crate::raft::RaftWriter;

/// Largest single grant, spend or transfer
pub const MAX_AMOUNT: i64 = 1_000_000_000_000;

/// Who holds a balance
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Holder {
    /// An account (player)
    Account(String),
    /// An object in the universe (a room's budget, a shop's till)
    Object(String),
}

impl Holder {
    /// An object id (a path, starting with `/`) or else an account id
    pub fn parse(id: &str) -> Self {
        if id.starts_with('/') {
            Holder::Object(id.to_string())
        } else {
            Holder::Account(id.to_string())
        }
    }

    fn kind(&self) -> &'static str {
        match self {
            Holder::Account(_) => "account",
            Holder::Object(_) => "object",
        }
    }

    fn id(&self) -> &str {
        match self {
            Holder::Account(id) | Holder::Object(id) => id,
        }
    }
}

type Statement = (String, Vec<Value>);

/// Credit balances (a stateless handle on the credits table)
pub struct CreditManager {
    pool: Option<SqlitePool>,
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

fn check_amount(amount: i64) -> anyhow::Result<()> {
    if !(1..=MAX_AMOUNT).contains(&amount) {
        anyhow::bail!("amount must be 1 to {}", MAX_AMOUNT);
    }
    Ok(())
}

/// Whether a failed write was the balance CHECK (an overdraft)
fn is_overdraft(e: &anyhow::Error) -> bool {
    format!("{:#}", e).contains("CHECK constraint failed")
}

/// Whether a failed write was the holder trigger (no such account/object)
fn is_missing_holder(e: &anyhow::Error) -> bool {
    format!("{:#}", e).contains("credits holder does not exist")
}

impl CreditManager {
    /// A credit manager over the database: through Raft when a writer is
    /// given (the server), else directly (tests)
    pub fn new(pool: Option<SqlitePool>, raft_writer: Option<Arc<RaftWriter>>) -> Self {
        Self { pool, raft_writer }
    }

    /// Create a shared instance
    pub fn shared(pool: Option<SqlitePool>, raft_writer: Option<Arc<RaftWriter>>) -> Arc<Self> {
        Arc::new(Self::new(pool, raft_writer))
    }

    /// Statement making sure a holder has a row (balance 0 if new)
    fn ensure_statement(universe_id: &str, holder: &Holder) -> Statement {
        (
            "INSERT OR IGNORE INTO credits (universe_id, holder_kind, holder_id, balance)
             VALUES (?, ?, ?, 0)"
                .to_string(),
            vec![json!(universe_id), json!(holder.kind()), json!(holder.id())],
        )
    }

    /// Statement adding to a holder's balance
    fn credit_statement(universe_id: &str, holder: &Holder, amount: i64) -> Statement {
        (
            "INSERT INTO credits (universe_id, holder_kind, holder_id, balance) VALUES (?, ?, ?, ?)
             ON CONFLICT (universe_id, holder_kind, holder_id)
             DO UPDATE SET balance = balance + excluded.balance"
                .to_string(),
            vec![
                json!(universe_id),
                json!(holder.kind()),
                json!(holder.id()),
                json!(amount),
            ],
        )
    }

    /// Statements taking from a holder's balance: the CHECK fails the batch
    /// if that would go below zero
    fn debit_statements(universe_id: &str, holder: &Holder, amount: i64) -> Vec<Statement> {
        vec![
            Self::ensure_statement(universe_id, holder),
            (
                "UPDATE credits SET balance = balance - ?
                 WHERE universe_id = ? AND holder_kind = ? AND holder_id = ?"
                    .to_string(),
                vec![
                    json!(amount),
                    json!(universe_id),
                    json!(holder.kind()),
                    json!(holder.id()),
                ],
            ),
        ]
    }

    /// Statement deleting a holder's balance (e.g. its object is deleted)
    pub fn delete_statement(universe_id: &str, holder: &Holder) -> Statement {
        (
            "DELETE FROM credits WHERE universe_id = ? AND holder_kind = ? AND holder_id = ?"
                .to_string(),
            vec![json!(universe_id), json!(holder.kind()), json!(holder.id())],
        )
    }

    async fn execute(&self, statements: Vec<Statement>) -> anyhow::Result<()> {
        self.execute_as(statements, false).await
    }

    /// `direct`: committed now, on its own, even inside a run (which must
    /// not hold the write lock then: see db::run::Run::checkpoint)
    async fn execute_as(&self, statements: Vec<Statement>, direct: bool) -> anyhow::Result<()> {
        if let Some(ref raft_writer) = self.raft_writer {
            if direct {
                return raft_writer.execute_batch_direct(statements).await;
            }
            return raft_writer.execute_batch(statements).await;
        }
        let Some(ref pool) = self.pool else {
            anyhow::bail!("credits: no database");
        };
        let mut tx = pool.begin().await?;
        for (sql, params) in &statements {
            // Statements built above from literals, values bound
            let mut query = sqlx::query(sqlx::AssertSqlSafe(sql.as_str()));
            for param in params {
                query = match param {
                    Value::String(s) => query.bind(s.clone()),
                    Value::Number(n) => query.bind(n.as_i64()),
                    other => query.bind(other.to_string()),
                };
            }
            query.execute(&mut *tx).await?;
        }
        tx.commit().await?;
        Ok(())
    }

    /// Whether a holder exists: an account, or an object in the universe
    /// (credits sent to a typo would be lost)
    pub async fn holder_exists(&self, universe_id: &str, holder: &Holder) -> anyhow::Result<bool> {
        let Some(ref pool) = self.pool else {
            anyhow::bail!("credits: no database");
        };
        let found: Option<i64> = match holder {
            Holder::Account(id) => {
                sqlx::query_scalar("SELECT 1 FROM accounts WHERE id = ?")
                    .bind(id)
                    .fetch_optional(&mut *crate::db::run::reader(pool).await?)
                    .await?
            }
            Holder::Object(id) => {
                sqlx::query_scalar("SELECT 1 FROM objects WHERE universe_id = ? AND id = ?")
                    .bind(universe_id)
                    .bind(id)
                    .fetch_optional(&mut *crate::db::run::reader(pool).await?)
                    .await?
            }
        };
        Ok(found.is_some())
    }

    /// A holder's balance (0 if it has none). A failed read is an error,
    /// never a silent zero.
    pub async fn balance(&self, universe_id: &str, holder: &Holder) -> anyhow::Result<i64> {
        let Some(ref pool) = self.pool else {
            anyhow::bail!("credits: no database");
        };
        let balance: Option<i64> = sqlx::query_scalar(
            "SELECT balance FROM credits WHERE universe_id = ? AND holder_kind = ? AND holder_id = ?",
        )
        .bind(universe_id)
        .bind(holder.kind())
        .bind(holder.id())
        .fetch_optional(&mut *crate::db::run::reader(pool).await?)
        .await?;
        Ok(balance.unwrap_or(0))
    }

    /// Add `amount` to a holder's balance
    pub async fn grant(
        &self,
        universe_id: &str,
        holder: &Holder,
        amount: i64,
    ) -> anyhow::Result<()> {
        check_amount(amount)?;
        self.execute(vec![Self::credit_statement(universe_id, holder, amount)])
            .await
    }

    /// Payment for something outside the database (a Venice call): like
    /// [`Self::deduct`], but committed now against committed balances, so it
    /// stands whatever the current run does next. The run must have
    /// checkpointed first.
    pub async fn deduct_direct(
        &self,
        universe_id: &str,
        holder: &Holder,
        amount: i64,
    ) -> anyhow::Result<bool> {
        check_amount(amount)?;
        match self
            .execute_as(Self::debit_statements(universe_id, holder, amount), true)
            .await
        {
            Ok(()) => Ok(true),
            Err(e) if is_overdraft(&e) || is_missing_holder(&e) => Ok(false),
            Err(e) => Err(e),
        }
    }

    /// The refund of a [`Self::deduct_direct`] (the call failed): committed
    /// now too
    pub async fn grant_direct(
        &self,
        universe_id: &str,
        holder: &Holder,
        amount: i64,
    ) -> anyhow::Result<()> {
        check_amount(amount)?;
        self.execute_as(
            vec![Self::credit_statement(universe_id, holder, amount)],
            true,
        )
        .await
    }

    /// Spend `amount` from a holder's balance. `Ok(false)` if it does not
    /// have enough (nothing changes).
    pub async fn deduct(
        &self,
        universe_id: &str,
        holder: &Holder,
        amount: i64,
    ) -> anyhow::Result<bool> {
        check_amount(amount)?;
        match self
            .execute(Self::debit_statements(universe_id, holder, amount))
            .await
        {
            Ok(()) => Ok(true),
            // A holder that does not exist has nothing to spend
            Err(e) if is_overdraft(&e) || is_missing_holder(&e) => Ok(false),
            Err(e) => Err(e),
        }
    }

    /// Move `amount` from one holder to another, atomically. `Ok(false)` if
    /// the payer does not have enough (nothing changes).
    pub async fn transfer(
        &self,
        universe_id: &str,
        from: &Holder,
        to: &Holder,
        amount: i64,
    ) -> anyhow::Result<bool> {
        check_amount(amount)?;
        if from == to {
            // Nothing moves, but the payer must still have the funds
            return Ok(self.balance(universe_id, from).await? >= amount);
        }
        let mut statements = Self::debit_statements(universe_id, from, amount);
        statements.push(Self::credit_statement(universe_id, to, amount));
        match self.execute(statements).await {
            Ok(()) => Ok(true),
            Err(e) if is_overdraft(&e) => Ok(false),
            // A missing payer has nothing to spend; a missing recipient is
            // the caller's error
            Err(e) if is_missing_holder(&e) && !self.holder_exists(universe_id, from).await? => {
                Ok(false)
            }
            Err(e) => Err(e),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::test_utils::test_pool;

    async fn manager() -> CreditManager {
        let pool = test_pool().await;
        for name in ["acct", "alice", "bob"] {
            sqlx::query(
                "INSERT INTO accounts (id, username, password_hash, access_level, created_at)
                 VALUES (?, ?, 'x', 'player', '2026-01-01')",
            )
            .bind(name)
            .bind(name)
            .execute(&pool)
            .await
            .unwrap();
        }
        for u in ["u1", "u2"] {
            sqlx::query(
                "INSERT INTO universes (id, name, owner_id, config, created_at)
                 VALUES (?, ?, 'acct', '{}', '2026-01-01')",
            )
            .bind(u)
            .bind(u)
            .execute(&pool)
            .await
            .unwrap();
        }
        sqlx::query(
            "INSERT INTO objects (id, universe_id, class, properties, created_at, updated_at)
             VALUES ('/shops/till', 'u1', 'item', '{}', '2026-01-01', '2026-01-01')",
        )
        .execute(&pool)
        .await
        .unwrap();
        CreditManager::new(Some(pool), None)
    }

    fn alice() -> Holder {
        Holder::Account("alice".into())
    }

    #[tokio::test]
    async fn test_grant_and_deduct() {
        let credits = manager().await;
        assert_eq!(credits.balance("u1", &alice()).await.unwrap(), 0);
        credits.grant("u1", &alice(), 100).await.unwrap();
        credits.grant("u1", &alice(), 50).await.unwrap();
        assert!(credits.deduct("u1", &alice(), 30).await.unwrap());
        assert_eq!(credits.balance("u1", &alice()).await.unwrap(), 120);
        // Per universe
        assert_eq!(credits.balance("u2", &alice()).await.unwrap(), 0);
    }

    /// An overdraft is refused and changes nothing
    #[tokio::test]
    async fn test_overdraft_refused() {
        let credits = manager().await;
        credits.grant("u1", &alice(), 10).await.unwrap();
        assert!(!credits.deduct("u1", &alice(), 11).await.unwrap());
        assert_eq!(credits.balance("u1", &alice()).await.unwrap(), 10);
        // A holder with no row at all has nothing to spend
        assert!(!credits
            .deduct("u1", &Holder::Account("bob".into()), 1)
            .await
            .unwrap());
    }

    /// Transfers are atomic between accounts and objects
    #[tokio::test]
    async fn test_transfer_atomic() {
        let credits = manager().await;
        let till = Holder::parse("/shops/till");
        assert_eq!(till, Holder::Object("/shops/till".into()));
        credits.grant("u1", &alice(), 40).await.unwrap();
        assert!(credits.transfer("u1", &alice(), &till, 25).await.unwrap());
        assert_eq!(credits.balance("u1", &till).await.unwrap(), 25);
        assert_eq!(credits.balance("u1", &alice()).await.unwrap(), 15);
        // Too much: neither side changes
        assert!(!credits.transfer("u1", &alice(), &till, 16).await.unwrap());
        assert_eq!(credits.balance("u1", &till).await.unwrap(), 25);
        assert_eq!(credits.balance("u1", &alice()).await.unwrap(), 15);
    }

    #[tokio::test]
    async fn test_amounts_are_bounded() {
        let credits = manager().await;
        for bad in [0, -5, MAX_AMOUNT + 1] {
            assert!(credits.grant("u1", &alice(), bad).await.is_err());
            assert!(credits.deduct("u1", &alice(), bad).await.is_err());
        }
    }

    /// A transfer to oneself still requires the funds
    #[tokio::test]
    async fn test_self_transfer_checks_balance() {
        let credits = manager().await;
        assert!(!credits.transfer("u1", &alice(), &alice(), 5).await.unwrap());
        credits.grant("u1", &alice(), 5).await.unwrap();
        assert!(credits.transfer("u1", &alice(), &alice(), 5).await.unwrap());
        assert_eq!(credits.balance("u1", &alice()).await.unwrap(), 5);
    }

    /// The table refuses a balance for a holder that does not exist (an
    /// object deleted meanwhile, or in another universe; an unknown
    /// account), so credits are never stranded
    #[tokio::test]
    async fn test_missing_holders_refused() {
        let credits = manager().await;
        let till = Holder::Object("/shops/till".into());
        assert!(credits.grant("u1", &till, 5).await.is_ok());
        assert!(credits.grant("u2", &till, 5).await.is_err());
        assert!(credits
            .grant("u1", &Holder::Account("ghost".into()), 5)
            .await
            .is_err());
        // A transfer to a missing holder changes nothing
        credits.grant("u1", &alice(), 10).await.unwrap();
        let gone = Holder::Object("/shops/gone".into());
        assert!(credits.transfer("u1", &alice(), &gone, 4).await.is_err());
        assert_eq!(credits.balance("u1", &alice()).await.unwrap(), 10);
    }

    /// Spending from a holder that does not exist (an object deleted
    /// meanwhile) is "not enough", not an error
    #[tokio::test]
    async fn test_deduct_from_missing_holder_is_false() {
        let credits = manager().await;
        let gone = Holder::Object("/shops/gone".into());
        assert!(!credits.deduct("u1", &gone, 1).await.unwrap());
        assert!(!credits.transfer("u1", &gone, &alice(), 1).await.unwrap());
    }
}
