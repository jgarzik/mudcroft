//! Authentication module
//!
//! Provides token-based authentication for the mudd server.

pub mod accounts;

use rand::Rng;
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

/// Generate a random salt for password hashing
pub fn generate_salt() -> String {
    let random_bytes: [u8; 16] = rand::rng().random();
    hex::encode(random_bytes)
}

/// Hash a password with a salt
pub fn hash_password(password: &str, salt: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(salt.as_bytes());
    hasher.update(password.as_bytes());
    hex::encode(hasher.finalize())
}

/// Verify a password against a stored hash
pub fn verify_password(password: &str, salt: &str, hash: &str) -> bool {
    hash_password(password, salt) == hash
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

    #[test]
    fn test_salt_generation() {
        let salt1 = generate_salt();
        let salt2 = generate_salt();

        // Salt should be 32 hex chars (128 bits)
        assert_eq!(salt1.len(), 32);
        assert_eq!(salt2.len(), 32);

        // Salts should be unique
        assert_ne!(salt1, salt2);
    }

    #[test]
    fn test_password_hash_deterministic() {
        let password = "secret123";
        let salt = "abcd1234";

        let hash1 = hash_password(password, salt);
        let hash2 = hash_password(password, salt);

        // Same password + salt = same hash
        assert_eq!(hash1, hash2);
    }

    #[test]
    fn test_password_hash_different_salts() {
        let password = "secret123";
        let salt1 = "salt1";
        let salt2 = "salt2";

        let hash1 = hash_password(password, salt1);
        let hash2 = hash_password(password, salt2);

        // Different salts = different hashes
        assert_ne!(hash1, hash2);
    }

    #[test]
    fn test_verify_password() {
        let password = "mysecret";
        let salt = generate_salt();
        let hash = hash_password(password, &salt);

        assert!(verify_password(password, &salt, &hash));
        assert!(!verify_password("wrongpassword", &salt, &hash));
    }
}
