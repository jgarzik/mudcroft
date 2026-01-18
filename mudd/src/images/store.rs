//! Content-addressed image storage
//!
//! Images are stored by their SHA-256 hash, enabling:
//! - Deduplication (same image content = same hash)
//! - Immutable caching (hash never changes)
//! - Portable storage (images in SQLite database)

use std::sync::Arc;

use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
use sha2::{Digest, Sha256};
use sqlx::SqlitePool;
use tracing::{debug, warn};

use crate::raft::RaftWriter;

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
    raft_writer: Arc<RaftWriter>,
}

impl ImageStore {
    /// Create a new image store
    pub fn new(pool: SqlitePool, raft_writer: Arc<RaftWriter>) -> Self {
        Self { pool, raft_writer }
    }

    /// Compute SHA-256 hash of data
    fn compute_hash(data: &[u8]) -> String {
        let mut hasher = Sha256::new();
        hasher.update(data);
        format!("{:x}", hasher.finalize())
    }

    /// Store image by hash (deduplication)
    ///
    /// Binary data is base64 encoded for Raft replication.
    /// The "blob:" prefix signals the execute_sql to decode as binary.
    pub async fn store(
        &self,
        data: &[u8],
        mime_type: &str,
        source: &str,
    ) -> Result<String, String> {
        let hash = Self::compute_hash(data);
        // Pre-compute timestamp for deterministic replication
        let created_at = chrono::Utc::now().to_rfc3339();
        // Base64 encode binary data with "blob:" prefix for execute_sql to decode
        let data_b64 = format!("blob:{}", BASE64.encode(data));

        self.raft_writer
            .execute(
                "INSERT INTO image_store (hash, data, mime_type, size_bytes, source, created_at) VALUES (?, ?, ?, ?, ?, ?) ON CONFLICT(hash) DO UPDATE SET reference_count = reference_count + 1",
                vec![
                    serde_json::json!(&hash),
                    serde_json::json!(&data_b64),
                    serde_json::json!(mime_type),
                    serde_json::json!(data.len() as i64),
                    serde_json::json!(source),
                    serde_json::json!(&created_at),
                ],
            )
            .await
            .map_err(|e| format!("Failed to store image: {}", e))?;

        debug!("Stored image with hash {} ({} bytes)", hash, data.len());
        Ok(hash)
    }

    /// Get image by hash
    pub async fn get(&self, hash: &str) -> Result<Option<ImageData>, String> {
        let row: Option<(String, Vec<u8>, String)> =
            sqlx::query_as("SELECT hash, data, mime_type FROM image_store WHERE hash = ?")
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
        let result: Result<Option<(i32,)>, _> =
            sqlx::query_as("SELECT 1 FROM image_store WHERE hash = ?")
                .bind(hash)
                .fetch_optional(&self.pool)
                .await;

        matches!(result, Ok(Some(_)))
    }

    /// Delete image by hash (only if reference_count is 0)
    pub async fn delete(&self, hash: &str) -> Result<bool, String> {
        let result = self
            .raft_writer
            .execute(
                "DELETE FROM image_store WHERE hash = ? AND reference_count <= 0",
                vec![serde_json::json!(hash)],
            )
            .await
            .map_err(|e| format!("Failed to delete image: {}", e))?;

        if result.rows_affected > 0 {
            debug!("Deleted image with hash {}", hash);
            Ok(true)
        } else {
            warn!(
                "Image {} not deleted (has references or doesn't exist)",
                hash
            );
            Ok(false)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Note: store/delete tests require RaftWriter and are covered by integration tests

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
