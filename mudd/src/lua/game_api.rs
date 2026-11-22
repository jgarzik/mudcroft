//! Game API exposed to Lua scripts

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use mlua::{Function, Lua, Result as LuaResult, Table, Value};
use parking_lot::Mutex;
use rand::rngs::StdRng;
use rand::{RngExt, SeedableRng};
use tokio::sync::RwLock;

use super::actions::{Action, ActionRegistry};
use super::messaging::MessageQueue;
use super::sandbox::{charge_db, charge_venice};
use crate::credits::CreditManager;
use crate::objects::{ClassRegistry, Object, ObjectStore};
use crate::permissions::{AccessLevel, Action as PermAction, ObjectContext, PermissionManager};
use crate::timers::{HeartBeat, Timer, TimerManager};
use crate::venice::{ChatMessage, ImageSize, ImageStyle, ModelTier, VeniceClient};

/// Actor override set by `game.set_actor`. Kept in Lua app data (Rust-side
/// state) rather than a Lua global so sandboxed code cannot forge it.
struct ActorOverride(Option<String>);

/// The actor override for this Lua state, if `game.set_actor` set one
fn actor_override(lua: &Lua) -> Option<String> {
    lua.app_data_ref::<ActorOverride>()
        .and_then(|a| a.0.clone())
}

/// Block on an async permission lookup from a synchronous Lua callback
fn block_on_perm<F, T>(fut: F) -> T
where
    F: std::future::Future<Output = T>,
{
    block_now(fut)
}

/// Who pays for a Venice call, and what it costs (universe config
/// `venice_chat_cost` / `venice_image_cost`, default 0)
#[derive(Clone)]
struct VeniceBilling {
    store: Arc<ObjectStore>,
    credits: Arc<CreditManager>,
    universe_id: String,
    owner: Option<String>,
    payer: Option<String>,
    object: Option<String>,
}

impl VeniceBilling {
    /// The holder `opts.payer` names: "actor" (default; None when nobody
    /// chose to run this code) or "object"
    fn payer_holder(&self, opts: Option<&Table>) -> LuaResult<Option<crate::credits::Holder>> {
        use crate::credits::Holder;
        let which: Option<String> = match opts {
            Some(t) => t.get("payer")?,
            None => None,
        };
        match which.as_deref().unwrap_or("actor") {
            "actor" => Ok(self.payer.clone().map(Holder::Account)),
            "object" => self
                .object
                .clone()
                .map(|o| Some(Holder::Object(o)))
                .ok_or_else(|| mlua::Error::runtime("no object here")),
            _ => Err(mlua::Error::runtime(
                "payer must be \"actor\" or \"object\"",
            )),
        }
    }

    /// Charge the price under `price_key` to `holder`, run `call` (given the
    /// rate-limit key: the code's owner), and refund if it fails
    async fn paid<T, F, Fut>(
        &self,
        holder: Option<crate::credits::Holder>,
        price_key: &str,
        call: F,
    ) -> Result<T, String>
    where
        F: FnOnce(String) -> Fut,
        Fut: std::future::Future<Output = Result<T, String>>,
    {
        let config = self
            .store
            .get_universe(&self.universe_id)
            .await
            .map_err(|e| e.to_string())?
            .map(|u| u.config)
            .unwrap_or_default();
        let price = match config.get(price_key) {
            None | Some(serde_json::Value::Null) => 0,
            Some(v) => v
                .as_i64()
                .filter(|p| (0..=crate::credits::MAX_AMOUNT).contains(p))
                .ok_or_else(|| format!("universe config {} is not a valid price", price_key))?,
        };
        // A slow call outside the database: the run's writes so far are
        // saved and the write lock released first (db::run)
        if let Some(run) = crate::db::run::current() {
            run.checkpoint().await.map_err(|e| e.to_string())?;
        }
        if price > 0 {
            if let Some(crate::credits::Holder::Account(_)) = holder {
                return Err("a player pays only through game.request_payment; bill the \
                            object ({payer = \"object\"}) instead"
                    .to_string());
            }
            let Some(ref holder) = holder else {
                return Err("no paying player here (only a verb, eval or script has one)".into());
            };
            if !self
                .credits
                .deduct_direct(&self.universe_id, holder, price)
                .await
                .map_err(|e| e.to_string())?
            {
                return Err(format!("not enough credits (costs {})", price));
            }
        }
        // Rate-limited under the code's owner, whoever pays: the limit
        // guards the operator's Venice bill, and keyed by payer every
        // object (or player) would be a fresh bucket
        let key = format!(
            "{}:{}",
            self.universe_id,
            self.owner.as_deref().unwrap_or("anonymous")
        );
        let result = call(key).await;
        if result.is_err() && price > 0 {
            if let Some(ref holder) = holder {
                if let Err(e) = self
                    .credits
                    .grant_direct(&self.universe_id, holder, price)
                    .await
                {
                    tracing::warn!("venice refund of {} to {:?} failed: {}", price, holder, e);
                }
            }
        }
        result
    }
}

/// Run a future to completion from synchronous Lua callback code, on the
/// current (multi-threaded) runtime. Cheap, unlike spawning a thread with
/// its own runtime per call, which made a few dozen game.* calls blow the
/// execution's time budget.
fn block_now<F: std::future::Future>(fut: F) -> F::Output {
    tokio::task::block_in_place(|| tokio::runtime::Handle::current().block_on(fut))
}

/// Check that `parent_id` (if any) exists in `universe_id` (object ids are
/// scoped per universe, so a parent is always in the same universe)
async fn check_parent(
    store: &ObjectStore,
    universe_id: &str,
    parent_id: Option<&str>,
) -> anyhow::Result<Result<(), String>> {
    let Some(parent_id) = parent_id else {
        return Ok(Ok(()));
    };
    Ok(match store.get(universe_id, parent_id).await? {
        Some(_) => Ok(()),
        None => Err(format!("parent not found: {}", parent_id)),
    })
}

/// Objects only the server may create: player-class objects and anything
/// under /players/ (player objects are made on connect)
fn is_reserved_object(path: &str, class: &str) -> bool {
    class == "player" || path == "/players" || path.starts_with("/players/")
}

/// Check `action` on an existing object for the acting user.
/// Returns the denial reason on failure; no user context is always denied.
async fn object_permission(
    permissions: &PermissionManager,
    user_id: Option<&str>,
    universe_id: &str,
    obj: &Object,
    action: PermAction,
) -> Result<(), String> {
    let Some(user_id) = user_id else {
        return Err("no user context".to_string());
    };
    // Objects are looked up per universe, so this only guards misuse
    if obj.universe_id != universe_id {
        return Err(format!("{} belongs to another universe", obj.id));
    }
    let ctx = permissions.get_user_context(user_id, universe_id).await;
    let target = ObjectContext {
        object_id: obj.id.clone(),
        owner_id: obj.owner_id.clone(),
        is_fixed: obj
            .properties
            .get("fixed")
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
    };
    match permissions.check_permission(&ctx, action, &target) {
        crate::permissions::PermissionResult::Allowed => Ok(()),
        crate::permissions::PermissionResult::Denied(reason) => Err(reason),
    }
}

/// Check permission to create an object at `path` for the acting user
async fn create_permission(
    permissions: &PermissionManager,
    user_id: Option<&str>,
    universe_id: &str,
    path: &str,
) -> Result<(), String> {
    let Some(user_id) = user_id else {
        return Err("no user context".to_string());
    };
    let ctx = permissions.get_user_context(user_id, universe_id).await;
    match permissions.check_create_permission(&ctx, path) {
        crate::permissions::PermissionResult::Allowed => Ok(()),
        crate::permissions::PermissionResult::Denied(reason) => Err(reason),
    }
}

/// Lua error for a denied object operation
fn denied(op: &str, reason: String) -> mlua::Error {
    mlua::Error::runtime(format!("{}: permission denied: {}", op, reason))
}

/// Game API context shared with Lua
#[allow(dead_code)]
pub struct GameApi {
    store: Arc<ObjectStore>,
    classes: Arc<RwLock<ClassRegistry>>,
    actions: Arc<ActionRegistry>,
    messages: Arc<MessageQueue>,
    permissions: Arc<PermissionManager>,
    timers: Arc<TimerManager>,
    credits: Arc<CreditManager>,
    venice: Arc<VeniceClient>,
    image_store: Arc<crate::images::ImageStore>,
    effects: Arc<crate::combat::EffectRegistry>,
    universe_id: String,
    current_room_id: Option<String>,
    current_user_id: Option<String>,
    current_object_id: Option<String>,
    /// Account paying for credit spending as `"actor"` (see ScriptContext)
    payer: Option<String>,
    /// Time override for testing (milliseconds since epoch, 0 = use real time)
    time_override: Arc<AtomicU64>,
    /// RNG for dice rolls - seeded for reproducibility in tests
    rng: Arc<Mutex<StdRng>>,
    /// How many game.use_object calls deep this API's script is nested
    use_depth: usize,
    /// game.use_object calls made so far by this top-level execution,
    /// shared with every nested sandbox it spawns
    use_calls: Arc<std::sync::atomic::AtomicUsize>,
    /// The combat engine (api::combat), set by the script runner
    engine: Option<Arc<dyn crate::lua::engine::Engine>>,
}

/// Deepest allowed game.use_object nesting (object code using objects...)
const MAX_USE_DEPTH: usize = 8;

/// Most game.use_object calls one top-level execution may make in total,
/// across all nesting levels (each call runs a whole new sandbox)
const MAX_USE_CALLS: usize = 32;

/// Whether `handler` is private to its object's own code: `_name` helpers
/// and engine hooks, which only the server invokes on real events. Neither
/// game.use_object nor player verbs may call these.
pub fn is_private_handler(handler: &str) -> bool {
    handler.starts_with('_') || ENGINE_HOOKS.contains(&handler)
}

/// Engine hooks: run by the server on events, never via game.use_object
const ENGINE_HOOKS: &[&str] = &[
    "on_enter",
    "on_leave",
    "on_damage",
    "on_death",
    "heart_beat",
    "on_create",
    "on_destroy",
    "ai_idle_tick",
    "ai_combat_tick",
    "on_init",
    "on_move",
];

impl GameApi {
    /// Create a new game API for a universe
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        store: Arc<ObjectStore>,
        classes: Arc<RwLock<ClassRegistry>>,
        actions: Arc<ActionRegistry>,
        messages: Arc<MessageQueue>,
        permissions: Arc<PermissionManager>,
        timers: Arc<TimerManager>,
        credits: Arc<CreditManager>,
        venice: Arc<VeniceClient>,
        image_store: Arc<crate::images::ImageStore>,
        effects: Arc<crate::combat::EffectRegistry>,
        universe_id: &str,
    ) -> Self {
        Self {
            store,
            classes,
            actions,
            messages,
            permissions,
            timers,
            credits,
            venice,
            image_store,
            effects,
            universe_id: universe_id.to_string(),
            current_room_id: None,
            current_user_id: None,
            current_object_id: None,
            payer: None,
            time_override: Arc::new(AtomicU64::new(0)),
            rng: Arc::new(Mutex::new(StdRng::from_rng(&mut rand::rng()))),
            use_depth: 0,
            use_calls: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            engine: None,
        }
    }

    /// A game API for running another object's code from inside a script:
    /// same managers and message queue, one level deeper, no contexts set.
    fn nested(&self) -> Self {
        Self {
            store: self.store.clone(),
            classes: self.classes.clone(),
            actions: self.actions.clone(),
            messages: self.messages.clone(),
            permissions: self.permissions.clone(),
            timers: self.timers.clone(),
            credits: self.credits.clone(),
            venice: self.venice.clone(),
            image_store: self.image_store.clone(),
            effects: self.effects.clone(),
            universe_id: self.universe_id.clone(),
            current_room_id: None,
            current_user_id: None,
            current_object_id: None,
            // Nested object code never spends the caller's credits
            payer: None,
            time_override: self.time_override.clone(),
            rng: self.rng.clone(),
            use_depth: self.use_depth + 1,
            use_calls: self.use_calls.clone(),
            engine: self.engine.clone(),
        }
    }

    /// Set the account paying for credit spending as `"actor"`
    pub fn set_payer(&mut self, payer: Option<String>) {
        self.payer = payer;
    }

    /// Set the combat engine reached by game.attack and friends
    pub fn set_engine(&mut self, engine: Arc<dyn crate::lua::engine::Engine>) {
        self.engine = Some(engine);
    }

    /// Set the current user context for permission checks
    pub fn set_user_context(&mut self, user_id: Option<String>) {
        self.current_user_id = user_id;
    }

    /// Get the permission manager
    pub fn permission_manager(&self) -> Arc<PermissionManager> {
        self.permissions.clone()
    }

    /// Set the current room context for action registration
    pub fn set_room_context(&mut self, room_id: Option<String>) {
        self.current_room_id = room_id;
    }

    /// Set the current object context for timer registration
    pub fn set_object_context(&mut self, object_id: Option<String>) {
        self.current_object_id = object_id;
    }

    /// Get the timer manager
    pub fn timer_manager(&self) -> Arc<TimerManager> {
        self.timers.clone()
    }

    /// Get the message queue for draining after execution
    pub fn message_queue(&self) -> Arc<MessageQueue> {
        self.messages.clone()
    }

    /// Get the action registry
    pub fn action_registry(&self) -> Arc<ActionRegistry> {
        self.actions.clone()
    }

    /// Register the game API in a Lua state
    pub fn register(&self, lua: &Lua) -> LuaResult<()> {
        let globals = lua.globals();

        // Create the 'game' table
        let game = lua.create_table()?;

        // Register functions
        self.register_object_functions(lua, &game)?;
        self.register_class_functions(lua, &game)?;
        self.register_query_functions(lua, &game)?;
        self.register_action_functions(lua, &game)?;
        self.register_message_functions(lua, &game)?;
        self.register_permission_functions(lua, &game)?;
        self.register_timer_functions(lua, &game)?;
        self.register_effect_functions(lua, &game)?;
        self.register_credit_functions(lua, &game)?;
        self.register_venice_functions(lua, &game)?;
        self.register_utility_functions(lua, &game)?;

        globals.set("game", game)?;

        // Add parent() function for calling parent class handlers
        // This is a global function, not on the game table
        let classes = self.classes.clone();
        let store = self.store.clone();
        let parent_universe = self.universe_id.clone();
        let parent_fn = lua.create_function(
            move |lua, (class_name, handler_name, args): (String, String, Table)| {
                charge_db(lua)?;
                let classes = classes.clone();
                let store = store.clone();
                let universe_id = parent_universe.clone();

                // Get the parent class name
                let parent_class = tokio::task::block_in_place(|| {
                    tokio::runtime::Handle::current().block_on(async {
                        let registry = classes.read().await;
                        let class = registry.get_class(&universe_id, &class_name);
                        class.and_then(|c| c.parent.clone())
                    })
                });

                let parent_class_name = match parent_class {
                    Some(p) => p,
                    None => return Ok(Value::Nil), // No parent class
                };

                // Get the parent class's code hash (if it has one)
                let parent_code = tokio::task::block_in_place(|| {
                    tokio::runtime::Handle::current().block_on(async {
                        let registry = classes.read().await;
                        let parent = registry.get_class(&universe_id, &parent_class_name);
                        match parent {
                            Some(class_def) => {
                                // The class's code: ClassDef.code_hash, or a code_hash
                                // property default (how define_class attaches it)
                                let hash = class_def.code_hash.clone().or_else(|| {
                                    class_def
                                        .properties
                                        .get("code_hash")
                                        .and_then(|v| v.as_str())
                                        .map(str::to_string)
                                });
                                match hash {
                                    Some(hash) => store.get_code(&hash).await.ok().flatten(),
                                    None => None,
                                }
                            }
                            None => None,
                        }
                    })
                });

                match parent_code {
                    Some(code) => {
                        // Execute parent code to get handlers
                        let chunk = lua.load(&code);
                        let handlers: LuaResult<Table> = chunk.eval();

                        match handlers {
                            Ok(handler_table) => {
                                if let Ok(handler) =
                                    handler_table.get::<Function>(handler_name.as_str())
                                {
                                    // Call parent handler with same args
                                    match handler.call::<Value>(args) {
                                        Ok(result) => Ok(result),
                                        Err(e) => Err(e),
                                    }
                                } else {
                                    Ok(Value::Nil)
                                }
                            }
                            Err(_) => Ok(Value::Nil),
                        }
                    }
                    None => Ok(Value::Nil),
                }
            },
        )?;
        globals.set("parent", parent_fn)?;

        Ok(())
    }

    fn register_object_functions(&self, lua: &Lua, game: &Table) -> LuaResult<()> {
        let store = self.store.clone();
        let universe_id = self.universe_id.clone();
        let current_user = self.current_user_id.clone();
        let permissions = self.permissions.clone();

        // game.create_object(path, class, parent_id, props)
        // Actually creates object in database with current user as owner
        // Returns object on success, or {error = "message"} on path validation failure
        let store_clone = store.clone();
        let universe_clone = universe_id.clone();
        let user_clone = current_user.clone();
        let permissions_clone = permissions.clone();
        let classes_clone = self.classes.clone();
        let create_engine = self.engine.clone();
        let create_object = lua.create_function(
            move |lua,
                  (path, class, parent_id, props): (
                String,
                String,
                Option<String>,
                Option<Table>,
            )| {
                charge_db(lua)?;
                let store = store_clone.clone();
                let universe_id = universe_clone.clone();

                // Owner is the acting user (set_actor override, else the real user)
                let owner_id = actor_override(lua).or(user_clone.clone());

                // Create object with path validation
                let mut obj = match Object::new(&path, &universe_id, &class) {
                    Ok(obj) => obj,
                    Err(e) => {
                        // Return error table for path validation failure
                        let error_table = lua.create_table()?;
                        error_table.set("error", e.to_string())?;
                        return Ok(Value::Table(error_table));
                    }
                };
                obj.parent_id = parent_id;
                obj.owner_id = owner_id; // Set creator as owner

                // Start from the class's defaults (inherited along its chain);
                // the props given here override them
                let classes = classes_clone.clone();
                let class_name = class.clone();
                obj.properties = tokio::task::block_in_place(|| {
                    tokio::runtime::Handle::current().block_on(async {
                        classes
                            .read()
                            .await
                            .resolve_properties(&universe_id, &class_name)
                    })
                });
                // A class's code (its code_hash default, used by parent()) is
                // never attached to instances implicitly: attaching code is an
                // explicit, permission-checked act. Empty placeholder strings
                // (the root class's name/description) are not real values.
                obj.properties.remove("code_hash");
                obj.properties.retain(|_, v| v.as_str() != Some(""));

                // Copy properties from Lua table if provided
                if let Some(props_table) = props {
                    for pair in props_table.pairs::<String, Value>() {
                        let (k, v) = pair?;
                        let json_val = lua_to_json(v)?;
                        obj.properties.insert(k, json_val);
                    }
                }

                // A `code_hash` prop attaches stored code rather than being a property
                match obj.properties.remove("code_hash") {
                    Some(serde_json::Value::String(hash)) => obj.code_hash = Some(hash),
                    Some(serde_json::Value::Null) | None => {}
                    Some(_) => {
                        let error_table = lua.create_table()?;
                        error_table.set("error", "code_hash must be a string")?;
                        return Ok(Value::Table(error_table));
                    }
                }

                if is_reserved_object(&obj.id, &obj.class) {
                    let error_table = lua.create_table()?;
                    error_table.set("error", "player objects are created by the server")?;
                    return Ok(Value::Table(error_table));
                }

                // Check permission, then save to database
                let obj_clone = obj.clone();
                let permissions = permissions_clone.clone();
                let result: anyhow::Result<Result<(), String>> =
                    tokio::task::block_in_place(|| {
                        tokio::runtime::Handle::current().block_on(async {
                            if let Err(reason) = create_permission(
                                &permissions,
                                obj_clone.owner_id.as_deref(),
                                &universe_id,
                                &obj_clone.id,
                            )
                            .await
                            {
                                return Ok(Err(format!("permission denied: {}", reason)));
                            }
                            if let Err(reason) =
                                check_parent(&store, &universe_id, obj_clone.parent_id.as_deref())
                                    .await?
                            {
                                return Ok(Err(reason));
                            }
                            if let Some(ref hash) = obj_clone.code_hash {
                                if store.get_code(hash).await?.is_none() {
                                    return Ok(Err(format!("unknown code_hash {}", hash)));
                                }
                            }
                            store.create(&obj_clone).await?;
                            // Its own code's on_create (as its owner)
                            if obj_clone.code_hash.is_some() {
                                if let Some(ref engine) = create_engine {
                                    engine
                                        .hook(
                                            &universe_id,
                                            &obj_clone.id,
                                            "on_create",
                                            serde_json::json!({}),
                                        )
                                        .await;
                                }
                            }
                            Ok(Ok(()))
                        })
                    });

                match result {
                    Ok(Ok(())) => Ok(Value::Table(object_to_lua(lua, &obj)?)),
                    Ok(Err(reason)) => {
                        let error_table = lua.create_table()?;
                        error_table.set("error", reason)?;
                        Ok(Value::Table(error_table))
                    }
                    Err(e) => Err(mlua::Error::external(e)),
                }
            },
        )?;
        game.set("create_object", create_object)?;

        // game.get_object(id)
        // Actually fetches from database
        let store_clone = store.clone();
        let uni = universe_id.clone();
        let get_object = lua.create_function(move |lua, id: String| {
            charge_db(lua)?;
            let store = store_clone.clone();

            let result = tokio::task::block_in_place(|| {
                tokio::runtime::Handle::current().block_on(async { store.get(&uni, &id).await })
            });

            match result {
                Ok(Some(obj)) => Ok(Value::Table(object_to_lua(lua, &obj)?)),
                Ok(None) => Ok(Value::Nil),
                Err(e) => Err(mlua::Error::external(e)),
            }
        })?;
        game.set("get_object", get_object)?;

        // game.update_object(id, changes)
        // Actually updates object in database
        let store_clone = store.clone();
        let ctx_clone = (
            current_user.clone(),
            universe_id.clone(),
            permissions.clone(),
        );
        let update_object = lua.create_function(move |lua, (id, changes): (String, Table)| {
            charge_db(lua)?;
            let store = store_clone.clone();
            let (user, universe_id, permissions) = ctx_clone.clone();
            let user = actor_override(lua).or(user);

            // Collect changes into a vec first (outside async)
            let mut changes_vec = Vec::new();
            for pair in changes.pairs::<String, Value>() {
                let (k, v) = pair?;
                let json_val = lua_to_json(v)?;
                changes_vec.push((k, json_val));
            }

            let result: anyhow::Result<Result<bool, String>> = tokio::task::block_in_place(|| {
                tokio::runtime::Handle::current().block_on(async {
                    // First get the existing object
                    let obj_result = store.get(&universe_id, &id).await?;
                    match obj_result {
                        Some(obj) => {
                            if let Err(reason) = object_permission(
                                &permissions,
                                user.as_deref(),
                                &universe_id,
                                &obj,
                                PermAction::Modify,
                            )
                            .await
                            {
                                return Ok(Err(reason));
                            }
                            // Write only what changed, in place and in one
                            // batch: rewriting the whole row could put back
                            // a property (hp) changed meanwhile. `code_hash`
                            // attaches (or, if nil, detaches) stored code
                            // instead of setting a property.
                            let mut props = Vec::new();
                            let mut code_change: Option<Option<String>> = None;
                            for (k, v) in changes_vec {
                                if k == "code_hash" {
                                    // The code will run as the object's owner,
                                    // so only the owner or a superior may set it
                                    if let Some(ref owner) = obj.owner_id {
                                        let actor = user.as_deref().unwrap_or_default();
                                        if actor != owner
                                            && !permissions
                                                .can_act_as(&universe_id, actor, owner)
                                                .await
                                        {
                                            return Ok(Err(
                                                "only the owner or a superior may attach code"
                                                    .to_string(),
                                            ));
                                        }
                                    }
                                    match v {
                                        serde_json::Value::String(hash) => {
                                            if store.get_code(&hash).await?.is_none() {
                                                anyhow::bail!("unknown code_hash {}", hash);
                                            }
                                            code_change = Some(Some(hash));
                                        }
                                        serde_json::Value::Null => code_change = Some(None),
                                        _ => anyhow::bail!("code_hash must be a string"),
                                    }
                                } else {
                                    props.push((k, v));
                                }
                            }
                            let mut statements = Vec::new();
                            if !props.is_empty() {
                                statements.push(ObjectStore::set_properties_statement(
                                    &universe_id,
                                    &id,
                                    &props,
                                )?);
                            }
                            if let Some(code) = code_change {
                                statements.push(ObjectStore::set_code_hash_statement(
                                    &universe_id,
                                    &id,
                                    code.as_deref(),
                                ));
                            }
                            if !statements.is_empty() {
                                store.execute_batch(statements).await?;
                            }
                            Ok(Ok(true))
                        }
                        None => Ok(Ok(false)),
                    }
                })
            });

            match result {
                Ok(Ok(success)) => Ok(success),
                Ok(Err(reason)) => Err(denied("update_object", reason)),
                Err(e) => Err(mlua::Error::external(e)),
            }
        })?;
        game.set("update_object", update_object)?;

        // game.delete_object(id)
        // Actually deletes from database
        let store_clone = store.clone();
        let ctx_clone = (
            current_user.clone(),
            universe_id.clone(),
            permissions.clone(),
        );
        let cleanup = (
            self.timers.clone(),
            self.actions.clone(),
            self.effects.clone(),
        );
        let delete_engine = self.engine.clone();
        let delete_object = lua.create_function(move |lua, id: String| {
            charge_db(lua)?;
            let store = store_clone.clone();
            let (user, universe_id, permissions) = ctx_clone.clone();
            let (timers, actions, effects) = cleanup.clone();
            let user = actor_override(lua).or(user);

            let result: anyhow::Result<Result<bool, String>> = tokio::task::block_in_place(|| {
                tokio::runtime::Handle::current().block_on(async {
                    let Some(obj) = store.get(&universe_id, &id).await? else {
                        return Ok(Ok(false));
                    };
                    if let Err(reason) = object_permission(
                        &permissions,
                        user.as_deref(),
                        &universe_id,
                        &obj,
                        PermAction::Delete,
                    )
                    .await
                    {
                        return Ok(Err(reason));
                    }
                    // Its own code's on_destroy first, while it still exists;
                    // anything it put inside goes to the object's room (a
                    // hook must not be able to block the deletion)
                    if obj.code_hash.is_some() {
                        if let Some(ref engine) = delete_engine {
                            let before: std::collections::HashSet<String> = store
                                .get_contents(&universe_id, &id)
                                .await?
                                .into_iter()
                                .map(|o| o.id)
                                .collect();
                            engine
                                .hook(&universe_id, &id, "on_destroy", serde_json::json!({}))
                                .await;
                            for added in store.get_contents(&universe_id, &id).await? {
                                if !before.contains(&added.id) {
                                    store
                                        .move_object(
                                            &universe_id,
                                            &added.id,
                                            obj.parent_id.as_deref(),
                                        )
                                        .await?;
                                }
                            }
                        }
                    }
                    // The object and its balance go together, atomically
                    // (credits has no foreign key: holders may be accounts)
                    store
                        .execute_batch(vec![
                            ObjectStore::delete_statement(&universe_id, &id),
                            crate::credits::CreditManager::delete_statement(
                                &universe_id,
                                &crate::credits::Holder::Object(id.clone()),
                            ),
                        ])
                        .await?;
                    // Nothing may keep running for (or be dispatched to) the
                    // deleted object, nor for a later object reusing its
                    // path. Its timer, heartbeat and effect rows went with
                    // it (cascade); drop them from the caches too.
                    timers.remove_timers_for_object(&universe_id, &id).await;
                    timers.remove_heartbeat(&universe_id, &id).await;
                    effects.cache_clear(&universe_id, &id).await;
                    // Its verb rows (and a room's) went with it (cascade)
                    actions.clear_by_object(&universe_id, &id).await;
                    actions.clear_room(&universe_id, &id).await;
                    Ok(Ok(true))
                })
            });

            match result {
                Ok(Ok(deleted)) => Ok(deleted),
                Ok(Err(reason)) => Err(denied("delete_object", reason)),
                Err(e) => Err(mlua::Error::external(e)),
            }
        })?;
        game.set("delete_object", delete_object)?;

        // game.move_object(id, new_parent_id)
        // Moves the object in the database, then (once the script ends) moves
        // a connected player's session and runs the containers' hooks
        let store_clone = store.clone();
        let move_queue = self.messages.clone();
        let ctx_clone = (
            current_user.clone(),
            universe_id.clone(),
            permissions.clone(),
        );
        let move_object =
            lua.create_function(move |lua, (id, new_parent_id): (String, Option<String>)| {
                charge_db(lua)?;
                let store = store_clone.clone();
                let (user, universe_id, permissions) = ctx_clone.clone();
                let user = actor_override(lua).or(user);

                let result: anyhow::Result<Result<Option<String>, String>> =
                    tokio::task::block_in_place(|| {
                        tokio::runtime::Handle::current().block_on(async {
                            let Some(obj) = store.get(&universe_id, &id).await? else {
                                anyhow::bail!("Object not found: {}", id);
                            };
                            if let Err(reason) = object_permission(
                                &permissions,
                                user.as_deref(),
                                &universe_id,
                                &obj,
                                PermAction::Move,
                            )
                            .await
                            {
                                return Ok(Err(reason));
                            }
                            if let Err(reason) =
                                check_parent(&store, &universe_id, new_parent_id.as_deref()).await?
                            {
                                anyhow::bail!("Destination {}", reason);
                            }
                            store
                                .move_object(&universe_id, &id, new_parent_id.as_deref())
                                .await?;
                            Ok(Ok(obj.parent_id))
                        })
                    });

                match result {
                    Ok(Ok(from)) => {
                        move_queue.moved(
                            &id,
                            from.as_deref(),
                            new_parent_id.as_deref(),
                            user.as_deref(),
                        );
                        Ok(true)
                    }
                    Ok(Err(reason)) => Err(denied("move_object", reason)),
                    Err(e) => Err(mlua::Error::external(e)),
                }
            })?;
        game.set("move_object", move_object)?;

        // game.clone_object(id, new_path, new_parent_id)
        // Actually clones object in database
        // Returns object on success, nil if not found, or {error = "message"} on path validation failure
        let store_clone = store.clone();
        let ctx_clone = (
            current_user.clone(),
            universe_id.clone(),
            permissions.clone(),
        );
        let clone_engine = self.engine.clone();
        let clone_object = lua.create_function(
            move |lua, (id, new_path, new_parent_id): (String, String, Option<String>)| {
                charge_db(lua)?;
                let store = store_clone.clone();
                let (user, universe_id, permissions) = ctx_clone.clone();
                let user = actor_override(lua).or(user);

                // Validate new path first
                let validated_path = match crate::objects::validate_object_path(&new_path) {
                    Ok(path) => path,
                    Err(e) => {
                        let error_table = lua.create_table()?;
                        error_table.set("error", e.to_string())?;
                        return Ok(Value::Table(error_table));
                    }
                };

                let result: anyhow::Result<Result<Option<Object>, String>> =
                    tokio::task::block_in_place(|| {
                        tokio::runtime::Handle::current().block_on(async {
                            let obj_result = store.get(&universe_id, &id).await?;
                            match obj_result {
                                Some(original)
                                    if is_reserved_object(&validated_path, &original.class) =>
                                {
                                    Ok(Err("player objects are created by the server".to_string()))
                                }
                                Some(original) if original.universe_id != universe_id => {
                                    Ok(Err(format!("{} belongs to another universe", original.id)))
                                }
                                Some(original) => {
                                    // Creating the copy needs create rights at the new path
                                    if let Err(reason) = create_permission(
                                        &permissions,
                                        user.as_deref(),
                                        &universe_id,
                                        &validated_path,
                                    )
                                    .await
                                    {
                                        return Ok(Err(reason));
                                    }
                                    let mut cloned = original.clone();
                                    cloned.id = validated_path;
                                    // A copy of worn gear is not worn
                                    cloned.properties.remove("equipped_by");
                                    if let Err(reason) =
                                        check_parent(&store, &universe_id, new_parent_id.as_deref())
                                            .await?
                                    {
                                        return Ok(Err(reason));
                                    }
                                    cloned.parent_id = new_parent_id;
                                    // The copy is the cloner's, so its code
                                    // would run as them: kept only if they
                                    // may act as the original's owner
                                    if let (Some(owner), Some(cloner)) =
                                        (original.owner_id.as_deref(), user.as_deref())
                                    {
                                        if owner != cloner
                                            && !permissions
                                                .can_act_as(&universe_id, cloner, owner)
                                                .await
                                        {
                                            cloned.code_hash = None;
                                        }
                                    }
                                    cloned.owner_id = user.clone();
                                    cloned.created_at = chrono::Utc::now().to_rfc3339();
                                    cloned.updated_at = cloned.created_at.clone();
                                    store.create(&cloned).await?;
                                    if cloned.code_hash.is_some() {
                                        if let Some(ref engine) = clone_engine {
                                            engine
                                                .hook(
                                                    &universe_id,
                                                    &cloned.id,
                                                    "on_create",
                                                    serde_json::json!({}),
                                                )
                                                .await;
                                        }
                                    }
                                    Ok(Ok(Some(cloned)))
                                }
                                None => Ok(Ok(None)),
                            }
                        })
                    });

                match result {
                    Ok(Ok(Some(obj))) => Ok(Value::Table(object_to_lua(lua, &obj)?)),
                    Ok(Ok(None)) => Ok(Value::Nil),
                    Ok(Err(reason)) => {
                        let error_table = lua.create_table()?;
                        error_table.set("error", format!("permission denied: {}", reason))?;
                        Ok(Value::Table(error_table))
                    }
                    Err(e) => Err(mlua::Error::external(e)),
                }
            },
        )?;
        game.set("clone_object", clone_object)?;

        // game.store_code(source) - returns hash (wizard+, PermAction::StoreCode)
        let store_clone = store.clone();
        let universe_clone = universe_id.clone();
        let user_clone = current_user.clone();
        let permissions_clone = permissions.clone();
        let store_code = lua.create_function(move |lua, source: String| {
            charge_db(lua)?;
            let store = store_clone.clone();

            let user_id = actor_override(lua)
                .or(user_clone.clone())
                .ok_or_else(|| mlua::Error::runtime("store_code: no user context"))?;
            let permissions = permissions_clone.clone();
            let universe_id = universe_clone.clone();
            let allowed = block_on_perm(async move {
                let ctx = permissions.get_user_context(&user_id, &universe_id).await;
                let target = ObjectContext {
                    object_id: format!("/universe/{}", universe_id),
                    owner_id: None,
                    is_fixed: false,
                };
                permissions
                    .check_permission(&ctx, PermAction::StoreCode, &target)
                    .is_allowed()
            });
            if !allowed {
                return Err(mlua::Error::runtime(
                    "store_code: permission denied (wizard+ required)",
                ));
            }

            let result = tokio::task::block_in_place(|| {
                tokio::runtime::Handle::current()
                    .block_on(async { store.store_code(&source).await })
            });

            match result {
                Ok(hash) => Ok(hash),
                Err(e) => Err(mlua::Error::external(e)),
            }
        })?;
        game.set("store_code", store_code)?;

        // game.get_code(hash) - returns source
        let store_clone = store.clone();
        let get_code = lua.create_function(move |lua, hash: String| {
            charge_db(lua)?;
            let store = store_clone.clone();

            let result: anyhow::Result<Option<String>> = tokio::task::block_in_place(|| {
                tokio::runtime::Handle::current().block_on(async { store.get_code(&hash).await })
            });

            match result {
                Ok(Some(source)) => Ok(Value::String(lua.create_string(&source)?)),
                Ok(None) => Ok(Value::Nil),
                Err(e) => Err(mlua::Error::external(e)),
            }
        })?;
        game.set("get_code", get_code)?;

        // game.get_children(parent_id, filter) - returns array of objects
        let store_clone = store;
        let get_children =
            lua.create_function(move |lua, (parent_id, filter): (String, Option<Table>)| {
                charge_db(lua)?;
                let store = store_clone.clone();

                let result = tokio::task::block_in_place(|| {
                    tokio::runtime::Handle::current()
                        .block_on(async { store.get_contents(&universe_id, &parent_id).await })
                });

                match result {
                    Ok(objects) => {
                        let table = lua.create_table()?;
                        let mut idx = 1;

                        // Optional class filter
                        let class_filter: Option<String> =
                            filter.as_ref().and_then(|f| f.get::<String>("class").ok());

                        for obj in objects {
                            // Apply filter if specified
                            if let Some(ref class) = class_filter {
                                if &obj.class != class {
                                    continue;
                                }
                            }
                            table.set(idx, object_to_lua(lua, &obj)?)?;
                            idx += 1;
                        }
                        Ok(table)
                    }
                    Err(e) => Err(mlua::Error::external(e)),
                }
            })?;
        game.set("get_children", get_children)?;

        Ok(())
    }

    fn register_class_functions(&self, lua: &Lua, game: &Table) -> LuaResult<()> {
        let classes = self.classes.clone();
        let store = self.store.clone();

        // game.define_class(name, definition)
        // Registers a class in the script's universe, with parent and
        // properties. Wizard+ only; built-in base classes cannot be redefined.
        let classes_clone = classes.clone();
        let perm_ctx = (
            self.permissions.clone(),
            self.current_user_id.clone(),
            self.universe_id.clone(),
        );
        let define_class =
            lua.create_function(move |lua, (name, definition): (String, Table)| {
                charge_db(lua)?;
                let classes = classes_clone.clone();

                if crate::objects::ClassRegistry::is_builtin(&name) {
                    return Err(mlua::Error::runtime(format!(
                        "define_class: {} is a built-in class",
                        name
                    )));
                }
                let (permissions, user, universe_id) = perm_ctx.clone();
                let class_universe = universe_id.clone();
                let user = actor_override(lua).or(user);
                let caller = user.clone();
                let (allowed, is_admin) = block_on_perm(async move {
                    let Some(user) = user else {
                        return (false, false);
                    };
                    let ctx = permissions.get_user_context(&user, &universe_id).await;
                    let target = ObjectContext {
                        object_id: format!("/universe/{}", universe_id),
                        owner_id: None,
                        is_fixed: false,
                    };
                    let allowed = permissions
                        .check_permission(&ctx, PermAction::StoreCode, &target)
                        .is_allowed();
                    (allowed, ctx.access_level.can_admin())
                });
                if !allowed {
                    return Err(mlua::Error::runtime(
                        "define_class: permission denied (wizard+ required)",
                    ));
                }

                // Extract parent from definition
                let parent: Option<String> = definition.get("parent").ok();

                // Extract properties
                let properties: Option<Table> = definition.get("properties").ok();
                let mut props_map = std::collections::HashMap::new();
                if let Some(props) = properties {
                    for (prop_name, prop_def) in props.pairs::<String, Table>().flatten() {
                        let prop_type: String = prop_def
                            .get("type")
                            .unwrap_or_else(|_| "string".to_string());
                        let default_val = prop_def.get::<Value>("default").ok();
                        let json_default = default_val
                            .map(|v| lua_to_json(v).unwrap_or(serde_json::Value::Null))
                            .unwrap_or(serde_json::Value::Null);
                        props_map.insert(prop_name, (prop_type, json_default));
                    }
                }

                // An existing class may be replaced only by whoever defined
                // it, or by an admin
                // Defines are serialized by the define lock. The registry lock
                // is held only briefly: a read lock to check the definer, and
                // the write lock to insert. The Raft write happens with no
                // registry lock held, so it never stalls class lookups.
                let replaced: Result<(), String> = tokio::task::block_in_place(|| {
                    tokio::runtime::Handle::current().block_on(async {
                        let define_lock = classes.read().await.define_lock();
                        let _defining = define_lock.lock().await;
                        let persister = {
                            let registry = classes.read().await;
                            if let Some(existing) = registry.get_class(&class_universe, &name) {
                                if existing.defined_by != caller && !is_admin {
                                    return Err(format!(
                                        "define_class: {} is already defined by another account",
                                        name
                                    ));
                                }
                            }
                            registry.persister()
                        };
                        let class = crate::objects::ClassRegistry::build_class(
                            &name,
                            parent.as_deref(),
                            props_map,
                            caller.as_deref(),
                        );
                        persister
                            .persist(&class_universe, &class)
                            .await
                            .map_err(|e| format!("define_class: {}", e))?;
                        classes.write().await.insert(&class_universe, class);
                        Ok(())
                    })
                });
                replaced.map_err(mlua::Error::runtime)?;

                Ok(true)
            })?;
        game.set("define_class", define_class)?;

        // game.get_class(name)
        // Returns class definition as table
        let classes_clone = classes.clone();
        let class_universe = self.universe_id.clone();
        let get_class = lua.create_function(move |lua, name: String| {
            charge_db(lua)?;
            let classes = classes_clone.clone();

            let result = tokio::task::block_in_place(|| {
                tokio::runtime::Handle::current().block_on(async {
                    let registry = classes.read().await;
                    registry.get_class(&class_universe, &name).cloned()
                })
            });

            match result {
                Some(class_def) => {
                    let table = lua.create_table()?;
                    table.set("name", class_def.name.as_str())?;
                    if let Some(parent) = &class_def.parent {
                        table.set("parent", parent.as_str())?;
                    }
                    // Add properties (simplified - just name -> default value)
                    let props_table = lua.create_table()?;
                    for (prop_name, default_val) in &class_def.properties {
                        props_table.set(prop_name.as_str(), json_to_lua(lua, default_val)?)?;
                    }
                    table.set("properties", props_table)?;
                    // Add handlers
                    let handlers_table = lua.create_table()?;
                    for (i, handler) in class_def.handlers.iter().enumerate() {
                        handlers_table.set(i + 1, handler.as_str())?;
                    }
                    table.set("handlers", handlers_table)?;
                    Ok(Value::Table(table))
                }
                None => Ok(Value::Nil),
            }
        })?;
        game.set("get_class", get_class)?;

        // game.is_a(obj_id, class_name)
        // Checks if object is of class or inherits from it
        let store_clone = store.clone();
        let classes_clone = classes.clone();
        let universe_id = self.universe_id.clone();
        let is_a = lua.create_function(move |lua, (obj_id, class_name): (String, String)| {
            charge_db(lua)?;
            let store = store_clone.clone();
            let classes = classes_clone.clone();

            let result: anyhow::Result<bool> = tokio::task::block_in_place(|| {
                tokio::runtime::Handle::current().block_on(async {
                    let obj = store.get(&universe_id, &obj_id).await?;
                    match obj {
                        Some(o) => {
                            let registry = classes.read().await;
                            Ok(registry.is_a(&universe_id, &o.class, &class_name))
                        }
                        None => Ok(false),
                    }
                })
            });

            match result {
                Ok(is_match) => Ok(is_match),
                Err(e) => Err(mlua::Error::external(e)),
            }
        })?;
        game.set("is_a", is_a)?;

        // game.get_class_chain(class_name)
        // Returns inheritance chain as array
        let class_universe = self.universe_id.clone();
        let get_class_chain = lua.create_function(move |lua, name: String| {
            charge_db(lua)?;
            let classes = classes.clone();

            let chain = tokio::task::block_in_place(|| {
                tokio::runtime::Handle::current().block_on(async {
                    let registry = classes.read().await;
                    registry.get_inheritance_chain(&class_universe, &name)
                })
            });

            let table = lua.create_table()?;
            for (i, class_name) in chain.iter().enumerate() {
                table.set(i + 1, class_name.as_str())?;
            }
            Ok(table)
        })?;
        game.set("get_class_chain", get_class_chain)?;

        Ok(())
    }

    fn register_query_functions(&self, lua: &Lua, game: &Table) -> LuaResult<()> {
        let store = self.store.clone();
        let universe = self.universe_id.clone();

        // game.environment(obj_id)
        // Returns the parent object (container/room)
        let store_clone = store.clone();
        let universe_id = universe.clone();
        let environment = lua.create_function(move |lua, obj_id: String| {
            charge_db(lua)?;
            let store = store_clone.clone();

            let result = tokio::task::block_in_place(|| {
                tokio::runtime::Handle::current()
                    .block_on(async { store.get_environment(&universe_id, &obj_id).await })
            });

            match result {
                Ok(Some(obj)) => Ok(Value::Table(object_to_lua(lua, &obj)?)),
                Ok(None) => Ok(Value::Nil),
                Err(e) => Err(mlua::Error::external(e)),
            }
        })?;
        game.set("environment", environment)?;

        // game.all_inventory(obj_id)
        // Returns all contents of an object
        let store_clone = store.clone();
        let universe_id = universe.clone();
        let all_inventory = lua.create_function(move |lua, obj_id: String| {
            charge_db(lua)?;
            let store = store_clone.clone();

            let result = tokio::task::block_in_place(|| {
                tokio::runtime::Handle::current()
                    .block_on(async { store.get_contents(&universe_id, &obj_id).await })
            });

            match result {
                Ok(objects) => {
                    let table = lua.create_table()?;
                    for (i, obj) in objects.iter().enumerate() {
                        table.set(i + 1, object_to_lua(lua, obj)?)?;
                    }
                    Ok(table)
                }
                Err(e) => Err(mlua::Error::external(e)),
            }
        })?;
        game.set("all_inventory", all_inventory)?;

        // game.present(name, env_id)
        // Find object by name in a location
        let store_clone = store.clone();
        let universe_id = universe.clone();
        let present = lua.create_function(move |lua, (name, env_id): (String, String)| {
            charge_db(lua)?;
            let store = store_clone.clone();

            let result = tokio::task::block_in_place(|| {
                tokio::runtime::Handle::current()
                    .block_on(async { store.find_by_name(&universe_id, &env_id, &name).await })
            });

            match result {
                Ok(Some(obj)) => Ok(Value::Table(object_to_lua(lua, &obj)?)),
                Ok(None) => Ok(Value::Nil),
                Err(e) => Err(mlua::Error::external(e)),
            }
        })?;
        game.set("present", present)?;

        // game.get_living_in(env_id)
        // Returns living entities in a location: anything whose class is
        // living or descends from it (players, npcs and their subclasses)
        let classes = self.classes.clone();
        let universe_id = universe.clone();
        let get_living_in = lua.create_function(move |lua, env_id: String| {
            charge_db(lua)?;
            let store = store.clone();
            let classes = classes.clone();

            let result = tokio::task::block_in_place(|| {
                tokio::runtime::Handle::current().block_on(async {
                    let contents = store.get_contents(&universe_id, &env_id).await?;
                    let registry = classes.read().await;
                    Ok::<_, anyhow::Error>(
                        contents
                            .into_iter()
                            .filter(|o| registry.is_a(&universe_id, &o.class, "living"))
                            .collect::<Vec<_>>(),
                    )
                })
            });

            match result {
                Ok(objects) => {
                    let table = lua.create_table()?;
                    for (i, obj) in objects.iter().enumerate() {
                        table.set(i + 1, object_to_lua(lua, obj)?)?;
                    }
                    Ok(table)
                }
                Err(e) => Err(mlua::Error::external(e)),
            }
        })?;
        game.set("get_living_in", get_living_in)?;

        Ok(())
    }

    fn register_action_functions(&self, lua: &Lua, game: &Table) -> LuaResult<()> {
        let actions = self.actions.clone();
        let room_id = self.current_room_id.clone();
        // (store, permissions, real user, universe) for the Modify check
        let perm_ctx = (
            self.store.clone(),
            self.permissions.clone(),
            self.current_user_id.clone(),
            self.universe_id.clone(),
        );

        /// Whether the acting user may wire verbs to `object_id`'s code. The
        /// code runs as the object's owner, so this needs Modify access *and*
        /// being the owner or outranking them (like attaching code).
        fn may_modify(
            lua: &Lua,
            perm_ctx: &(
                Arc<ObjectStore>,
                Arc<PermissionManager>,
                Option<String>,
                String,
            ),
            object_id: &str,
        ) -> bool {
            let (store, permissions, user, universe_id) = perm_ctx.clone();
            let user = actor_override(lua).or(user);
            let object_id = object_id.to_string();
            block_on_perm(async move {
                let Ok(Some(obj)) = store.get(&universe_id, &object_id).await else {
                    return false;
                };
                if object_permission(
                    &permissions,
                    user.as_deref(),
                    &universe_id,
                    &obj,
                    PermAction::Modify,
                )
                .await
                .is_err()
                {
                    return false;
                }
                match (&obj.owner_id, &user) {
                    (None, _) => true,
                    (Some(owner), Some(actor)) if owner == actor => true,
                    (Some(owner), Some(actor)) => {
                        permissions.can_act_as(&universe_id, actor, owner).await
                    }
                    (Some(_), None) => false,
                }
            })
        }

        // game.add_action(verb, object_id, method)
        // Adds a contextual action for the current room
        let actions_clone = actions.clone();
        let room_id_clone = room_id.clone();
        let perm_clone = perm_ctx.clone();
        let action_universe = self.universe_id.clone();
        let add_action = lua.create_function(
            move |lua, (verb, object_id, method): (String, String, String)| {
                let actions = actions_clone.clone();
                let room_id = room_id_clone.clone();
                let universe_id = action_universe.clone();
                if is_private_handler(&method) || !may_modify(lua, &perm_clone, &object_id) {
                    return Ok(false);
                }

                // Use blocking task since we're in sync Lua context
                // In production, this would be handled differently
                let action = Action {
                    verb: verb.clone(),
                    object_id,
                    method,
                };

                // Store action - for now we'll use the object_id as the scope
                // The caller should set up proper room context before execution
                let stored = block_now(async {
                    if let Some(rid) = room_id {
                        actions.add_room_action(&universe_id, &rid, action).await
                    } else {
                        // If no room context, add as object action
                        actions
                            .add_object_action(&universe_id, &action.object_id, action.clone())
                            .await
                    }
                });
                match stored {
                    Ok(()) => Ok(true),
                    Err(e) => {
                        tracing::warn!("add_action not stored: {}", e);
                        Ok(false)
                    }
                }
            },
        )?;
        game.set("add_action", add_action)?;

        // game.remove_action(verb, object_id)
        // Removes a contextual action
        let actions_clone = actions.clone();
        let room_id_clone = room_id;
        let action_universe = self.universe_id.clone();
        let remove_action =
            lua.create_function(move |lua, (verb, object_id): (String, String)| {
                let actions = actions_clone.clone();
                let room_id = room_id_clone.clone();
                let universe_id = action_universe.clone();
                if !may_modify(lua, &perm_ctx, &object_id) {
                    return Ok(false);
                }

                let removed = block_now(async {
                    if let Some(rid) = room_id {
                        actions
                            .remove_room_action(&universe_id, &rid, &verb, &object_id)
                            .await
                    } else {
                        actions
                            .remove_object_action(&universe_id, &object_id, &verb)
                            .await
                    }
                });
                match removed {
                    Ok(()) => Ok(true),
                    Err(e) => {
                        tracing::warn!("remove_action not stored: {}", e);
                        Ok(false)
                    }
                }
            })?;
        game.set("remove_action", remove_action)?;

        Ok(())
    }

    fn register_message_functions(&self, lua: &Lua, game: &Table) -> LuaResult<()> {
        // Messages are queued here and delivered by the script runner after
        // the execution finishes (see api::scripting::deliver).

        // game.send(target_id, message)
        // Send a private message to a player
        let messages = self.messages.clone();
        let send = lua.create_function(move |_, (target_id, message): (String, String)| {
            messages.send(&target_id, &message);
            Ok(true)
        })?;
        game.set("send", send)?;

        // game.broadcast(room_id, message, exclude_id?)
        // Broadcast to all players in a room, optionally skipping one player
        let messages = self.messages.clone();
        let broadcast = lua.create_function(
            move |_, (room_id, message, exclude_id): (String, String, Option<String>)| {
                messages.broadcast(&room_id, &message, exclude_id.as_deref());
                Ok(true)
            },
        )?;
        game.set("broadcast", broadcast)?;

        // game.broadcast_region(region_id, message)
        // Broadcast to all players in rooms under a region
        let messages = self.messages.clone();
        let broadcast_region =
            lua.create_function(move |_, (region_id, message): (String, String)| {
                messages.broadcast_region(&region_id, &message);
                Ok(true)
            })?;
        game.set("broadcast_region", broadcast_region)?;

        Ok(())
    }

    fn register_permission_functions(&self, lua: &Lua, game: &Table) -> LuaResult<()> {
        let permissions = self.permissions.clone();
        let current_user = self.current_user_id.clone();
        let universe_id = self.universe_id.clone();

        // game.set_actor(actor_id)
        // Sets the current actor for permission checks in this Lua context.
        // Callers may only act as accounts strictly below their own level
        // (see PermissionManager::can_act_as); nil clears the override.
        let permissions_clone = permissions.clone();
        let current_user_clone = current_user.clone();
        let actor_universe = self.universe_id.clone();
        let set_actor = lua.create_function(move |lua, actor_id: Option<String>| {
            charge_db(lua)?;
            let Some(id) = actor_id else {
                lua.set_app_data(ActorOverride(None));
                return Ok(true);
            };
            // Without a real user there is no level to check against: refuse
            let Some(real) = current_user_clone.clone() else {
                return Ok(false);
            };
            if id != real {
                let permissions = permissions_clone.clone();
                let target = id.clone();
                let universe_id = actor_universe.clone();
                let allowed = block_on_perm(async move {
                    permissions.can_act_as(&universe_id, &real, &target).await
                });
                if !allowed {
                    return Ok(false);
                }
            }
            lua.set_app_data(ActorOverride(Some(id)));
            Ok(true)
        })?;
        game.set("set_actor", set_actor)?;

        // game.get_actor()
        // Gets the current actor ID for permission checks
        let get_actor = lua.create_function(|lua, ()| Ok(actor_override(lua)))?;
        game.set("get_actor", get_actor)?;

        // game.check_permission(action, target_id, is_fixed, owner_id)
        // Returns {allowed: bool, error?: string}
        let permissions_clone = permissions.clone();
        let current_user_clone = current_user.clone();
        let universe_clone = universe_id.clone();
        let check_permission = lua.create_function(
            move |lua,
                  (action_str, target_id, is_fixed, owner_id): (
                String,
                String,
                Option<bool>,
                Option<String>,
            )| {
                charge_db(lua)?;
                let permissions = permissions_clone.clone();
                let current_user = current_user_clone.clone();
                let universe_id = universe_clone.clone();

                // Parse action string
                let action = match action_str.as_str() {
                    "read" => PermAction::Read,
                    "modify" => PermAction::Modify,
                    "move" => PermAction::Move,
                    "delete" => PermAction::Delete,
                    "create" => PermAction::Create,
                    "execute" => PermAction::Execute,
                    "store_code" => PermAction::StoreCode,
                    "admin_config" => PermAction::AdminConfig,
                    "grant_credits" => PermAction::GrantCredits,
                    _ => {
                        let result = lua.create_table()?;
                        result.set("allowed", false)?;
                        result.set("error", format!("Unknown action: {}", action_str))?;
                        return Ok(result);
                    }
                };

                // Get user context - prefer the game.set_actor override if set
                let actor_override: Option<String> = actor_override(lua);
                let user_id = actor_override
                    .or(current_user)
                    .unwrap_or_else(|| "anonymous".to_string());

                // Build contexts synchronously using thread spawn
                let result_data = block_now(async {
                    let user_ctx = permissions.get_user_context(&user_id, &universe_id).await;
                    let obj_ctx = ObjectContext {
                        object_id: target_id,
                        owner_id,
                        is_fixed: is_fixed.unwrap_or(false),
                    };
                    permissions.check_permission(&user_ctx, action, &obj_ctx)
                });

                let result = lua.create_table()?;
                match result_data {
                    crate::permissions::PermissionResult::Allowed => {
                        result.set("allowed", true)?;
                    }
                    crate::permissions::PermissionResult::Denied(reason) => {
                        result.set("allowed", false)?;
                        result.set("error", reason)?;
                    }
                }
                Ok(result)
            },
        )?;
        game.set("check_permission", check_permission)?;

        // game.can_access_path(path)
        // Returns true if the current user can access the given path
        let permissions_clone = permissions.clone();
        let current_user_clone = current_user.clone();
        let universe_clone = universe_id.clone();
        let can_access_path = lua.create_function(move |lua, path: String| {
            let permissions = permissions_clone.clone();
            let current_user = current_user_clone.clone();
            let universe_id = universe_clone.clone();

            let actor_override: Option<String> = actor_override(lua);
            let user_id = actor_override
                .or(current_user)
                .unwrap_or_else(|| "anonymous".to_string());

            let result = block_now(async {
                permissions
                    .can_access_path(&user_id, &universe_id, &path)
                    .await
            });

            Ok(result)
        })?;
        game.set("can_access_path", can_access_path)?;

        // game.get_access_level(account_id)
        // Returns the access level of a user as a string
        let permissions_clone = permissions.clone();
        let level_universe = self.universe_id.clone();
        let get_access_level = lua.create_function(move |lua, account_id: String| {
            charge_db(lua)?;
            let permissions = permissions_clone.clone();
            let universe_id = level_universe.clone();

            let level = block_on_perm(async move {
                permissions
                    .get_access_level(&universe_id, &account_id)
                    .await
            });
            Ok(level.as_str().to_string())
        })?;
        game.set("get_access_level", get_access_level)?;

        // game.set_access_level(account_id, level_str)
        // Sets an account's level in the script's universe. Rules:
        // AccessLevel::can_set_access_level (admin+, target below caller,
        // admin grants need owner); "owner" is not a member level. Returns
        // false if refused or if the write fails.
        let permissions_clone = permissions.clone();
        let current_user_clone = current_user.clone();
        let level_universe = self.universe_id.clone();
        let set_access_level =
            lua.create_function(move |lua, (account_id, level_str): (String, String)| {
                charge_db(lua)?;
                let permissions = permissions_clone.clone();

                let level = match level_str.as_str() {
                    "player" => AccessLevel::Player,
                    "builder" => AccessLevel::Builder,
                    "wizard" => AccessLevel::Wizard,
                    "admin" => AccessLevel::Admin,
                    "owner" => AccessLevel::Owner,
                    _ => return Ok(false),
                };

                let Some(caller) = actor_override(lua).or(current_user_clone.clone()) else {
                    return Ok(false);
                };

                let universe_id = level_universe.clone();
                let changed = block_on_perm(async move {
                    let caller_level = permissions.get_access_level(&universe_id, &caller).await;
                    let target_level = permissions
                        .get_access_level(&universe_id, &account_id)
                        .await;
                    if !caller_level.can_set_access_level(target_level, level) {
                        return false;
                    }
                    permissions
                        .set_access_level(&universe_id, &account_id, level)
                        .await
                        .is_ok()
                });

                Ok(changed)
            })?;
        game.set("set_access_level", set_access_level)?;

        // game.grant_path(grantee_id, path_prefix, can_delegate)
        // Grant path access to a user. Returns grant info table or {error: string}
        let permissions_clone = permissions.clone();
        let current_user_clone = current_user.clone();
        let universe_clone = universe_id.clone();
        let grant_path = lua.create_function(
            move |lua, (grantee_id, path_prefix, can_delegate): (String, String, Option<bool>)| {
                charge_db(lua)?;
                let permissions = permissions_clone.clone();
                let current_user = current_user_clone.clone();
                let universe_id = universe_clone.clone();

                let actor_override: Option<String> = actor_override(lua);
                let user_id = actor_override
                    .or(current_user)
                    .unwrap_or_else(|| "anonymous".to_string());

                let result: Result<crate::permissions::PathGrant, String> = block_now(async {
                    let grantor_ctx = permissions.get_user_context(&user_id, &universe_id).await;
                    permissions
                        .grant_path(
                            &grantor_ctx,
                            &grantee_id,
                            &universe_id,
                            &path_prefix,
                            can_delegate.unwrap_or(false),
                        )
                        .await
                        .map_err(|e| e.to_string())
                });

                match result {
                    Ok(grant) => {
                        let table = lua.create_table()?;
                        table.set("id", grant.id.as_str())?;
                        table.set("grantee_id", grant.grantee_id.as_str())?;
                        table.set("path_prefix", grant.path_prefix.as_str())?;
                        table.set("can_delegate", grant.can_delegate)?;
                        table.set("granted_by", grant.granted_by.as_str())?;
                        table.set("granted_at", grant.granted_at.as_str())?;
                        Ok(Value::Table(table))
                    }
                    Err(e) => {
                        let table = lua.create_table()?;
                        table.set("error", e)?;
                        Ok(Value::Table(table))
                    }
                }
            },
        )?;
        game.set("grant_path", grant_path)?;

        // game.revoke_path(grant_id)
        // Revoke a path grant. Returns true if revoked, false if not found, or {error: string}
        let permissions_clone = permissions.clone();
        let current_user_clone = current_user.clone();
        let universe_clone = universe_id.clone();
        let revoke_path = lua.create_function(move |lua, grant_id: String| {
            charge_db(lua)?;
            let permissions = permissions_clone.clone();
            let current_user = current_user_clone.clone();
            let universe_id = universe_clone.clone();

            let actor_override: Option<String> = actor_override(lua);
            let user_id = actor_override
                .or(current_user)
                .unwrap_or_else(|| "anonymous".to_string());

            let result: Result<bool, String> = block_now(async {
                let revoker_ctx = permissions.get_user_context(&user_id, &universe_id).await;
                permissions
                    .revoke_path(&revoker_ctx, &grant_id, &universe_id)
                    .await
                    .map_err(|e| e.to_string())
            });

            match result {
                Ok(revoked) => Ok(Value::Boolean(revoked)),
                Err(e) => {
                    let table = lua.create_table()?;
                    table.set("error", e)?;
                    Ok(Value::Table(table))
                }
            }
        })?;
        game.set("revoke_path", revoke_path)?;

        // game.get_path_grants(account_id)
        // Get all path grants for a user. Returns array of grant info tables.
        let permissions_clone = permissions;
        let universe_clone = universe_id;
        let get_path_grants = lua.create_function(move |lua, account_id: String| {
            charge_db(lua)?;
            let permissions = permissions_clone.clone();
            let universe_id = universe_clone.clone();

            let grants =
                block_now(async { permissions.get_path_grants(&account_id, &universe_id).await });

            let table = lua.create_table()?;
            for (i, grant) in grants.iter().enumerate() {
                let grant_table = lua.create_table()?;
                grant_table.set("id", grant.id.as_str())?;
                grant_table.set("grantee_id", grant.grantee_id.as_str())?;
                grant_table.set("path_prefix", grant.path_prefix.as_str())?;
                grant_table.set("can_delegate", grant.can_delegate)?;
                grant_table.set("granted_by", grant.granted_by.as_str())?;
                grant_table.set("granted_at", grant.granted_at.as_str())?;
                table.set(i + 1, grant_table)?;
            }
            Ok(table)
        })?;
        game.set("get_path_grants", get_path_grants)?;

        Ok(())
    }

    fn register_effect_functions(&self, lua: &Lua, game: &Table) -> LuaResult<()> {
        use crate::combat::{DamageType, EffectType, StatusEffect};
        use std::str::FromStr;

        /// Bounds a script may set: duration and pulse period (seconds),
        /// and magnitude
        const MAX_DURATION_SECS: f64 = 86_400.0;
        const MIN_PERIOD_SECS: f64 = 0.5;
        const MAX_PERIOD_SECS: f64 = 3_600.0;
        const MAX_MAGNITUDE: i64 = 10_000;

        fn effect_type(name: &str) -> LuaResult<EffectType> {
            EffectType::from_str(name)
                .map_err(|_| mlua::Error::runtime(format!("unknown effect type {:?}", name)))
        }

        // game.apply_effect(target_id, type, duration_secs, magnitude [, damage_type [, period_secs]])
        // Adds (or refreshes) a status effect. Damage over time (or any
        // effect given a damage type) and regenerating pulse every period
        // (default 1 s). Checked like other writes to the target (Modify);
        // persisted, so it survives a restart.
        let (store, perms, effects, universe, user) = (
            self.store.clone(),
            self.permissions.clone(),
            self.effects.clone(),
            self.universe_id.clone(),
            self.current_user_id.clone(),
        );
        let apply_effect = lua.create_function(
            move |lua,
                  (target, kind, duration, magnitude, dtype, period): (
                String,
                String,
                f64,
                i64,
                Option<String>,
                Option<f64>,
            )| {
                charge_db(lua)?;
                if !duration.is_finite() || period.is_some_and(|p| !p.is_finite()) {
                    return Err(mlua::Error::runtime(
                        "apply_effect: duration and period must be finite numbers",
                    ));
                }
                let effect_type = effect_type(&kind)?;
                let damage_type = match dtype {
                    Some(name) => Some(DamageType::from_str(&name).map_err(|_| {
                        mlua::Error::runtime(format!("unknown damage type {:?}", name))
                    })?),
                    // Damage-over-time effects default their damage type
                    None => match effect_type {
                        EffectType::Poisoned => Some(DamageType::Poison),
                        EffectType::Burning => Some(DamageType::Fire),
                        EffectType::Bleeding => Some(DamageType::Physical),
                        _ => None,
                    },
                };
                let now = crate::combat::now_ms();
                let duration_ms = (duration.clamp(0.1, MAX_DURATION_SECS) * 1000.0) as u64;
                let magnitude = magnitude.clamp(-MAX_MAGNITUDE, MAX_MAGNITUDE) as i32;
                let pulses = damage_type.is_some() || effect_type == EffectType::Regenerating;
                let effect = if pulses {
                    let period = period
                        .unwrap_or(1.0)
                        .clamp(MIN_PERIOD_SECS, MAX_PERIOD_SECS);
                    StatusEffect::periodic(
                        effect_type,
                        now,
                        duration_ms,
                        (period * 1000.0) as u64,
                        magnitude,
                        damage_type,
                    )
                } else {
                    StatusEffect::new(effect_type, now, duration_ms, magnitude)
                };
                let user = actor_override(lua).or(user.clone());
                let (store, perms, effects, universe) = (
                    store.clone(),
                    perms.clone(),
                    effects.clone(),
                    universe.clone(),
                );
                let result: anyhow::Result<Result<(), String>> = block_now(async move {
                    let Some(obj) = store.get(&universe, &target).await? else {
                        anyhow::bail!("Object not found: {}", target);
                    };
                    if let Err(reason) = object_permission(
                        &perms,
                        user.as_deref(),
                        &universe,
                        &obj,
                        PermAction::Modify,
                    )
                    .await
                    {
                        return Ok(Err(reason));
                    }
                    effects.apply(&store, &universe, &target, effect).await?;
                    Ok(Ok(()))
                });
                match result {
                    Ok(Ok(())) => Ok(true),
                    Ok(Err(reason)) => Err(denied("apply_effect", reason)),
                    Err(e) => Err(mlua::Error::external(e)),
                }
            },
        )?;
        game.set("apply_effect", apply_effect)?;

        // game.remove_effect(target_id, type) -> true if it was present
        let (store, perms, effects, universe, user) = (
            self.store.clone(),
            self.permissions.clone(),
            self.effects.clone(),
            self.universe_id.clone(),
            self.current_user_id.clone(),
        );
        let remove_effect = lua.create_function(move |lua, (target, kind): (String, String)| {
            charge_db(lua)?;
            let effect_type = effect_type(&kind)?;
            let user = actor_override(lua).or(user.clone());
            let (store, perms, effects, universe) = (
                store.clone(),
                perms.clone(),
                effects.clone(),
                universe.clone(),
            );
            let result: anyhow::Result<Result<bool, String>> = block_now(async move {
                let Some(obj) = store.get(&universe, &target).await? else {
                    anyhow::bail!("Object not found: {}", target);
                };
                if let Err(reason) =
                    object_permission(&perms, user.as_deref(), &universe, &obj, PermAction::Modify)
                        .await
                {
                    return Ok(Err(reason));
                }
                if !effects
                    .get(&universe, &target)
                    .await
                    .has(effect_type, crate::combat::now_ms())
                {
                    return Ok(Ok(false));
                }
                effects
                    .remove(&store, &universe, &target, effect_type)
                    .await?;
                Ok(Ok(true))
            });
            match result {
                Ok(Ok(removed)) => Ok(removed),
                Ok(Err(reason)) => Err(denied("remove_effect", reason)),
                Err(e) => Err(mlua::Error::external(e)),
            }
        })?;
        game.set("remove_effect", remove_effect)?;

        // game.get_effects(target_id) -> {{type, remaining, magnitude, damage_type, period}, ...}
        let (effects, universe) = (self.effects.clone(), self.universe_id.clone());
        let get_effects = lua.create_function(move |lua, target: String| {
            charge_db(lua)?;
            let (effects, universe) = (effects.clone(), universe.clone());
            let current = block_now(async move { effects.get(&universe, &target).await });
            let now = crate::combat::now_ms();
            let list = lua.create_table()?;
            for (i, e) in current.active(now).into_iter().enumerate() {
                let t = lua.create_table()?;
                t.set("type", e.effect_type.to_string())?;
                // Whole seconds left, rounded up
                t.set("remaining", (e.expires_at - now).div_ceil(1000))?;
                t.set("magnitude", e.magnitude)?;
                t.set("damage_type", e.damage_type.map(|d| d.to_string()))?;
                t.set("period", e.period_ms.map(|p| p as f64 / 1000.0))?;
                list.set(i + 1, t)?;
            }
            Ok(list)
        })?;
        game.set("get_effects", get_effects)?;

        self.register_combat(lua, game)?;
        Ok(())
    }

    fn register_timer_functions(&self, lua: &Lua, game: &Table) -> LuaResult<()> {
        let timers = self.timers.clone();
        let universe_id = self.universe_id.clone();
        let current_object = self.current_object_id.clone();

        // game.call_out(delay_secs, method, ...)
        // Schedule a one-shot timer to call a method after delay
        // Returns timer_id
        let timers_clone = timers.clone();
        let universe_clone = universe_id.clone();
        let object_clone = current_object.clone();
        let call_out = lua.create_function(
            move |lua, (delay_secs, method, args): (f64, String, Option<String>)| {
                charge_db(lua)?;
                let timers = timers_clone.clone();
                let universe_id = universe_clone.clone();
                let object_id = object_clone.clone();

                // Negative/NaN delays fire after the 100 ms minimum (MIN_TIMER_DELAY_MS)
                let delay_ms = (delay_secs.max(0.0) * 1000.0) as u64;

                let timer_id = block_now(async {
                    // "" when there is no object context or the object is
                    // at its pending-timer limit
                    match object_id {
                        Some(obj_id) => {
                            let timer = Timer::new(&universe_id, &obj_id, &method, delay_ms, args);
                            timers.try_add_timer(timer).await.unwrap_or_default()
                        }
                        None => String::new(),
                    }
                });

                Ok(timer_id)
            },
        )?;
        game.set("call_out", call_out)?;

        // game.remove_call_out(timer_id)
        // Cancel one of the current object's own timers (false otherwise,
        // and always false outside object code)
        let timers_clone = timers.clone();
        let universe_clone = universe_id.clone();
        let object_clone = current_object.clone();
        let remove_call_out = lua.create_function(move |lua, timer_id: String| {
            charge_db(lua)?;
            let timers = timers_clone.clone();
            let universe_id = universe_clone.clone();
            let Some(object_id) = object_clone.clone() else {
                return Ok(false);
            };

            let removed = block_now(async {
                timers
                    .remove_timer_owned(&timer_id, &universe_id, &object_id)
                    .await
            });

            Ok(removed)
        })?;
        game.set("remove_call_out", remove_call_out)?;

        // game.set_heart_beat(interval_ms)
        // Set a recurring heartbeat for the current object (false outside
        // object code, where there is no object to call)
        let timers_clone = timers.clone();
        let universe_clone = universe_id.clone();
        let object_clone = current_object.clone();
        let set_heart_beat = lua.create_function(move |lua, interval_ms: u64| {
            charge_db(lua)?;
            let timers = timers_clone.clone();
            let universe_id = universe_clone.clone();
            let Some(obj_id) = object_clone.clone() else {
                return Ok(false);
            };

            let stored = block_now(async {
                let hb = HeartBeat::new(&universe_id, &obj_id, interval_ms);
                timers.set_heartbeat(hb).await
            });
            Ok(stored.is_ok())
        })?;
        game.set("set_heart_beat", set_heart_beat)?;

        // game.remove_heart_beat()
        // Remove the heartbeat for the current object
        let object_clone = current_object;
        let remove_heart_beat = lua.create_function(move |lua, ()| {
            charge_db(lua)?;
            let timers = timers.clone();
            let object_id = object_clone.clone();
            let universe_id = universe_id.clone();

            let removed = block_now(async {
                if let Some(obj_id) = object_id {
                    timers.remove_heartbeat(&universe_id, &obj_id).await
                } else {
                    false
                }
            });

            Ok(removed)
        })?;
        game.set("remove_heart_beat", remove_heart_beat)?;

        Ok(())
    }

    fn register_credit_functions(&self, lua: &Lua, game: &Table) -> LuaResult<()> {
        use crate::credits::Holder;
        let credits = self.credits.clone();
        let universe_id = self.universe_id.clone();
        let current_user = self.current_user_id.clone();
        let permissions = self.permissions.clone();
        let payer = self.payer.clone();
        let object = self.current_object_id.clone();

        // "actor" (the paying player) or "object" (the running object)
        // A player pays only through game.request_payment (they confirm)
        let spender = {
            let object = object.clone();
            move |which: &str| -> LuaResult<Holder> {
                match which {
                    "actor" => Err(mlua::Error::runtime(
                        "a player's credits are spent only with their consent: use \
                         game.request_payment",
                    )),
                    "object" => object
                        .clone()
                        .map(Holder::Object)
                        .ok_or_else(|| mlua::Error::runtime("no object here")),
                    _ => Err(mlua::Error::runtime(
                        "payer must be \"actor\" or \"object\"",
                    )),
                }
            }
        };

        // game.get_credits(): the paying player's balance (0 if none)
        let (c, u, p) = (credits.clone(), universe_id.clone(), payer.clone());
        let get_credits = lua.create_function(move |lua, ()| {
            charge_db(lua)?;
            let Some(account) = p.clone() else {
                return Ok(0);
            };
            block_now(c.balance(&u, &Holder::Account(account))).map_err(mlua::Error::external)
        })?;
        game.set("get_credits", get_credits)?;

        // game.object_credits(): the running object's balance
        let (c, u, s) = (credits.clone(), universe_id.clone(), spender.clone());
        let object_credits = lua.create_function(move |lua, ()| {
            charge_db(lua)?;
            let holder = s("object")?;
            block_now(c.balance(&u, &holder)).map_err(mlua::Error::external)
        })?;
        game.set("object_credits", object_credits)?;

        // game.deduct_credits(amount, reason, payer?): spend from "actor"
        // (default) or "object"; false if it has too little
        let (c, u, s) = (credits.clone(), universe_id.clone(), spender.clone());
        let deduct_credits = lua.create_function(
            move |lua, (amount, reason, which): (i64, String, Option<String>)| {
                charge_db(lua)?;
                let holder = s(which.as_deref().unwrap_or("actor"))?;
                let paid =
                    block_now(c.deduct(&u, &holder, amount)).map_err(mlua::Error::external)?;
                if paid {
                    tracing::debug!("credits: {:?} spent {} on {:?}", holder, amount, reason);
                }
                Ok(paid)
            },
        )?;
        game.set("deduct_credits", deduct_credits)?;

        // game.transfer_credits(from, to, amount): from "actor" or "object";
        // to "actor", "object", an account id or an object path. Atomic;
        // false if the payer has too little.
        let (c, u, s) = (credits.clone(), universe_id.clone(), spender);
        let transfer_credits =
            lua.create_function(move |lua, (from, to, amount): (String, String, i64)| {
                charge_db(lua)?;
                let from = s(&from)?;
                let to = match to.as_str() {
                    "actor" | "object" => s(&to)?,
                    other => Holder::parse(other),
                };
                block_now(async {
                    if !c.holder_exists(&u, &to).await? {
                        anyhow::bail!("no such holder {:?}", to);
                    }
                    c.transfer(&u, &from, &to, amount).await
                })
                .map_err(mlua::Error::external)
            })?;
        game.set("transfer_credits", transfer_credits)?;

        // game.request_payment(amount, reason, handler, data?): ask the
        // paying player (the one who typed this verb) to pay `amount` to
        // this object. They get a prompt; if they accept within a minute the
        // credits move and `handler` runs with {payment_id, amount, reason,
        // data, actor_id}. Returns the request id at once.
        let (u, p, o) = (universe_id.clone(), payer.clone(), object.clone());
        let store = self.store.clone();
        let messages = self.messages.clone();
        let request_payment = lua.create_function(
            move |lua, (amount, reason, handler, data): (i64, String, String, Option<String>)| {
                charge_db(lua)?;
                let Some(account) = p.clone() else {
                    return Err(mlua::Error::runtime(
                        "no paying player here (only code a player chose to run, such as a \
                         verb, can ask them to pay)",
                    ));
                };
                let Some(object_id) = o.clone() else {
                    return Err(mlua::Error::runtime("request_payment needs an object"));
                };
                if !(1..=crate::credits::MAX_AMOUNT).contains(&amount) {
                    return Err(mlua::Error::runtime("amount out of range"));
                }
                if is_private_handler(&handler) || handler.is_empty() {
                    return Err(mlua::Error::runtime(
                        "handler cannot be private or an engine hook",
                    ));
                }
                let reason: String = reason.chars().take(200).collect();
                let data = data.unwrap_or_default();
                if data.len() > 4096 {
                    return Err(mlua::Error::runtime("data is limited to 4096 bytes"));
                }
                let id = uuid::Uuid::new_v4().to_string();
                let now = crate::combat::now_ms() as i64;
                let expires = now + crate::api::payments::PAYMENT_DEADLINE_MS as i64;
                let universe_id = u.clone();
                let store = store.clone();
                let pending: i64 = block_now(async {
                    let mut conn = crate::db::run::reader(store.pool()).await?;
                    let count: i64 = sqlx::query_scalar(
                        "SELECT COUNT(*) FROM payment_requests
                         WHERE universe_id = ? AND account_id = ? AND expires_at > ?",
                    )
                    .bind(&universe_id)
                    .bind(&account)
                    .bind(now)
                    .fetch_one(&mut *conn)
                    .await?;
                    anyhow::Ok(count)
                })
                .map_err(mlua::Error::external)?;
                if pending >= crate::api::payments::MAX_PENDING_PER_PLAYER {
                    return Err(mlua::Error::runtime(
                        "this player already has too many unanswered payment requests",
                    ));
                }
                block_now(store.execute_batch(vec![
                        (
                            "DELETE FROM payment_requests WHERE universe_id = ? AND account_id = ?
                         AND expires_at <= ?"
                                .to_string(),
                            vec![
                                serde_json::json!(universe_id),
                                serde_json::json!(account),
                                serde_json::json!(now),
                            ],
                        ),
                        (
                            // The cap is enforced in the insert itself
                            // (concurrent requests cannot both pass a check)
                            "INSERT INTO payment_requests (id, universe_id, account_id, object_id,
                         handler, amount, reason, data, expires_at)
                         SELECT ?, ?, ?, ?, ?, ?, ?, ?, ?
                         WHERE (SELECT COUNT(*) FROM payment_requests
                                WHERE universe_id = ? AND account_id = ? AND expires_at > ?) < ?"
                                .to_string(),
                            vec![
                                serde_json::json!(id),
                                serde_json::json!(universe_id),
                                serde_json::json!(account),
                                serde_json::json!(object_id),
                                serde_json::json!(handler),
                                serde_json::json!(amount),
                                serde_json::json!(reason),
                                serde_json::json!(data),
                                serde_json::json!(expires),
                                serde_json::json!(universe_id),
                                serde_json::json!(account),
                                serde_json::json!(now),
                                serde_json::json!(crate::api::payments::MAX_PENDING_PER_PLAYER),
                            ],
                        ),
                    ]))
                .map_err(mlua::Error::external)?;
                // Read through the run (the insert is not committed yet)
                let stored: Option<i64> = block_now(async {
                    let mut conn = crate::db::run::reader(store.pool()).await?;
                    let row = sqlx::query_scalar("SELECT 1 FROM payment_requests WHERE id = ?")
                        .bind(&id)
                        .fetch_optional(&mut *conn)
                        .await?;
                    anyhow::Ok(row)
                })
                .map_err(mlua::Error::external)?;
                if stored.is_none() {
                    return Err(mlua::Error::runtime(
                        "this player already has too many unanswered payment requests",
                    ));
                }
                let player = crate::player::PlayerManager::player_path(&account);
                messages.confirm(
                    &player,
                    &id,
                    &format!("Pay {} credits for {}?", amount, reason),
                    amount,
                );
                Ok(id)
            },
        )?;
        game.set("request_payment", request_payment)?;

        // game.admin_grant_credits(holder, amount): an account id or object
        // path (admin+, via PermAction::GrantCredits)
        let (c, u) = (credits, universe_id);
        let admin_grant_credits =
            lua.create_function(move |lua, (holder, amount): (String, i64)| {
                charge_db(lua)?;
                let credits = c.clone();
                let universe_id = u.clone();
                let permissions = permissions.clone();
                let Some(user_id) = actor_override(lua).or(current_user.clone()) else {
                    return Ok(false);
                };
                let holder = Holder::parse(&holder);

                let result = block_on_perm(async move {
                    let ctx = permissions.get_user_context(&user_id, &universe_id).await;
                    let target = ObjectContext {
                        object_id: format!("/universe/{}", universe_id),
                        owner_id: None,
                        is_fixed: false,
                    };
                    if !permissions
                        .check_permission(&ctx, PermAction::GrantCredits, &target)
                        .is_allowed()
                    {
                        return false;
                    }
                    matches!(credits.holder_exists(&universe_id, &holder).await, Ok(true))
                        && credits.grant(&universe_id, &holder, amount).await.is_ok()
                });

                Ok(result)
            })?;
        game.set("admin_grant_credits", admin_grant_credits)?;

        Ok(())
    }

    fn register_venice_functions(&self, lua: &Lua, game: &Table) -> LuaResult<()> {
        let venice = self.venice.clone();
        let image_store = self.image_store.clone();
        let billing = VeniceBilling {
            store: self.store.clone(),
            credits: self.credits.clone(),
            universe_id: self.universe_id.clone(),
            owner: self.current_user_id.clone(),
            payer: self.payer.clone(),
            object: self.current_object_id.clone(),
        };

        // game.llm_chat(messages, tier?, opts?)
        // messages: array of {role, content}; tier: "fast", "balanced" or
        // "quality"; opts.payer: "actor" (default) or "object".
        // Returns the response text, or {error = ...}
        let (venice_clone, bill) = (venice.clone(), billing.clone());
        let llm_chat = lua.create_function(
            move |lua, (messages_table, tier_str, opts): (Table, Option<String>, Option<Table>)| {
                charge_venice(lua)?;
                let venice = venice_clone.clone();
                let holder = bill.payer_holder(opts.as_ref())?;

                let mut messages = Vec::new();
                for pair in messages_table.sequence_values::<Table>() {
                    let msg_table = pair?;
                    let role: String = msg_table.get("role")?;
                    let content: String = msg_table.get("content")?;
                    messages.push(ChatMessage { role, content });
                }
                let tier = tier_str
                    .as_deref()
                    .and_then(ModelTier::parse)
                    .unwrap_or(ModelTier::Balanced);

                let result = block_now(bill.paid(holder, "venice_chat_cost", |key| async move {
                    venice.chat(&key, messages, tier).await
                }));
                match result {
                    Ok(response) => Ok(Value::String(lua.create_string(&response)?)),
                    Err(e) => {
                        let result = lua.create_table()?;
                        result.set("error", e)?;
                        Ok(Value::Table(result))
                    }
                }
            },
        )?;
        game.set("llm_chat", llm_chat)?;

        // game.llm_image(prompt, style?, size?, opts?)
        // style: "realistic", "anime", "digital", "painterly"; size: "small",
        // "medium", "large"; opts.payer as for llm_chat.
        // Returns an image hash (for /images/{hash}), or {error = ...}
        let llm_image = lua.create_function(
            move |lua,
                  (prompt, style_str, size_str, opts): (
                String,
                Option<String>,
                Option<String>,
                Option<Table>,
            )| {
                charge_venice(lua)?;
                let venice = venice.clone();
                let image_store = image_store.clone();
                let holder = billing.payer_holder(opts.as_ref())?;
                let style = style_str
                    .as_deref()
                    .and_then(ImageStyle::parse)
                    .unwrap_or(ImageStyle::Realistic);
                let size = size_str
                    .as_deref()
                    .and_then(ImageSize::parse)
                    .unwrap_or(ImageSize::Medium);

                let result =
                    block_now(billing.paid(holder, "venice_image_cost", |key| async move {
                        let image_bytes = venice.generate_image(&key, &prompt, style, size).await?;
                        image_store
                            .store(&image_bytes, "image/png", "llm_image")
                            .await
                    }));
                match result {
                    Ok(hash) => Ok(Value::String(lua.create_string(&hash)?)),
                    Err(e) => {
                        let result = lua.create_table()?;
                        result.set("error", e)?;
                        Ok(Value::Table(result))
                    }
                }
            },
        )?;
        game.set("llm_image", llm_image)?;

        Ok(())
    }

    fn register_utility_functions(&self, lua: &Lua, game: &Table) -> LuaResult<()> {
        let time_override = self.time_override.clone();
        let rng = self.rng.clone();
        let permissions = self.permissions.clone();
        let current_user = self.current_user_id.clone();
        let store = self.store.clone();
        let universe_id = self.universe_id.clone();

        // game.time()
        // Returns current time in milliseconds since epoch
        // If time is overridden (for testing), returns the override value
        let time_override_clone = time_override.clone();
        let get_time = lua.create_function(move |_, ()| {
            let override_val = time_override_clone.load(Ordering::Relaxed);
            if override_val > 0 {
                Ok(override_val)
            } else {
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis() as u64;
                Ok(now)
            }
        })?;
        game.set("time", get_time)?;

        // game.set_time(t)
        // Override current time for testing (wizard+ only)
        // Set to 0 to return to real time
        let time_override_clone = time_override.clone();
        let permissions_clone = permissions.clone();
        let user_clone = current_user.clone();
        let time_universe = self.universe_id.clone();
        let set_time = lua.create_function(move |lua, time_ms: u64| {
            charge_db(lua)?;
            let permissions = permissions_clone.clone();
            let user_id = user_clone.clone();
            let time_override = time_override_clone.clone();
            let universe_id = time_universe.clone();

            // Check for actor override from game.set_actor()
            let actor_override: Option<String> = actor_override(lua);
            let effective_user = actor_override.or(user_id);

            // Check wizard+ permission
            let allowed = block_now(async {
                if let Some(ref uid) = effective_user {
                    let level = permissions.get_access_level(&universe_id, uid).await;
                    level >= AccessLevel::Wizard
                } else {
                    false
                }
            });

            if allowed {
                time_override.store(time_ms, Ordering::Relaxed);
                Ok(true)
            } else {
                Ok(false)
            }
        })?;
        game.set("set_time", set_time)?;

        // game.set_rng_seed(seed)
        // Set RNG seed for reproducible testing (wizard+ only)
        let rng_clone = rng.clone();
        let permissions_clone = permissions.clone();
        let user_clone = current_user.clone();
        let seed_universe = self.universe_id.clone();
        let set_rng_seed = lua.create_function(move |lua, seed: u64| {
            charge_db(lua)?;
            let universe_id = seed_universe.clone();
            let permissions = permissions_clone.clone();
            let user_id = user_clone.clone();
            let rng = rng_clone.clone();

            // Check for actor override from game.set_actor()
            let actor_override: Option<String> = actor_override(lua);
            let effective_user = actor_override.or(user_id);

            // Check wizard+ permission
            let allowed = block_now(async {
                if let Some(ref uid) = effective_user {
                    let level = permissions.get_access_level(&universe_id, uid).await;
                    level >= AccessLevel::Wizard
                } else {
                    false
                }
            });

            if allowed {
                *rng.lock() = StdRng::seed_from_u64(seed);
                Ok(true)
            } else {
                Ok(false)
            }
        })?;
        game.set("set_rng_seed", set_rng_seed)?;

        // game.roll_dice(dice_str)
        // Parse and roll dice notation like "2d6+3", "1d20-2"
        // Returns total roll result
        let roll_dice = lua.create_function(move |_, dice_str: String| {
            let rng = rng.clone();

            // Parse dice notation: NdM[+/-K]
            let result = parse_and_roll_dice(&dice_str, &rng);
            match result {
                Ok(total) => Ok(total),
                Err(e) => Err(mlua::Error::external(e)),
            }
        })?;
        game.set("roll_dice", roll_dice)?;

        // game.use_object(obj_id, actor_id, verb, target_id)
        // Invoke an object's handler. Like every object hook, the handler runs
        // in its own sandbox as the object's owner (never as the caller), with
        // {object_id, actor_id, verb, code_hash, target_id}. Returns the
        // handler's result (tables, strings, numbers, booleans), or nil if the
        // object has no code or no such handler.
        let nested_api = self.nested();
        let use_object =
            lua.create_function(
                move |lua,
                      (obj_id, actor_id, verb, target_id): (
                    String,
                    String,
                    String,
                    Option<String>,
                )| {
                    charge_db(lua)?;
                    if nested_api.use_depth > MAX_USE_DEPTH {
                        return Err(mlua::Error::runtime(format!(
                            "use_object: nested deeper than {} levels",
                            MAX_USE_DEPTH
                        )));
                    }
                    let calls = nested_api
                        .use_calls
                        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    if calls >= MAX_USE_CALLS {
                        return Err(mlua::Error::runtime(format!(
                            "use_object: more than {} calls in one execution",
                            MAX_USE_CALLS
                        )));
                    }
                    let store = nested_api.store.clone();
                    let universe_id = nested_api.universe_id.clone();

                    // Load the object, its code and the universe libraries
                    type Loaded = Option<(Object, String, String, Vec<(String, String)>)>;
                    let loaded: Result<Loaded, String> = tokio::task::block_in_place(|| {
                        tokio::runtime::Handle::current().block_on(async {
                            let obj = match store.get(&universe_id, &obj_id).await {
                                Ok(Some(o)) => o,
                                Ok(None) => return Ok(None),
                                Err(e) => return Err(e.to_string()),
                            };
                            if obj.universe_id != universe_id {
                                return Ok(None);
                            }
                            let Some(hash) = obj.code_hash.clone() else {
                                return Ok(None);
                            };
                            let Some(code) =
                                store.get_code(&hash).await.map_err(|e| e.to_string())?
                            else {
                                return Ok(None);
                            };
                            let libs =
                                super::libs::universe_lib_codes(&store, &universe_id).await?;
                            Ok(Some((obj, hash, code, libs)))
                        })
                    });
                    let Some((obj, code_hash, code, libs)) =
                        loaded.map_err(mlua::Error::runtime)?
                    else {
                        return Ok(Value::Nil);
                    };

                    // Map verb to handler name
                    let handler_name = match verb.as_str() {
                        "use" => "on_use",
                        "hit" => "on_hit",
                        "look" => "on_look",
                        "init" => "on_init",
                        other => other, // Use verb directly if no mapping
                    };
                    // `_name` handlers are private to the object's own code, and
                    // engine hooks only run on real events
                    if is_private_handler(handler_name) {
                        return Ok(Value::Nil);
                    }

                    let mut args = serde_json::json!({
                        "object_id": obj_id,
                        "actor_id": actor_id,
                        "verb": verb,
                        "code_hash": code_hash,
                    });
                    if let Some(tid) = target_id {
                        args["target_id"] = serde_json::Value::String(tid);
                    }

                    // Fresh sandbox, contexts set before registration
                    let mut api = nested_api.nested_for(&obj);
                    api.use_depth = nested_api.use_depth;
                    let deadline = super::Sandbox::deadline_of(lua);
                    let result =
                        run_handler_sandboxed(&api, &libs, &code, handler_name, &args, deadline)
                            .map_err(mlua::Error::runtime)?;
                    match result {
                        Some(json) => json_to_lua(lua, &json),
                        None => Ok(Value::Nil),
                    }
                },
            )?;
        game.set("use_object", use_object)?;

        // game.get_universe()
        // Returns universe info as table {id, name, owner_id, config, created_at}
        let store_clone = store.clone();
        let universe_clone = universe_id.clone();
        let get_universe = lua.create_function(move |lua, ()| {
            charge_db(lua)?;
            let store = store_clone.clone();
            let universe_id = universe_clone.clone();

            let result = tokio::task::block_in_place(|| {
                tokio::runtime::Handle::current()
                    .block_on(async { store.get_universe(&universe_id).await })
            });

            match result {
                Ok(Some(info)) => {
                    let table = lua.create_table()?;
                    table.set("id", info.id.as_str())?;
                    table.set("name", info.name.as_str())?;
                    table.set("owner_id", info.owner_id.as_str())?;
                    table.set("config", json_to_lua(lua, &info.config)?)?;
                    table.set("created_at", info.created_at.as_str())?;
                    Ok(Value::Table(table))
                }
                Ok(None) => Ok(Value::Nil),
                Err(e) => Err(mlua::Error::external(e)),
            }
        })?;
        game.set("get_universe", get_universe)?;

        // game.update_universe(config)
        // Update universe config (admin+, via PermAction::AdminConfig)
        // Merges config into existing universe config
        let store_clone = store;
        let universe_clone = universe_id;
        let permissions_clone = permissions.clone();
        let user_clone = current_user.clone();
        let update_universe = lua.create_function(move |lua, config: Table| {
            charge_db(lua)?;
            let store = store_clone.clone();
            let universe_id = universe_clone.clone();
            let permissions = permissions_clone.clone();
            let Some(user_id) = actor_override(lua).or(user_clone.clone()) else {
                return Ok(false);
            };

            // Convert Lua table to JSON
            let config_json = lua_to_json(Value::Table(config))?;

            let result: Result<bool, anyhow::Error> = tokio::task::block_in_place(|| {
                tokio::runtime::Handle::current().block_on(async {
                    let ctx = permissions.get_user_context(&user_id, &universe_id).await;
                    let target = ObjectContext {
                        object_id: format!("/universe/{}", universe_id),
                        owner_id: None,
                        is_fixed: false,
                    };
                    if !permissions
                        .check_permission(&ctx, PermAction::AdminConfig, &target)
                        .is_allowed()
                    {
                        return Ok(false);
                    }

                    store.update_universe(&universe_id, config_json).await
                })
            });

            match result {
                Ok(success) => Ok(success),
                Err(e) => Err(mlua::Error::external(e)),
            }
        })?;
        game.set("update_universe", update_universe)?;

        Ok(())
    }
}

impl GameApi {
    /// Nested API for running `obj`'s code as its owner, in its room
    fn nested_for(&self, obj: &Object) -> Self {
        let mut api = self.nested();
        api.current_user_id = obj.owner_id.clone();
        api.current_room_id = obj.parent_id.clone();
        api.current_object_id = Some(obj.id.clone());
        api
    }
}

/// Run `handler` from object `code` in a fresh sandbox with `api`
/// registered and the universe libraries loaded. The result crosses back
/// as JSON, so only plain data (no functions) survives.
fn run_handler_sandboxed(
    api: &GameApi,
    libs: &[(String, String)],
    code: &str,
    handler: &str,
    args: &serde_json::Value,
    caller_deadline: Option<std::time::Instant>,
) -> Result<Option<serde_json::Value>, String> {
    let mut sandbox = super::Sandbox::new(super::SandboxConfig::default())
        .map_err(|e| format!("use_object: {}", e))?;
    // Nesting must not extend the calling run's time
    if let Some(deadline) = caller_deadline {
        sandbox.cap_deadline(deadline);
    }
    api.register(sandbox.lua())
        .map_err(|e| format!("use_object: {}", e))?;
    for (name, lib) in libs {
        sandbox
            .execute::<()>(lib)
            .map_err(|e| format!("use_object: library {}: {}", name, e))?;
    }
    let handlers: Table = sandbox
        .execute(code)
        .map_err(|e| format!("use_object: object code: {}", e))?;
    let Ok(func) = handlers.get::<Function>(handler) else {
        return Ok(None);
    };
    let lua_args = json_to_lua(sandbox.lua(), args).map_err(|e| e.to_string())?;
    let value: Value = sandbox
        .call(func, lua_args)
        .map_err(|e| format!("use_object: {}: {}", handler, e))?;
    lua_to_json(value)
        .map(Some)
        .map_err(|e| format!("use_object: {} result: {}", handler, e))
}

/// Parse dice notation (e.g., "2d6+3") and roll
fn parse_and_roll_dice(dice_str: &str, rng: &Arc<Mutex<StdRng>>) -> Result<i64, String> {
    let dice_str = dice_str.trim().to_lowercase();

    // Find the 'd' separator
    let d_pos = dice_str
        .find('d')
        .ok_or("Invalid dice notation: missing 'd'")?;

    // Parse number of dice (default 1)
    let num_dice: i64 = if d_pos == 0 {
        1
    } else {
        dice_str[..d_pos]
            .parse()
            .map_err(|_| "Invalid number of dice")?
    };

    if !(1..=100).contains(&num_dice) {
        return Err("Number of dice must be between 1 and 100".to_string());
    }

    // Find modifier (+/-)
    let rest = &dice_str[d_pos + 1..];
    let (die_size_str, modifier): (&str, i64) = if let Some(plus_pos) = rest.find('+') {
        let mod_val: i64 = rest[plus_pos + 1..]
            .parse()
            .map_err(|_| "Invalid modifier")?;
        (&rest[..plus_pos], mod_val)
    } else if let Some(minus_pos) = rest.find('-') {
        let mod_val: i64 = rest[minus_pos + 1..]
            .parse()
            .map_err(|_| "Invalid modifier")?;
        (&rest[..minus_pos], -mod_val)
    } else {
        (rest, 0)
    };

    let die_size: i64 = die_size_str.parse().map_err(|_| "Invalid die size")?;
    if !(1..=1000).contains(&die_size) {
        return Err("Die size must be between 1 and 1000".to_string());
    }

    // Roll the dice
    let mut total = 0i64;
    let mut rng_guard = rng.lock();
    for _ in 0..num_dice {
        total += rng_guard.random_range(1..=die_size);
    }
    total += modifier;

    Ok(total)
}

/// Convert a Lua value to JSON
fn lua_to_json(value: Value) -> LuaResult<serde_json::Value> {
    match value {
        Value::Nil => Ok(serde_json::Value::Null),
        Value::Boolean(b) => Ok(serde_json::Value::Bool(b)),
        Value::Integer(i) => Ok(serde_json::Value::Number(i.into())),
        Value::Number(n) => Ok(serde_json::json!(n)),
        Value::String(s) => Ok(serde_json::Value::String(s.to_str()?.to_string())),
        Value::Table(t) => {
            // Check if array-like or object-like
            let mut is_array = true;
            let mut max_idx = 0i64;
            for pair in t.clone().pairs::<Value, Value>() {
                let (k, _) = pair?;
                if let Value::Integer(i) = k {
                    if i > 0 {
                        max_idx = max_idx.max(i);
                    } else {
                        is_array = false;
                    }
                } else {
                    is_array = false;
                }
            }

            if is_array && max_idx > 0 {
                let mut arr = Vec::new();
                for i in 1..=max_idx {
                    let v: Value = t.get(i)?;
                    arr.push(lua_to_json(v)?);
                }
                Ok(serde_json::Value::Array(arr))
            } else {
                let mut map = serde_json::Map::new();
                for pair in t.pairs::<String, Value>() {
                    let (k, v) = pair?;
                    map.insert(k, lua_to_json(v)?);
                }
                Ok(serde_json::Value::Object(map))
            }
        }
        _ => Ok(serde_json::Value::Null), // Functions, userdata, etc. become null
    }
}

/// Convert an Object to a Lua table
/// Flattens name and description to root level, remaining properties go to metadata
fn object_to_lua(lua: &Lua, obj: &Object) -> LuaResult<Table> {
    let table = lua.create_table()?;
    table.set("id", obj.id.as_str())?;
    table.set("universe_id", obj.universe_id.as_str())?;
    table.set("class", obj.class.as_str())?;
    table.set("parent_id", obj.parent_id.clone())?;
    table.set("owner_id", obj.owner_id.clone())?;
    table.set("code_hash", obj.code_hash.clone())?;

    // Flatten common properties to root level
    if let Some(name) = obj.properties.get("name") {
        table.set("name", json_to_lua(lua, name)?)?;
    }
    if let Some(desc) = obj.properties.get("description") {
        table.set("description", json_to_lua(lua, desc)?)?;
    }

    // Remaining properties go to metadata
    let metadata = lua.create_table()?;
    for (k, v) in &obj.properties {
        if k != "name" && k != "description" {
            metadata.set(k.as_str(), json_to_lua(lua, v)?)?;
        }
    }
    table.set("metadata", metadata)?;

    Ok(table)
}

/// Convert JSON to Lua value
pub fn json_to_lua(lua: &Lua, value: &serde_json::Value) -> LuaResult<Value> {
    match value {
        serde_json::Value::Null => Ok(Value::Nil),
        serde_json::Value::Bool(b) => Ok(Value::Boolean(*b)),
        serde_json::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                Ok(Value::Integer(i))
            } else if let Some(f) = n.as_f64() {
                Ok(Value::Number(f))
            } else {
                Ok(Value::Nil)
            }
        }
        serde_json::Value::String(s) => Ok(Value::String(lua.create_string(s)?)),
        serde_json::Value::Array(arr) => {
            let table = lua.create_table()?;
            for (i, v) in arr.iter().enumerate() {
                table.set(i + 1, json_to_lua(lua, v)?)?;
            }
            Ok(Value::Table(table))
        }
        serde_json::Value::Object(map) => {
            let table = lua.create_table()?;
            for (k, v) in map {
                table.set(k.as_str(), json_to_lua(lua, v)?)?;
            }
            Ok(Value::Table(table))
        }
    }
}

/// May the acting user make `obj` act (fight, flee)? A player acts only as
/// themselves and never through object code (not even code its own account
/// owns: hooks run when others trigger them); anything else needs `action`
/// permission on it.
async fn may_act_for(
    permissions: &PermissionManager,
    user: Option<&str>,
    object_code: bool,
    universe_id: &str,
    obj: &crate::objects::Object,
    action: PermAction,
) -> Result<(), String> {
    if obj.class == "player" {
        return match user {
            Some(account)
                if !object_code && crate::player::PlayerManager::player_path(account) == obj.id =>
            {
                Ok(())
            }
            _ => Err("a player acts only for themselves".to_string()),
        };
    }
    object_permission(permissions, user, universe_id, obj, action).await
}

impl GameApi {
    /// game.attack / combat_state / damage / heal / can_attack / flee: the
    /// combat engine (api::combat), the single source of combat rules. The
    /// mudlib's Combat library wraps these.
    fn register_combat(&self, lua: &Lua, game: &mlua::Table) -> LuaResult<()> {
        let no_engine = || mlua::Error::runtime("the combat engine is not available here");

        // game.can_attack(attacker, target) -> true | false, reason
        let (engine, universe) = (self.engine.clone(), self.universe_id.clone());
        game.set(
            "can_attack",
            lua.create_function(move |lua, (a, t): (String, String)| {
                charge_db(lua)?;
                let engine = engine.clone().ok_or_else(no_engine)?;
                let universe = universe.clone();
                let r = block_now(async move { engine.can_attack(&universe, &a, &t).await });
                Ok(match r {
                    Ok(()) => (true, None),
                    Err(reason) => (false, Some(reason)),
                })
            })?,
        )?;

        // game.attack(attacker, target) -> {hit, critical, fumble, damage,
        // killed, text} | nil, reason. One blow now; the fight goes on in
        // rounds. The acting user must control the attacker (a player only
        // as themselves).
        let (engine, universe, perms, store, user) = (
            self.engine.clone(),
            self.universe_id.clone(),
            self.permissions.clone(),
            self.store.clone(),
            self.current_user_id.clone(),
        );
        let object_code = self.current_object_id.is_some();
        game.set(
            "attack",
            lua.create_function(move |lua, (a, t): (String, String)| {
                charge_db(lua)?;
                let engine = engine.clone().ok_or_else(no_engine)?;
                let user = actor_override(lua).or(user.clone());
                let (universe, perms, store) = (universe.clone(), perms.clone(), store.clone());
                let r: Result<crate::lua::engine::BlowReport, String> = block_now(async move {
                    let attacker = store
                        .get(&universe, &a)
                        .await
                        .map_err(|e| e.to_string())?
                        .ok_or_else(|| format!("no such object: {}", a))?;
                    may_act_for(
                        &perms,
                        user.as_deref(),
                        object_code,
                        &universe,
                        &attacker,
                        PermAction::Modify,
                    )
                    .await?;
                    engine.attack(&universe, &a, &t).await
                });
                match r {
                    Ok(b) => {
                        let tbl = lua.create_table()?;
                        tbl.set("hit", b.hit)?;
                        tbl.set("critical", b.critical)?;
                        tbl.set("fumble", b.fumble)?;
                        tbl.set("damage", b.damage)?;
                        tbl.set("killed", b.killed)?;
                        tbl.set("text", b.text)?;
                        let parts = lua.create_table()?;
                        for (i, (dtype, rolled, applied)) in b.parts.into_iter().enumerate() {
                            let p = lua.create_table()?;
                            p.set("type", dtype)?;
                            p.set("rolled", rolled)?;
                            p.set("applied", applied)?;
                            parts.set(i + 1, p)?;
                        }
                        tbl.set("parts", parts)?;
                        Ok((Some(tbl), None))
                    }
                    Err(reason) => Ok((None, Some(reason))),
                }
            })?,
        )?;

        // game.combat_state(id) -> {in_combat, attacking, attackers, next_round_at}
        let (engine, universe) = (self.engine.clone(), self.universe_id.clone());
        game.set(
            "combat_state",
            lua.create_function(move |lua, id: String| {
                charge_db(lua)?;
                let engine = engine.clone().ok_or_else(no_engine)?;
                let universe = universe.clone();
                let s = block_now(async move { engine.combat_state(&universe, &id).await })
                    .map_err(mlua::Error::runtime)?;
                let tbl = lua.create_table()?;
                tbl.set("in_combat", s.in_combat)?;
                tbl.set("attacking", s.attacking)?;
                tbl.set("attackers", s.attackers)?;
                tbl.set("next_round_at", s.next_round_at)?;
                Ok(tbl)
            })?,
        )?;

        // game.damage(target, amount, type [, source]) -> {damage, hp, killed}
        // | nil, reason. Resistances apply; a death follows the universe's
        // policy. Checked like other writes to the target (Modify).
        let (engine, universe, perms, store, user) = (
            self.engine.clone(),
            self.universe_id.clone(),
            self.permissions.clone(),
            self.store.clone(),
            self.current_user_id.clone(),
        );
        let object_code = self.current_object_id.is_some();
        game.set(
            "damage",
            lua.create_function(
                move |lua, (t, amount, dtype, source): (String, i64, Option<String>, Option<String>)| {
                    charge_db(lua)?;
                    let engine = engine.clone().ok_or_else(no_engine)?;
                    let user = actor_override(lua).or(user.clone());
                    let (universe, perms, store) = (universe.clone(), perms.clone(), store.clone());
                    let dtype = dtype.unwrap_or_else(|| "physical".to_string());
                    let r: Result<crate::lua::engine::DamageReport, String> = block_now(async move {
                        let target = store
                            .get(&universe, &t)
                            .await
                            .map_err(|e| e.to_string())?
                            .ok_or_else(|| format!("no such object: {}", t))?;
                        object_permission(&perms, user.as_deref(), &universe, &target, PermAction::Modify)
                            .await?;
                        // The source is credited (a kill's XP): the caller
                        // must control it, like an attacker
                        if let Some(ref src) = source {
                            let src_obj = store
                                .get(&universe, src)
                                .await
                                .map_err(|e| e.to_string())?
                                .ok_or_else(|| format!("no such object: {}", src))?;
                            may_act_for(
                                &perms,
                                user.as_deref(),
                                object_code,
                                &universe,
                                &src_obj,
                                PermAction::Modify,
                            )
                            .await?;
                        }
                        engine
                            .damage(&universe, &t, amount, &dtype, source.as_deref())
                            .await
                    });
                    match r {
                        Ok(d) => {
                            let tbl = lua.create_table()?;
                            tbl.set("damage", d.damage)?;
                            tbl.set("hp", d.hp)?;
                            tbl.set("killed", d.killed)?;
                            Ok((Some(tbl), None))
                        }
                        Err(reason) => Ok((None, Some(reason))),
                    }
                },
            )?,
        )?;

        // game.heal(target, amount) -> new hp | nil, reason (Modify)
        let (engine, universe, perms, store, user) = (
            self.engine.clone(),
            self.universe_id.clone(),
            self.permissions.clone(),
            self.store.clone(),
            self.current_user_id.clone(),
        );
        game.set(
            "heal",
            lua.create_function(move |lua, (t, amount): (String, i64)| {
                charge_db(lua)?;
                let engine = engine.clone().ok_or_else(no_engine)?;
                let user = actor_override(lua).or(user.clone());
                let (universe, perms, store) = (universe.clone(), perms.clone(), store.clone());
                let r: Result<i64, String> = block_now(async move {
                    let target = store
                        .get(&universe, &t)
                        .await
                        .map_err(|e| e.to_string())?
                        .ok_or_else(|| format!("no such object: {}", t))?;
                    object_permission(
                        &perms,
                        user.as_deref(),
                        &universe,
                        &target,
                        PermAction::Modify,
                    )
                    .await?;
                    engine.heal(&universe, &t, amount).await
                });
                Ok(match r {
                    Ok(hp) => (Some(hp), None),
                    Err(reason) => (None, Some(reason)),
                })
            })?,
        )?;

        // game.stop_combat(id) -> fighters stopped | nil, reason: ends every
        // fight `id` is in (Modify permission on it)
        let (engine, universe, perms, store, user) = (
            self.engine.clone(),
            self.universe_id.clone(),
            self.permissions.clone(),
            self.store.clone(),
            self.current_user_id.clone(),
        );
        game.set(
            "stop_combat",
            lua.create_function(move |lua, id: String| {
                charge_db(lua)?;
                let engine = engine.clone().ok_or_else(no_engine)?;
                let user = actor_override(lua).or(user.clone());
                let (universe, perms, store) = (universe.clone(), perms.clone(), store.clone());
                let r: Result<u64, String> = block_now(async move {
                    let who = store
                        .get(&universe, &id)
                        .await
                        .map_err(|e| e.to_string())?
                        .ok_or_else(|| format!("no such object: {}", id))?;
                    object_permission(&perms, user.as_deref(), &universe, &who, PermAction::Modify)
                        .await?;
                    engine.stop(&universe, &id).await
                });
                Ok(match r {
                    Ok(n) => (Some(n), None),
                    Err(reason) => (None, Some(reason)),
                })
            })?,
        )?;

        // game.flee(id) -> direction | nil, reason. A player only as
        // themselves; anything else needs Move permission.
        let (engine, universe, perms, store, user) = (
            self.engine.clone(),
            self.universe_id.clone(),
            self.permissions.clone(),
            self.store.clone(),
            self.current_user_id.clone(),
        );
        let object_code = self.current_object_id.is_some();
        game.set(
            "flee",
            lua.create_function(move |lua, id: String| {
                charge_db(lua)?;
                let engine = engine.clone().ok_or_else(no_engine)?;
                let user = actor_override(lua).or(user.clone());
                let (universe, perms, store) = (universe.clone(), perms.clone(), store.clone());
                let r: Result<String, String> = block_now(async move {
                    let who = store
                        .get(&universe, &id)
                        .await
                        .map_err(|e| e.to_string())?
                        .ok_or_else(|| format!("no such object: {}", id))?;
                    may_act_for(
                        &perms,
                        user.as_deref(),
                        object_code,
                        &universe,
                        &who,
                        PermAction::Move,
                    )
                    .await?;
                    engine.flee(&universe, &id).await
                });
                Ok(match r {
                    Ok(dir) => (Some(dir), None),
                    Err(reason) => (None, Some(reason)),
                })
            })?,
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Venice calls are rate-limited under the code's owner whoever pays:
    /// keyed by payer, every object (or player) would be a fresh bucket
    #[tokio::test]
    async fn test_venice_rate_key_is_the_owner() {
        let pool = crate::db::test_utils::test_pool().await;
        let billing = VeniceBilling {
            store: Arc::new(ObjectStore::new(pool.clone(), None)),
            credits: CreditManager::shared(Some(pool), None),
            universe_id: "u".into(),
            owner: Some("builder".into()),
            payer: Some("player".into()),
            object: Some("/items/oracle".into()),
        };
        for holder in [
            None,
            Some(crate::credits::Holder::Account("player".into())),
            Some(crate::credits::Holder::Object("/items/oracle".into())),
        ] {
            let key = billing
                .paid(holder, "venice_chat_cost", |key| async move {
                    Ok::<_, String>(key)
                })
                .await
                .unwrap();
            assert_eq!(key, "u:builder");
        }
        let ownerless = VeniceBilling {
            owner: None,
            ..billing
        };
        let key = ownerless
            .paid(None, "venice_chat_cost", |key| async move {
                Ok::<_, String>(key)
            })
            .await
            .unwrap();
        assert_eq!(key, "u:anonymous");
    }

    #[test]
    fn test_lua_to_json_primitives() {
        let _lua = Lua::new();

        // Nil
        let nil_json = lua_to_json(Value::Nil).unwrap();
        assert!(nil_json.is_null());

        // Boolean
        let bool_json = lua_to_json(Value::Boolean(true)).unwrap();
        assert_eq!(bool_json, serde_json::json!(true));

        // Integer
        let int_json = lua_to_json(Value::Integer(42)).unwrap();
        assert_eq!(int_json, serde_json::json!(42));

        // Number
        let num_json = lua_to_json(Value::Number(2.5)).unwrap();
        assert!((num_json.as_f64().unwrap() - 2.5).abs() < 0.001);
    }

    #[test]
    fn test_json_to_lua_primitives() {
        let lua = Lua::new();

        // Null
        let nil = json_to_lua(&lua, &serde_json::json!(null)).unwrap();
        assert!(matches!(nil, Value::Nil));

        // Boolean
        let bool_val = json_to_lua(&lua, &serde_json::json!(true)).unwrap();
        assert!(matches!(bool_val, Value::Boolean(true)));

        // Integer
        let int_val = json_to_lua(&lua, &serde_json::json!(42)).unwrap();
        assert!(matches!(int_val, Value::Integer(42)));
    }
}
