//! Database module - SQLite with shared and per-universe schemas

pub mod run;
#[cfg(test)]
pub mod test_utils;

use anyhow::{bail, Result};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePool, SqlitePoolOptions};
use std::path::Path;
use std::str::FromStr;
use tracing::info;

/// Schema version, stored in `PRAGMA user_version`. Bump it with every
/// incompatible schema change and add the upgrade step from the previous
/// version to `Database::upgrade` (mudd_init is the only place upgrades run;
/// mudd refuses any other version).
pub const SCHEMA_VERSION: i64 = 11;

/// Custom verbs (`game.add_action`): room-scoped (any nearby object's verb
/// in a room) or object-scoped (follows the object). They go with their
/// object and with their room.
const ACTIONS_TABLE: &str = r#"
    CREATE TABLE IF NOT EXISTS actions (
        universe_id TEXT NOT NULL,
        scope TEXT NOT NULL CHECK (scope IN ('room', 'object')),
        scope_id TEXT NOT NULL,
        verb TEXT NOT NULL,
        object_id TEXT NOT NULL,
        method TEXT NOT NULL,
        PRIMARY KEY (universe_id, scope, scope_id, verb, object_id),
        FOREIGN KEY (universe_id, object_id)
            REFERENCES objects(universe_id, id) ON DELETE CASCADE,
        FOREIGN KEY (universe_id, scope_id)
            REFERENCES objects(universe_id, id) ON DELETE CASCADE
    )
"#;

/// Every node ever admitted to the cluster (replicated, so it travels in
/// snapshots): node ids are never reused, so a removed node returning with
/// its old database cannot pass for a newer member
const CLUSTER_NODES_TABLE: &str = r#"
    CREATE TABLE IF NOT EXISTS cluster_nodes (
        node_id INTEGER PRIMARY KEY,
        addr TEXT NOT NULL,
        added_at TEXT NOT NULL
    )
"#;

/// Players with a game session (written on connect, removed when their
/// disconnect is handled): a new primary handles the disconnect of anyone
/// listed who does not reconnect, even if the server they were on died
const ONLINE_PLAYERS_TABLE: &str = r#"
    CREATE TABLE IF NOT EXISTS online_players (
        universe_id TEXT NOT NULL,
        player_id TEXT NOT NULL,
        since TEXT NOT NULL,
        PRIMARY KEY (universe_id, player_id),
        FOREIGN KEY (universe_id, player_id)
            REFERENCES objects(universe_id, id) ON DELETE CASCADE
    )
"#;

/// Payments a player has been asked to confirm (game.request_payment):
/// answered or expired rows are deleted. They go with the requesting object.
const PAYMENT_REQUESTS_TABLE: &str = r#"
    CREATE TABLE IF NOT EXISTS payment_requests (
        id TEXT PRIMARY KEY,
        universe_id TEXT NOT NULL,
        account_id TEXT NOT NULL,
        object_id TEXT NOT NULL,
        handler TEXT NOT NULL,
        amount INTEGER NOT NULL CHECK (amount > 0),
        reason TEXT NOT NULL,
        data TEXT NOT NULL,
        expires_at INTEGER NOT NULL,
        FOREIGN KEY (universe_id, object_id)
            REFERENCES objects(universe_id, id) ON DELETE CASCADE
    )
"#;

/// A credits balance needs a holder that exists (credits has no foreign
/// key, as holders are accounts or objects): checked in the same write, so
/// credits can never land on an object deleted meanwhile
const CREDITS_HOLDER_TRIGGER: &str = r#"
            CREATE TRIGGER IF NOT EXISTS credits_holder_exists
            BEFORE INSERT ON credits
            WHEN (NEW.holder_kind = 'account'
                  AND NOT EXISTS (SELECT 1 FROM accounts WHERE id = NEW.holder_id))
              OR (NEW.holder_kind = 'object'
                  AND NOT EXISTS (SELECT 1 FROM objects
                                  WHERE universe_id = NEW.universe_id AND id = NEW.holder_id))
            BEGIN
                SELECT RAISE(ABORT, 'credits holder does not exist');
            END
            "#;

/// The Raft members this database's node last applied (from `raft_meta`
/// `last_membership`): (node id, address)
pub async fn recorded_members(pool: &SqlitePool) -> Result<Vec<(i64, String)>> {
    let json: Option<String> =
        sqlx::query_scalar("SELECT value FROM raft_meta WHERE key = 'last_membership'")
            .fetch_optional(pool)
            .await?;
    let Some(json) = json else {
        return Ok(Vec::new());
    };
    let value: serde_json::Value = serde_json::from_str(&json)?;
    let mut members: Vec<(i64, String)> = value["membership"]["nodes"]
        .as_object()
        .map(|nodes| {
            nodes
                .iter()
                .filter_map(|(id, node)| {
                    let addr = node["addr"].as_str().unwrap_or_default().to_string();
                    id.parse().ok().map(|id| (id, addr))
                })
                .collect()
        })
        .unwrap_or_default();
    members.sort();
    Ok(members)
}

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
        db.set_schema_version(SCHEMA_VERSION).await?;

        Ok(db)
    }

    /// The database's schema version (`PRAGMA user_version`)
    pub async fn schema_version(&self) -> Result<i64> {
        Ok(sqlx::query_scalar("PRAGMA user_version")
            .fetch_one(&self.pool)
            .await?)
    }

    async fn set_schema_version(&self, version: i64) -> Result<()> {
        // PRAGMA takes no bound parameters; version is our own integer
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "PRAGMA user_version = {}",
            version
        )))
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Bring an existing database up to SCHEMA_VERSION, one version at a
    /// time. Versions without an upgrade step are refused:
    /// - 1 -> 2 (argon2id passwords, accounts.salt dropped): old salted
    ///   SHA-256 hashes cannot be converted, so there is no step.
    /// - 2 -> 3 (combat_state dropped: HP lives on the object row;
    ///   active_effects keyed (universe, entity, type), cascading with the
    ///   object).
    /// - 3 -> 4 (active_effects stores absolute deadlines: expires_at,
    ///   period_ms, next_pulse_at, instead of remaining_ticks).
    /// - 4 -> 5 (heartbeats table; timers cascade with their object).
    /// - 5 -> 6 (credits held by accounts or objects, CHECK balance >= 0).
    /// - 6 -> 7 (credits_holder_exists trigger): upgraded in place.
    /// - 7 -> 8 (actions table: custom verbs persisted): upgraded in place.
    /// - 8 -> 9 (cluster_nodes table: node ids never reused): in place.
    /// - 9 -> 10 (online_players table): in place.
    /// - 10 -> 11 (payment_requests table): in place.
    async fn upgrade(&self) -> Result<()> {
        let version = self.schema_version().await?;
        if version > SCHEMA_VERSION {
            bail!(
                "Database schema version {} is newer than this mudd_init supports ({})",
                version,
                SCHEMA_VERSION
            );
        }
        let mut version = version;
        if version == 6 {
            sqlx::query(CREDITS_HOLDER_TRIGGER)
                .execute(&self.pool)
                .await?;
            self.set_schema_version(7).await?;
            info!("Upgraded database schema 6 -> 7");
            version = 7;
        }
        if version == 7 {
            sqlx::query(ACTIONS_TABLE).execute(&self.pool).await?;
            self.set_schema_version(8).await?;
            info!("Upgraded database schema 7 -> 8");
            version = 8;
        }
        if version == 8 {
            sqlx::query(CLUSTER_NODES_TABLE).execute(&self.pool).await?;
            // A cluster member already knows its members: record them, so
            // their ids are never handed out again
            for (node_id, addr) in recorded_members(&self.pool).await? {
                sqlx::query(
                    "INSERT OR IGNORE INTO cluster_nodes (node_id, addr, added_at) VALUES (?, ?, ?)",
                )
                .bind(node_id)
                .bind(addr)
                .bind(chrono::Utc::now().to_rfc3339())
                .execute(&self.pool)
                .await?;
            }
            self.set_schema_version(9).await?;
            info!("Upgraded database schema 8 -> 9");
            version = 9;
        }
        if version == 9 {
            sqlx::query(ONLINE_PLAYERS_TABLE)
                .execute(&self.pool)
                .await?;
            self.set_schema_version(10).await?;
            info!("Upgraded database schema 9 -> 10");
            version = 10;
        }
        if version == 10 {
            sqlx::query(PAYMENT_REQUESTS_TABLE)
                .execute(&self.pool)
                .await?;
            self.set_schema_version(11).await?;
            info!("Upgraded database schema 10 -> 11");
            return Ok(());
        }
        if version < SCHEMA_VERSION {
            bail!(
                "Database schema version {} predates version {} and there is no upgrade \
                 from it; create a new database",
                version,
                SCHEMA_VERSION
            );
        }
        Ok(())
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

        // Upgrade to the current schema version, then create anything missing
        db.upgrade().await?;
        db.run_migrations().await?;

        Ok(db)
    }

    /// Open an existing database (does not create or run migrations)
    /// Used by mudd server at runtime - requires pre-initialized database
    pub async fn open(path: &str) -> Result<Self> {
        let db = Self::open_unvalidated(path).await?;
        db.validate().await?;
        db.validate_has_admin().await?;
        Ok(db)
    }

    /// Open the database of a node joining a cluster (`mudd --join`): it may
    /// still be empty, as its data (admin account included) arrives in a
    /// snapshot, so only the schema is checked
    pub async fn open_joining(path: &str) -> Result<Self> {
        let db = Self::open_unvalidated(path).await?;
        db.validate().await?;
        Ok(db)
    }

    async fn open_unvalidated(path: &str) -> Result<Self> {
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

        Ok(Self { pool })
    }

    /// Validate the schema version and that the required tables exist
    pub async fn validate(&self) -> Result<()> {
        let version = self.schema_version().await?;
        if version != SCHEMA_VERSION {
            bail!(
                "Database schema version {} does not match this mudd ({}). Run mudd_init \
                 to upgrade it.",
                version,
                SCHEMA_VERSION
            );
        }

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
            "actions",
            "cluster_nodes",
            "online_players",
            "payment_requests",
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
        Ok(())
    }

    /// Check that at least one administrator (admin or owner) exists
    async fn validate_has_admin(&self) -> Result<()> {
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
                universe_id TEXT NOT NULL REFERENCES universes(id),
                holder_kind TEXT NOT NULL CHECK (holder_kind IN ('account', 'object')),
                holder_id TEXT NOT NULL,
                balance INTEGER NOT NULL DEFAULT 0 CHECK (balance >= 0),
                PRIMARY KEY (universe_id, holder_kind, holder_id)
            )
            "#,
        )
        .execute(&self.pool)
        .await?;
        // A balance needs a holder that exists (credits has no foreign key,
        // as holders are accounts or objects): checked in the same write,
        // so credits can never land on an object deleted meanwhile
        sqlx::query(CREDITS_HOLDER_TRIGGER)
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
                created_at TEXT NOT NULL DEFAULT (datetime('now')),
                FOREIGN KEY (universe_id, object_id)
                    REFERENCES objects(universe_id, id) ON DELETE CASCADE
            )
            "#,
        )
        .execute(&self.pool)
        .await?;

        // Heartbeats (recurring): one row per object; a beat writes nothing
        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS heartbeats (
                universe_id TEXT NOT NULL,
                object_id TEXT NOT NULL,
                interval_ms INTEGER NOT NULL,
                PRIMARY KEY (universe_id, object_id),
                FOREIGN KEY (universe_id, object_id)
                    REFERENCES objects(universe_id, id) ON DELETE CASCADE
            )
            "#,
        )
        .execute(&self.pool)
        .await?;

        sqlx::query(ACTIONS_TABLE).execute(&self.pool).await?;
        sqlx::query(CLUSTER_NODES_TABLE).execute(&self.pool).await?;
        sqlx::query(ONLINE_PLAYERS_TABLE)
            .execute(&self.pool)
            .await?;
        sqlx::query(PAYMENT_REQUESTS_TABLE)
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

        // Active effects table (status effect persistence)
        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS active_effects (
                universe_id TEXT NOT NULL,
                entity_id TEXT NOT NULL,
                effect_type TEXT NOT NULL,
                expires_at INTEGER NOT NULL,
                period_ms INTEGER,
                next_pulse_at INTEGER,
                magnitude INTEGER NOT NULL DEFAULT 0,
                damage_type TEXT,
                source_id TEXT,
                PRIMARY KEY (universe_id, entity_id, effect_type),
                FOREIGN KEY (universe_id, entity_id)
                    REFERENCES objects(universe_id, id) ON DELETE CASCADE
            )
            "#,
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
