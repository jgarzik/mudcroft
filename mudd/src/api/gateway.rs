//! Gateways (docs/cluster-design.md, Gateways)
//!
//! A cluster node that is not the primary still accepts players and API
//! calls, but runs nothing itself:
//! - `/auth/*` and `/universe/*` requests are forwarded to the primary over
//!   the cluster port and answered with its response;
//! - a game connection is relayed to a session on the primary (one cluster
//!   connection per player). If that connection drops without the primary
//!   ending the session, the primary has failed over: the player is told,
//!   and the gateway reopens the session on the new primary. A command in
//!   flight at that moment may or may not have taken effect; it is never
//!   retried.
//!
//! The primary serves forwarded requests with its own router, and
//! forwarded sessions exactly like local ones.

use std::sync::Arc;
use std::time::Duration;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Request, State};
use axum::http::{HeaderName, HeaderValue, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::Router;
use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
use openraft::async_runtime::WatchReceiver;
use tracing::{info, warn};

use super::websocket::{ServerMessage, WsParams};
use super::AppState;
use crate::raft::transport::{
    self, GameFrame, GameHook, GameOpen, HttpHook, HttpRequest, HttpResponse, PeerClient,
};

/// Marks a request a gateway forwarded: a node that receives one and is not
/// the primary either answers 503 instead of forwarding it again
const FORWARDED: &str = "x-mudd-forwarded";

/// Largest forwarded request body (a universe upload)
const MAX_BODY: usize = 64 * 1024 * 1024;

/// How long a gateway waits for a primary (an election) before giving up
const PRIMARY_WAIT: Duration = Duration::from_secs(30);

/// Headers that travel with a forwarded request or response
const PASSED_HEADERS: [&str; 2] = ["authorization", "content-type"];

/// The primary's cluster address (this node's own if it is the primary)
fn primary_addr(state: &AppState) -> Option<String> {
    let metrics = state.raft_writer.raft().metrics().borrow_watched().clone();
    let leader = metrics.current_leader?;
    // A node that stepped down can still name itself as leader
    if leader == metrics.id && metrics.state != openraft::ServerState::Leader {
        return None;
    }
    metrics
        .membership_config
        .membership()
        .get_node(&leader)
        .map(|n| n.addr.clone())
        .filter(|a| !a.is_empty())
}

fn plain(status: StatusCode, text: &str) -> Response {
    (status, text.to_string()).into_response()
}

/// Middleware on the API routes: run locally on the primary, forward
/// elsewhere
pub async fn forward_to_primary(
    State(state): State<AppState>,
    request: Request,
    next: Next,
) -> Response {
    let Some((_, security)) = state.raft_writer.cluster() else {
        return next.run(request).await;
    };
    if state.primary_active()
        || (state.raft_writer.is_leader()
            && state.wait_primary_active(Duration::from_secs(5)).await)
    {
        return next.run(request).await;
    }
    if state.raft_writer.is_leader() {
        return plain(
            StatusCode::SERVICE_UNAVAILABLE,
            "the primary is starting; try again",
        );
    }
    if request.headers().contains_key(FORWARDED) {
        return plain(
            StatusCode::SERVICE_UNAVAILABLE,
            "not the primary; try again",
        );
    }
    let Some(addr) = primary_addr(&state) else {
        return plain(
            StatusCode::SERVICE_UNAVAILABLE,
            "no primary right now; try again",
        );
    };
    let (parts, body) = request.into_parts();
    let body = match axum::body::to_bytes(body, MAX_BODY).await {
        Ok(body) => body,
        Err(_) => return plain(StatusCode::PAYLOAD_TOO_LARGE, "request body too large"),
    };
    let forwarded = HttpRequest {
        method: parts.method.to_string(),
        uri: parts
            .uri
            .path_and_query()
            .map(|p| p.to_string())
            .unwrap_or_else(|| "/".to_string()),
        headers: PASSED_HEADERS
            .iter()
            .filter_map(|name| {
                parts
                    .headers
                    .get(*name)
                    .and_then(|v| v.to_str().ok())
                    .map(|v| (name.to_string(), v.to_string()))
            })
            .collect(),
        body: BASE64.encode(&body),
    };
    match PeerClient::new(addr, security.clone())
        .http(&forwarded)
        .await
    {
        Ok(response) => into_response(response),
        Err(e) => {
            // The path only: a query string can carry a token
            warn!(
                "gateway: forwarding {} {} failed: {}",
                parts.method,
                parts.uri.path(),
                e
            );
            plain(
                StatusCode::BAD_GATEWAY,
                "the primary did not answer; try again",
            )
        }
    }
}

fn into_response(forwarded: HttpResponse) -> Response {
    let status = StatusCode::from_u16(forwarded.status).unwrap_or(StatusCode::BAD_GATEWAY);
    let body = BASE64.decode(forwarded.body).unwrap_or_default();
    let mut response = (status, body).into_response();
    for (name, value) in forwarded.headers {
        if let (Ok(name), Ok(value)) = (HeaderName::try_from(name), HeaderValue::try_from(value)) {
            response.headers_mut().insert(name, value);
        }
    }
    response
}

/// On the primary: serve a forwarded request with the node's own router
async fn serve_forwarded(app: Router, forwarded: HttpRequest) -> HttpResponse {
    use tower::ServiceExt;
    let failed = |status: StatusCode| HttpResponse {
        status: status.as_u16(),
        headers: Vec::new(),
        body: String::new(),
    };
    let Ok(body) = BASE64.decode(&forwarded.body) else {
        return failed(StatusCode::BAD_REQUEST);
    };
    let mut builder = axum::http::Request::builder()
        .method(forwarded.method.as_str())
        .uri(forwarded.uri.as_str())
        .header(FORWARDED, "1");
    for (name, value) in &forwarded.headers {
        if PASSED_HEADERS.contains(&name.as_str()) {
            builder = builder.header(name.as_str(), value.as_str());
        }
    }
    let Ok(request) = builder.body(axum::body::Body::from(body)) else {
        return failed(StatusCode::BAD_REQUEST);
    };
    let response = match app.oneshot(request).await {
        Ok(response) => response,
        Err(never) => match never {},
    };
    let status = response.status().as_u16();
    let headers = response
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .map(|v| vec![("content-type".to_string(), v.to_string())])
        .unwrap_or_default();
    match axum::body::to_bytes(response.into_body(), MAX_BODY).await {
        Ok(body) => HttpResponse {
            status,
            headers,
            body: BASE64.encode(&body),
        },
        Err(_) => failed(StatusCode::INTERNAL_SERVER_ERROR),
    }
}

/// Install the handlers the cluster port uses to serve forwarded requests
/// and sessions (when this node is, or becomes, the primary)
pub fn install_hooks(state: &AppState, app: &Router) {
    let Some((service, _)) = state.raft_writer.cluster() else {
        return;
    };
    let app = app.clone();
    let http: HttpHook = Arc::new(move |request| {
        let app = app.clone();
        Box::pin(serve_forwarded(app, request))
    });
    let session_state = state.clone();
    let game: GameHook = Arc::new(move |open, stream| {
        let state = session_state.clone();
        Box::pin(super::websocket::serve_remote_session(state, open, stream))
    });
    service.set_hooks(http, game);
}

/// `/ws` on a gateway: check the token with the primary (this node's copy
/// may not have a just-registered account yet), then relay
pub async fn ws_upgrade(ws: WebSocketUpgrade, params: WsParams, state: AppState) -> Response {
    let (Some(universe), Some(token)) = (params.universe, params.token) else {
        return plain(
            StatusCode::BAD_REQUEST,
            "Universe and token required: ?universe=<id>&token=<token>",
        );
    };
    let universe = match crate::universe::validate_universe_id(&universe) {
        Ok(id) => id,
        Err(e) => {
            return plain(
                StatusCode::BAD_REQUEST,
                &format!("Invalid universe ID: {}", e),
            )
        }
    };
    let Some((_, security)) = state.raft_writer.cluster().cloned() else {
        return plain(StatusCode::SERVICE_UNAVAILABLE, "not a cluster member");
    };
    let Some(addr) = primary_addr(&state) else {
        return plain(
            StatusCode::SERVICE_UNAVAILABLE,
            "no primary right now; try again",
        );
    };
    let check = HttpRequest {
        method: "GET".to_string(),
        uri: format!("/auth/validate?token={}", urlencoding_minimal(&token)),
        headers: Vec::new(),
        body: String::new(),
    };
    // /auth/validate answers 200 either way: the body says whether it is
    match PeerClient::new(addr, security).http(&check).await {
        Ok(response) if response.status == 200 && token_valid(&response) => {}
        Ok(_) => {
            return plain(
                StatusCode::UNAUTHORIZED,
                "A valid token is required: ?token=<token>",
            )
        }
        Err(_) => {
            return plain(
                StatusCode::BAD_GATEWAY,
                "the primary did not answer; try again",
            )
        }
    }
    let open = GameOpen { universe, token };
    ws.on_upgrade(move |socket| relay(socket, state, open))
}

fn token_valid(response: &HttpResponse) -> bool {
    BASE64
        .decode(&response.body)
        .ok()
        .and_then(|body| serde_json::from_slice::<serde_json::Value>(&body).ok())
        .and_then(|v| v["valid"].as_bool())
        .unwrap_or(false)
}

/// Percent-encode a token for a query string (tokens are hex; anything else
/// is encoded byte by byte)
fn urlencoding_minimal(value: &str) -> String {
    value
        .bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{:02X}", b),
        })
        .collect()
}

enum Outcome {
    /// The player left
    ClientGone,
    /// The primary ended the session on purpose
    Ended,
    /// The connection to the primary dropped: a failover
    Lost,
    /// The node was not (yet) the primary, or dropped the connection before
    /// any message: try again quietly
    NotReady,
}

async fn send_json(socket: &mut WebSocket, msg: &ServerMessage) -> bool {
    match serde_json::to_string(msg) {
        Ok(json) => socket.send(Message::Text(json.into())).await.is_ok(),
        Err(_) => true,
    }
}

/// Relay a player's connection to their session on the primary, following
/// failovers
async fn relay(mut socket: WebSocket, state: AppState, open: GameOpen) {
    let Some((_, security)) = state.raft_writer.cluster().cloned() else {
        return;
    };
    // Until a session is open, try whichever node is primary now: one that
    // died refuses quickly, and an election names the next
    let mut deadline = tokio::time::Instant::now() + PRIMARY_WAIT;
    // The player was told about the current outage
    let mut notified = false;
    loop {
        let opened = match primary_addr(&state) {
            Some(addr) => transport::open_game_session(&addr, &security, &open)
                .await
                .map(|remote| (addr, remote))
                .map_err(|e| e.to_string()),
            None => Err("no primary".to_string()),
        };
        let (addr, mut remote) = match opened {
            Ok(opened) => opened,
            Err(e) if tokio::time::Instant::now() < deadline => {
                tracing::debug!("gateway: no session yet: {}", e);
                tokio::time::sleep(Duration::from_millis(200)).await;
                continue;
            }
            Err(e) => {
                warn!("gateway: giving up on a primary: {}", e);
                let _ = send_json(
                    &mut socket,
                    &ServerMessage::Error {
                        message: "The world is unavailable (no primary). Try again later."
                            .to_string(),
                    },
                )
                .await;
                return;
            }
        };
        // Whether this session got anything from the primary yet
        let mut live = false;
        let outcome = loop {
            tokio::select! {
                incoming = socket.recv() => match incoming {
                    Some(Ok(Message::Text(text))) => {
                        if !remote.send(&text).await {
                            break Outcome::Lost;
                        }
                    }
                    Some(Ok(Message::Close(_))) | None => break Outcome::ClientGone,
                    _ => {}
                },
                frame = remote.recv() => match frame {
                    Some(GameFrame::Text(text)) => {
                        if !live {
                            live = true;
                            notified = false;
                        }
                        if socket.send(Message::Text(text.into())).await.is_err() {
                            break Outcome::ClientGone;
                        }
                    }
                    Some(GameFrame::End(reason)) if reason == transport::NOT_PRIMARY => {
                        break Outcome::NotReady
                    }
                    Some(GameFrame::End(_)) => break Outcome::Ended,
                    None if live => break Outcome::Lost,
                    None => break Outcome::NotReady,
                },
            }
        };
        match outcome {
            // Dropping `remote` closes the session on the primary, as a
            // closed WebSocket would
            Outcome::ClientGone => return,
            Outcome::Ended => {
                let _ = socket.send(Message::Close(None)).await;
                return;
            }
            Outcome::NotReady => {
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
            Outcome::Lost => {
                info!("gateway: lost the primary at {}; reconnecting", addr);
                deadline = tokio::time::Instant::now() + PRIMARY_WAIT;
                if notified {
                    // Once per outage
                    tokio::time::sleep(Duration::from_millis(200)).await;
                    continue;
                }
                notified = true;
                let still_here = send_json(
                    &mut socket,
                    &ServerMessage::Output {
                        text: "The world shimmers as the servers change hands. Your last \
                               command may not have taken effect."
                            .to_string(),
                    },
                )
                .await;
                if !still_here {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
        }
    }
}
