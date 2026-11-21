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

/// Sandbox limits for interactive wizard `eval` and admin scripts
pub fn eval_config() -> SandboxConfig {
    SandboxConfig {
        max_instructions: 10_000_000,
        timeout: std::time::Duration::from_secs(5),
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
            .map_err(|e| format!("Lua error: {}", e))
    };

    deliver(state, &queue).await;
    result
}

/// Call handler `method` on an object's attached code (`code_hash`).
///
/// The object's code must evaluate to a table of handler functions; the
/// handler receives `args` converted to a Lua table. `ctx.object_id` must be
/// the object. Returns `Ok(None)` when the object has no code or no such
/// handler, otherwise the handler's result rendered as text.
pub async fn invoke_handler(
    state: &AppState,
    ctx: &ScriptContext,
    method: &str,
    args: serde_json::Value,
) -> Result<Option<String>, String> {
    let Some(object_id) = ctx.object_id.as_deref() else {
        return Err("invoke_handler: no object context".to_string());
    };
    let Some(code) = object_code(state, object_id).await? else {
        return Ok(None);
    };
    let libs = load_universe_lib_codes(state, &ctx.universe_id).await?;
    let queue = MessageQueue::shared();

    let result = {
        let mut sandbox = prepare(state, ctx, &queue, &libs, SandboxConfig::default())?;
        call_handler(&mut sandbox, &code, method, &args)
    };

    deliver(state, &queue).await;
    result
}

/// Source of an object's attached code, if any
async fn object_code(state: &AppState, object_id: &str) -> Result<Option<String>, String> {
    let obj = match state.object_store.get(object_id).await {
        Ok(Some(obj)) => obj,
        Ok(None) => return Ok(None),
        Err(e) => return Err(format!("Failed to load {}: {}", object_id, e)),
    };
    let Some(hash) = obj.code_hash else {
        return Ok(None);
    };
    state
        .object_store
        .get_code(&hash)
        .await
        .map_err(|e| format!("Failed to load code {}: {}", hash, e))
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

/// Deliver queued script messages to connected players
pub async fn deliver(state: &AppState, queue: &MessageQueue) {
    for msg in queue.drain() {
        match msg {
            GameMessage::Send { target_id, message } => {
                state
                    .connections
                    .send_to_player(&target_id, ServerMessage::Output { text: message })
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
                        &room_id,
                        exclude_id.as_deref(),
                        ServerMessage::Output { text: message },
                    )
                    .await;
            }
            GameMessage::BroadcastRegion { region_id, message } => {
                broadcast_region(state, &region_id, &message).await;
            }
        }
    }
}

/// Send to every player whose room is in the region: the room's path is
/// under the region's path, or the room's parent is the region object.
async fn broadcast_region(state: &AppState, region_id: &str, message: &str) {
    let prefix = format!("{}/", region_id.trim_end_matches('/'));
    for (player_id, room_id) in state.connections.player_rooms().await {
        let in_region = room_id.starts_with(&prefix)
            || matches!(
                state.object_store.get(&room_id).await,
                Ok(Some(room)) if room.parent_id.as_deref() == Some(region_id)
            );
        if in_region {
            state
                .connections
                .send_to_player(
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
    let universe = match state.object_store.get_universe(universe_id).await {
        Ok(Some(u)) => u,
        Ok(None) => return Ok(Vec::new()), // No universe = no libs to load
        Err(e) => return Err(format!("Failed to get universe: {}", e)),
    };

    let Some(hashes_obj) = universe
        .config
        .get("lib_hashes")
        .and_then(|h| h.as_object())
    else {
        return Ok(Vec::new()); // No (or invalid) libs defined
    };

    // Load each library in order (sorted for determinism)
    let mut lib_names: Vec<_> = hashes_obj.keys().collect();
    lib_names.sort();

    let mut lib_codes = Vec::new();
    for lib_name in lib_names {
        let Some(hash) = hashes_obj.get(lib_name).and_then(|v| v.as_str()) else {
            continue;
        };
        match state.object_store.get_code(hash).await {
            Ok(Some(code)) => lib_codes.push((lib_name.clone(), code)),
            Ok(None) => tracing::warn!(
                "Library {} with hash {} not found in code_store",
                lib_name,
                hash
            ),
            Err(e) => tracing::warn!("Failed to load library {}: {}", lib_name, e),
        }
    }

    Ok(lib_codes)
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
