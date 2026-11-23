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

use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// How long a token stays valid (renewed by each login that presents it)
pub const DEVICE_TOKEN_TTL: Duration = Duration::from_secs(30 * 24 * 60 * 60);

/// Most tokens one account keeps (its devices); a new one replaces its
/// oldest, so no account can push out another's
pub const PER_ACCOUNT: usize = 10;

/// Most tokens kept in all; when full (after dropping expired ones) no new
/// token is issued, rather than evicting anyone's
const MAX_TOKENS: usize = 200_000;

#[derive(Default)]
struct Tokens {
    /// username (lowercased) -> its tokens, oldest first
    by_account: HashMap<String, VecDeque<(String, Instant)>>,
    /// token -> username, for revocation by token
    index: HashMap<String, String>,
}

impl Tokens {
    fn drop_expired(&mut self, now: Instant) {
        let index = &mut self.index;
        self.by_account.retain(|_, tokens| {
            tokens.retain(|(token, expires)| {
                let live = *expires > now;
                if !live {
                    index.remove(token);
                }
                live
            });
            !tokens.is_empty()
        });
    }
}

/// Device tokens in memory
pub struct DeviceTokens {
    capacity: usize,
    tokens: Mutex<Tokens>,
}

impl Default for DeviceTokens {
    fn default() -> Self {
        Self::with_capacity(MAX_TOKENS)
    }
}

impl DeviceTokens {
    fn with_capacity(capacity: usize) -> Self {
        Self {
            capacity,
            tokens: Mutex::new(Tokens::default()),
        }
    }

    /// A new token for `username`, or None if the store is full of live
    /// tokens (the device is then simply not remembered)
    pub fn issue(&self, username: &str) -> Option<String> {
        self.issue_with_ttl(username, DEVICE_TOKEN_TTL)
    }

    fn issue_with_ttl(&self, username: &str, ttl: Duration) -> Option<String> {
        let now = Instant::now();
        let name = username.to_lowercase();
        let mut tokens = self.tokens.lock().unwrap_or_else(|e| e.into_inner());
        let at_cap = tokens
            .by_account
            .get(&name)
            .is_some_and(|t| t.len() >= PER_ACCOUNT);
        if !at_cap && tokens.index.len() >= self.capacity {
            tokens.drop_expired(now);
            if tokens.index.len() >= self.capacity {
                return None;
            }
        }
        let token = crate::auth::generate_token();
        let Tokens { by_account, index } = &mut *tokens;
        let mine = by_account.entry(name.clone()).or_default();
        while mine.len() >= PER_ACCOUNT {
            if let Some((old, _)) = mine.pop_front() {
                index.remove(&old);
            }
        }
        mine.push_back((token.clone(), now + ttl));
        index.insert(token.clone(), name);
        Some(token)
    }

    /// Whether `token` is a valid, unexpired token for `username`
    pub fn verify(&self, username: &str, token: &str) -> bool {
        let tokens = self.tokens.lock().unwrap_or_else(|e| e.into_inner());
        tokens
            .by_account
            .get(&username.to_lowercase())
            .is_some_and(|mine| {
                mine.iter()
                    .any(|(t, expires)| t == token && *expires > Instant::now())
            })
    }

    /// Forget a token (logout, or replaced by a renewal)
    pub fn revoke(&self, token: &str) {
        let mut tokens = self.tokens.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(name) = tokens.index.remove(token)
            && let Some(mine) = tokens.by_account.get_mut(&name)
        {
            mine.retain(|(t, _)| t != token);
            if mine.is_empty() {
                tokens.by_account.remove(&name);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_device_tokens() {
        let devices = DeviceTokens::default();
        let token = devices.issue("Alice").unwrap();
        assert!(
            devices.verify("alice", &token),
            "usernames are case-insensitive"
        );
        assert!(!devices.verify("bob", &token), "bound to one account");
        assert!(!devices.verify("alice", "made-up"));
        devices.revoke(&token);
        assert!(!devices.verify("alice", &token), "revoked");

        let short = devices
            .issue_with_ttl("alice", Duration::from_millis(0))
            .unwrap();
        std::thread::sleep(Duration::from_millis(5));
        assert!(!devices.verify("alice", &short), "expired");
    }

    /// One account cannot push out another's tokens: each account keeps at
    /// most PER_ACCOUNT (its oldest goes), and a full store issues no more
    /// rather than evicting anyone
    #[test]
    fn test_no_displacement() {
        let devices = DeviceTokens::with_capacity(4);
        let owner = devices.issue("owner").expect("issued");
        let mut mine = Vec::new();
        for _ in 0..(PER_ACCOUNT + 3) {
            mine.push(devices.issue("flooder"));
        }
        assert!(
            devices.verify("owner", &owner),
            "untouched by another account"
        );
        let kept = mine
            .iter()
            .flatten()
            .filter(|t| devices.verify("flooder", t))
            .count();
        assert!(kept <= PER_ACCOUNT.min(3));
        // Store full (capacity 4): new accounts get nothing, nobody is evicted
        let full = DeviceTokens::with_capacity(2);
        let a = full.issue("a").unwrap();
        let b = full.issue("b").unwrap();
        assert!(full.issue("c").is_none());
        assert!(full.verify("a", &a) && full.verify("b", &b));
    }
}
