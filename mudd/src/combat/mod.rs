//! Combat system module
//!
//! Implements D&D-style combat with:
//! - Dice rolling (e.g., "2d6+3")
//! - Attack resolution with to-hit and damage
//! - Damage types (fire, ice, poison, etc.)
//! - Immunity, resistance, and vulnerability
//! - Status effects (poisoned, stunned, etc.)
//! - HP and combat stats on the object rows (engine)

mod damage;
mod dice;
mod effects;
pub mod engine;
mod state;

pub use damage::{DamageModifier, DamageResult, DamageType};
pub use dice::{parse_dice, roll_d20, roll_dice, DiceRoll};
pub use effects::{EffectRegistry, EffectType, EntityEffects, StatusEffect};
pub use state::{AttackResult, PvpPolicy};
