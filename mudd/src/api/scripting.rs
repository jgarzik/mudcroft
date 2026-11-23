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
use crate::lua::{GameApi, GameMessage, MessageQueue, Sandbox, SandboxConfig, json_to_lua};

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
    /// The account that pays for credit spending (`"actor"`): the player
    /// who typed the verb, or whoever ran eval / a script. None for hooks
    /// and timers, which nobody chose to run.
    pub payer: Option<String>,
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
    // One change: a failing run keeps nothing (see db::run)
    in_run(state, async {
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
        // Messages (and the hooks moves trigger) only if the run succeeded
        if result.is_ok() {
            deliver(state, &queue, &ctx.universe_id).await;
        }
        result
    })
    .await
}

/// Run `f` as one change (db::run): its writes are one Raft batch, kept
/// only if it succeeds. A failed commit becomes the run's error.
pub async fn in_run<T, F>(state: &AppState, f: F) -> Result<T, String>
where
    F: Future<Output = Result<T, String>>,
{
    in_run_with(state, Vec::new(), f).await
}

/// [`in_run`] with statements leading the batch (if anything is written)
pub async fn in_run_with<T, F>(
    state: &AppState,
    prefix: Vec<crate::db::run::Statement>,
    f: F,
) -> Result<T, String>
where
    F: Future<Output = Result<T, String>>,
{
    match in_run_settled(state, prefix, f).await {
        Ok(result) => result,
        Err(e) => Err(format!("not saved: {}", e)),
    }
}

/// [`in_run_with`], telling a failed commit (the outer `Err`: nothing was
/// saved although `f` succeeded; see `raft::writer::is_apply_failure`)
/// from `f`'s own error
pub async fn in_run_settled<T, F>(
    state: &AppState,
    prefix: Vec<crate::db::run::Statement>,
    f: F,
) -> anyhow::Result<Result<T, String>>
where
    F: Future<Output = Result<T, String>>,
{
    // A failed run's cache changes are undone by rebuilding from the tables
    let rebuilder = state.clone();
    let rebuild = move || async move {
        if let Err(e) = super::primary::rebuild_caches(&rebuilder).await {
            tracing::error!("rebuilding caches after a failed run: {}", e);
        }
    };
    let committer: std::sync::Arc<dyn crate::db::run::Committer> = state.raft_writer.clone();
    // One engine hook budget for the whole run (its moves, creates, ...)
    // (boxed: the run's future is large, and runs nest)
    let outcome = super::combat::with_ai_budget(Box::pin(crate::db::run::scope(
        state.db.pool(),
        committer,
        prefix,
        f,
        rebuild,
    )))
    .await;
    // A committed run may have added a fight or a deadline (some come from
    // triggers, e.g. wander_seconds): the combat scheduler looks again
    // (one indexed query)
    if matches!(outcome, Ok(Ok(_))) {
        state.combat_changed.notify_one();
    }
    outcome
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
    invoke_handler_at(
        state,
        universe_id,
        object_id,
        room_id,
        method,
        args,
        Origin {
            payer: None,
            hooks: HookBudget::new(),
        },
    )
    .await
}

/// invoke_handler for a verb a player typed. `payer` is their account when
/// they clearly picked this object: it then pays for the handler's credit
/// spending (`"actor"`)
pub async fn invoke_verb_handler(
    state: &AppState,
    universe_id: &str,
    object_id: &str,
    room_id: Option<String>,
    method: &str,
    args: serde_json::Value,
    payer: Option<&str>,
) -> Result<Option<String>, String> {
    invoke_handler_at(
        state,
        universe_id,
        object_id,
        room_id,
        method,
        args,
        Origin {
            payer: payer.map(str::to_string),
            hooks: HookBudget::new(),
        },
    )
    .await
}

type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// How deeply move hooks may nest: a hook that moves something runs more
/// hooks, so rooms that pass an object back and forth stop here
const MAX_MOVE_HOOK_DEPTH: u32 = 4;

/// How many move hooks one top-level run may cause in total: a hook can
/// move many objects, so depth alone does not bound the work
const MAX_MOVE_HOOKS_PER_RUN: u32 = 32;

/// The move-hook allowance of one top-level run, carried down through the
/// hooks it causes
#[derive(Clone)]
struct HookBudget {
    depth: u32,
    remaining: Arc<std::sync::atomic::AtomicU32>,
}

impl HookBudget {
    /// A fresh allowance, or, inside an action (a run, an engine hook),
    /// that action's shared one: an engine hook's moves do not get a new
    /// allowance each time
    fn new() -> Self {
        Self {
            depth: 0,
            remaining: super::combat::hook_counter().unwrap_or_else(|| {
                Arc::new(std::sync::atomic::AtomicU32::new(MAX_MOVE_HOOKS_PER_RUN))
            }),
        }
    }

    fn deeper(&self) -> Self {
        Self {
            depth: self.depth + 1,
            remaining: self.remaining.clone(),
        }
    }

    /// Take one hook from the allowance; false when none is left
    fn take(&self) -> bool {
        use std::sync::atomic::Ordering;
        self.remaining
            .try_update(Ordering::Relaxed, Ordering::Relaxed, |n| n.checked_sub(1))
            .is_ok()
    }
}

/// What a handler run inherits from the run that caused it
struct Origin {
    /// Who pays for its credit spending (see ScriptContext::payer)
    payer: Option<String>,
    /// The move-hook allowance it may use
    hooks: HookBudget,
}

/// invoke_handler at a hook nesting depth (move hooks run handlers that can
/// move things and so run more hooks; see MAX_MOVE_HOOK_DEPTH)
fn invoke_handler_at<'a>(
    state: &'a AppState,
    universe_id: &'a str,
    object_id: &'a str,
    room_id: Option<String>,
    method: &'a str,
    args: serde_json::Value,
    origin: Origin,
) -> BoxFuture<'a, Result<Option<String>, String>> {
    let Origin {
        payer,
        hooks: budget,
    } = origin;
    Box::pin(async move {
        let Some((code, owner_id)) = object_code(state, object_id, universe_id).await? else {
            return Ok(None);
        };
        let ctx = ScriptContext {
            universe_id: universe_id.to_string(),
            user_id: owner_id,
            room_id,
            object_id: Some(object_id.to_string()),
            payer,
        };
        let libs = load_universe_lib_codes(state, universe_id).await?;
        // One change (joins the caller's if inside one): a failing handler
        // keeps nothing
        in_run(state, async {
            // A panic (a server bug outside Lua) fails the handler inside its
            // run, which is then undone as for any error (and, nested, only
            // its own part)
            let handler = async {
                let queue = MessageQueue::shared();
                let result = {
                    let mut sandbox =
                        prepare(state, &ctx, &queue, &libs, SandboxConfig::default())?;
                    call_handler(&mut sandbox, &code, method, &args)
                };
                if result.is_ok() {
                    deliver_at(state, &queue, &ctx.universe_id, budget).await;
                }
                result
            };
            match futures_util::FutureExt::catch_unwind(std::panic::AssertUnwindSafe(handler)).await
            {
                Ok(result) => result,
                Err(panic) => {
                    let msg = panic
                        .downcast_ref::<&str>()
                        .map(|s| s.to_string())
                        .or_else(|| panic.downcast_ref::<String>().cloned())
                        .unwrap_or_default();
                    tracing::error!("handler {} on {} panicked: {}", method, object_id, msg);
                    Err("internal error".to_string())
                }
            }
        })
        .await
    })
}

/// Longest timer-loop sleep with nothing due (a safety net: additions wake
/// it sooner)
const TIMER_IDLE_WAIT: std::time::Duration = std::time::Duration::from_secs(60);

/// Pause after a call_out could not be saved or read (as the combat
/// scheduler does), before trying again: its row is still stored
const TIMER_RETRY_PAUSE: std::time::Duration = std::time::Duration::from_millis(500);

/// Most due call_outs fired per pass (each its own run)
const CALL_OUT_BATCH: i64 = 64;

/// How far a call_out is pushed back when firing it failed for a reason of
/// its own (a failed read): it is retried then, without holding up others
const CALL_OUT_RETRY_MS: i64 = 5_000;

/// Fire due call_outs and heartbeats forever. Deadline-driven: the loop
/// sleeps until the earliest call_out (from the table) or heartbeat, or
/// until one is added, not on a fixed poll.
///
/// Each callback runs the named handler on the object's code (as the
/// object's owner, see [`invoke_handler`]) with {object_id, room_id, args}.
pub async fn run_timer_loop(state: AppState) {
    loop {
        // Register for change notification before reading the deadline,
        // so a timer added in between is not missed
        let changed = state.timers.changed();
        tokio::pin!(changed);
        let now = chrono::Utc::now().timestamp_millis();
        let wait = match state.timers.next_deadline().await {
            Ok(Some(due)) if due <= now => std::time::Duration::ZERO,
            Ok(Some(due)) => {
                std::time::Duration::from_millis((due - now) as u64).min(TIMER_IDLE_WAIT)
            }
            Ok(None) => TIMER_IDLE_WAIT,
            Err(e) => {
                tracing::warn!("timers: reading the next deadline failed: {}", e);
                TIMER_RETRY_PAUSE
            }
        };
        if !wait.is_zero() {
            tokio::select! {
                _ = tokio::time::sleep(wait) => {}
                _ = &mut changed => continue,
            }
        }
        let now = chrono::Utc::now().timestamp_millis();
        match state.timers.due_call_outs(now, CALL_OUT_BATCH).await {
            Ok(due) => {
                for timer in due {
                    if fire_call_out(&state, &timer).await == Fired::Pause {
                        tokio::time::sleep(TIMER_RETRY_PAUSE).await;
                        break;
                    }
                }
            }
            Err(e) => {
                tracing::warn!("timers: reading due call_outs failed: {}", e);
                tokio::time::sleep(TIMER_RETRY_PAUSE).await;
            }
        }
        for fired in state.timers.tick_heartbeats().await {
            fire_heart_beat(&state, &fired).await;
        }
    }
}

/// The callback's args: {object_id, room_id, args}
fn timer_args(
    object_id: &str,
    room_id: &Option<String>,
    args: &Option<String>,
) -> serde_json::Value {
    serde_json::json!({ "object_id": object_id, "room_id": room_id, "args": args })
}

/// What became of a due call_out
#[derive(PartialEq)]
enum Fired {
    /// Ran, dropped, or gone already
    Done,
    /// Nothing could be saved here (not the primary, Raft timed out): stop
    /// this pass and pause; its row is still stored
    Pause,
}

/// Fire one due call_out as one run. Under the write lock (so cancel and
/// fire are one decision and a call_out runs at most once) it checks the
/// row is still there, reads the object, then claims the call_out by
/// deleting its row and runs its handler as a nested part. Success commits
/// the claim and the handler's writes as one Raft entry: never the timer
/// gone without its effects, or the reverse. A handler that fails (or
/// whose code cannot be loaded) keeps none of its writes and the call_out
/// is dropped (the claim commits), as in LPMud; so is one whose change is
/// refused when applied (the same on every try). A failure of its own (a
/// failed read) pushes it back a few seconds without holding up the rest.
async fn fire_call_out(state: &AppState, timer: &crate::timers::Timer) -> Fired {
    let what = format!("call_out {} on {}", timer.method, timer.object_id);
    let outcome = in_run_settled(state, Vec::new(), async {
        let run = crate::db::run::current().ok_or("not in a run")?;
        run.lock_writes().await.map_err(|e| e.to_string())?;
        if !state
            .timers
            .is_stored(&timer.id)
            .await
            .map_err(|e| e.to_string())?
        {
            return Ok(()); // cancelled, or fired already: nothing to save
        }
        let obj = state
            .object_store
            .get(&timer.universe_id, &timer.object_id)
            .await
            .map_err(|e| format!("reading its object: {}", e))?;
        let (sql, params) = crate::timers::TimerManager::consume_statement(&timer.id);
        state
            .raft_writer
            .execute(sql, params)
            .await
            .map_err(|e| e.to_string())?;
        // Gone: nothing to call (its delete cascades to its timers anyway)
        let Some(obj) = obj else {
            return Ok(());
        };
        let room_id = obj.parent_id;
        let args = timer_args(&timer.object_id, &room_id, &timer.args);
        if let Err(e) = invoke_handler(
            state,
            &timer.universe_id,
            &timer.object_id,
            room_id,
            &timer.method,
            args,
        )
        .await
        {
            tracing::warn!("{} failed (dropped): {}", what, e);
        }
        Ok(())
    })
    .await;
    match outcome {
        Ok(Ok(())) => Fired::Done,
        Ok(Err(e)) => {
            // A failure of its own: nothing saved; retried a little later
            tracing::warn!(
                "{}: not run ({}), retried in {} ms",
                what,
                e,
                CALL_OUT_RETRY_MS
            );
            let later = chrono::Utc::now().timestamp_millis() + CALL_OUT_RETRY_MS;
            match state
                .raft_writer
                .execute(
                    "UPDATE timers SET fire_at = ? WHERE id = ?",
                    vec![serde_json::json!(later), serde_json::json!(timer.id)],
                )
                .await
            {
                Ok(_) => Fired::Done,
                Err(e) => {
                    tracing::warn!("{}: postponing it failed: {}", what, e);
                    Fired::Pause
                }
            }
        }
        Err(e) if crate::raft::writer::is_apply_failure(&e) => {
            // Refused when applied: the same on every try, so dropped
            tracing::warn!("{}: its change was refused (dropped): {:#}", what, e);
            let (sql, params) = crate::timers::TimerManager::consume_statement(&timer.id);
            if let Err(e) = state.raft_writer.execute(sql, params).await {
                tracing::warn!("{}: dropping it failed: {}", what, e);
                return Fired::Pause;
            }
            Fired::Done
        }
        Err(e) => {
            // Not the primary any more, or Raft timed out: still stored
            tracing::warn!("{}: not saved, retried: {:#}", what, e);
            Fired::Pause
        }
    }
}

/// Fire one heartbeat beat (beats write nothing themselves; the handler
/// runs as its own change)
async fn fire_heart_beat(state: &AppState, fired: &crate::timers::TimerFired) {
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
            return;
        }
        Err(e) => {
            tracing::warn!("Timer object {} lookup failed: {}", fired.object_id, e);
            return;
        }
    };
    let args = timer_args(&fired.object_id, &room_id, &fired.args);
    if let Err(e) = invoke_handler(
        state,
        &fired.universe_id,
        &fired.object_id,
        room_id,
        &fired.method,
        args,
    )
    .await
    {
        tracing::warn!(
            "heart_beat {} on {} failed: {}",
            fired.method,
            fired.object_id,
            e
        );
    }
}

/// An object's attached code and its owner, if it has code (objects are
/// looked up within the universe, so foreign objects never run here)
pub(crate) async fn object_code(
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
        state.effects.clone(),
        &ctx.universe_id,
    );
    // Contexts are captured when functions are registered, so set them first
    game_api.set_user_context(ctx.user_id.clone());
    game_api.set_room_context(ctx.room_id.clone());
    game_api.set_object_context(ctx.object_id.clone());
    game_api.set_payer(ctx.payer.clone());
    game_api.set_engine(Arc::new(super::combat::CombatEngine {
        state: state.clone(),
    }));

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
    deliver_at(state, queue, universe_id, HookBudget::new()).await
}

fn deliver_at<'a>(
    state: &'a AppState,
    queue: &'a MessageQueue,
    universe_id: &'a str,
    budget: HookBudget,
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
                GameMessage::Confirm {
                    target_id,
                    id,
                    text,
                    amount,
                } => {
                    state
                        .connections
                        .send_to_player(
                            universe_id,
                            &target_id,
                            ServerMessage::Confirm { id, text, amount },
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
                    moved_by,
                } => {
                    handle_moved(state, universe_id, &object_id, from, to, moved_by, &budget).await;
                }
            }
        }
    })
}

/// A Lua move_object: move a connected player's session (and show them the
/// new room), then run on_leave on the old container and on_enter on the
/// new one, with moved_id naming what moved and moved_by the account the
/// script acted as. actor_id is not set: no player chose to move, so hook
/// code that trusts actor_id cannot be triggered on someone's behalf.
async fn handle_moved(
    state: &AppState,
    universe_id: &str,
    object_id: &str,
    from: Option<String>,
    to: Option<String>,
    moved_by: Option<String>,
    budget: &HookBudget,
) {
    if state
        .connections
        .account_id(universe_id, object_id)
        .await
        .is_some()
    {
        state
            .connections
            .update_room(universe_id, object_id, to.clone())
            .await;
        if let Some(ref room) = to {
            // No account: a script's move must not start (and charge the
            // moved player for) room image generation
            if let Some(room_msg) =
                super::websocket::build_room_message(state, universe_id, room, None).await
            {
                state
                    .connections
                    .send_to_player(universe_id, object_id, room_msg)
                    .await;
            }
        }
    }

    // A player moved into a safe room respawns there (as for a walk)
    if let Some(ref room) = to
        && object_id.starts_with("/players/")
        && let Ok(Some(player)) = state.object_store.get(universe_id, object_id).await
        && let Ok(true) = state
            .player_manager
            .is_safe_zone(universe_id, room, &player)
            .await
        && let Err(e) = state
            .player_manager
            .update_safe_location(universe_id, object_id, room)
            .await
    {
        tracing::warn!("safe location for {} not saved: {}", object_id, e);
    }

    // Aggressive NPCs notice an arrival (and an aggressive NPC moved in
    // notices players), whatever moved it
    if let Some(ref room) = to {
        super::combat::arrival(state, universe_id, object_id, room).await;
    }

    run_move_hooks(state, universe_id, object_id, &from, &to, &moved_by, budget).await;
    // Then regions crossed, on_init, on_move (within the action's budget)
    super::combat::move_extras(
        state,
        universe_id,
        object_id,
        from.as_deref(),
        to.as_deref(),
        None,
        moved_by.as_deref(),
    )
    .await;
}

/// The containers' on_leave / on_enter for a Lua move
async fn run_move_hooks(
    state: &AppState,
    universe_id: &str,
    object_id: &str,
    from: &Option<String>,
    to: &Option<String>,
    moved_by: &Option<String>,
    budget: &HookBudget,
) {
    if budget.depth >= MAX_MOVE_HOOK_DEPTH {
        tracing::warn!(
            "move hooks for {} not run: nested {} deep",
            object_id,
            budget.depth
        );
        return;
    }
    let hooks = [
        (from.clone(), "on_leave", serde_json::json!({ "to": to })),
        (to.clone(), "on_enter", serde_json::json!({ "from": from })),
    ];
    for (container, method, mut args) in hooks {
        let Some(container) = container else { continue };
        // Only containers with code use up the budget (most have none)
        if !matches!(
            object_code(state, &container, universe_id).await,
            Ok(Some(_))
        ) {
            continue;
        }
        if !budget.take() {
            tracing::warn!(
                "{} hook on {} not run: the run's move-hook budget is spent",
                method,
                container
            );
            return;
        }
        if let Some(map) = args.as_object_mut() {
            map.insert("object_id".into(), container.clone().into());
            map.insert("room_id".into(), container.clone().into());
            map.insert("moved_id".into(), object_id.into());
            map.insert("moved_by".into(), moved_by.clone().into());
        }
        if let Err(e) = invoke_handler_at(
            state,
            universe_id,
            &container,
            Some(container.clone()),
            method,
            args,
            Origin {
                // A move hook is passive: nobody chose to run it, nobody pays
                payer: None,
                hooks: budget.deeper(),
            },
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
