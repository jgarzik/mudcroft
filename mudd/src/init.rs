//! Database initialization module
//!
//! Provides one-time database setup functionality for the mudd_init tool.

use std::collections::HashMap;
use std::path::Path;

use anyhow::{bail, Result};
use tracing::info;

use crate::auth::accounts::AccountService;
use crate::db::Database;

/// Initialize or upgrade a game server database (idempotent)
///
/// # Arguments
/// * `path` - Path to the SQLite database file (creates new or upgrades existing)
/// * `admin_username` - Username for the admin account (required for new DB only)
/// * `admin_password` - Password for the admin account (required for new DB only)
/// * `libs` - Map of library name to Lua source code (additional libs)
/// * `core_lib_dir` - Optional path to core mudlib directory (defaults to "lib/")
///
/// # Behavior
/// * If database doesn't exist: creates new DB, requires admin credentials
/// * If database exists: runs migrations, refreshes mudlib, ignores credentials
///
/// # Errors
/// * New database without admin credentials
/// * Password too short (for new database)
/// * Database creation/migration fails
pub async fn init_database(
    path: &Path,
    admin_username: Option<&str>,
    admin_password: Option<&str>,
    libs: HashMap<String, String>,
    core_lib_dir: Option<&Path>,
) -> Result<()> {
    let db_exists = path.exists();

    let db = if db_exists {
        // Upgrade existing database
        info!("Upgrading existing database at {}", path.display());
        Database::open_for_migration(path.to_str().unwrap()).await?
    } else {
        // Create new database - require admin credentials
        let admin_username = admin_username
            .ok_or_else(|| anyhow::anyhow!("Admin username required for new database"))?;
        let admin_password = admin_password
            .ok_or_else(|| anyhow::anyhow!("Admin password required for new database"))?;

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

        db
    };

    // Load and store core mudlib from lib/ directory (always refresh)
    let lib_dir = core_lib_dir.unwrap_or(Path::new("lib"));
    if lib_dir.exists() && lib_dir.is_dir() {
        info!("Loading core mudlib from {}...", lib_dir.display());
        let mut core_lib_count = 0;

        for entry in std::fs::read_dir(lib_dir)? {
            let entry = entry?;
            let entry_path = entry.path();

            if entry_path.extension().map(|e| e == "lua").unwrap_or(false) {
                let name = entry_path
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .ok_or_else(|| anyhow::anyhow!("Invalid lib filename"))?;

                let source = std::fs::read_to_string(&entry_path)?;
                let hash = store_code(db.pool(), &source).await?;

                // Store the core lib hash mapping
                store_core_lib_hash(db.pool(), name, &hash).await?;

                info!("  {} -> {}", name, &hash[..12]);
                core_lib_count += 1;
            }
        }

        if core_lib_count > 0 {
            info!("Loaded {} core mudlib files", core_lib_count);
        }
    } else {
        info!(
            "No core mudlib directory found at {} (skipping)",
            lib_dir.display()
        );
    }

    // Store any additional provided Lua libraries
    if !libs.is_empty() {
        info!("Storing {} additional Lua library files...", libs.len());
        for (name, source) in &libs {
            let hash = store_code(db.pool(), source).await?;
            info!("  {} -> {}", name, &hash[..12]);
        }
    }

    if db_exists {
        info!("Database upgrade complete");
    } else {
        info!("Database initialization complete");
    }
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

/// Store a core lib hash mapping in core_lib_hashes table
async fn store_core_lib_hash(pool: &sqlx::SqlitePool, name: &str, hash: &str) -> Result<()> {
    let updated_at = chrono::Utc::now().to_rfc3339();

    sqlx::query("INSERT OR REPLACE INTO core_lib_hashes (name, hash, updated_at) VALUES (?, ?, ?)")
        .bind(name)
        .bind(hash)
        .bind(&updated_at)
        .execute(pool)
        .await?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[tokio::test]
    async fn test_init_database_creates_new() {
        let temp_dir = TempDir::new().unwrap();
        let db_path = temp_dir.path().join("test.db");

        init_database(
            &db_path,
            Some("admin"),
            Some("password123"),
            HashMap::new(),
            None,
        )
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
    async fn test_init_database_upgrade_existing() {
        let temp_dir = TempDir::new().unwrap();
        let db_path = temp_dir.path().join("test.db");

        // Create first
        init_database(
            &db_path,
            Some("admin"),
            Some("password123"),
            HashMap::new(),
            None,
        )
        .await
        .unwrap();

        // Run again - should succeed (idempotent upgrade)
        let result = init_database(&db_path, None, None, HashMap::new(), None).await;
        assert!(result.is_ok());

        // Verify admin account still exists
        let db = Database::open(db_path.to_str().unwrap()).await.unwrap();
        let service = AccountService::new(db.pool().clone());
        let account = service.get_by_username("admin").await.unwrap().unwrap();
        assert_eq!(account.access_level, "admin");
    }

    #[tokio::test]
    async fn test_init_database_new_requires_credentials() {
        let temp_dir = TempDir::new().unwrap();
        let db_path = temp_dir.path().join("test.db");

        // Try without credentials - should fail
        let result = init_database(&db_path, None, None, HashMap::new(), None).await;
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("username required"));
    }

    #[tokio::test]
    async fn test_init_database_password_validation() {
        let temp_dir = TempDir::new().unwrap();
        let db_path = temp_dir.path().join("test.db");

        let result =
            init_database(&db_path, Some("admin"), Some("short"), HashMap::new(), None).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("8 characters"));
    }

    #[tokio::test]
    async fn test_init_database_with_libs() {
        let temp_dir = TempDir::new().unwrap();
        let db_path = temp_dir.path().join("test.db");
        // Use a non-existent path for core_lib_dir so no core libs are loaded
        let empty_lib_dir = temp_dir.path().join("no_libs");

        let mut libs = HashMap::new();
        libs.insert("test".to_string(), "-- Test lib\nreturn {}".to_string());

        init_database(
            &db_path,
            Some("admin"),
            Some("password123"),
            libs,
            Some(empty_lib_dir.as_path()),
        )
        .await
        .unwrap();

        // Verify lib was stored (only the additional lib, no core libs)
        let db = Database::open(db_path.to_str().unwrap()).await.unwrap();
        let count: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM code_store")
            .fetch_one(db.pool())
            .await
            .unwrap();
        assert_eq!(count.0, 1);
    }
}
