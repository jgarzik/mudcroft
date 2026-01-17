//! Object persistence and CRUD operations

use anyhow::Result;
use sha2::{Digest, Sha256};
use sqlx::SqlitePool;

use super::{Object, Properties};

/// Object storage with database backing
pub struct ObjectStore {
    pool: SqlitePool,
}

impl ObjectStore {
    /// Create a new object store with the given connection pool
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    /// Create a new object in the database
    pub async fn create(&self, obj: &Object) -> Result<()> {
        let properties = serde_json::to_string(&obj.properties)?;

        sqlx::query(
            r#"
            INSERT INTO objects (id, universe_id, class, parent_id, properties, code_hash, created_at, updated_at)
            VALUES (?, ?, ?, ?, ?, ?, ?, ?)
            "#,
        )
        .bind(&obj.id)
        .bind(&obj.universe_id)
        .bind(&obj.class)
        .bind(&obj.parent_id)
        .bind(&properties)
        .bind(&obj.code_hash)
        .bind(&obj.created_at)
        .bind(&obj.updated_at)
        .execute(&self.pool)
        .await?;

        Ok(())
    }

    /// Get an object by ID
    pub async fn get(&self, id: &str) -> Result<Option<Object>> {
        let row: Option<ObjectRow> = sqlx::query_as(
            r#"
            SELECT id, universe_id, class, parent_id, properties, code_hash, created_at, updated_at
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
        let updated_at = chrono::Utc::now().to_rfc3339();

        sqlx::query(
            r#"
            UPDATE objects
            SET class = ?, parent_id = ?, properties = ?, code_hash = ?, updated_at = ?
            WHERE id = ?
            "#,
        )
        .bind(&obj.class)
        .bind(&obj.parent_id)
        .bind(&properties)
        .bind(&obj.code_hash)
        .bind(&updated_at)
        .bind(&obj.id)
        .execute(&self.pool)
        .await?;

        Ok(())
    }

    /// Delete an object
    pub async fn delete(&self, id: &str) -> Result<bool> {
        let result = sqlx::query("DELETE FROM objects WHERE id = ?")
            .bind(id)
            .execute(&self.pool)
            .await?;

        Ok(result.rows_affected() > 0)
    }

    /// Get all objects with a given parent (contents of a room/container)
    pub async fn get_contents(&self, parent_id: &str) -> Result<Vec<Object>> {
        let rows: Vec<ObjectRow> = sqlx::query_as(
            r#"
            SELECT id, universe_id, class, parent_id, properties, code_hash, created_at, updated_at
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
        let updated_at = chrono::Utc::now().to_rfc3339();

        sqlx::query(
            r#"
            UPDATE objects SET parent_id = ?, updated_at = ? WHERE id = ?
            "#,
        )
        .bind(new_parent_id)
        .bind(&updated_at)
        .bind(id)
        .execute(&self.pool)
        .await?;

        Ok(())
    }

    /// Get objects by class in a universe
    pub async fn get_by_class(&self, universe_id: &str, class: &str) -> Result<Vec<Object>> {
        let rows: Vec<ObjectRow> = sqlx::query_as(
            r#"
            SELECT id, universe_id, class, parent_id, properties, code_hash, created_at, updated_at
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

        // Insert or ignore if already exists
        sqlx::query(
            r#"
            INSERT OR IGNORE INTO code_store (hash, source, created_at)
            VALUES (?, ?, datetime('now'))
            "#,
        )
        .bind(&hash)
        .bind(source)
        .execute(&self.pool)
        .await?;

        Ok(hash)
    }

    /// Get code by hash
    pub async fn get_code(&self, hash: &str) -> Result<Option<String>> {
        let row: Option<(String,)> =
            sqlx::query_as("SELECT source FROM code_store WHERE hash = ?")
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
            SELECT id, universe_id, class, parent_id, properties, code_hash, created_at, updated_at
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
            SELECT id, universe_id, class, parent_id, properties, code_hash, created_at, updated_at
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
            let exits = r.properties
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
            created_at: self.created_at,
            updated_at: self.updated_at,
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
