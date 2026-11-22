//! Script runner: executes Lua for commands, hooks, verbs and timers
//!
//! Every execution gets a fresh sandbox and its own [`MessageQueue`]. The
//! game API contexts (user, room, object) are set before the API is
//! registered, the universe libraries are loaded, and after the script
//! finishes its queued `game.send` / `game.broadcast` messages are
//! delivered to connected players.
//!
//! The Lua state is not `Send`, so each run does all async preparation
//! first, then the synchronous Lua work in a scope that drops the sandbox,
//! and only then awaits message delivery. That keeps the futures `Send`.

use std::sync::Arc;

use mlua::{Function, Table, Value};

use std::future::Future;
use std::pin::Pin;

use super::{AppState, ServerMessage};
use crate::lua::{json_to_lua, GameApi, GameMessage, MessageQueue, Sandbox, SandboxConfig};

/// Who and where a script runs as
#[derive(Debug, Clone, Default)]
pub struct ScriptContext {
    /// Universe the script runs in
    pub universe_id: String,
    /// Acting account (permission checks, object ownership)
    pub user_id: Option<String>,
    /// Room context (room-scoped actions)
    pub room_id: Option<String>,
    /// Object context (timers, object-scoped actions)
    pub object_id: Option<String>,
}

/// Sandbox limits for interactive wizard `eval` and admin scripts, which
/// legitimately build whole areas in one run. Object code (hooks, verbs,
/// timers) uses the stricter `SandboxConfig::default()`.
pub fn eval_config() -> SandboxConfig {
    SandboxConfig {
        max_instructions: 10_000_000,
        timeout: std::time::Duration::from_secs(5),
        max_db_queries: 10_000,
        ..Default::default()
    }
}

/// Run a Lua chunk with `args` bound to its varargs (`...`).
/// Returns the chunk's result rendered as text.
pub async fn run_lua(
    state: &AppState,
    ctx: &ScriptContext,
    code: &str,
    args: Vec<String>,
    config: SandboxConfig,
) -> Result<String, String> {
    let libs = load_universe_lib_codes(state, &ctx.universe_id).await?;
    let queue = MessageQueue::shared();

    let result = {
        let mut sandbox = prepare(state, ctx, &queue, &libs, config)?;
        let value: Result<Value, _> = if args.is_empty() {
            // `execute` also accepts bare expressions (e.g. `1 + 2`)
            sandbox.execute(code)
        } else {
            sandbox.execute_with_args(code, mlua::Variadic::from_iter(args))
        };
        value
            .map(|v| lua_value_to_string(&v))
            .map_err(|e| e.to_string())
    };

    deliver(state, &queue, &ctx.universe_id).await;
    result
}

/// Call handler `method` on an object's attached code (`code_hash`).
///
/// The code runs **as the object's owner** (LPC-style effective user), never
/// as whoever triggered it: the triggering player is only data in `args`.
/// Otherwise code planted by a lower-privileged author would borrow the
/// privileges of anyone who walks into it. Objects without an owner run
/// with no user, so permission-checked writes fail.
///
/// The object's code must evaluate to a table of handler functions; the
/// handler receives `args` converted to a Lua table. Returns `Ok(None)`
/// when the object has no code or no such handler, otherwise the handler's
/// result rendered as text.
pub async fn invoke_handler(
    state: &AppState,
    universe_id: &str,
    object_id: &str,
    room_id: Option<String>,
    method: &str,
    args: serde_json::Value,
) -> Result<Option<String>, String> {
    invoke_handler_at(state, universe_id, object_id, room_id, method, args, 0).await
}

type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// invoke_handler at a hook nesting depth (move hooks run handlers that can
/// move things and so run more hooks; see MAX_MOVE_HOOK_DEPTH)
fn invoke_handler_at<'a>(
    state: &'a AppState,
    universe_id: &'a str,
    object_id: &'a str,
    room_id: Option<String>,
    method: &'a str,
    args: serde_json::Value,
    depth: u32,
) -> BoxFuture<'a, Result<Option<String>, String>> {
    Box::pin(async move {
        let Some((code, owner_id)) = object_code(state, object_id, universe_id).await? else {
            return Ok(None);
        };
        let ctx = ScriptContext {
            universe_id: universe_id.to_string(),
            user_id: owner_id,
            room_id,
            object_id: Some(object_id.to_string()),
        };
        let libs = load_universe_lib_codes(state, universe_id).await?;
        let queue = MessageQueue::shared();

        let result = {
            let mut sandbox = prepare(state, &ctx, &queue, &libs, SandboxConfig::default())?;
            call_handler(&mut sandbox, &code, method, &args)
        };

        deliver_at(state, &queue, &ctx.universe_id, depth).await;
        result
    })
}

/// How often the timer loop checks for due call_outs and heartbeats
const TIMER_TICK: std::time::Duration = std::time::Duration::from_millis(100);

/// Fire due call_out timers and heartbeats forever.
///
/// Each callback runs the named handler on the object's code (as the
/// object's owner, see [`invoke_handler`]) with {object_id, room_id, args}.
pub async fn run_timer_loop(state: AppState) {
    let mut interval = tokio::time::interval(TIMER_TICK);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        interval.tick().await;
        for fired in state.timers.tick().await {
            let room_id = match state
                .object_store
                .get(&fired.universe_id, &fired.object_id)
                .await
            {
                Ok(Some(obj)) => obj.parent_id,
                Ok(None) => {
                    // Object is gone: stop its heartbeat too
                    state
                        .timers
                        .remove_heartbeat(&fired.universe_id, &fired.object_id)
                        .await;
                    continue;
                }
                Err(e) => {
                    tracing::warn!("Timer object {} lookup failed: {}", fired.object_id, e);
                    continue;
                }
            };
            let args = serde_json::json!({
                "object_id": fired.object_id,
                "room_id": room_id,
                "args": fired.args,
            });
            if let Err(e) = invoke_handler(
                &state,
                &fired.universe_id,
                &fired.object_id,
                room_id,
                &fired.method,
                args,
            )
            .await
            {
                tracing::warn!(
                    "Timer {} on {} failed: {}",
                    fired.method,
                    fired.object_id,
                    e
                );
            }
        }
    }
}

/// An object's attached code and its owner, if it has code (objects are
/// looked up within the universe, so foreign objects never run here)
async fn object_code(
    state: &AppState,
    object_id: &str,
    universe_id: &str,
) -> Result<Option<(String, Option<String>)>, String> {
    let obj = match state.object_store.get(universe_id, object_id).await {
        Ok(Some(obj)) => obj,
        Ok(None) => return Ok(None),
        Err(e) => return Err(format!("Failed to load {}: {}", object_id, e)),
    };
    let Some(hash) = obj.code_hash else {
        return Ok(None);
    };
    let code = state
        .object_store
        .get_code(&hash)
        .await
        .map_err(|e| format!("Failed to load code {}: {}", hash, e))?;
    Ok(code.map(|c| (c, obj.owner_id)))
}

/// Evaluate object code to its handler table and call `method`
fn call_handler(
    sandbox: &mut Sandbox,
    code: &str,
    method: &str,
    args: &serde_json::Value,
) -> Result<Option<String>, String> {
    let handlers: Table = sandbox
        .execute(code)
        .map_err(|e| format!("Object code error: {}", e))?;
    let Ok(func) = handlers.get::<Function>(method) else {
        return Ok(None);
    };
    let lua_args = json_to_lua(sandbox.lua(), args).map_err(|e| e.to_string())?;
    let value: Value = sandbox
        .call(func, lua_args)
        .map_err(|e| format!("Handler {} error: {}", method, e))?;
    Ok(Some(lua_value_to_string(&value)))
}

/// Build a sandbox with the game API (contexts set first) and universe libs
fn prepare(
    state: &AppState,
    ctx: &ScriptContext,
    queue: &Arc<MessageQueue>,
    libs: &[(String, String)],
    config: SandboxConfig,
) -> Result<Sandbox, String> {
    let mut game_api = GameApi::new(
        state.object_store.clone(),
        state.classes.clone(),
        state.actions.clone(),
        queue.clone(),
        state.permissions.clone(),
        state.timers.clone(),
        state.credits.clone(),
        state.venice.clone(),
        state.image_store.clone(),
        &ctx.universe_id,
    );
    // Contexts are captured when functions are registered, so set them first
    game_api.set_user_context(ctx.user_id.clone());
    game_api.set_room_context(ctx.room_id.clone());
    game_api.set_object_context(ctx.object_id.clone());

    let mut sandbox =
        Sandbox::new(config).map_err(|e| format!("Failed to create sandbox: {}", e))?;
    game_api
        .register(sandbox.lua())
        .map_err(|e| format!("Failed to register game API: {}", e))?;

    for (lib_name, lib_code) in libs {
        sandbox
            .execute::<()>(lib_code)
            .map_err(|e| format!("Failed to execute library {}: {}", lib_name, e))?;
    }
    Ok(sandbox)
}

/// Deliver queued script messages to connected players in `universe_id`
/// (sessions are keyed per universe, so nothing reaches other universes)
pub async fn deliver(state: &AppState, queue: &MessageQueue, universe_id: &str) {
    deliver_at(state, queue, universe_id, 0).await
}

/// How deeply move hooks may nest: a hook that moves something runs more
/// hooks, so rooms that pass an object back and forth stop here
const MAX_MOVE_HOOK_DEPTH: u32 = 4;

fn deliver_at<'a>(
    state: &'a AppState,
    queue: &'a MessageQueue,
    universe_id: &'a str,
    depth: u32,
) -> BoxFuture<'a, ()> {
    Box::pin(async move {
        for msg in queue.drain() {
            match msg {
                GameMessage::Send { target_id, message } => {
                    state
                        .connections
                        .send_to_player(
                            universe_id,
                            &target_id,
                            ServerMessage::Output { text: message },
                        )
                        .await;
                }
                GameMessage::Broadcast {
                    room_id,
                    message,
                    exclude_id,
                } => {
                    state
                        .connections
                        .broadcast_room_except(
                            universe_id,
                            &room_id,
                            exclude_id.as_deref(),
                            ServerMessage::Output { text: message },
                        )
                        .await;
                }
                GameMessage::BroadcastRegion { region_id, message } => {
                    broadcast_region(state, universe_id, &region_id, &message).await;
                }
                GameMessage::Moved {
                    object_id,
                    from,
                    to,
                } => {
                    handle_moved(state, universe_id, &object_id, from, to, depth).await;
                }
            }
        }
    })
}

/// A Lua move_object: move a connected player's session (and show them the
/// new room), then run on_leave on the old container and on_enter on the
/// new one, with moved_id naming what moved
async fn handle_moved(
    state: &AppState,
    universe_id: &str,
    object_id: &str,
    from: Option<String>,
    to: Option<String>,
    depth: u32,
) {
    if let Some(account_id) = state.connections.account_id(universe_id, object_id).await {
        state
            .connections
            .update_room(universe_id, object_id, to.clone())
            .await;
        if let Some(ref room) = to {
            if let Some(room_msg) =
                super::websocket::build_room_message(state, universe_id, room, Some(&account_id))
                    .await
            {
                state
                    .connections
                    .send_to_player(universe_id, object_id, room_msg)
                    .await;
            }
        }
    }

    if depth >= MAX_MOVE_HOOK_DEPTH {
        tracing::warn!(
            "move hooks for {} not run: nested {} deep",
            object_id,
            depth
        );
        return;
    }
    let hooks = [
        (from.clone(), "on_leave", serde_json::json!({ "to": to })),
        (to.clone(), "on_enter", serde_json::json!({ "from": from })),
    ];
    for (container, method, mut args) in hooks {
        let Some(container) = container else { continue };
        if let Some(map) = args.as_object_mut() {
            map.insert("object_id".into(), container.clone().into());
            map.insert("room_id".into(), container.clone().into());
            map.insert("actor_id".into(), object_id.into());
            map.insert("moved_id".into(), object_id.into());
        }
        if let Err(e) = invoke_handler_at(
            state,
            universe_id,
            &container,
            Some(container.clone()),
            method,
            args,
            depth + 1,
        )
        .await
        {
            tracing::warn!("{} hook on {} failed: {}", method, container, e);
        }
    }
}

/// Send to every player whose room is in the region: the room's path is
/// under the region's path, or the room's parent is the region object.
async fn broadcast_region(state: &AppState, universe_id: &str, region_id: &str, message: &str) {
    let prefix = format!("{}/", region_id.trim_end_matches('/'));
    for (player_id, room_id) in state.connections.player_rooms(universe_id).await {
        let in_region = room_id.starts_with(&prefix)
            || matches!(
                state.object_store.get(universe_id, &room_id).await,
                Ok(Some(room)) if room.parent_id.as_deref() == Some(region_id)
            );
        if in_region {
            state
                .connections
                .send_to_player(
                    universe_id,
                    &player_id,
                    ServerMessage::Output {
                        text: message.to_string(),
                    },
                )
                .await;
        }
    }
}

/// Load universe library codes from database
/// Returns Vec of (lib_name, code) pairs, sorted by name for determinism
pub async fn load_universe_lib_codes(
    state: &AppState,
    universe_id: &str,
) -> Result<Vec<(String, String)>, String> {
    crate::lua::universe_lib_codes(&state.object_store, universe_id).await
}

/// Convert Lua value to displayable string
pub fn lua_value_to_string(value: &Value) -> String {
    match value {
        Value::Nil => "nil".to_string(),
        Value::Boolean(b) => b.to_string(),
        Value::Integer(i) => i.to_string(),
        Value::Number(n) => n.to_string(),
        Value::String(s) => s
            .to_str()
            .map(|s| s.to_string())
            .unwrap_or_else(|_| "<invalid utf8>".to_string()),
        Value::Table(_) => "[table]".to_string(),
        Value::Function(_) => "[function]".to_string(),
        Value::Thread(_) => "[thread]".to_string(),
        Value::UserData(_) => "[userdata]".to_string(),
        Value::LightUserData(_) => "[lightuserdata]".to_string(),
        Value::Error(e) => format!("[error: {}]", e),
        _ => "[unknown]".to_string(),
    }
}
