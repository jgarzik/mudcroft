//! Rate limits on login and registration, checked before any password
//! hashing: per client IP (on the node the client reached, before a gateway
//! forwards) and per username (on the primary, where every login runs, so
//! cluster-wide). Ephemeral by nature: kept in memory.

use std::collections::HashMap;
use std::net::SocketAddr;
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
        if self.per_minute == 0 {
            return Ok(());
        }
        let capacity = f64::from(self.per_minute);
        let per_sec = capacity / 60.0;
        let now = Instant::now();
        let mut buckets = self.buckets.lock().unwrap_or_else(|e| e.into_inner());
        if buckets.len() >= MAX_KEYS && !buckets.contains_key(key) {
            buckets.retain(|_, (tokens, at)| {
                (*tokens + now.duration_since(*at).as_secs_f64() * per_sec) < capacity
            });
        }
        let (tokens, at) = buckets.entry(key.to_string()).or_insert((capacity, now));
        *tokens = (*tokens + now.duration_since(*at).as_secs_f64() * per_sec).min(capacity);
        *at = now;
        if *tokens >= 1.0 {
            *tokens -= 1.0;
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

/// Middleware: limit login and registration by client IP. Requests with no
/// client address (forwarded within the cluster) were limited where the
/// client connected.
pub async fn limit_auth_by_ip(
    State(state): State<AppState>,
    request: Request,
    next: Next,
) -> Response {
    let path = request.uri().path();
    if path == "/auth/login" || path == "/auth/register" {
        if let Some(ConnectInfo(addr)) = request.extensions().get::<ConnectInfo<SocketAddr>>() {
            if let Err(retry) = state.auth_by_ip.check(&addr.ip().to_string()) {
                return too_many(retry);
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
    }
}
