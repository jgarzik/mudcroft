//! Database module - SQLite with shared and per-universe schemas

#[cfg(test)]
pub mod test_utils;

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

    /// Open an existing database for migration/upgrade
    /// Used by mudd_init to upgrade existing databases
    pub async fn open_for_migration(path: &str) -> Result<Self> {
        // Verify file exists
        if !Path::new(path).exists() {
            bail!(
                "Database file not found: {}. Cannot upgrade non-existent database.",
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

        // Run migrations to upgrade schema
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
            .foreign_keys(true)
            .busy_timeout(std::time::Duration::from_secs(30))
            .shared_cache(true);

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
            "universe_members",
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

        // Check that at least one administrator (admin or owner) exists
        let admin_count: (i64,) = sqlx::query_as(
            "SELECT COUNT(*) FROM accounts WHERE access_level IN ('admin', 'owner')",
        )
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

        // Objects: ids are paths scoped per universe, so the same path can
        // exist in every universe. A parent is always in the same universe.
        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS objects (
                universe_id TEXT NOT NULL REFERENCES universes(id),
                id TEXT NOT NULL,
                class TEXT NOT NULL,
                parent_id TEXT,
                properties TEXT NOT NULL DEFAULT '{}',
                code_hash TEXT,
                owner_id TEXT REFERENCES accounts(id),
                created_at TEXT NOT NULL DEFAULT (datetime('now')),
                updated_at TEXT NOT NULL DEFAULT (datetime('now')),
                PRIMARY KEY (universe_id, id),
                FOREIGN KEY (universe_id, parent_id) REFERENCES objects(universe_id, id)
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

        // Classes defined per universe (built-in classes live in code; a
        // parent may be built-in, so it has no foreign key)
        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS classes (
                universe_id TEXT NOT NULL REFERENCES universes(id),
                name TEXT NOT NULL,
                parent TEXT,
                code_hash TEXT,
                defined_by TEXT REFERENCES accounts(id),
                created_at TEXT NOT NULL DEFAULT (datetime('now')),
                PRIMARY KEY (universe_id, name)
            )
            "#,
        )
        .execute(&self.pool)
        .await?;

        // Class properties table (normalized from JSON blob)
        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS class_properties (
                universe_id TEXT NOT NULL,
                class_name TEXT NOT NULL,
                key TEXT NOT NULL,
                value TEXT NOT NULL,
                PRIMARY KEY (universe_id, class_name, key),
                FOREIGN KEY (universe_id, class_name)
                    REFERENCES classes(universe_id, name) ON DELETE CASCADE
            )
            "#,
        )
        .execute(&self.pool)
        .await?;

        // Class handlers table (normalized from JSON blob)
        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS class_handlers (
                universe_id TEXT NOT NULL,
                class_name TEXT NOT NULL,
                handler TEXT NOT NULL,
                PRIMARY KEY (universe_id, class_name, handler),
                FOREIGN KEY (universe_id, class_name)
                    REFERENCES classes(universe_id, name) ON DELETE CASCADE
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

        // Path grants table (for path-based permission delegation)
        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS path_grants (
                id TEXT PRIMARY KEY,
                universe_id TEXT NOT NULL REFERENCES universes(id),
                grantee_id TEXT NOT NULL REFERENCES accounts(id),
                path_prefix TEXT NOT NULL,
                can_delegate BOOLEAN NOT NULL DEFAULT FALSE,
                granted_by TEXT NOT NULL REFERENCES accounts(id),
                granted_at TEXT NOT NULL,
                UNIQUE(universe_id, grantee_id, path_prefix)
            )
            "#,
        )
        .execute(&self.pool)
        .await?;

        // Per-universe access levels (absent row = player)
        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS universe_members (
                universe_id TEXT NOT NULL REFERENCES universes(id),
                account_id TEXT NOT NULL REFERENCES accounts(id),
                level TEXT NOT NULL,
                PRIMARY KEY (universe_id, account_id)
            )
            "#,
        )
        .execute(&self.pool)
        .await?;

        // Index for path grants lookup by grantee
        sqlx::query(
            "CREATE INDEX IF NOT EXISTS idx_path_grants_grantee ON path_grants(universe_id, grantee_id)",
        )
        .execute(&self.pool)
        .await?;

        // Combat state table (HP persistence)
        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS combat_state (
                universe_id TEXT NOT NULL,
                entity_id TEXT NOT NULL,
                hp INTEGER NOT NULL,
                max_hp INTEGER NOT NULL,
                armor_class INTEGER NOT NULL DEFAULT 10,
                attack_bonus INTEGER NOT NULL DEFAULT 0,
                PRIMARY KEY (universe_id, entity_id)
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
                universe_id TEXT,
                entity_id TEXT NOT NULL,
                effect_type TEXT NOT NULL,
                remaining_ticks INTEGER NOT NULL,
                magnitude INTEGER NOT NULL DEFAULT 0,
                damage_type TEXT,
                source_id TEXT
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
                leader_node INTEGER NOT NULL DEFAULT 0,
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

        // Core lib hashes table (maps lib name to content hash)
        // This stores the canonical mudlib version loaded at init time
        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS core_lib_hashes (
                name TEXT PRIMARY KEY,
                hash TEXT NOT NULL,
                updated_at TEXT NOT NULL DEFAULT (datetime('now'))
            )
            "#,
        )
        .execute(&self.pool)
        .await?;

        // Create indexes
        sqlx::query("CREATE INDEX IF NOT EXISTS idx_objects_universe ON objects(universe_id)")
            .execute(&self.pool)
            .await?;
        sqlx::query(
            "CREATE INDEX IF NOT EXISTS idx_objects_parent ON objects(universe_id, parent_id)",
        )
        .execute(&self.pool)
        .await?;
        sqlx::query("CREATE INDEX IF NOT EXISTS idx_objects_owner ON objects(owner_id)")
            .execute(&self.pool)
            .await?;
        sqlx::query("CREATE INDEX IF NOT EXISTS idx_timers_fire_at ON timers(fire_at)")
            .execute(&self.pool)
            .await?;
        sqlx::query("CREATE INDEX IF NOT EXISTS idx_raft_log_term ON raft_log(term)")
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
