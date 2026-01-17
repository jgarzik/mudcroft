//! Combat system module
//!
//! Implements D&D-style combat with:
//! - Dice rolling (e.g., "2d6+3")
//! - Attack resolution with to-hit and damage
//! - Damage types (fire, ice, poison, etc.)
//! - Immunity, resistance, and vulnerability
//! - Status effects (poisoned, stunned, etc.)
//! - Combat state tracking

mod dice;
mod damage;
mod effects;
mod state;

pub use dice::{DiceRoll, roll_dice, parse_dice};
pub use damage::{DamageType, DamageModifier, DamageResult};
pub use effects::{StatusEffect, EffectType, EffectRegistry};
pub use state::{CombatState, CombatManager};
