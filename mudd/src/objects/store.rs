//! Object persistence and CRUD operations

use std::sync::Arc;

use anyhow::Result;
use sha2::{Digest, Sha256};
use sqlx::SqlitePool;

use super::{Object, Properties};
use crate::raft::RaftWriter;

/// Object storage with database backing
pub struct ObjectStore {
    pool: SqlitePool,
    raft_writer: Option<Arc<RaftWriter>>,
}

impl ObjectStore {
    /// Create a new object store with the given connection pool
    /// The database pool (reads)
    pub fn pool(&self) -> &SqlitePool {
        &self.pool
    }

    pub fn new(pool: SqlitePool, raft_writer: Option<Arc<RaftWriter>>) -> Self {
        Self { pool, raft_writer }
    }

    /// Execute a write operation either through Raft (if available) or directly
    async fn execute_write(
        &self,
        sql: &'static str,
        params: Vec<serde_json::Value>,
    ) -> Result<u64> {
        if let Some(ref raft_writer) = self.raft_writer {
            let result = raft_writer.execute(sql, params).await?;
            Ok(result.rows_affected)
        } else {
            // Direct execution fallback (for tests)
            let mut query = sqlx::query(sql);
            for param in &params {
                match param {
                    serde_json::Value::String(s) => query = query.bind(s.clone()),
                    serde_json::Value::Number(n) => {
                        if let Some(i) = n.as_i64() {
                            query = query.bind(i);
                        } else if let Some(f) = n.as_f64() {
                            query = query.bind(f);
                        }
                    }
                    serde_json::Value::Bool(b) => query = query.bind(*b),
                    serde_json::Value::Null => query = query.bind(Option::<String>::None),
                    _ => query = query.bind(param.to_string()),
                }
            }
            let result = query.execute(&self.pool).await?;
            Ok(result.rows_affected())
        }
    }

    /// Create a new object in the database
    pub async fn create(&self, obj: &Object) -> Result<()> {
        let properties = serde_json::to_string(&obj.properties)?;

        self.execute_write(
            "INSERT INTO objects (id, universe_id, class, parent_id, properties, code_hash, owner_id, created_at, updated_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
            vec![
                serde_json::json!(&obj.id),
                serde_json::json!(&obj.universe_id),
                serde_json::json!(&obj.class),
                serde_json::json!(&obj.parent_id),
                serde_json::json!(&properties),
                serde_json::json!(&obj.code_hash),
                serde_json::json!(&obj.owner_id),
                serde_json::json!(&obj.created_at),
                serde_json::json!(&obj.updated_at),
            ],
        )
        .await?;

        Ok(())
    }

    /// Get an object by ID within a universe
    pub async fn get(&self, universe_id: &str, id: &str) -> Result<Option<Object>> {
        let row: Option<ObjectRow> = sqlx::query_as(
            r#"
            SELECT id, universe_id, class, parent_id, properties, code_hash, owner_id, created_at, updated_at
            FROM objects WHERE universe_id = ? AND id = ?
            "#,
        )
        .bind(universe_id)
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;

        match row {
            Some(r) => Ok(Some(r.into_object()?)),
            None => Ok(None),
        }
    }

    /// Update an existing object
    pub async fn update(&self, obj: &Object) -> Result<()> {
        let properties = serde_json::to_string(&obj.properties)?;
        // Pre-compute timestamp for deterministic replication
        let updated_at = chrono::Utc::now().to_rfc3339();

        self.execute_write(
            "UPDATE objects SET class = ?, parent_id = ?, properties = ?, code_hash = ?, updated_at = ? WHERE universe_id = ? AND id = ?",
            vec![
                serde_json::json!(&obj.class),
                serde_json::json!(&obj.parent_id),
                serde_json::json!(&properties),
                serde_json::json!(&obj.code_hash),
                serde_json::json!(&updated_at),
                serde_json::json!(&obj.universe_id),
                serde_json::json!(&obj.id),
            ],
        )
        .await?;

        Ok(())
    }

    /// One statement that sets some properties of an object in place
    /// (`json_set`), leaving the rest of the row untouched: unlike `update`
    /// it cannot lose a concurrent change to other properties, and it can
    /// go into an atomic batch with other statements.
    ///
    /// The JSON path is bound (quoted), so any property name works except
    /// one containing `"`, `\` or a control character.
    pub fn set_properties_statement<K: AsRef<str>>(
        universe_id: &str,
        id: &str,
        props: &[(K, serde_json::Value)],
    ) -> Result<(String, Vec<serde_json::Value>)> {
        let mut sql = String::from("UPDATE objects SET properties = json_set(properties");
        let mut params = Vec::with_capacity(props.len() * 2 + 3);
        for (key, value) in props {
            let key = key.as_ref();
            if key.is_empty()
                || key.len() > 255
                || key.chars().any(|c| c == '"' || c == '\\' || c.is_control())
            {
                anyhow::bail!("invalid property name {:?}", key);
            }
            sql.push_str(", ?, json(?)");
            params.push(serde_json::json!(format!("$.\"{}\"", key)));
            params.push(serde_json::json!(value.to_string()));
        }
        sql.push_str("), updated_at = ? WHERE universe_id = ? AND id = ?");
        params.push(serde_json::json!(chrono::Utc::now().to_rfc3339()));
        params.push(serde_json::json!(universe_id));
        params.push(serde_json::json!(id));
        Ok((sql, params))
    }

    /// Statement setting an object's container (`None`: nowhere)
    pub fn set_parent_statement(
        universe_id: &str,
        id: &str,
        parent_id: Option<&str>,
    ) -> (String, Vec<serde_json::Value>) {
        (
            "UPDATE objects SET parent_id = ?, updated_at = ? WHERE universe_id = ? AND id = ?"
                .to_string(),
            vec![
                serde_json::json!(parent_id),
                serde_json::json!(chrono::Utc::now().to_rfc3339()),
                serde_json::json!(universe_id),
                serde_json::json!(id),
            ],
        )
    }

    /// Statement attaching (or, with `None`, detaching) an object's code
    pub fn set_code_hash_statement(
        universe_id: &str,
        id: &str,
        code_hash: Option<&str>,
    ) -> (String, Vec<serde_json::Value>) {
        (
            "UPDATE objects SET code_hash = ?, updated_at = ? WHERE universe_id = ? AND id = ?"
                .to_string(),
            vec![
                serde_json::json!(code_hash),
                serde_json::json!(chrono::Utc::now().to_rfc3339()),
                serde_json::json!(universe_id),
                serde_json::json!(id),
            ],
        )
    }

    /// Statement deleting an object
    pub fn delete_statement(universe_id: &str, id: &str) -> (String, Vec<serde_json::Value>) {
        (
            "DELETE FROM objects WHERE universe_id = ? AND id = ?".to_string(),
            vec![serde_json::json!(universe_id), serde_json::json!(id)],
        )
    }

    /// Set some properties of an object in place (see
    /// set_properties_statement)
    pub async fn set_properties<K: AsRef<str>>(
        &self,
        universe_id: &str,
        id: &str,
        props: &[(K, serde_json::Value)],
    ) -> Result<()> {
        let statement = Self::set_properties_statement(universe_id, id, props)?;
        self.execute_batch(vec![statement]).await
    }

    /// Run statements atomically: one Raft batch, or one transaction
    /// without Raft (tests)
    pub async fn execute_batch(
        &self,
        statements: Vec<(String, Vec<serde_json::Value>)>,
    ) -> Result<()> {
        if let Some(ref raft_writer) = self.raft_writer {
            return raft_writer.execute_batch(statements).await;
        }
        let mut tx = self.pool.begin().await?;
        for (sql, params) in &statements {
            // Statements come from builders in this crate, values bound
            let mut query = sqlx::query(sqlx::AssertSqlSafe(sql.as_str()));
            for param in params {
                query = match param {
                    serde_json::Value::String(s) => query.bind(s.clone()),
                    serde_json::Value::Number(n) => match n.as_i64() {
                        Some(i) => query.bind(i),
                        None => query.bind(n.as_f64()),
                    },
                    serde_json::Value::Bool(b) => query.bind(*b),
                    serde_json::Value::Null => query.bind(Option::<String>::None),
                    other => query.bind(other.to_string()),
                };
            }
            query.execute(&mut *tx).await?;
        }
        tx.commit().await?;
        Ok(())
    }

    /// Delete an object
    pub async fn delete(&self, universe_id: &str, id: &str) -> Result<bool> {
        let rows = self
            .execute_write(
                "DELETE FROM objects WHERE universe_id = ? AND id = ?",
                vec![serde_json::json!(universe_id), serde_json::json!(id)],
            )
            .await?;

        Ok(rows > 0)
    }

    /// Get all objects with a given parent (contents of a room/container)
    pub async fn get_contents(&self, universe_id: &str, parent_id: &str) -> Result<Vec<Object>> {
        let rows: Vec<ObjectRow> = sqlx::query_as(
            r#"
            SELECT id, universe_id, class, parent_id, properties, code_hash, owner_id, created_at, updated_at
            FROM objects WHERE universe_id = ? AND parent_id = ?
            "#,
        )
        .bind(universe_id)
        .bind(parent_id)
        .fetch_all(&self.pool)
        .await?;

        rows.into_iter().map(|r| r.into_object()).collect()
    }

    /// Move an object to a new parent
    pub async fn move_object(
        &self,
        universe_id: &str,
        id: &str,
        new_parent_id: Option<&str>,
    ) -> Result<()> {
        // Pre-compute timestamp for deterministic replication
        let updated_at = chrono::Utc::now().to_rfc3339();

        self.execute_write(
            "UPDATE objects SET parent_id = ?, updated_at = ? WHERE universe_id = ? AND id = ?",
            vec![
                serde_json::json!(new_parent_id),
                serde_json::json!(&updated_at),
                serde_json::json!(universe_id),
                serde_json::json!(id),
            ],
        )
        .await?;

        Ok(())
    }

    /// Get objects by class in a universe
    pub async fn get_by_class(&self, universe_id: &str, class: &str) -> Result<Vec<Object>> {
        let rows: Vec<ObjectRow> = sqlx::query_as(
            r#"
            SELECT id, universe_id, class, parent_id, properties, code_hash, owner_id, created_at, updated_at
            FROM objects WHERE universe_id = ? AND class = ?
            "#,
        )
        .bind(universe_id)
        .bind(class)
        .fetch_all(&self.pool)
        .await?;

        rows.into_iter().map(|r| r.into_object()).collect()
    }

    /// Store code by content hash (content-addressed storage)
    pub async fn store_code(&self, source: &str) -> Result<String> {
        let hash = Self::hash_code(source);
        // Pre-compute timestamp for deterministic replication
        let created_at = chrono::Utc::now().to_rfc3339();

        // Insert or ignore if already exists
        self.execute_write(
            "INSERT OR IGNORE INTO code_store (hash, source, created_at) VALUES (?, ?, ?)",
            vec![
                serde_json::json!(&hash),
                serde_json::json!(source),
                serde_json::json!(&created_at),
            ],
        )
        .await?;

        Ok(hash)
    }

    /// Get code by hash
    pub async fn get_code(&self, hash: &str) -> Result<Option<String>> {
        let row: Option<(String,)> = sqlx::query_as("SELECT source FROM code_store WHERE hash = ?")
            .bind(hash)
            .fetch_optional(&self.pool)
            .await?;

        Ok(row.map(|(s,)| s))
    }

    /// Compute SHA-256 hash of code
    pub fn hash_code(source: &str) -> String {
        let mut hasher = Sha256::new();
        hasher.update(source.as_bytes());
        hex::encode(hasher.finalize())
    }

    /// Find object by name property in a given location.
    /// Does case-insensitive partial matching: "sword" matches "Rusty Short Sword".
    pub async fn find_by_name(
        &self,
        universe_id: &str,
        parent_id: &str,
        name: &str,
    ) -> Result<Option<Object>> {
        // SQLite JSON extraction with case-insensitive LIKE matching.
        // The search term is matched literally (its own % and _ escaped) and
        // wrapped in % for partial matching.
        let escaped = name
            .replace('\\', "\\\\")
            .replace('%', "\\%")
            .replace('_', "\\_");
        let search_pattern = format!("%{}%", escaped);
        let rows: Vec<ObjectRow> = sqlx::query_as(
            r#"
            SELECT id, universe_id, class, parent_id, properties, code_hash, owner_id, created_at, updated_at
            FROM objects
            WHERE universe_id = ? AND parent_id = ?
              AND lower(json_extract(properties, '$.name')) LIKE lower(?) ESCAPE '\'
            "#,
        )
        .bind(universe_id)
        .bind(parent_id)
        .bind(&search_pattern)
        .fetch_all(&self.pool)
        .await?;

        match rows.into_iter().next() {
            Some(r) => Ok(Some(r.into_object()?)),
            None => Ok(None),
        }
    }

    /// Get the environment (parent) of an object
    pub async fn get_environment(&self, universe_id: &str, obj_id: &str) -> Result<Option<Object>> {
        let obj = self.get(universe_id, obj_id).await?;
        match obj {
            Some(o) => match &o.parent_id {
                Some(parent_id) => self.get(universe_id, parent_id).await,
                None => Ok(None),
            },
            None => Ok(None),
        }
    }

    /// Get all living entities (players, npcs) in a location
    pub async fn get_living_in(&self, universe_id: &str, parent_id: &str) -> Result<Vec<Object>> {
        let rows: Vec<ObjectRow> = sqlx::query_as(
            r#"
            SELECT id, universe_id, class, parent_id, properties, code_hash, owner_id, created_at, updated_at
            FROM objects
            WHERE universe_id = ? AND parent_id = ? AND (class = 'player' OR class = 'npc' OR class = 'living')
            "#,
        )
        .bind(universe_id)
        .bind(parent_id)
        .fetch_all(&self.pool)
        .await?;

        rows.into_iter().map(|r| r.into_object()).collect()
    }

    /// Get room exit destination
    pub async fn get_exit(
        &self,
        universe_id: &str,
        room_id: &str,
        direction: &str,
    ) -> Result<Option<String>> {
        let room = self.get(universe_id, room_id).await?;
        match room {
            Some(r) => {
                if let Some(exits) = r.properties.get("exits") {
                    if let Some(exits_obj) = exits.as_object() {
                        if let Some(dest) = exits_obj.get(direction) {
                            return Ok(dest.as_str().map(|s| s.to_string()));
                        }
                    }
                }
                Ok(None)
            }
            None => Ok(None),
        }
    }

    /// Set a room exit
    pub async fn set_exit(
        &self,
        universe_id: &str,
        room_id: &str,
        direction: &str,
        dest_room_id: &str,
    ) -> Result<()> {
        let room = self.get(universe_id, room_id).await?;
        if let Some(mut r) = room {
            let exits = r
                .properties
                .entry("exits".to_string())
                .or_insert_with(|| serde_json::json!({}));

            if let Some(exits_obj) = exits.as_object_mut() {
                exits_obj.insert(direction.to_string(), serde_json::json!(dest_room_id));
            }

            self.update(&r).await?;
        }
        Ok(())
    }

    /// Remove a room exit
    pub async fn remove_exit(
        &self,
        universe_id: &str,
        room_id: &str,
        direction: &str,
    ) -> Result<()> {
        let room = self.get(universe_id, room_id).await?;
        if let Some(mut r) = room {
            if let Some(exits) = r.properties.get_mut("exits") {
                if let Some(exits_obj) = exits.as_object_mut() {
                    exits_obj.remove(direction);
                }
            }
            self.update(&r).await?;
        }
        Ok(())
    }

    /// Check if a universe exists
    pub async fn universe_exists(&self, universe_id: &str) -> Result<bool> {
        let row: Option<(i64,)> = sqlx::query_as("SELECT 1 FROM universes WHERE id = ?")
            .bind(universe_id)
            .fetch_optional(&self.pool)
            .await?;
        Ok(row.is_some())
    }

    /// Get universe config by ID
    pub async fn get_universe(&self, universe_id: &str) -> Result<Option<UniverseInfo>> {
        let row: Option<UniverseRow> = sqlx::query_as(
            r#"
            SELECT id, name, owner_id, config, created_at
            FROM universes WHERE id = ?
            "#,
        )
        .bind(universe_id)
        .fetch_optional(&self.pool)
        .await?;

        match row {
            Some(r) => Ok(Some(r.into_info()?)),
            None => Ok(None),
        }
    }

    /// List all universes (basic info only - id and name)
    pub async fn list_universes(&self) -> Result<Vec<(String, String)>> {
        let rows: Vec<(String, String)> =
            sqlx::query_as("SELECT id, name FROM universes ORDER BY name")
                .fetch_all(&self.pool)
                .await?;
        Ok(rows)
    }

    /// Update universe config (merge with existing)
    pub async fn update_universe(
        &self,
        universe_id: &str,
        config: serde_json::Value,
    ) -> Result<bool> {
        // First get existing config
        let existing = self.get_universe(universe_id).await?;
        match existing {
            Some(mut info) => {
                // Merge new config into existing
                if let (Some(existing_obj), Some(new_obj)) =
                    (info.config.as_object_mut(), config.as_object())
                {
                    for (k, v) in new_obj {
                        existing_obj.insert(k.clone(), v.clone());
                    }
                }

                let config_str = serde_json::to_string(&info.config)?;
                self.execute_write(
                    "UPDATE universes SET config = ? WHERE id = ?",
                    vec![
                        serde_json::json!(&config_str),
                        serde_json::json!(universe_id),
                    ],
                )
                .await?;
                Ok(true)
            }
            None => Ok(false),
        }
    }

    /// Create a new universe
    pub async fn create_universe(
        &self,
        id: &str,
        name: &str,
        owner_id: &str,
        config: serde_json::Value,
    ) -> Result<()> {
        let config_str = serde_json::to_string(&config)?;
        // Pre-compute timestamp for deterministic replication
        let created_at = chrono::Utc::now().to_rfc3339();

        self.execute_write(
            "INSERT INTO universes (id, name, owner_id, config, created_at) VALUES (?, ?, ?, ?, ?)",
            vec![
                serde_json::json!(id),
                serde_json::json!(name),
                serde_json::json!(owner_id),
                serde_json::json!(&config_str),
                serde_json::json!(&created_at),
            ],
        )
        .await?;

        Ok(())
    }

    /// Get a universe setting by key
    pub async fn get_universe_setting(
        &self,
        universe_id: &str,
        key: &str,
    ) -> Result<Option<String>> {
        let row: Option<(String,)> =
            sqlx::query_as("SELECT value FROM universe_settings WHERE universe_id = ? AND key = ?")
                .bind(universe_id)
                .bind(key)
                .fetch_optional(&self.pool)
                .await?;

        Ok(row.map(|(v,)| v))
    }

    /// Set a universe setting
    pub async fn set_universe_setting(
        &self,
        universe_id: &str,
        key: &str,
        value: &str,
    ) -> Result<()> {
        self.execute_write(
            "INSERT OR REPLACE INTO universe_settings (universe_id, key, value) VALUES (?, ?, ?)",
            vec![
                serde_json::json!(universe_id),
                serde_json::json!(key),
                serde_json::json!(value),
            ],
        )
        .await?;

        Ok(())
    }

    /// Get portal room ID for a universe
    pub async fn get_portal(&self, universe_id: &str) -> Result<Option<String>> {
        self.get_universe_setting(universe_id, "portal_room_id")
            .await
    }

    /// Set portal room ID for a universe
    pub async fn set_portal(&self, universe_id: &str, room_id: &str) -> Result<()> {
        self.set_universe_setting(universe_id, "portal_room_id", room_id)
            .await
    }

    /// Get all core lib hashes (canonical mudlib versions)
    pub async fn get_core_lib_hashes(&self) -> Result<std::collections::HashMap<String, String>> {
        let rows: Vec<(String, String)> = sqlx::query_as("SELECT name, hash FROM core_lib_hashes")
            .fetch_all(&self.pool)
            .await?;
        Ok(rows.into_iter().collect())
    }

    /// Store a core lib hash (used during init)
    pub async fn store_core_lib_hash(&self, name: &str, hash: &str) -> Result<()> {
        let updated_at = chrono::Utc::now().to_rfc3339();
        self.execute_write(
            "INSERT OR REPLACE INTO core_lib_hashes (name, hash, updated_at) VALUES (?, ?, ?)",
            vec![
                serde_json::json!(name),
                serde_json::json!(hash),
                serde_json::json!(&updated_at),
            ],
        )
        .await?;
        Ok(())
    }
}

/// Row type for SQLite queries
#[derive(sqlx::FromRow)]
struct ObjectRow {
    id: String,
    universe_id: String,
    class: String,
    parent_id: Option<String>,
    properties: String,
    code_hash: Option<String>,
    owner_id: Option<String>,
    created_at: String,
    updated_at: String,
}

impl ObjectRow {
    fn into_object(self) -> Result<Object> {
        let properties: Properties = serde_json::from_str(&self.properties)?;
        Ok(Object {
            id: self.id,
            universe_id: self.universe_id,
            class: self.class,
            parent_id: self.parent_id,
            properties,
            code_hash: self.code_hash,
            owner_id: self.owner_id,
            created_at: self.created_at,
            updated_at: self.updated_at,
        })
    }
}

/// Universe information
#[derive(Debug, Clone)]
pub struct UniverseInfo {
    pub id: String,
    pub name: String,
    pub owner_id: String,
    pub config: serde_json::Value,
    pub created_at: String,
}

/// Row type for universe queries
#[derive(sqlx::FromRow)]
struct UniverseRow {
    id: String,
    name: String,
    owner_id: String,
    config: String,
    created_at: String,
}

impl UniverseRow {
    fn into_info(self) -> Result<UniverseInfo> {
        let config: serde_json::Value =
            serde_json::from_str(&self.config).unwrap_or_else(|_| serde_json::json!({}));
        Ok(UniverseInfo {
            id: self.id,
            name: self.name,
            owner_id: self.owner_id,
            config,
            created_at: self.created_at,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_hash_code() {
        let hash1 = ObjectStore::hash_code("function on_init() end");
        let hash2 = ObjectStore::hash_code("function on_init() end");
        let hash3 = ObjectStore::hash_code("function on_init() return true end");

        assert_eq!(hash1, hash2);
        assert_ne!(hash1, hash3);
        assert_eq!(hash1.len(), 64); // SHA-256 = 64 hex chars
    }

    /// set_properties changes only the named properties, in place
    #[tokio::test]
    async fn test_set_properties_in_place() {
        let pool = crate::db::test_utils::test_pool().await;
        sqlx::query(
            "INSERT INTO accounts (id, username, password_hash, access_level, created_at)
             VALUES ('acct', 'acct', 'x', 'admin', '2026-01-01')",
        )
        .execute(&pool)
        .await
        .unwrap();
        let store = ObjectStore::new(pool, None);
        store
            .create_universe("u", "U", "acct", serde_json::json!({}))
            .await
            .unwrap();
        let mut obj = Object::new("/npcs/rat", "u", "npc").unwrap();
        obj.set_property("name", serde_json::json!("rat"));
        obj.set_property("hp", serde_json::json!(10));
        store.create(&obj).await.unwrap();

        store
            .set_properties(
                "u",
                "/npcs/rat",
                &[
                    ("hp", serde_json::json!(3)),
                    ("angry", serde_json::json!(true)),
                ],
            )
            .await
            .unwrap();
        let rat = store.get("u", "/npcs/rat").await.unwrap().unwrap();
        assert_eq!(rat.properties["hp"], 3);
        assert_eq!(rat.properties["angry"], true);
        assert_eq!(rat.properties["name"], "rat", "other properties untouched");

        // Any name works (the path is bound, quoted), except ones that
        // could not be quoted
        store
            .set_properties("u", "/npcs/rat", &[("max-hp.v2", serde_json::json!(7))])
            .await
            .unwrap();
        let rat = store.get("u", "/npcs/rat").await.unwrap().unwrap();
        assert_eq!(rat.properties["max-hp.v2"], 7);
        for bad in ["", "q\"r", "back\\slash", "nl\n"] {
            assert!(ObjectStore::set_properties_statement(
                "u",
                "/npcs/rat",
                &[(bad, serde_json::json!(1))]
            )
            .is_err());
        }
    }
}
