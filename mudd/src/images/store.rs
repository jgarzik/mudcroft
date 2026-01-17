//! Content-addressed image storage
//!
//! Images are stored by their SHA-256 hash, enabling:
//! - Deduplication (same image content = same hash)
//! - Immutable caching (hash never changes)
//! - Portable storage (images in SQLite database)

use sha2::{Digest, Sha256};
use sqlx::SqlitePool;
use tracing::{debug, warn};

/// Image data from storage
#[derive(Debug, Clone)]
pub struct ImageData {
    pub hash: String,
    pub data: Vec<u8>,
    pub mime_type: String,
}

/// Content-addressed image store
#[derive(Clone)]
pub struct ImageStore {
    pool: SqlitePool,
}

impl ImageStore {
    /// Create a new image store
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    /// Initialize the image_store table
    pub async fn init(&self) -> Result<(), sqlx::Error> {
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

        Ok(())
    }

    /// Compute SHA-256 hash of data
    fn compute_hash(data: &[u8]) -> String {
        let mut hasher = Sha256::new();
        hasher.update(data);
        format!("{:x}", hasher.finalize())
    }

    /// Store image by hash (deduplication)
    pub async fn store(
        &self,
        data: &[u8],
        mime_type: &str,
        source: &str,
    ) -> Result<String, String> {
        let hash = Self::compute_hash(data);

        sqlx::query(
            r#"
            INSERT INTO image_store (hash, data, mime_type, size_bytes, source, created_at)
            VALUES (?, ?, ?, ?, ?, datetime('now'))
            ON CONFLICT(hash) DO UPDATE SET reference_count = reference_count + 1
            "#,
        )
        .bind(&hash)
        .bind(data)
        .bind(mime_type)
        .bind(data.len() as i64)
        .bind(source)
        .execute(&self.pool)
        .await
        .map_err(|e| format!("Failed to store image: {}", e))?;

        debug!("Stored image with hash {} ({} bytes)", hash, data.len());
        Ok(hash)
    }

    /// Get image by hash
    pub async fn get(&self, hash: &str) -> Result<Option<ImageData>, String> {
        let row: Option<(String, Vec<u8>, String)> = sqlx::query_as(
            "SELECT hash, data, mime_type FROM image_store WHERE hash = ?",
        )
        .bind(hash)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| format!("Failed to get image: {}", e))?;

        Ok(row.map(|(hash, data, mime_type)| ImageData {
            hash,
            data,
            mime_type,
        }))
    }

    /// Download from URL and store locally
    pub async fn store_from_url(&self, url: &str) -> Result<String, String> {
        debug!("Downloading image from: {}", url);

        let response = reqwest::get(url)
            .await
            .map_err(|e| format!("Failed to fetch image: {}", e))?;

        if !response.status().is_success() {
            return Err(format!("HTTP error: {}", response.status()));
        }

        let mime_type = response
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("image/png")
            .to_string();

        let data = response
            .bytes()
            .await
            .map_err(|e| format!("Failed to read image bytes: {}", e))?;

        self.store(&data, &mime_type, "venice").await
    }

    /// Check if an image exists
    pub async fn exists(&self, hash: &str) -> bool {
        let result: Result<Option<(i32,)>, _> = sqlx::query_as(
            "SELECT 1 FROM image_store WHERE hash = ?",
        )
        .bind(hash)
        .fetch_optional(&self.pool)
        .await;

        matches!(result, Ok(Some(_)))
    }

    /// Delete image by hash (only if reference_count is 0)
    pub async fn delete(&self, hash: &str) -> Result<bool, String> {
        let result = sqlx::query(
            "DELETE FROM image_store WHERE hash = ? AND reference_count <= 0",
        )
        .bind(hash)
        .execute(&self.pool)
        .await
        .map_err(|e| format!("Failed to delete image: {}", e))?;

        if result.rows_affected() > 0 {
            debug!("Deleted image with hash {}", hash);
            Ok(true)
        } else {
            warn!("Image {} not deleted (has references or doesn't exist)", hash);
            Ok(false)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::sqlite::SqlitePoolOptions;

    async fn test_pool() -> SqlitePool {
        SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn test_store_and_get() {
        let pool = test_pool().await;
        let store = ImageStore::new(pool);
        store.init().await.unwrap();

        let data = b"test image data";
        let hash = store.store(data, "image/png", "test").await.unwrap();

        assert!(!hash.is_empty());
        assert!(store.exists(&hash).await);

        let retrieved = store.get(&hash).await.unwrap().unwrap();
        assert_eq!(retrieved.data, data);
        assert_eq!(retrieved.mime_type, "image/png");
    }

    #[tokio::test]
    async fn test_deduplication() {
        let pool = test_pool().await;
        let store = ImageStore::new(pool);
        store.init().await.unwrap();

        let data = b"same data twice";
        let hash1 = store.store(data, "image/png", "test1").await.unwrap();
        let hash2 = store.store(data, "image/png", "test2").await.unwrap();

        // Same content = same hash
        assert_eq!(hash1, hash2);
    }

    #[tokio::test]
    async fn test_hash_computation() {
        let hash = ImageStore::compute_hash(b"test");
        // Known SHA-256 of "test"
        assert_eq!(
            hash,
            "9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08"
        );
    }
}
