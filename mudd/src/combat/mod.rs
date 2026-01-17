//! Combat system module
//!
//! Implements D&D-style combat with:
//! - Dice rolling (e.g., "2d6+3")
//! - Attack resolution with to-hit and damage
//! - Damage types (fire, ice, poison, etc.)
//! - Immunity, resistance, and vulnerability
//! - Status effects (poisoned, stunned, etc.)
//! - Combat state tracking

mod damage;
mod dice;
mod effects;
mod state;

pub use damage::{DamageModifier, DamageResult, DamageType};
pub use dice::{parse_dice, roll_dice, DiceRoll};
pub use effects::{EffectRegistry, EffectType, StatusEffect};
pub use state::{CombatManager, CombatState};
