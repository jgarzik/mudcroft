//! Game API exposed to Lua scripts

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use mlua::{Function, Lua, Result as LuaResult, Table, Value};
use parking_lot::Mutex;
use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};
use tokio::sync::RwLock;

use super::actions::{Action, ActionRegistry};
use super::messaging::MessageQueue;
use crate::credits::CreditManager;
use crate::objects::{ClassRegistry, Object, ObjectStore};
use crate::permissions::{AccessLevel, Action as PermAction, ObjectContext, PermissionManager};
use crate::timers::{HeartBeat, Timer, TimerManager};
use crate::venice::{ChatMessage, ImageSize, ImageStyle, ModelTier, VeniceClient};

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
    universe_id: String,
    current_room_id: Option<String>,
    current_user_id: Option<String>,
    current_object_id: Option<String>,
    /// Time override for testing (milliseconds since epoch, 0 = use real time)
    time_override: Arc<AtomicU64>,
    /// RNG for dice rolls - seeded for reproducibility in tests
    rng: Arc<Mutex<StdRng>>,
}

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
            universe_id: universe_id.to_string(),
            current_room_id: None,
            current_user_id: None,
            current_object_id: None,
            time_override: Arc::new(AtomicU64::new(0)),
            rng: Arc::new(Mutex::new(StdRng::from_rng(&mut rand::rng()))),
        }
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
        self.register_credit_functions(lua, &game)?;
        self.register_venice_functions(lua, &game)?;
        self.register_utility_functions(lua, &game)?;

        globals.set("game", game)?;

        // Add parent() function for calling parent class handlers
        // This is a global function, not on the game table
        let classes = self.classes.clone();
        let store = self.store.clone();
        let parent_fn = lua.create_function(
            move |lua, (class_name, handler_name, args): (String, String, Table)| {
                let classes = classes.clone();
                let store = store.clone();

                // Get the parent class name
                let parent_class = tokio::task::block_in_place(|| {
                    tokio::runtime::Handle::current().block_on(async {
                        let registry = classes.read().await;
                        let class = registry.get_class(&class_name);
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
                        let parent = registry.get_class(&parent_class_name);
                        match parent {
                            Some(class_def) => {
                                // Classes may store their code hash in handlers list
                                // For now, we look for a code_hash in properties
                                if let Some(serde_json::Value::String(hash)) =
                                    class_def.properties.get("code_hash")
                                {
                                    store.get_code(hash).await.ok().flatten()
                                } else {
                                    None
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

        // game.create_object(path, class, parent_id, props)
        // Actually creates object in database with current user as owner
        // Returns object on success, or {error = "message"} on path validation failure
        let store_clone = store.clone();
        let universe_clone = universe_id.clone();
        let create_object = lua.create_function(
            move |lua,
                  (path, class, parent_id, props): (
                String,
                String,
                Option<String>,
                Option<Table>,
            )| {
                let store = store_clone.clone();
                let universe_id = universe_clone.clone();

                // Get current user for ownership
                let globals = lua.globals();
                let actor_override: Option<String> = globals.get("_current_actor_id").ok();
                let owner_id = actor_override;

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

                // Copy properties from Lua table if provided
                if let Some(props_table) = props {
                    for pair in props_table.pairs::<String, Value>() {
                        let (k, v) = pair?;
                        let json_val = lua_to_json(v)?;
                        obj.properties.insert(k, json_val);
                    }
                }

                // Save to database
                let obj_clone = obj.clone();
                let result = tokio::task::block_in_place(|| {
                    tokio::runtime::Handle::current()
                        .block_on(async { store.create(&obj_clone).await })
                });

                match result {
                    Ok(()) => Ok(Value::Table(object_to_lua(lua, &obj)?)),
                    Err(e) => Err(mlua::Error::external(e)),
                }
            },
        )?;
        game.set("create_object", create_object)?;

        // game.get_object(id)
        // Actually fetches from database
        let store_clone = store.clone();
        let get_object = lua.create_function(move |lua, id: String| {
            let store = store_clone.clone();

            let result = tokio::task::block_in_place(|| {
                tokio::runtime::Handle::current().block_on(async { store.get(&id).await })
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
        let update_object = lua.create_function(move |_, (id, changes): (String, Table)| {
            let store = store_clone.clone();

            // Collect changes into a vec first (outside async)
            let mut changes_vec = Vec::new();
            for pair in changes.pairs::<String, Value>() {
                let (k, v) = pair?;
                let json_val = lua_to_json(v)?;
                changes_vec.push((k, json_val));
            }

            let result: anyhow::Result<bool> = tokio::task::block_in_place(|| {
                tokio::runtime::Handle::current().block_on(async {
                    // First get the existing object
                    let obj_result = store.get(&id).await?;
                    match obj_result {
                        Some(mut obj) => {
                            // Apply changes
                            for (k, v) in changes_vec {
                                obj.properties.insert(k, v);
                            }
                            store.update(&obj).await?;
                            Ok(true)
                        }
                        None => Ok(false),
                    }
                })
            });

            match result {
                Ok(success) => Ok(success),
                Err(e) => Err(mlua::Error::external(e)),
            }
        })?;
        game.set("update_object", update_object)?;

        // game.delete_object(id)
        // Actually deletes from database
        let store_clone = store.clone();
        let delete_object = lua.create_function(move |_, id: String| {
            let store = store_clone.clone();

            let result = tokio::task::block_in_place(|| {
                tokio::runtime::Handle::current().block_on(async { store.delete(&id).await })
            });

            match result {
                Ok(deleted) => Ok(deleted),
                Err(e) => Err(mlua::Error::external(e)),
            }
        })?;
        game.set("delete_object", delete_object)?;

        // game.move_object(id, new_parent_id)
        // Actually moves object in database
        let store_clone = store.clone();
        let move_object =
            lua.create_function(move |_, (id, new_parent_id): (String, Option<String>)| {
                let store = store_clone.clone();

                let result = tokio::task::block_in_place(|| {
                    tokio::runtime::Handle::current()
                        .block_on(async { store.move_object(&id, new_parent_id.as_deref()).await })
                });

                match result {
                    Ok(()) => Ok(true),
                    Err(e) => Err(mlua::Error::external(e)),
                }
            })?;
        game.set("move_object", move_object)?;

        // game.clone_object(id, new_path, new_parent_id)
        // Actually clones object in database
        // Returns object on success, nil if not found, or {error = "message"} on path validation failure
        let store_clone = store.clone();
        let clone_object = lua.create_function(
            move |lua, (id, new_path, new_parent_id): (String, String, Option<String>)| {
                let store = store_clone.clone();

                // Validate new path first
                let validated_path = match crate::objects::validate_object_path(&new_path) {
                    Ok(path) => path,
                    Err(e) => {
                        let error_table = lua.create_table()?;
                        error_table.set("error", e.to_string())?;
                        return Ok(Value::Table(error_table));
                    }
                };

                let result: anyhow::Result<Option<Object>> = tokio::task::block_in_place(|| {
                    tokio::runtime::Handle::current().block_on(async {
                        let obj_result = store.get(&id).await?;
                        match obj_result {
                            Some(original) => {
                                let mut cloned = original.clone();
                                cloned.id = validated_path;
                                cloned.parent_id = new_parent_id;
                                cloned.created_at = chrono::Utc::now().to_rfc3339();
                                cloned.updated_at = cloned.created_at.clone();
                                store.create(&cloned).await?;
                                Ok(Some(cloned))
                            }
                            None => Ok(None),
                        }
                    })
                });

                match result {
                    Ok(Some(obj)) => Ok(Value::Table(object_to_lua(lua, &obj)?)),
                    Ok(None) => Ok(Value::Nil),
                    Err(e) => Err(mlua::Error::external(e)),
                }
            },
        )?;
        game.set("clone_object", clone_object)?;

        // game.store_code(source) - returns hash
        let store_clone = store.clone();
        let store_code = lua.create_function(move |_, source: String| {
            let store = store_clone.clone();

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
                let store = store_clone.clone();

                let result = tokio::task::block_in_place(|| {
                    tokio::runtime::Handle::current()
                        .block_on(async { store.get_contents(&parent_id).await })
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
        // Registers a new class with parent and properties
        let classes_clone = classes.clone();
        let define_class = lua.create_function(move |_, (name, definition): (String, Table)| {
            let classes = classes_clone.clone();

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

            tokio::task::block_in_place(|| {
                tokio::runtime::Handle::current().block_on(async {
                    let mut registry = classes.write().await;
                    registry.define_class(&name, parent.as_deref(), props_map);
                })
            });

            Ok(true)
        })?;
        game.set("define_class", define_class)?;

        // game.get_class(name)
        // Returns class definition as table
        let classes_clone = classes.clone();
        let get_class = lua.create_function(move |lua, name: String| {
            let classes = classes_clone.clone();

            let result = tokio::task::block_in_place(|| {
                tokio::runtime::Handle::current().block_on(async {
                    let registry = classes.read().await;
                    registry.get_class(&name).cloned()
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
        let is_a = lua.create_function(move |_, (obj_id, class_name): (String, String)| {
            let store = store_clone.clone();
            let classes = classes_clone.clone();

            let result: anyhow::Result<bool> = tokio::task::block_in_place(|| {
                tokio::runtime::Handle::current().block_on(async {
                    let obj = store.get(&obj_id).await?;
                    match obj {
                        Some(o) => {
                            let registry = classes.read().await;
                            Ok(registry.is_a(&o.class, &class_name))
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
        let get_class_chain = lua.create_function(move |lua, name: String| {
            let classes = classes.clone();

            let chain = tokio::task::block_in_place(|| {
                tokio::runtime::Handle::current().block_on(async {
                    let registry = classes.read().await;
                    registry.get_inheritance_chain(&name)
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

        // game.environment(obj_id)
        // Returns the parent object (container/room)
        let store_clone = store.clone();
        let environment = lua.create_function(move |lua, obj_id: String| {
            let store = store_clone.clone();

            let result = tokio::task::block_in_place(|| {
                tokio::runtime::Handle::current()
                    .block_on(async { store.get_environment(&obj_id).await })
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
        let all_inventory = lua.create_function(move |lua, obj_id: String| {
            let store = store_clone.clone();

            let result = tokio::task::block_in_place(|| {
                tokio::runtime::Handle::current()
                    .block_on(async { store.get_contents(&obj_id).await })
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
        let present = lua.create_function(move |lua, (name, env_id): (String, String)| {
            let store = store_clone.clone();

            let result = tokio::task::block_in_place(|| {
                tokio::runtime::Handle::current()
                    .block_on(async { store.find_by_name(&env_id, &name).await })
            });

            match result {
                Ok(Some(obj)) => Ok(Value::Table(object_to_lua(lua, &obj)?)),
                Ok(None) => Ok(Value::Nil),
                Err(e) => Err(mlua::Error::external(e)),
            }
        })?;
        game.set("present", present)?;

        // game.get_living_in(env_id)
        // Returns living entities (players, npcs) in a location
        let get_living_in = lua.create_function(move |lua, env_id: String| {
            let store = store.clone();

            let result = tokio::task::block_in_place(|| {
                tokio::runtime::Handle::current()
                    .block_on(async { store.get_living_in(&env_id).await })
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

        // game.add_action(verb, object_id, method)
        // Adds a contextual action for the current room
        let actions_clone = actions.clone();
        let room_id_clone = room_id.clone();
        let add_action = lua.create_function(
            move |_, (verb, object_id, method): (String, String, String)| {
                let actions = actions_clone.clone();
                let room_id = room_id_clone.clone();

                // Use blocking task since we're in sync Lua context
                // In production, this would be handled differently
                let action = Action {
                    verb: verb.clone(),
                    object_id,
                    method,
                };

                // Store action - for now we'll use the object_id as the scope
                // The caller should set up proper room context before execution
                std::thread::spawn(move || {
                    let rt = tokio::runtime::Runtime::new().unwrap();
                    rt.block_on(async {
                        if let Some(rid) = room_id {
                            actions.add_room_action(&rid, action).await;
                        } else {
                            // If no room context, add as object action
                            actions
                                .add_object_action(&action.object_id, action.clone())
                                .await;
                        }
                    });
                })
                .join()
                .ok();

                Ok(true)
            },
        )?;
        game.set("add_action", add_action)?;

        // game.remove_action(verb, object_id)
        // Removes a contextual action
        let actions_clone = actions.clone();
        let room_id_clone = room_id;
        let remove_action =
            lua.create_function(move |_, (verb, object_id): (String, String)| {
                let actions = actions_clone.clone();
                let room_id = room_id_clone.clone();

                std::thread::spawn(move || {
                    let rt = tokio::runtime::Runtime::new().unwrap();
                    rt.block_on(async {
                        if let Some(rid) = room_id {
                            actions.remove_room_action(&rid, &verb, &object_id).await;
                        } else {
                            actions.remove_object_action(&object_id, &verb).await;
                        }
                    });
                })
                .join()
                .ok();

                Ok(true)
            })?;
        game.set("remove_action", remove_action)?;

        Ok(())
    }

    fn register_message_functions(&self, lua: &Lua, game: &Table) -> LuaResult<()> {
        let messages = self.messages.clone();

        // game.send(target_id, message)
        // Send a private message to a player
        let messages_clone = messages.clone();
        let send = lua.create_function(move |_, (target_id, message): (String, String)| {
            let messages = messages_clone.clone();

            std::thread::spawn(move || {
                let rt = tokio::runtime::Runtime::new().unwrap();
                rt.block_on(async {
                    messages.send(&target_id, &message).await;
                });
            })
            .join()
            .ok();

            Ok(true)
        })?;
        game.set("send", send)?;

        // game.broadcast(room_id, message)
        // Broadcast a message to all players in a room
        let messages_clone = messages.clone();
        let broadcast = lua.create_function(move |_, (room_id, message): (String, String)| {
            let messages = messages_clone.clone();

            std::thread::spawn(move || {
                let rt = tokio::runtime::Runtime::new().unwrap();
                rt.block_on(async {
                    messages.broadcast(&room_id, &message).await;
                });
            })
            .join()
            .ok();

            Ok(true)
        })?;
        game.set("broadcast", broadcast)?;

        // game.broadcast_region(region_id, message)
        // Broadcast a message to all players in a region
        let broadcast_region =
            lua.create_function(move |_, (region_id, message): (String, String)| {
                let messages = messages.clone();

                std::thread::spawn(move || {
                    let rt = tokio::runtime::Runtime::new().unwrap();
                    rt.block_on(async {
                        messages.broadcast_region(&region_id, &message).await;
                    });
                })
                .join()
                .ok();

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
        // Sets the current actor for permission checks in this Lua context
        // Used by tests to simulate different users
        let set_actor = lua.create_function(|lua, actor_id: Option<String>| {
            let globals = lua.globals();
            match actor_id {
                Some(id) => globals.set("_current_actor_id", id)?,
                None => globals.set("_current_actor_id", Value::Nil)?,
            }
            Ok(true)
        })?;
        game.set("set_actor", set_actor)?;

        // game.get_actor()
        // Gets the current actor ID for permission checks
        let get_actor = lua.create_function(|lua, ()| {
            let globals = lua.globals();
            let actor: Option<String> = globals.get("_current_actor_id").ok();
            Ok(actor)
        })?;
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

                // Get user context - prefer _current_actor_id from Lua globals if set
                let globals = lua.globals();
                let actor_override: Option<String> = globals.get("_current_actor_id").ok();
                let user_id = actor_override
                    .or(current_user)
                    .unwrap_or_else(|| "anonymous".to_string());

                // Build contexts synchronously using thread spawn
                let result_data = std::thread::spawn(move || {
                    let rt = tokio::runtime::Runtime::new().unwrap();
                    rt.block_on(async {
                        let user_ctx = permissions.get_user_context(&user_id, &universe_id).await;
                        let obj_ctx = ObjectContext {
                            object_id: target_id,
                            owner_id,
                            is_fixed: is_fixed.unwrap_or(false),
                        };
                        permissions.check_permission(&user_ctx, action, &obj_ctx)
                    })
                })
                .join()
                .expect("Thread panicked");

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

            let globals = lua.globals();
            let actor_override: Option<String> = globals.get("_current_actor_id").ok();
            let user_id = actor_override
                .or(current_user)
                .unwrap_or_else(|| "anonymous".to_string());

            let result = std::thread::spawn(move || {
                let rt = tokio::runtime::Runtime::new().unwrap();
                rt.block_on(async {
                    permissions.can_access_path(&user_id, &universe_id, &path).await
                })
            })
            .join()
            .expect("Thread panicked");

            Ok(result)
        })?;
        game.set("can_access_path", can_access_path)?;

        // game.get_access_level(account_id)
        // Returns the access level of a user as a string
        let permissions_clone = permissions.clone();
        let get_access_level = lua.create_function(move |_, account_id: String| {
            let permissions = permissions_clone.clone();

            let level = std::thread::spawn(move || {
                let rt = tokio::runtime::Runtime::new().unwrap();
                rt.block_on(async { permissions.get_access_level(&account_id).await })
            })
            .join()
            .expect("Thread panicked");

            let level_str = match level {
                AccessLevel::Player => "player",
                AccessLevel::Builder => "builder",
                AccessLevel::Wizard => "wizard",
                AccessLevel::Admin => "admin",
                AccessLevel::Owner => "owner",
            };
            Ok(level_str.to_string())
        })?;
        game.set("get_access_level", get_access_level)?;

        // game.set_access_level(account_id, level_str)
        // Sets a user's access level (requires admin)
        let permissions_clone = permissions.clone();
        let set_access_level =
            lua.create_function(move |_, (account_id, level_str): (String, String)| {
                let permissions = permissions_clone.clone();

                let level = match level_str.as_str() {
                    "player" => AccessLevel::Player,
                    "builder" => AccessLevel::Builder,
                    "wizard" => AccessLevel::Wizard,
                    "admin" => AccessLevel::Admin,
                    "owner" => AccessLevel::Owner,
                    _ => return Ok(false),
                };

                std::thread::spawn(move || {
                    let rt = tokio::runtime::Runtime::new().unwrap();
                    rt.block_on(async {
                        permissions.set_access_level(&account_id, level).await;
                    });
                })
                .join()
                .ok();

                Ok(true)
            })?;
        game.set("set_access_level", set_access_level)?;

        // game.grant_path(grantee_id, path_prefix, can_delegate)
        // Grant path access to a user. Returns grant info table or {error: string}
        let permissions_clone = permissions.clone();
        let current_user_clone = current_user.clone();
        let universe_clone = universe_id.clone();
        let grant_path = lua.create_function(
            move |lua, (grantee_id, path_prefix, can_delegate): (String, String, Option<bool>)| {
                let permissions = permissions_clone.clone();
                let current_user = current_user_clone.clone();
                let universe_id = universe_clone.clone();

                let globals = lua.globals();
                let actor_override: Option<String> = globals.get("_current_actor_id").ok();
                let user_id = actor_override
                    .or(current_user)
                    .unwrap_or_else(|| "anonymous".to_string());

                let result: Result<crate::permissions::PathGrant, String> =
                    std::thread::spawn(move || {
                        let rt = tokio::runtime::Runtime::new().unwrap();
                        rt.block_on(async {
                            let grantor_ctx =
                                permissions.get_user_context(&user_id, &universe_id).await;
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
                        })
                    })
                    .join()
                    .expect("Thread panicked");

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
            let permissions = permissions_clone.clone();
            let current_user = current_user_clone.clone();
            let universe_id = universe_clone.clone();

            let globals = lua.globals();
            let actor_override: Option<String> = globals.get("_current_actor_id").ok();
            let user_id = actor_override
                .or(current_user)
                .unwrap_or_else(|| "anonymous".to_string());

            let result: Result<bool, String> = std::thread::spawn(move || {
                let rt = tokio::runtime::Runtime::new().unwrap();
                rt.block_on(async {
                    let revoker_ctx = permissions.get_user_context(&user_id, &universe_id).await;
                    permissions
                        .revoke_path(&revoker_ctx, &grant_id, &universe_id)
                        .await
                        .map_err(|e| e.to_string())
                })
            })
            .join()
            .expect("Thread panicked");

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
            let permissions = permissions_clone.clone();
            let universe_id = universe_clone.clone();

            let grants = std::thread::spawn(move || {
                let rt = tokio::runtime::Runtime::new().unwrap();
                rt.block_on(async { permissions.get_path_grants(&account_id, &universe_id).await })
            })
            .join()
            .expect("Thread panicked");

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
            move |_, (delay_secs, method, args): (f64, String, Option<String>)| {
                let timers = timers_clone.clone();
                let universe_id = universe_clone.clone();
                let object_id = object_clone.clone();

                let delay_ms = (delay_secs * 1000.0) as u64;

                let timer_id = std::thread::spawn(move || {
                    let rt = tokio::runtime::Runtime::new().unwrap();
                    rt.block_on(async {
                        if let Some(obj_id) = object_id {
                            let timer = Timer::new(&universe_id, &obj_id, &method, delay_ms, args);
                            timers.add_timer(timer).await
                        } else {
                            String::new()
                        }
                    })
                })
                .join()
                .expect("Thread panicked");

                Ok(timer_id)
            },
        )?;
        game.set("call_out", call_out)?;

        // game.remove_call_out(timer_id)
        // Cancel a scheduled timer
        let timers_clone = timers.clone();
        let remove_call_out = lua.create_function(move |_, timer_id: String| {
            let timers = timers_clone.clone();

            let removed = std::thread::spawn(move || {
                let rt = tokio::runtime::Runtime::new().unwrap();
                rt.block_on(async { timers.remove_timer(&timer_id).await })
            })
            .join()
            .expect("Thread panicked");

            Ok(removed)
        })?;
        game.set("remove_call_out", remove_call_out)?;

        // game.set_heart_beat(interval_ms)
        // Set a recurring heartbeat for the current object
        let timers_clone = timers.clone();
        let universe_clone = universe_id.clone();
        let object_clone = current_object.clone();
        let set_heart_beat = lua.create_function(move |_, interval_ms: u64| {
            let timers = timers_clone.clone();
            let universe_id = universe_clone.clone();
            let object_id = object_clone.clone();

            std::thread::spawn(move || {
                let rt = tokio::runtime::Runtime::new().unwrap();
                rt.block_on(async {
                    if let Some(obj_id) = object_id {
                        let hb = HeartBeat::new(&universe_id, &obj_id, interval_ms);
                        timers.set_heartbeat(hb).await;
                    }
                });
            })
            .join()
            .ok();

            Ok(true)
        })?;
        game.set("set_heart_beat", set_heart_beat)?;

        // game.remove_heart_beat()
        // Remove the heartbeat for the current object
        let object_clone = current_object;
        let remove_heart_beat = lua.create_function(move |_, ()| {
            let timers = timers.clone();
            let object_id = object_clone.clone();

            let removed = std::thread::spawn(move || {
                let rt = tokio::runtime::Runtime::new().unwrap();
                rt.block_on(async {
                    if let Some(obj_id) = object_id {
                        timers.remove_heartbeat(&obj_id).await
                    } else {
                        false
                    }
                })
            })
            .join()
            .expect("Thread panicked");

            Ok(removed)
        })?;
        game.set("remove_heart_beat", remove_heart_beat)?;

        Ok(())
    }

    fn register_credit_functions(&self, lua: &Lua, game: &Table) -> LuaResult<()> {
        let credits = self.credits.clone();
        let universe_id = self.universe_id.clone();
        let current_user = self.current_user_id.clone();
        let permissions = self.permissions.clone();

        // game.get_credits()
        // Get the current player's credit balance
        let credits_clone = credits.clone();
        let universe_clone = universe_id.clone();
        let user_clone = current_user.clone();
        let get_credits = lua.create_function(move |_, ()| {
            let credits = credits_clone.clone();
            let universe_id = universe_clone.clone();
            let user_id = user_clone.clone();

            let balance = std::thread::spawn(move || {
                let rt = tokio::runtime::Runtime::new().unwrap();
                rt.block_on(async {
                    if let Some(uid) = user_id {
                        credits.get_balance(&universe_id, &uid).await
                    } else {
                        0
                    }
                })
            })
            .join()
            .expect("Thread panicked");

            Ok(balance)
        })?;
        game.set("get_credits", get_credits)?;

        // game.deduct_credits(amount, reason)
        // Deduct credits from the current player
        // Returns true if successful, false if insufficient funds
        let credits_clone = credits.clone();
        let universe_clone = universe_id.clone();
        let user_clone = current_user.clone();
        let deduct_credits = lua.create_function(move |_, (amount, reason): (i64, String)| {
            let credits = credits_clone.clone();
            let universe_id = universe_clone.clone();
            let user_id = user_clone.clone();

            let result = std::thread::spawn(move || {
                let rt = tokio::runtime::Runtime::new().unwrap();
                rt.block_on(async {
                    if let Some(uid) = user_id {
                        credits.deduct(&universe_id, &uid, amount, &reason).await
                    } else {
                        false
                    }
                })
            })
            .join()
            .expect("Thread panicked");

            Ok(result)
        })?;
        game.set("deduct_credits", deduct_credits)?;

        // game.admin_grant_credits(account_id, amount)
        // Grant credits to a player (wizard+ only)
        let credits_clone = credits;
        let universe_clone = universe_id;
        let user_clone = current_user;
        let admin_grant_credits =
            lua.create_function(move |_, (account_id, amount): (String, i64)| {
                let credits = credits_clone.clone();
                let universe_id = universe_clone.clone();
                let user_id = user_clone.clone();
                let permissions = permissions.clone();

                let result = std::thread::spawn(move || {
                    let rt = tokio::runtime::Runtime::new().unwrap();
                    rt.block_on(async {
                        // Check if current user is wizard+
                        if let Some(ref uid) = user_id {
                            let level = permissions.get_access_level(uid).await;
                            if level < AccessLevel::Wizard {
                                return false;
                            }
                        } else {
                            return false;
                        }

                        credits
                            .grant(&universe_id, &account_id, amount, "admin_grant")
                            .await;
                        true
                    })
                })
                .join()
                .expect("Thread panicked");

                Ok(result)
            })?;
        game.set("admin_grant_credits", admin_grant_credits)?;

        Ok(())
    }

    fn register_venice_functions(&self, lua: &Lua, game: &Table) -> LuaResult<()> {
        let venice = self.venice.clone();
        let image_store = self.image_store.clone();
        let current_user = self.current_user_id.clone();

        // game.llm_chat(messages, tier)
        // Send a chat completion request to Venice AI
        // messages: array of {role, content} tables
        // tier: "fast", "balanced", or "quality"
        // Returns response text or nil on error
        let venice_clone = venice.clone();
        let user_clone = current_user.clone();
        let llm_chat = lua.create_function(
            move |lua, (messages_table, tier_str): (Table, Option<String>)| {
                let venice = venice_clone.clone();
                let user_id = user_clone.clone();

                // Parse messages from Lua table
                let mut messages = Vec::new();
                for pair in messages_table.sequence_values::<Table>() {
                    let msg_table = pair?;
                    let role: String = msg_table.get("role")?;
                    let content: String = msg_table.get("content")?;
                    messages.push(ChatMessage { role, content });
                }

                // Parse tier
                let tier = tier_str
                    .as_deref()
                    .and_then(ModelTier::parse)
                    .unwrap_or(ModelTier::Balanced);

                let result = std::thread::spawn(move || {
                    let rt = tokio::runtime::Runtime::new().unwrap();
                    rt.block_on(async {
                        let account_id = user_id.as_deref().unwrap_or("anonymous");
                        venice.chat(account_id, messages, tier).await
                    })
                })
                .join()
                .expect("Thread panicked");

                match result {
                    Ok(response) => Ok(Value::String(lua.create_string(&response)?)),
                    Err(e) => {
                        // Return nil and error message
                        let result = lua.create_table()?;
                        result.set("error", e)?;
                        Ok(Value::Table(result))
                    }
                }
            },
        )?;
        game.set("llm_chat", llm_chat)?;

        // game.llm_image(prompt, style, size)
        // Generate an image using Venice AI
        // prompt: text description
        // style: "realistic", "anime", "digital", "painterly"
        // size: "small", "medium", "large"
        // Returns image hash string (for use with /images/{hash}) or error table
        let user_clone = current_user;
        let llm_image = lua.create_function(
            move |lua, (prompt, style_str, size_str): (String, Option<String>, Option<String>)| {
                let venice = venice.clone();
                let image_store = image_store.clone();
                let user_id = user_clone.clone();

                // Parse style and size
                let style = style_str
                    .as_deref()
                    .and_then(ImageStyle::parse)
                    .unwrap_or(ImageStyle::Realistic);
                let size = size_str
                    .as_deref()
                    .and_then(ImageSize::parse)
                    .unwrap_or(ImageSize::Medium);

                let result = std::thread::spawn(move || {
                    let rt = tokio::runtime::Runtime::new().unwrap();
                    rt.block_on(async {
                        let account_id = user_id.as_deref().unwrap_or("anonymous");
                        // Generate image (returns raw binary bytes)
                        let image_bytes = venice
                            .generate_image(account_id, &prompt, style, size)
                            .await?;
                        // Store binary data and get hash
                        image_store
                            .store(&image_bytes, "image/png", "llm_image")
                            .await
                    })
                })
                .join()
                .expect("Thread panicked");

                match result {
                    Ok(hash) => Ok(Value::String(lua.create_string(&hash)?)),
                    Err(e) => {
                        // Return nil and error message
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
        let set_time = lua.create_function(move |lua, time_ms: u64| {
            let permissions = permissions_clone.clone();
            let user_id = user_clone.clone();
            let time_override = time_override_clone.clone();

            // Check for actor override from game.set_actor()
            let globals = lua.globals();
            let actor_override: Option<String> = globals.get("_current_actor_id").ok();
            let effective_user = actor_override.or(user_id);

            // Check wizard+ permission
            let allowed = std::thread::spawn(move || {
                let rt = tokio::runtime::Runtime::new().unwrap();
                rt.block_on(async {
                    if let Some(ref uid) = effective_user {
                        let level = permissions.get_access_level(uid).await;
                        level >= AccessLevel::Wizard
                    } else {
                        false
                    }
                })
            })
            .join()
            .expect("Thread panicked");

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
        let set_rng_seed = lua.create_function(move |lua, seed: u64| {
            let permissions = permissions_clone.clone();
            let user_id = user_clone.clone();
            let rng = rng_clone.clone();

            // Check for actor override from game.set_actor()
            let globals = lua.globals();
            let actor_override: Option<String> = globals.get("_current_actor_id").ok();
            let effective_user = actor_override.or(user_id);

            // Check wizard+ permission
            let allowed = std::thread::spawn(move || {
                let rt = tokio::runtime::Runtime::new().unwrap();
                rt.block_on(async {
                    if let Some(ref uid) = effective_user {
                        let level = permissions.get_access_level(uid).await;
                        level >= AccessLevel::Wizard
                    } else {
                        false
                    }
                })
            })
            .join()
            .expect("Thread panicked");

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
        // Invoke an object's handler method
        // Returns the result from the handler, or nil if not found
        let store_clone = store.clone();
        let use_object =
            lua.create_function(
                move |lua,
                      (obj_id, actor_id, verb, target_id): (
                    String,
                    String,
                    String,
                    Option<String>,
                )| {
                    let store = store_clone.clone();

                    // Get the object
                    let result: anyhow::Result<Option<(String, String)>> =
                        tokio::task::block_in_place(|| {
                            tokio::runtime::Handle::current().block_on(async {
                                let obj = store.get(&obj_id).await?;
                                match obj {
                                    Some(o) => {
                                        if let Some(hash) = o.code_hash {
                                            let code = store.get_code(&hash).await?;
                                            Ok(code.map(|c| (hash, c)))
                                        } else {
                                            Ok(None)
                                        }
                                    }
                                    None => Ok(None),
                                }
                            })
                        });

                    let (code_hash, code) = match result {
                        Ok(Some((h, c))) => (h, c),
                        Ok(None) => return Ok(Value::Nil),
                        Err(e) => return Err(mlua::Error::external(e)),
                    };

                    // Create a minimal sub-environment to execute the object code
                    // First, evaluate the code to get the object's function table
                    let chunk = lua.load(&code);
                    let handlers: LuaResult<Table> = chunk.eval();

                    match handlers {
                        Ok(handler_table) => {
                            // Map verb to handler name
                            let handler_name = match verb.as_str() {
                                "use" => "on_use",
                                "hit" => "on_hit",
                                "look" => "on_look",
                                "init" => "on_init",
                                _ => verb.as_str(), // Use verb directly if no mapping
                            };

                            // Try to get the handler function
                            if let Ok(handler) = handler_table.get::<Function>(handler_name) {
                                // Call the handler with context
                                let args_table = lua.create_table()?;
                                args_table.set("object_id", obj_id.as_str())?;
                                args_table.set("actor_id", actor_id.as_str())?;
                                args_table.set("verb", verb.as_str())?;
                                args_table.set("code_hash", code_hash.as_str())?;
                                if let Some(ref tid) = target_id {
                                    args_table.set("target_id", tid.as_str())?;
                                }

                                match handler.call::<Value>(args_table) {
                                    Ok(result) => Ok(result),
                                    Err(e) => Err(e),
                                }
                            } else {
                                // No handler for this verb
                                Ok(Value::Nil)
                            }
                        }
                        Err(_) => {
                            // Code didn't return a table, try calling it as a function
                            Ok(Value::Nil)
                        }
                    }
                },
            )?;
        game.set("use_object", use_object)?;

        // game.get_universe()
        // Returns universe info as table {id, name, owner_id, config, created_at}
        let store_clone = store.clone();
        let universe_clone = universe_id.clone();
        let get_universe = lua.create_function(move |lua, ()| {
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
        // Update universe config (wizard+ only)
        // Merges config into existing universe config
        let store_clone = store;
        let universe_clone = universe_id;
        let permissions_clone = permissions.clone();
        let user_clone = current_user.clone();
        let update_universe = lua.create_function(move |_, config: Table| {
            let store = store_clone.clone();
            let universe_id = universe_clone.clone();
            let permissions = permissions_clone.clone();
            let user_id = user_clone.clone();

            // Convert Lua table to JSON
            let config_json = lua_to_json(Value::Table(config))?;

            // Check wizard+ permission and update
            let result: Result<bool, anyhow::Error> = tokio::task::block_in_place(|| {
                tokio::runtime::Handle::current().block_on(async {
                    // Check permission
                    if let Some(ref uid) = user_id {
                        let level = permissions.get_access_level(uid).await;
                        if level < AccessLevel::Wizard {
                            return Ok(false);
                        }
                    } else {
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
fn json_to_lua(lua: &Lua, value: &serde_json::Value) -> LuaResult<Value> {
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

#[cfg(test)]
mod tests {
    use super::*;

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
