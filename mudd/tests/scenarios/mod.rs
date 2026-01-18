//! Scenario Tests for HemiMUD
//!
//! Modular play test scenarios covering:
//! - Movement: Room navigation via cardinal directions
//! - Inventory: Item pickup, drop, and listing
//! - Combat: NPC attacks and damage
//! - Chat: Say command and messaging
//! - Multiuser: Builder permissions, path grants, multi-user interactions

pub mod chat;
pub mod combat;
pub mod inventory;
pub mod movement;
pub mod multiuser;
