//! Authentication module
//!
//! Provides token-based authentication for the mudd server.

pub mod accounts;

use rand::RngExt;
use sha2::{Digest, Sha256};

/// Generate a secure random token
pub fn generate_token() -> String {
    let random_bytes: [u8; 32] = rand::rng().random();
    let mut hasher = Sha256::new();
    hasher.update(random_bytes);
    hasher.update(
        chrono::Utc::now()
            .timestamp_nanos_opt()
            .unwrap_or(0)
            .to_le_bytes(),
    );
    hex::encode(hasher.finalize())
}

/// Hash a password for storage: argon2id (default parameters) with a
/// random salt, as a PHC string (`$argon2id$v=19$...`). CPU-heavy: call it
/// off the async executor (`spawn_blocking`).
pub fn hash_password(password: &str) -> Result<String, argon2::password_hash::Error> {
    use argon2::password_hash::PasswordHasher;
    Ok(argon2::Argon2::default()
        .hash_password(password.as_bytes())?
        .to_string())
}

/// Verify a password against a stored PHC hash (constant-time compare).
/// A malformed stored hash never verifies. CPU-heavy, like hash_password.
pub fn verify_password(password: &str, stored: &str) -> bool {
    use argon2::password_hash::{phc::PasswordHash, PasswordVerifier};
    let Ok(hash) = PasswordHash::new(stored) else {
        return false;
    };
    argon2::Argon2::default()
        .verify_password(password.as_bytes(), &hash)
        .is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_token_generation() {
        let token1 = generate_token();
        let token2 = generate_token();

        // Tokens should be 64 hex chars (256 bits)
        assert_eq!(token1.len(), 64);
        assert_eq!(token2.len(), 64);

        // Tokens should be unique
        assert_ne!(token1, token2);
    }

    /// Passwords are stored as argon2id PHC strings with a random salt
    #[test]
    fn test_password_hash_is_argon2id() {
        let hash1 = hash_password("secret123").unwrap();
        let hash2 = hash_password("secret123").unwrap();
        assert!(hash1.starts_with("$argon2id$"), "{}", hash1);
        assert_ne!(hash1, hash2, "each hash has its own salt");
    }

    #[test]
    fn test_verify_password() {
        let hash = hash_password("mysecret").unwrap();
        assert!(verify_password("mysecret", &hash));
        assert!(!verify_password("wrongpassword", &hash));
        // A malformed stored hash never verifies
        assert!(!verify_password("mysecret", "not-a-phc-string"));
        assert!(!verify_password("mysecret", ""));
    }
}
