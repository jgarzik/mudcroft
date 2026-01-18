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
    pub fn new(pool: SqlitePool, raft_writer: Option<Arc<RaftWriter>>) -> Self {
        Self { pool, raft_writer }
    }

    /// Execute a write operation either through Raft (if available) or directly
    async fn execute_write(&self, sql: &str, params: Vec<serde_json::Value>) -> Result<u64> {
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

    /// Get an object by ID
    pub async fn get(&self, id: &str) -> Result<Option<Object>> {
        let row: Option<ObjectRow> = sqlx::query_as(
            r#"
            SELECT id, universe_id, class, parent_id, properties, code_hash, owner_id, created_at, updated_at
            FROM objects WHERE id = ?
            "#,
        )
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
            "UPDATE objects SET class = ?, parent_id = ?, properties = ?, code_hash = ?, updated_at = ? WHERE id = ?",
            vec![
                serde_json::json!(&obj.class),
                serde_json::json!(&obj.parent_id),
                serde_json::json!(&properties),
                serde_json::json!(&obj.code_hash),
                serde_json::json!(&updated_at),
                serde_json::json!(&obj.id),
            ],
        )
        .await?;

        Ok(())
    }

    /// Delete an object
    pub async fn delete(&self, id: &str) -> Result<bool> {
        let rows = self
            .execute_write(
                "DELETE FROM objects WHERE id = ?",
                vec![serde_json::json!(id)],
            )
            .await?;

        Ok(rows > 0)
    }

    /// Get all objects with a given parent (contents of a room/container)
    pub async fn get_contents(&self, parent_id: &str) -> Result<Vec<Object>> {
        let rows: Vec<ObjectRow> = sqlx::query_as(
            r#"
            SELECT id, universe_id, class, parent_id, properties, code_hash, owner_id, created_at, updated_at
            FROM objects WHERE parent_id = ?
            "#,
        )
        .bind(parent_id)
        .fetch_all(&self.pool)
        .await?;

        rows.into_iter().map(|r| r.into_object()).collect()
    }

    /// Move an object to a new parent
    pub async fn move_object(&self, id: &str, new_parent_id: Option<&str>) -> Result<()> {
        // Pre-compute timestamp for deterministic replication
        let updated_at = chrono::Utc::now().to_rfc3339();

        self.execute_write(
            "UPDATE objects SET parent_id = ?, updated_at = ? WHERE id = ?",
            vec![
                serde_json::json!(new_parent_id),
                serde_json::json!(&updated_at),
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

    /// Find object by name property in a given location
    pub async fn find_by_name(&self, parent_id: &str, name: &str) -> Result<Option<Object>> {
        // SQLite JSON extraction
        let rows: Vec<ObjectRow> = sqlx::query_as(
            r#"
            SELECT id, universe_id, class, parent_id, properties, code_hash, owner_id, created_at, updated_at
            FROM objects
            WHERE parent_id = ? AND json_extract(properties, '$.name') = ?
            "#,
        )
        .bind(parent_id)
        .bind(name)
        .fetch_all(&self.pool)
        .await?;

        match rows.into_iter().next() {
            Some(r) => Ok(Some(r.into_object()?)),
            None => Ok(None),
        }
    }

    /// Get the environment (parent) of an object
    pub async fn get_environment(&self, obj_id: &str) -> Result<Option<Object>> {
        let obj = self.get(obj_id).await?;
        match obj {
            Some(o) => match &o.parent_id {
                Some(parent_id) => self.get(parent_id).await,
                None => Ok(None),
            },
            None => Ok(None),
        }
    }

    /// Get all living entities (players, npcs) in a location
    pub async fn get_living_in(&self, parent_id: &str) -> Result<Vec<Object>> {
        let rows: Vec<ObjectRow> = sqlx::query_as(
            r#"
            SELECT id, universe_id, class, parent_id, properties, code_hash, owner_id, created_at, updated_at
            FROM objects
            WHERE parent_id = ? AND (class = 'player' OR class = 'npc' OR class = 'living')
            "#,
        )
        .bind(parent_id)
        .fetch_all(&self.pool)
        .await?;

        rows.into_iter().map(|r| r.into_object()).collect()
    }

    /// Get room exit destination
    pub async fn get_exit(&self, room_id: &str, direction: &str) -> Result<Option<String>> {
        let room = self.get(room_id).await?;
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
    pub async fn set_exit(&self, room_id: &str, direction: &str, dest_room_id: &str) -> Result<()> {
        let room = self.get(room_id).await?;
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
    pub async fn remove_exit(&self, room_id: &str, direction: &str) -> Result<()> {
        let room = self.get(room_id).await?;
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
}
