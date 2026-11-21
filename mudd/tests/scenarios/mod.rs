//! Scenario Tests for Mudcroft
//!
//! Modular play test scenarios covering:
//! - Movement: Room navigation via cardinal directions
//! - Inventory: Item pickup, drop, and listing
//! - Combat: NPC attacks and damage
//! - Chat: Say command and messaging
//! - Messaging: Lua send/broadcast delivery, single action reports
//! - Multiuser: Builder permissions, path grants, multi-user interactions
//! - Player lifecycle: Persistent objects, safe zones, disconnect, reconnect
//! - Security: Access-level enforcement in privileged Lua functions
//! - Scripting: Object code, hooks, custom verbs, timers
//! - Static files: Serving the web client with --static-dir
//! - Robustness: Input handling and state consistency

pub mod chat;
pub mod combat;
pub mod inventory;
pub mod messaging;
pub mod movement;
pub mod multiuser;
pub mod player_lifecycle;
pub mod robustness;
pub mod scripting;
pub mod security;
pub mod static_files;
