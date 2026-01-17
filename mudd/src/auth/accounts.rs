//! Account management service
//!
//! Handles account creation, authentication, and token management.

use sqlx::sqlite::SqlitePool;
use thiserror::Error;

use super::{generate_salt, generate_token, hash_password, verify_password};

/// Account data
#[derive(Debug, Clone)]
pub struct Account {
    pub id: String,
    pub username: String,
    pub access_level: String,
    pub created_at: String,
}

/// Authentication errors
#[derive(Debug, Error)]
pub enum AuthError {
    #[error("username already exists")]
    UsernameExists,

    #[error("invalid credentials")]
    InvalidCredentials,

    #[error("account not found")]
    AccountNotFound,

    #[error("database error: {0}")]
    Database(#[from] sqlx::Error),
}

/// Account service for authentication operations
pub struct AccountService {
    pool: SqlitePool,
}

impl AccountService {
    /// Create a new account service
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    /// Create a new account
    pub async fn create_account(
        &self,
        username: &str,
        password: &str,
    ) -> Result<(Account, String), AuthError> {
        // Check if username already exists
        let existing: Option<(String,)> =
            sqlx::query_as("SELECT id FROM accounts WHERE username = ?")
                .bind(username)
                .fetch_optional(&self.pool)
                .await?;

        if existing.is_some() {
            return Err(AuthError::UsernameExists);
        }

        // Generate credentials
        let id = uuid::Uuid::new_v4().to_string();
        let salt = generate_salt();
        let password_hash = hash_password(password, &salt);
        let token = generate_token();
        let now = chrono::Utc::now().to_rfc3339();

        // Insert account
        sqlx::query(
            "INSERT INTO accounts (id, username, password_hash, salt, token, access_level, created_at)
             VALUES (?, ?, ?, ?, ?, 'player', ?)",
        )
        .bind(&id)
        .bind(username)
        .bind(&password_hash)
        .bind(&salt)
        .bind(&token)
        .bind(&now)
        .execute(&self.pool)
        .await?;

        let account = Account {
            id,
            username: username.to_string(),
            access_level: "player".to_string(),
            created_at: now,
        };

        Ok((account, token))
    }

    /// Login with username and password, returns token
    pub async fn login(&self, username: &str, password: &str) -> Result<(Account, String), AuthError> {
        // Get account with password info
        let row: Option<(String, String, String, String, String)> = sqlx::query_as(
            "SELECT id, password_hash, salt, access_level, created_at FROM accounts WHERE username = ?",
        )
        .bind(username)
        .fetch_optional(&self.pool)
        .await?;

        let (id, stored_hash, salt, access_level, created_at) =
            row.ok_or(AuthError::InvalidCredentials)?;

        // Verify password
        if !verify_password(password, &salt, &stored_hash) {
            return Err(AuthError::InvalidCredentials);
        }

        // Generate new token
        let token = generate_token();

        // Update token in database
        sqlx::query("UPDATE accounts SET token = ? WHERE id = ?")
            .bind(&token)
            .bind(&id)
            .execute(&self.pool)
            .await?;

        let account = Account {
            id,
            username: username.to_string(),
            access_level,
            created_at,
        };

        Ok((account, token))
    }

    /// Validate a token and return the associated account
    pub async fn validate_token(&self, token: &str) -> Result<Option<Account>, AuthError> {
        let row: Option<(String, String, String, String)> = sqlx::query_as(
            "SELECT id, username, access_level, created_at FROM accounts WHERE token = ?",
        )
        .bind(token)
        .fetch_optional(&self.pool)
        .await?;

        Ok(row.map(|(id, username, access_level, created_at)| Account {
            id,
            username,
            access_level,
            created_at,
        }))
    }

    /// Logout by clearing the token
    pub async fn logout(&self, token: &str) -> Result<bool, AuthError> {
        let result = sqlx::query("UPDATE accounts SET token = NULL WHERE token = ?")
            .bind(token)
            .execute(&self.pool)
            .await?;

        Ok(result.rows_affected() > 0)
    }

    /// Get account by ID
    pub async fn get_account(&self, id: &str) -> Result<Option<Account>, AuthError> {
        let row: Option<(String, String, String, String)> = sqlx::query_as(
            "SELECT id, username, access_level, created_at FROM accounts WHERE id = ?",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;

        Ok(row.map(|(id, username, access_level, created_at)| Account {
            id,
            username,
            access_level,
            created_at,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::sqlite::SqlitePoolOptions;

    async fn test_pool() -> SqlitePool {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();

        // Run migrations
        sqlx::query(
            "CREATE TABLE accounts (
                id TEXT PRIMARY KEY,
                username TEXT UNIQUE NOT NULL,
                password_hash TEXT,
                salt TEXT,
                token TEXT,
                access_level TEXT NOT NULL DEFAULT 'player',
                created_at TEXT NOT NULL
            )",
        )
        .execute(&pool)
        .await
        .unwrap();

        pool
    }

    #[tokio::test]
    async fn test_account_create() {
        let pool = test_pool().await;
        let service = AccountService::new(pool);

        let (account, token) = service
            .create_account("testuser", "password123")
            .await
            .unwrap();

        assert_eq!(account.username, "testuser");
        assert_eq!(account.access_level, "player");
        assert!(!token.is_empty());
    }

    #[tokio::test]
    async fn test_account_create_duplicate() {
        let pool = test_pool().await;
        let service = AccountService::new(pool);

        service
            .create_account("testuser", "password123")
            .await
            .unwrap();

        let result = service.create_account("testuser", "different").await;
        assert!(matches!(result, Err(AuthError::UsernameExists)));
    }

    #[tokio::test]
    async fn test_login_success() {
        let pool = test_pool().await;
        let service = AccountService::new(pool);

        service
            .create_account("testuser", "password123")
            .await
            .unwrap();

        let (account, token) = service.login("testuser", "password123").await.unwrap();

        assert_eq!(account.username, "testuser");
        assert!(!token.is_empty());
    }

    #[tokio::test]
    async fn test_login_wrong_password() {
        let pool = test_pool().await;
        let service = AccountService::new(pool);

        service
            .create_account("testuser", "password123")
            .await
            .unwrap();

        let result = service.login("testuser", "wrongpassword").await;
        assert!(matches!(result, Err(AuthError::InvalidCredentials)));
    }

    #[tokio::test]
    async fn test_login_nonexistent_user() {
        let pool = test_pool().await;
        let service = AccountService::new(pool);

        let result = service.login("nouser", "password").await;
        assert!(matches!(result, Err(AuthError::InvalidCredentials)));
    }

    #[tokio::test]
    async fn test_validate_token() {
        let pool = test_pool().await;
        let service = AccountService::new(pool);

        let (_, token) = service
            .create_account("testuser", "password123")
            .await
            .unwrap();

        let account = service.validate_token(&token).await.unwrap();
        assert!(account.is_some());
        assert_eq!(account.unwrap().username, "testuser");
    }

    #[tokio::test]
    async fn test_validate_invalid_token() {
        let pool = test_pool().await;
        let service = AccountService::new(pool);

        let account = service.validate_token("invalidtoken").await.unwrap();
        assert!(account.is_none());
    }

    #[tokio::test]
    async fn test_logout() {
        let pool = test_pool().await;
        let service = AccountService::new(pool);

        let (_, token) = service
            .create_account("testuser", "password123")
            .await
            .unwrap();

        // Token should be valid
        assert!(service.validate_token(&token).await.unwrap().is_some());

        // Logout
        let logged_out = service.logout(&token).await.unwrap();
        assert!(logged_out);

        // Token should be invalid now
        assert!(service.validate_token(&token).await.unwrap().is_none());
    }
}
