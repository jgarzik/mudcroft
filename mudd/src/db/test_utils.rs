//! Shared test utilities for database operations
//!
//! Provides a common test_pool() function that creates an in-memory
//! database with the full schema, eliminating duplicate schema definitions
//! across test modules.

use sqlx::SqlitePool;

use super::Database;

/// Create an in-memory test database pool with full schema
///
/// Uses Database::new(None) to create a complete database with all
/// migrations applied, ensuring tests run against the same schema
/// as production.
pub async fn test_pool() -> SqlitePool {
    let db = Database::new(None)
        .await
        .expect("Failed to create test database");
    db.pool().clone()
}
