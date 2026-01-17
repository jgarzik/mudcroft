//! Database module - SQLite with shared and per-universe schemas

use anyhow::Result;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePool, SqlitePoolOptions};
use std::str::FromStr;
use tracing::info;

/// Database handle wrapping SQLite connection pool
pub struct Database {
    pool: SqlitePool,
}

impl Database {
    /// Create a new database connection
    /// If path is None, uses in-memory database (for testing)
    pub async fn new(path: Option<&str>) -> Result<Self> {
        let conn_str = match path {
            Some(p) => format!("sqlite:{}?mode=rwc", p),
            None => "sqlite::memory:".to_string(),
        };

        let options = SqliteConnectOptions::from_str(&conn_str)?
            .create_if_missing(true)
            .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal)
            .foreign_keys(true);

        let pool = SqlitePoolOptions::new()
            .max_connections(10)
            .connect_with(options)
            .await?;

        let db = Self { pool };
        db.run_migrations().await?;

        Ok(db)
    }

    /// Run database migrations
    async fn run_migrations(&self) -> Result<()> {
        info!("Running database migrations...");

        // Shared schema: accounts, universes
        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS accounts (
                id TEXT PRIMARY KEY,
                username TEXT UNIQUE NOT NULL,
                token TEXT,
                access_level TEXT NOT NULL DEFAULT 'player',
                created_at TEXT NOT NULL DEFAULT (datetime('now'))
            )
            "#,
        )
        .execute(&self.pool)
        .await?;

        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS universes (
                id TEXT PRIMARY KEY,
                name TEXT NOT NULL,
                owner_id TEXT NOT NULL REFERENCES accounts(id),
                config TEXT NOT NULL DEFAULT '{}',
                created_at TEXT NOT NULL DEFAULT (datetime('now'))
            )
            "#,
        )
        .execute(&self.pool)
        .await?;

        // Per-universe schema (for now, single shared DB)
        // Objects table
        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS objects (
                id TEXT PRIMARY KEY,
                universe_id TEXT NOT NULL REFERENCES universes(id),
                class TEXT NOT NULL,
                parent_id TEXT REFERENCES objects(id),
                properties TEXT NOT NULL DEFAULT '{}',
                code_hash TEXT,
                created_at TEXT NOT NULL DEFAULT (datetime('now')),
                updated_at TEXT NOT NULL DEFAULT (datetime('now'))
            )
            "#,
        )
        .execute(&self.pool)
        .await?;

        // Code store (content-addressed)
        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS code_store (
                hash TEXT PRIMARY KEY,
                source TEXT NOT NULL,
                created_at TEXT NOT NULL DEFAULT (datetime('now'))
            )
            "#,
        )
        .execute(&self.pool)
        .await?;

        // Credits table
        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS credits (
                id TEXT PRIMARY KEY,
                universe_id TEXT NOT NULL REFERENCES universes(id),
                player_id TEXT NOT NULL REFERENCES accounts(id),
                balance INTEGER NOT NULL DEFAULT 0,
                UNIQUE(universe_id, player_id)
            )
            "#,
        )
        .execute(&self.pool)
        .await?;

        // Classes table (for class definitions)
        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS classes (
                name TEXT PRIMARY KEY,
                universe_id TEXT NOT NULL REFERENCES universes(id),
                parent TEXT,
                definition TEXT NOT NULL,
                created_at TEXT NOT NULL DEFAULT (datetime('now'))
            )
            "#,
        )
        .execute(&self.pool)
        .await?;

        // Timers table (for call_out)
        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS timers (
                id TEXT PRIMARY KEY,
                universe_id TEXT NOT NULL REFERENCES universes(id),
                object_id TEXT NOT NULL,
                method TEXT NOT NULL,
                fire_at INTEGER NOT NULL,
                args TEXT,
                created_at TEXT NOT NULL DEFAULT (datetime('now'))
            )
            "#,
        )
        .execute(&self.pool)
        .await?;

        // Create indexes
        sqlx::query("CREATE INDEX IF NOT EXISTS idx_objects_universe ON objects(universe_id)")
            .execute(&self.pool)
            .await?;
        sqlx::query("CREATE INDEX IF NOT EXISTS idx_objects_parent ON objects(parent_id)")
            .execute(&self.pool)
            .await?;
        sqlx::query("CREATE INDEX IF NOT EXISTS idx_timers_fire_at ON timers(fire_at)")
            .execute(&self.pool)
            .await?;

        info!("Database migrations complete");
        Ok(())
    }

    /// Get the connection pool
    pub fn pool(&self) -> &SqlitePool {
        &self.pool
    }

    /// Check if database is healthy
    pub async fn health_check(&self) -> Result<()> {
        sqlx::query("SELECT 1").execute(&self.pool).await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_db_creation() {
        let db = Database::new(None).await.unwrap();
        db.health_check().await.unwrap();
    }

    #[tokio::test]
    async fn test_migrations_run() {
        let db = Database::new(None).await.unwrap();

        // Verify tables exist
        let result: (i32,) = sqlx::query_as("SELECT COUNT(*) FROM accounts")
            .fetch_one(db.pool())
            .await
            .unwrap();
        assert_eq!(result.0, 0);
    }
}
