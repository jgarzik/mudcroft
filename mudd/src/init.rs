//! Database initialization module
//!
//! Provides one-time database setup functionality for the mudd_init tool.

use std::collections::HashMap;
use std::path::Path;

use anyhow::{bail, Result};
use tracing::info;

use crate::auth::accounts::AccountService;
use crate::db::Database;

/// Initialize a new game server database
///
/// # Arguments
/// * `path` - Path to the SQLite database file (must not exist)
/// * `admin_username` - Username for the admin account
/// * `admin_password` - Password for the admin account (must be >= 8 chars)
/// * `libs` - Map of library name to Lua source code
///
/// # Errors
/// * Database file already exists
/// * Password too short
/// * Database creation fails
pub async fn init_database(
    path: &Path,
    admin_username: &str,
    admin_password: &str,
    libs: HashMap<String, String>,
) -> Result<()> {
    // Fail if database already exists
    if path.exists() {
        bail!(
            "Database file already exists: {}. Remove it first or use a different path.",
            path.display()
        );
    }

    // Validate password
    if admin_password.len() < 8 {
        bail!("Admin password must be at least 8 characters");
    }

    info!("Creating new database at {}", path.display());

    // Create the database (runs migrations)
    let db = Database::new(Some(path.to_str().unwrap())).await?;

    // Create admin account
    let service = AccountService::new(db.pool().clone());
    let account = service.create(admin_username, admin_password).await?;
    info!(
        "Created admin account '{}' ({})",
        admin_username, account.id
    );

    // Promote to admin level
    service.set_access_level(&account.id, "admin").await?;
    info!("Promoted '{}' to admin level", admin_username);

    // Store any provided Lua libraries
    if !libs.is_empty() {
        info!("Storing {} Lua library files...", libs.len());
        for (name, source) in &libs {
            let hash = store_code(db.pool(), source).await?;
            info!("  {} -> {}", name, &hash[..12]);
        }
    }

    info!("Database initialization complete");
    Ok(())
}

/// Store code in the code_store table (content-addressed)
async fn store_code(pool: &sqlx::SqlitePool, source: &str) -> Result<String> {
    use sha2::{Digest, Sha256};

    let mut hasher = Sha256::new();
    hasher.update(source.as_bytes());
    let hash = hex::encode(hasher.finalize());

    // Insert or ignore (content-addressed)
    sqlx::query("INSERT OR IGNORE INTO code_store (hash, source) VALUES (?, ?)")
        .bind(&hash)
        .bind(source)
        .execute(pool)
        .await?;

    Ok(hash)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[tokio::test]
    async fn test_init_database_creates_new() {
        let temp_dir = TempDir::new().unwrap();
        let db_path = temp_dir.path().join("test.db");

        init_database(&db_path, "admin", "password123", HashMap::new())
            .await
            .unwrap();

        // Verify file was created
        assert!(db_path.exists());

        // Verify admin account exists with correct level
        let db = Database::open(db_path.to_str().unwrap()).await.unwrap();
        let service = AccountService::new(db.pool().clone());
        let account = service.get_by_username("admin").await.unwrap().unwrap();
        assert_eq!(account.access_level, "admin");
    }

    #[tokio::test]
    async fn test_init_database_fails_if_exists() {
        let temp_dir = TempDir::new().unwrap();
        let db_path = temp_dir.path().join("test.db");

        // Create first
        init_database(&db_path, "admin", "password123", HashMap::new())
            .await
            .unwrap();

        // Try again - should fail
        let result = init_database(&db_path, "admin", "password123", HashMap::new()).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("already exists"));
    }

    #[tokio::test]
    async fn test_init_database_password_validation() {
        let temp_dir = TempDir::new().unwrap();
        let db_path = temp_dir.path().join("test.db");

        let result = init_database(&db_path, "admin", "short", HashMap::new()).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("8 characters"));
    }

    #[tokio::test]
    async fn test_init_database_with_libs() {
        let temp_dir = TempDir::new().unwrap();
        let db_path = temp_dir.path().join("test.db");

        let mut libs = HashMap::new();
        libs.insert("test".to_string(), "-- Test lib\nreturn {}".to_string());

        init_database(&db_path, "admin", "password123", libs)
            .await
            .unwrap();

        // Verify lib was stored
        let db = Database::open(db_path.to_str().unwrap()).await.unwrap();
        let count: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM code_store")
            .fetch_one(db.pool())
            .await
            .unwrap();
        assert_eq!(count.0, 1);
    }
}
