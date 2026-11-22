//! Account management service
//!
//! Handles account creation, authentication, and token management.
//!
//! In the server every account write goes through Raft. Offline tools
//! (`mudd_init`, before any Raft node exists) and unit tests write directly.

use std::sync::Arc;

use serde_json::{json, Value};
use sqlx::sqlite::SqlitePool;
use thiserror::Error;

use crate::raft::RaftWriter;

use super::{generate_token, hash_password, verify_password};

/// Hash a password off the async executor (argon2 is CPU-heavy)
async fn hash_blocking(password: &str) -> Result<String, AuthError> {
    let password = password.to_string();
    tokio::task::spawn_blocking(move || hash_password(&password))
        .await
        .map_err(|e| AuthError::Write(e.to_string()))?
        .map_err(|e| AuthError::Write(format!("password hashing failed: {}", e)))
}

/// Verify a password off the async executor. With no stored hash (unknown
/// user) it still runs a verification against a dummy hash, so the time
/// taken does not reveal whether the account exists.
async fn verify_blocking(password: &str, stored: Option<String>) -> bool {
    static DUMMY: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    let password = password.to_string();
    tokio::task::spawn_blocking(move || {
        let found = stored.is_some();
        let hash = stored.unwrap_or_else(|| {
            DUMMY
                .get_or_init(|| hash_password("dummy password").unwrap_or_default())
                .clone()
        });
        verify_password(&password, &hash) && found
    })
    .await
    .unwrap_or(false)
}

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

    #[error("write failed: {0}")]
    Write(String),
}

/// Account service for authentication operations
pub struct AccountService {
    pool: SqlitePool,
    raft_writer: Option<Arc<RaftWriter>>,
}

impl AccountService {
    /// An account service that writes through Raft (the server)
    pub fn new(pool: SqlitePool, raft_writer: Arc<RaftWriter>) -> Self {
        Self {
            pool,
            raft_writer: Some(raft_writer),
        }
    }

    /// An account service that writes directly to SQLite: only for offline
    /// tools that run without a Raft node (`mudd_init`) and tests
    pub fn offline(pool: SqlitePool) -> Self {
        Self {
            pool,
            raft_writer: None,
        }
    }

    /// Run one write, through Raft when available; returns rows affected
    async fn write(&self, sql: &'static str, params: Vec<Value>) -> Result<u64, AuthError> {
        if let Some(ref raft_writer) = self.raft_writer {
            return raft_writer
                .execute(sql, params)
                .await
                .map(|r| r.rows_affected)
                .map_err(|e| AuthError::Write(e.to_string()));
        }
        let mut query = sqlx::query(sql);
        for param in params {
            query = match param {
                Value::Null => query.bind(Option::<String>::None),
                Value::String(v) => query.bind(v),
                other => query.bind(other.to_string()),
            };
        }
        Ok(query.execute(&self.pool).await?.rows_affected())
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
        let password_hash = hash_blocking(password).await?;
        let token = generate_token();
        let now = chrono::Utc::now().to_rfc3339();

        // Insert account (a concurrent registration of the same name loses
        // on the UNIQUE constraint)
        self.write(
            "INSERT INTO accounts (id, username, password_hash, token, access_level, created_at)
             VALUES (?, ?, ?, ?, 'player', ?)",
            vec![
                json!(id),
                json!(username),
                json!(password_hash),
                json!(token),
                json!(now),
            ],
        )
        .await
        .map_err(|e| match e {
            AuthError::Write(msg) if msg.contains("UNIQUE") => AuthError::UsernameExists,
            AuthError::Database(sqlx::Error::Database(db)) if db.is_unique_violation() => {
                AuthError::UsernameExists
            }
            other => other,
        })?;

        let account = Account {
            id,
            username: username.to_string(),
            access_level: "player".to_string(),
            created_at: now,
        };

        Ok((account, token))
    }

    /// Login with username and password, returns token
    pub async fn login(
        &self,
        username: &str,
        password: &str,
    ) -> Result<(Account, String), AuthError> {
        // Get account with password info
        let row: Option<(String, Option<String>, String, String)> = sqlx::query_as(
            "SELECT id, password_hash, access_level, created_at FROM accounts WHERE username = ?",
        )
        .bind(username)
        .fetch_optional(&self.pool)
        .await?;

        // Verify even when the account does not exist (constant work)
        let stored_hash = row.as_ref().and_then(|r| r.1.clone());
        if !verify_blocking(password, stored_hash).await {
            return Err(AuthError::InvalidCredentials);
        }
        let (id, _, access_level, created_at) = row.ok_or(AuthError::InvalidCredentials)?;

        // Generate new token
        let token = generate_token();

        // Update token in database
        self.write(
            "UPDATE accounts SET token = ? WHERE id = ?",
            vec![json!(token), json!(id)],
        )
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
        let rows = self
            .write(
                "UPDATE accounts SET token = NULL WHERE token = ?",
                vec![json!(token)],
            )
            .await?;
        Ok(rows > 0)
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

    /// Get account by username
    pub async fn get_by_username(&self, username: &str) -> Result<Option<Account>, AuthError> {
        let row: Option<(String, String, String, String)> = sqlx::query_as(
            "SELECT id, username, access_level, created_at FROM accounts WHERE username = ?",
        )
        .bind(username)
        .fetch_optional(&self.pool)
        .await?;

        Ok(row.map(|(id, username, access_level, created_at)| Account {
            id,
            username,
            access_level,
            created_at,
        }))
    }

    /// Create account (simpler alias for create_account, returns just Account)
    pub async fn create(&self, username: &str, password: &str) -> Result<Account, AuthError> {
        let (account, _token) = self.create_account(username, password).await?;
        Ok(account)
    }

    /// Set an account's global access level
    pub async fn set_access_level(&self, account_id: &str, level: &str) -> Result<(), AuthError> {
        let rows = self
            .write(
                "UPDATE accounts SET access_level = ? WHERE id = ?",
                vec![json!(level), json!(account_id)],
            )
            .await?;

        if rows == 0 {
            return Err(AuthError::AccountNotFound);
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::test_utils::test_pool;

    #[tokio::test]
    async fn test_account_create() {
        let pool = test_pool().await;
        let service = AccountService::offline(pool);

        let (account, token) = service
            .create_account("testuser", "password123")
            .await
            .unwrap();

        assert_eq!(account.username, "testuser");
        assert_eq!(account.access_level, "player");
        assert!(!token.is_empty());

        // The password is stored as an argon2id PHC string, never in clear
        let stored: String = sqlx::query_scalar("SELECT password_hash FROM accounts WHERE id = ?")
            .bind(&account.id)
            .fetch_one(&service.pool)
            .await
            .unwrap();
        assert!(stored.starts_with("$argon2id$"), "{}", stored);
        assert!(!stored.contains("password123"));
    }

    #[tokio::test]
    async fn test_account_create_duplicate() {
        let pool = test_pool().await;
        let service = AccountService::offline(pool);

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
        let service = AccountService::offline(pool);

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
        let service = AccountService::offline(pool);

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
        let service = AccountService::offline(pool);

        let result = service.login("nouser", "password").await;
        assert!(matches!(result, Err(AuthError::InvalidCredentials)));
    }

    #[tokio::test]
    async fn test_validate_token() {
        let pool = test_pool().await;
        let service = AccountService::offline(pool);

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
        let service = AccountService::offline(pool);

        let account = service.validate_token("invalidtoken").await.unwrap();
        assert!(account.is_none());
    }

    #[tokio::test]
    async fn test_logout() {
        let pool = test_pool().await;
        let service = AccountService::offline(pool);

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
