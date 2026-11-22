//! Device tokens: proof that a client has logged in to an account before
//!
//! A successful login (or registration) hands the client a random token for
//! that username; presenting it at the next login renews it. Over the
//! per-username failure budget, only logins with a valid token skip the slow
//! queue, so attackers spraying from fresh addresses cannot use the owner's
//! fast lane, and cannot lock the owner out either.
//!
//! Kept in memory only (a user decision): like rate-limit buckets they are
//! ephemeral, and a restart or failover simply forgets them (every device is
//! unknown again until its next login). Logins run on the primary, so the
//! primary holds them.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// How long a token stays valid (renewed by each login that presents it)
pub const DEVICE_TOKEN_TTL: Duration = Duration::from_secs(30 * 24 * 60 * 60);

/// Most tokens kept; beyond this, expired ones go first, then the oldest
const MAX_TOKENS: usize = 200_000;

/// Device tokens in memory: token -> (username, lowercased; expiry)
#[derive(Default)]
pub struct DeviceTokens {
    tokens: Mutex<HashMap<String, (String, Instant)>>,
}

impl DeviceTokens {
    /// A new token for `username`
    pub fn issue(&self, username: &str) -> String {
        self.issue_with_ttl(username, DEVICE_TOKEN_TTL)
    }

    fn issue_with_ttl(&self, username: &str, ttl: Duration) -> String {
        let token = crate::auth::generate_token();
        let now = Instant::now();
        let mut tokens = self.tokens.lock().unwrap_or_else(|e| e.into_inner());
        if tokens.len() >= MAX_TOKENS {
            tokens.retain(|_, (_, expires)| *expires > now);
            if tokens.len() >= MAX_TOKENS {
                let mut expiries: Vec<Instant> = tokens.values().map(|(_, e)| *e).collect();
                expiries.sort_unstable();
                let cutoff = expiries[expiries.len() / 2];
                tokens.retain(|_, (_, expires)| *expires > cutoff);
            }
        }
        tokens.insert(token.clone(), (username.to_lowercase(), now + ttl));
        token
    }

    /// Whether `token` is a valid, unexpired token for `username`
    pub fn verify(&self, username: &str, token: &str) -> bool {
        let tokens = self.tokens.lock().unwrap_or_else(|e| e.into_inner());
        tokens.get(token).is_some_and(|(name, expires)| {
            *name == username.to_lowercase() && *expires > Instant::now()
        })
    }

    /// Forget a token (logout, or replaced by a renewal)
    pub fn revoke(&self, token: &str) {
        self.tokens
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(token);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_device_tokens() {
        let devices = DeviceTokens::default();
        let token = devices.issue("Alice");
        assert!(
            devices.verify("alice", &token),
            "usernames are case-insensitive"
        );
        assert!(!devices.verify("bob", &token), "bound to one account");
        assert!(!devices.verify("alice", "made-up"));
        devices.revoke(&token);
        assert!(!devices.verify("alice", &token), "revoked");

        let short = devices.issue_with_ttl("alice", Duration::from_millis(0));
        std::thread::sleep(Duration::from_millis(5));
        assert!(!devices.verify("alice", &short), "expired");
    }
}
