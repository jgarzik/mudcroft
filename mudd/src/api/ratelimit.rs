//! Rate limits on login and registration, checked before any password
//! hashing:
//! - per client (an IPv4 address, or an IPv6 /64), on the node the client
//!   reached, before a gateway forwards;
//! - per username and client, counting failed logins only, on the primary
//!   (where every login runs): keyed with the client too, so nobody can
//!   lock another person out by failing logins under their name.
//!
//! Ephemeral by nature: kept in memory.

use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use axum::extract::{ConnectInfo, Request, State};
use axum::http::{HeaderValue, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};

use super::AppState;

/// Most keys tracked; beyond this, full buckets are forgotten first
const MAX_KEYS: usize = 100_000;

/// Token buckets: `per_minute` requests a minute per key, in bursts of up
/// to `per_minute`. A limit of 0 turns limiting off.
pub struct RateLimiter {
    per_minute: u32,
    buckets: Mutex<HashMap<String, (f64, Instant)>>,
}

impl RateLimiter {
    pub fn new(per_minute: u32) -> Self {
        Self {
            per_minute,
            buckets: Mutex::new(HashMap::new()),
        }
    }

    /// Take one request for `key`: Ok, or how long until one is allowed
    pub fn check(&self, key: &str) -> Result<(), Duration> {
        self.take(key, true)
    }

    /// Whether `key` has a request left, without taking it
    pub fn peek(&self, key: &str) -> Result<(), Duration> {
        self.take(key, false)
    }

    /// Count one request against `key` (e.g. a failed login)
    pub fn charge(&self, key: &str) {
        let _ = self.take(key, true);
    }

    fn take(&self, key: &str, consume: bool) -> Result<(), Duration> {
        if self.per_minute == 0 {
            return Ok(());
        }
        let capacity = f64::from(self.per_minute);
        let per_sec = capacity / 60.0;
        let now = Instant::now();
        let mut buckets = self.buckets.lock().unwrap_or_else(|e| e.into_inner());
        if buckets.len() >= MAX_KEYS && !buckets.contains_key(key) {
            // Forget keys whose buckets have refilled; if that is not
            // enough, the least recently used half
            buckets.retain(|_, (tokens, at)| {
                (*tokens + now.duration_since(*at).as_secs_f64() * per_sec) < capacity
            });
            if buckets.len() >= MAX_KEYS {
                let mut ages: Vec<Instant> = buckets.values().map(|(_, at)| *at).collect();
                ages.sort_unstable();
                let cutoff = ages[ages.len() / 2];
                buckets.retain(|_, (_, at)| *at > cutoff);
            }
        }
        let (tokens, at) = buckets.entry(key.to_string()).or_insert((capacity, now));
        *tokens = (*tokens + now.duration_since(*at).as_secs_f64() * per_sec).min(capacity);
        *at = now;
        if *tokens >= 1.0 {
            if consume {
                *tokens -= 1.0;
            }
            Ok(())
        } else {
            Err(Duration::from_secs_f64((1.0 - *tokens) / per_sec))
        }
    }
}

/// 429 with Retry-After
pub fn too_many(retry: Duration) -> Response {
    let mut response = (
        StatusCode::TOO_MANY_REQUESTS,
        axum::Json(serde_json::json!({"error": "too many attempts; try again later"})),
    )
        .into_response();
    let secs = retry.as_secs().max(1).to_string();
    if let Ok(value) = HeaderValue::from_str(&secs) {
        response.headers_mut().insert("retry-after", value);
    }
    response
}

/// Header a gateway sets on a forwarded request: the client's key (see
/// `client_key`). Trusted only on requests from inside the node (no client
/// address: forwarded through the cluster port), never from a socket.
pub const CLIENT_HEADER: &str = "x-mudd-client";

/// The rate-limit key for a client address: IPv4 as is (IPv4-mapped IPv6
/// too), IPv6 by its /64 (a client usually holds the whole /64)
pub fn client_key(ip: IpAddr) -> String {
    match ip {
        IpAddr::V4(v4) => v4.to_string(),
        IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
            Some(v4) => v4.to_string(),
            None => {
                let s = v6.segments();
                format!("{:x}:{:x}:{:x}:{:x}::/64", s[0], s[1], s[2], s[3])
            }
        },
    }
}

/// Middleware: limit login and registration per client, on the node the
/// client reached (requests forwarded within the cluster were limited
/// there). Records the client for the login handler and for forwarding.
pub async fn limit_auth_by_ip(
    State(state): State<AppState>,
    mut request: Request,
    next: Next,
) -> Response {
    let path = request.uri().path();
    if path == "/auth/login" || path == "/auth/register" {
        let socket = request
            .extensions()
            .get::<ConnectInfo<SocketAddr>>()
            .map(|ConnectInfo(addr)| client_key(addr.ip()));
        if let Some(client) = socket {
            if let Err(retry) = state.auth_by_ip.check(&client) {
                return too_many(retry);
            }
            // The client as this node saw it (never a header a client sent)
            if let Ok(value) = HeaderValue::from_str(&client) {
                request.headers_mut().insert(CLIENT_HEADER, value);
            }
        }
    }
    next.run(request).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_bucket() {
        let limiter = RateLimiter::new(2);
        assert!(limiter.check("a").is_ok());
        assert!(limiter.check("a").is_ok());
        let retry = limiter.check("a").unwrap_err();
        assert!(retry > Duration::from_secs(20) && retry <= Duration::from_secs(30));
        assert!(limiter.check("b").is_ok(), "keys are separate");
        assert!(RateLimiter::new(0).check("a").is_ok());
        // peek does not take, charge does
        let limiter = RateLimiter::new(1);
        assert!(limiter.peek("x").is_ok());
        assert!(limiter.peek("x").is_ok());
        limiter.charge("x");
        assert!(limiter.peek("x").is_err());
    }

    #[test]
    fn test_client_key() {
        assert_eq!(client_key("10.1.2.3".parse().unwrap()), "10.1.2.3");
        assert_eq!(client_key("::ffff:10.1.2.3".parse().unwrap()), "10.1.2.3");
        assert_eq!(
            client_key("2001:db8:1:2:aaaa::1".parse().unwrap()),
            client_key("2001:db8:1:2:bbbb::9".parse().unwrap()),
            "one /64 is one client"
        );
        assert_ne!(
            client_key("2001:db8:1:2::1".parse().unwrap()),
            client_key("2001:db8:1:3::1".parse().unwrap())
        );
    }
}
