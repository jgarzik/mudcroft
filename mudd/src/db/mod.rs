//! Database module - SQLite with shared and per-universe schemas

use anyhow::{bail, Result};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePool, SqlitePoolOptions};
use std::path::Path;
use std::str::FromStr;
use tracing::info;

/// Database handle wrapping SQLite connection pool
pub struct Database {
    pool: SqlitePool,
}

impl Database {
    /// Create a new database connection and run migrations
    /// Used by mudd_init for initial database creation
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

    /// Open an existing database (does not create or run migrations)
    /// Used by mudd server at runtime - requires pre-initialized database
    pub async fn open(path: &str) -> Result<Self> {
        // Verify file exists
        if !Path::new(path).exists() {
            bail!(
                "Database file not found: {}. Run mudd_init to create it.",
                path
            );
        }

        let conn_str = format!("sqlite:{}?mode=rw", path);

        let options = SqliteConnectOptions::from_str(&conn_str)?
            .create_if_missing(false)
            .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal)
            .foreign_keys(true);

        let pool = SqlitePoolOptions::new()
            .max_connections(10)
            .connect_with(options)
            .await?;

        let db = Self { pool };

        // Validate the database is properly initialized
        db.validate().await?;

        Ok(db)
    }

    /// Validate that the database has required tables and an admin account
    pub async fn validate(&self) -> Result<()> {
        // Check required tables exist
        let required_tables = [
            "accounts",
            "universes",
            "objects",
            "code_store",
            "credits",
            "classes",
            "class_properties",
            "class_handlers",
            "timers",
            "raft_log",
            "raft_vote",
            "raft_meta",
        ];

        for table in &required_tables {
            let exists: Option<(String,)> =
                sqlx::query_as("SELECT name FROM sqlite_master WHERE type='table' AND name=?")
                    .bind(table)
                    .fetch_optional(&self.pool)
                    .await?;

            if exists.is_none() {
                bail!(
                    "Database is missing required table '{}'. Run mudd_init to create a valid database.",
                    table
                );
            }
        }

        // Check that at least one admin account exists
        let admin_count: (i64,) =
            sqlx::query_as("SELECT COUNT(*) FROM accounts WHERE access_level = 'admin'")
                .fetch_one(&self.pool)
                .await?;

        if admin_count.0 == 0 {
            bail!("Database has no admin account. Run mudd_init to create a valid database.");
        }

        Ok(())
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
                password_hash TEXT,
                salt TEXT,
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
                theme_id TEXT NOT NULL DEFAULT 'sierra-retro',
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
                parent TEXT REFERENCES classes(name),
                code_hash TEXT,
                created_at TEXT NOT NULL DEFAULT (datetime('now'))
            )
            "#,
        )
        .execute(&self.pool)
        .await?;

        // Class properties table (normalized from JSON blob)
        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS class_properties (
                class_name TEXT NOT NULL REFERENCES classes(name) ON DELETE CASCADE,
                universe_id TEXT NOT NULL REFERENCES universes(id),
                key TEXT NOT NULL,
                value TEXT NOT NULL,
                PRIMARY KEY (class_name, key)
            )
            "#,
        )
        .execute(&self.pool)
        .await?;

        // Class handlers table (normalized from JSON blob)
        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS class_handlers (
                class_name TEXT NOT NULL REFERENCES classes(name) ON DELETE CASCADE,
                universe_id TEXT NOT NULL REFERENCES universes(id),
                handler TEXT NOT NULL,
                PRIMARY KEY (class_name, handler)
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

        // Builder regions table (for permission persistence)
        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS builder_regions (
                account_id TEXT NOT NULL,
                region_id TEXT NOT NULL,
                PRIMARY KEY (account_id, region_id)
            )
            "#,
        )
        .execute(&self.pool)
        .await?;

        // Combat state table (HP persistence)
        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS combat_state (
                entity_id TEXT PRIMARY KEY,
                universe_id TEXT NOT NULL,
                hp INTEGER NOT NULL,
                max_hp INTEGER NOT NULL,
                armor_class INTEGER NOT NULL DEFAULT 10,
                attack_bonus INTEGER NOT NULL DEFAULT 0
            )
            "#,
        )
        .execute(&self.pool)
        .await?;

        // Active effects table (status effect persistence)
        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS active_effects (
                id TEXT PRIMARY KEY,
                entity_id TEXT NOT NULL,
                effect_type TEXT NOT NULL,
                remaining_ticks INTEGER NOT NULL,
                magnitude INTEGER NOT NULL DEFAULT 0,
                damage_type TEXT,
                source_id TEXT,
                FOREIGN KEY (entity_id) REFERENCES combat_state(entity_id) ON DELETE CASCADE
            )
            "#,
        )
        .execute(&self.pool)
        .await?;

        // Index for active effects lookup
        sqlx::query(
            "CREATE INDEX IF NOT EXISTS idx_active_effects_entity ON active_effects(entity_id)",
        )
        .execute(&self.pool)
        .await?;

        // Image store (content-addressed)
        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS image_store (
                hash TEXT PRIMARY KEY,
                data BLOB NOT NULL,
                mime_type TEXT NOT NULL,
                size_bytes INTEGER NOT NULL,
                source TEXT,
                created_at TEXT NOT NULL DEFAULT (datetime('now')),
                reference_count INTEGER DEFAULT 0
            )
            "#,
        )
        .execute(&self.pool)
        .await?;

        // Raft consensus tables
        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS raft_log (
                log_index INTEGER PRIMARY KEY,
                term INTEGER NOT NULL,
                entry_type TEXT NOT NULL,
                payload TEXT,
                created_at INTEGER NOT NULL DEFAULT (unixepoch())
            )
            "#,
        )
        .execute(&self.pool)
        .await?;

        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS raft_vote (
                id INTEGER PRIMARY KEY CHECK (id = 1),
                term INTEGER NOT NULL,
                node_id INTEGER,
                committed INTEGER NOT NULL DEFAULT 0
            )
            "#,
        )
        .execute(&self.pool)
        .await?;

        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS raft_meta (
                key TEXT PRIMARY KEY,
                value TEXT NOT NULL
            )
            "#,
        )
        .execute(&self.pool)
        .await?;

        // Universe settings table (key-value per universe)
        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS universe_settings (
                universe_id TEXT NOT NULL REFERENCES universes(id),
                key TEXT NOT NULL,
                value TEXT NOT NULL,
                PRIMARY KEY (universe_id, key)
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
        sqlx::query("CREATE INDEX IF NOT EXISTS idx_raft_log_term ON raft_log(term)")
            .execute(&self.pool)
            .await?;
        sqlx::query(
            "CREATE INDEX IF NOT EXISTS idx_class_props_universe ON class_properties(universe_id)",
        )
        .execute(&self.pool)
        .await?;
        sqlx::query(
            "CREATE INDEX IF NOT EXISTS idx_class_handlers_universe ON class_handlers(universe_id)",
        )
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
