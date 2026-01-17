//! Game API exposed to Lua scripts

use std::sync::Arc;

use mlua::{Lua, Result as LuaResult, Table, Value};
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
    universe_id: String,
    current_room_id: Option<String>,
    current_user_id: Option<String>,
    current_object_id: Option<String>,
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
            universe_id: universe_id.to_string(),
            current_room_id: None,
            current_user_id: None,
            current_object_id: None,
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

        globals.set("game", game)?;
        Ok(())
    }

    fn register_object_functions(&self, lua: &Lua, game: &Table) -> LuaResult<()> {
        let universe_id = self.universe_id.clone();

        // game.create_object(class, parent_id, props)
        let create_object = lua.create_function(
            move |lua, (class, parent_id, props): (String, Option<String>, Option<Table>)| {
                let mut obj = Object::new(&universe_id, &class);
                obj.parent_id = parent_id;

                // Copy properties from Lua table if provided
                if let Some(props_table) = props {
                    for pair in props_table.pairs::<String, Value>() {
                        let (k, v) = pair?;
                        let json_val = lua_to_json(v)?;
                        obj.properties.insert(k, json_val);
                    }
                }

                // Return the object as a table
                object_to_lua(lua, &obj)
            },
        )?;
        game.set("create_object", create_object)?;

        // game.get_object(id)
        let get_object = lua.create_function(|_lua, _id: String| {
            // For now, return nil - actual DB access requires async
            // In production, this would be handled via a different mechanism
            Ok(Value::Nil)
        })?;
        game.set("get_object", get_object)?;

        // game.update_object(id, changes)
        let update_object = lua.create_function(|_, (_id, _changes): (String, Table)| {
            // Stub - requires async DB access
            Ok(true)
        })?;
        game.set("update_object", update_object)?;

        // game.delete_object(id)
        let delete_object = lua.create_function(|_, _id: String| {
            // Stub - requires async DB access
            Ok(true)
        })?;
        game.set("delete_object", delete_object)?;

        // game.move_object(id, new_parent_id)
        let move_object =
            lua.create_function(|_, (_id, _new_parent_id): (String, Option<String>)| {
                // Stub - requires async DB access + init cascade
                Ok(true)
            })?;
        game.set("move_object", move_object)?;

        // game.clone_object(id, new_parent_id)
        let clone_object =
            lua.create_function(|_lua, (_id, _new_parent_id): (String, Option<String>)| {
                // Stub - requires async DB access
                Ok(Value::Nil)
            })?;
        game.set("clone_object", clone_object)?;

        Ok(())
    }

    fn register_class_functions(&self, lua: &Lua, game: &Table) -> LuaResult<()> {
        // game.define_class(name, definition)
        let define_class = lua.create_function(|_, (_name, _definition): (String, Table)| {
            // Stub - would register class with registry
            Ok(true)
        })?;
        game.set("define_class", define_class)?;

        // game.get_class(name)
        let get_class = lua.create_function(|_lua, _name: String| {
            // Stub - would look up class from registry
            Ok(Value::Nil)
        })?;
        game.set("get_class", get_class)?;

        // game.is_a(obj_id, class_name)
        let is_a = lua.create_function(|_, (_obj_id, _class_name): (String, String)| {
            // Stub - requires object lookup + class check
            Ok(false)
        })?;
        game.set("is_a", is_a)?;

        // game.get_class_chain(class_name)
        let get_class_chain = lua.create_function(|lua, _name: String| {
            // Stub - would return inheritance chain
            let chain = lua.create_table()?;
            Ok(chain)
        })?;
        game.set("get_class_chain", get_class_chain)?;

        Ok(())
    }

    fn register_query_functions(&self, lua: &Lua, game: &Table) -> LuaResult<()> {
        // game.environment(obj_id)
        let environment = lua.create_function(|_, _obj_id: String| {
            // Stub - returns parent_id
            Ok(Value::Nil)
        })?;
        game.set("environment", environment)?;

        // game.all_inventory(obj_id)
        let all_inventory = lua.create_function(|lua, _obj_id: String| {
            // Stub - returns contents
            let contents = lua.create_table()?;
            Ok(contents)
        })?;
        game.set("all_inventory", all_inventory)?;

        // game.present(name, env_id)
        let present = lua.create_function(|_, (_name, _env_id): (String, String)| {
            // Stub - find by name in location
            Ok(Value::Nil)
        })?;
        game.set("present", present)?;

        // game.get_living_in(env_id)
        let get_living_in = lua.create_function(|lua, _env_id: String| {
            // Stub - returns living entities in location
            let living = lua.create_table()?;
            Ok(living)
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

        // game.check_permission(action, target_id, is_fixed, region_id)
        // Returns true if permission is allowed, false and error message otherwise
        let check_permission = lua.create_function(
            move |lua,
                  (action_str, target_id, is_fixed, region_id): (
                String,
                String,
                Option<bool>,
                Option<String>,
            )| {
                let permissions = permissions.clone();
                let current_user = current_user.clone();

                // Parse action string
                let action = match action_str.as_str() {
                    "read" => PermAction::Read,
                    "modify" => PermAction::Modify,
                    "move" => PermAction::Move,
                    "delete" => PermAction::Delete,
                    "create" => PermAction::Create,
                    "execute" => PermAction::Execute,
                    "admin_config" => PermAction::AdminConfig,
                    "grant_credits" => PermAction::GrantCredits,
                    _ => {
                        let result = lua.create_table()?;
                        result.set("allowed", false)?;
                        result.set("error", format!("Unknown action: {}", action_str))?;
                        return Ok(result);
                    }
                };

                // Get user context
                let user_id = current_user.unwrap_or_else(|| "anonymous".to_string());

                // Build contexts synchronously using thread spawn
                let result_data = std::thread::spawn(move || {
                    let rt = tokio::runtime::Runtime::new().unwrap();
                    rt.block_on(async {
                        let user_ctx = permissions.get_user_context(&user_id).await;
                        let obj_ctx = ObjectContext {
                            object_id: target_id,
                            owner_id: None,
                            is_fixed: is_fixed.unwrap_or(false),
                            region_id,
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

        // game.get_access_level(account_id)
        // Returns the access level of a user as a string
        let permissions_clone = self.permissions.clone();
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
        let permissions_clone = self.permissions.clone();
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

        // game.assign_region(account_id, region_id)
        // Assigns a region to a builder
        let permissions_clone = self.permissions.clone();
        let assign_region =
            lua.create_function(move |_, (account_id, region_id): (String, String)| {
                let permissions = permissions_clone.clone();

                std::thread::spawn(move || {
                    let rt = tokio::runtime::Runtime::new().unwrap();
                    rt.block_on(async {
                        permissions.assign_region(&account_id, &region_id).await;
                    });
                })
                .join()
                .ok();

                Ok(true)
            })?;
        game.set("assign_region", assign_region)?;

        // game.unassign_region(account_id, region_id)
        // Removes a region assignment from a builder
        let permissions_clone = self.permissions.clone();
        let unassign_region =
            lua.create_function(move |_, (account_id, region_id): (String, String)| {
                let permissions = permissions_clone.clone();

                std::thread::spawn(move || {
                    let rt = tokio::runtime::Runtime::new().unwrap();
                    rt.block_on(async {
                        permissions.unassign_region(&account_id, &region_id).await;
                    });
                })
                .join()
                .ok();

                Ok(true)
            })?;
        game.set("unassign_region", unassign_region)?;

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
                    .and_then(ModelTier::from_str)
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
        // Returns URL string or nil on error
        let user_clone = current_user;
        let llm_image = lua.create_function(
            move |lua, (prompt, style_str, size_str): (String, Option<String>, Option<String>)| {
                let venice = venice.clone();
                let user_id = user_clone.clone();

                // Parse style and size
                let style = style_str
                    .as_deref()
                    .and_then(ImageStyle::from_str)
                    .unwrap_or(ImageStyle::Realistic);
                let size = size_str
                    .as_deref()
                    .and_then(ImageSize::from_str)
                    .unwrap_or(ImageSize::Medium);

                let result = std::thread::spawn(move || {
                    let rt = tokio::runtime::Runtime::new().unwrap();
                    rt.block_on(async {
                        let account_id = user_id.as_deref().unwrap_or("anonymous");
                        venice
                            .generate_image(account_id, &prompt, style, size)
                            .await
                    })
                })
                .join()
                .expect("Thread panicked");

                match result {
                    Ok(url) => Ok(Value::String(lua.create_string(&url)?)),
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
fn object_to_lua(lua: &Lua, obj: &Object) -> LuaResult<Table> {
    let table = lua.create_table()?;
    table.set("id", obj.id.as_str())?;
    table.set("universe_id", obj.universe_id.as_str())?;
    table.set("class", obj.class.as_str())?;
    table.set("parent_id", obj.parent_id.clone())?;

    // Convert properties
    let props = lua.create_table()?;
    for (k, v) in &obj.properties {
        props.set(k.as_str(), json_to_lua(lua, v)?)?;
    }
    table.set("properties", props)?;

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
        let lua = Lua::new();

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
        let num_json = lua_to_json(Value::Number(3.14)).unwrap();
        assert!((num_json.as_f64().unwrap() - 3.14).abs() < 0.001);
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
