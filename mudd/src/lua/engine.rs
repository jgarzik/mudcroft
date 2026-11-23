//! The combat engine as seen from Lua
//!
//! Combat lives in `api::combat` (it needs the whole server state); the Lua
//! layer reaches it through this trait, set on each sandbox by the script
//! runner. The mudlib's `Combat` library wraps these calls: the engine is
//! the single source of combat rules.

use std::future::Future;
use std::pin::Pin;

pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// What a blow did (game.attack)
#[derive(Debug, Clone, Default)]
pub struct BlowReport {
    pub hit: bool,
    pub critical: bool,
    pub fumble: bool,
    pub damage: i64,
    pub killed: bool,
    /// The attacker's view of it
    pub text: String,
    /// Damage per part (type, rolled, applied) for weapons with several
    pub parts: Vec<(String, i64, i64)>,
}

/// A fighter's state (game.combat_state)
#[derive(Debug, Clone, Default)]
pub struct CombatState {
    pub in_combat: bool,
    pub attacking: Option<String>,
    pub attackers: Vec<String>,
    pub next_round_at: Option<u64>,
}

/// Damage dealt (game.damage)
#[derive(Debug, Clone, Default)]
pub struct DamageReport {
    pub damage: i64,
    pub hp: i64,
    pub killed: bool,
}

/// Engine operations, all in the current run. Errors are refusals or
/// failures, reported to Lua as `nil, reason`.
pub trait Engine: Send + Sync {
    /// Whether `attacker` may fight `target` (combat setting, PvP, presence)
    fn can_attack<'a>(
        &'a self,
        universe_id: &'a str,
        attacker: &'a str,
        target: &'a str,
    ) -> BoxFuture<'a, Result<(), String>>;

    /// One blow now, and the fight starts (both sides) if it is not on
    fn attack<'a>(
        &'a self,
        universe_id: &'a str,
        attacker: &'a str,
        target: &'a str,
    ) -> BoxFuture<'a, Result<BlowReport, String>>;

    fn combat_state<'a>(
        &'a self,
        universe_id: &'a str,
        id: &'a str,
    ) -> BoxFuture<'a, Result<CombatState, String>>;

    /// Damage of a type (resistances apply); a death is handled
    fn damage<'a>(
        &'a self,
        universe_id: &'a str,
        target: &'a str,
        amount: i64,
        damage_type: &'a str,
        source: Option<&'a str>,
    ) -> BoxFuture<'a, Result<DamageReport, String>>;

    /// Heal up to max HP; returns the new HP
    fn heal<'a>(
        &'a self,
        universe_id: &'a str,
        target: &'a str,
        amount: i64,
    ) -> BoxFuture<'a, Result<i64, String>>;

    /// End every fight `id` is in (as fighter or target); returns how many
    /// fighters stopped
    fn stop<'a>(&'a self, universe_id: &'a str, id: &'a str) -> BoxFuture<'a, Result<u64, String>>;

    /// Run an engine hook (on_create, on_destroy) of `object`'s own code, as
    /// its owner, within the action's hook budget; failures are logged
    fn hook<'a>(
        &'a self,
        universe_id: &'a str,
        object: &'a str,
        handler: &'a str,
        args: serde_json::Value,
    ) -> BoxFuture<'a, ()>;

    /// Move an object as the engine does: a player's session follows (and
    /// is shown the room)
    fn relocate<'a>(
        &'a self,
        universe_id: &'a str,
        id: &'a str,
        to: Option<&'a str>,
    ) -> BoxFuture<'a, Result<(), String>>;

    /// Leave through a random exit, ending fights; returns the direction
    fn flee<'a>(
        &'a self,
        universe_id: &'a str,
        id: &'a str,
    ) -> BoxFuture<'a, Result<String, String>>;
}
