//! Attack resolution over object rows
//!
//! HP and combat stats have one home: the combatant's object row
//! (`hp`, `max_hp`, `armor_class`, `attack_bonus`, `resistances`), the same
//! properties Lua sees. Resolution is a pure function; its result becomes
//! one atomic batch of statements, so a round is all-or-nothing.

use std::str::FromStr;

use super::damage::{DamageModifier, DamageProfile};
use super::{AttackResult, DamageType};
use crate::objects::{Object, ObjectStore};

/// Largest HP (and max HP) a combatant can have
pub const MAX_HP: i32 = 1_000_000;
/// Bound on armor class and attack bonus, either sign
pub const MAX_STAT: i32 = 1_000;

/// A combatant's stats, read from its object row
#[derive(Debug, Clone)]
pub struct Combatant {
    pub id: String,
    pub hp: i32,
    pub max_hp: i32,
    pub armor_class: i32,
    pub attack_bonus: i32,
    pub profile: DamageProfile,
}

impl Combatant {
    /// Stats from the object's properties; `default_hp` when it has no `hp`.
    /// `resistances` maps a damage type to "immune", "resistant" or
    /// "vulnerable", e.g. `{fire = "resistant"}`.
    pub fn from_object(obj: &Object, default_hp: i32) -> Self {
        // Properties are builder-set and unbounded: clamp before narrowing
        let int = |key: &str, lo: i32, hi: i32| {
            obj.properties
                .get(key)
                .and_then(|v| v.as_i64())
                .map(|v| v.clamp(lo as i64, hi as i64) as i32)
        };
        let hp = int("hp", 0, MAX_HP).unwrap_or(default_hp.clamp(0, MAX_HP));
        let mut profile = DamageProfile::new();
        if let Some(map) = obj
            .properties
            .get("resistances")
            .and_then(|v| v.as_object())
        {
            for (dtype, kind) in map {
                let (Ok(dtype), Some(kind)) = (DamageType::from_str(dtype), kind.as_str()) else {
                    continue;
                };
                let modifier = match kind {
                    "immune" => DamageModifier::Immune,
                    "resistant" => DamageModifier::Resistant,
                    "vulnerable" => DamageModifier::Vulnerable,
                    _ => continue,
                };
                profile.set(dtype, modifier);
            }
        }
        Self {
            id: obj.id.clone(),
            hp,
            max_hp: int("max_hp", 1, MAX_HP).unwrap_or(hp.max(1)),
            armor_class: int("armor_class", -MAX_STAT, MAX_STAT).unwrap_or(10),
            attack_bonus: int("attack_bonus", -MAX_STAT, MAX_STAT).unwrap_or(0),
            profile,
        }
    }
}

/// What an attack did
#[derive(Debug, Clone)]
pub struct AttackOutcome {
    pub result: AttackResult,
    /// The target's HP afterwards (never below 0)
    pub target_hp: i32,
    pub killed: bool,
}

/// Resolve one attack: `d20` to hit against the target's armor class, and
/// on a hit `damage_roll` of `dtype` (doubled on a critical, then the
/// target's resistances)
pub fn resolve_attack(
    attacker: &Combatant,
    target: &Combatant,
    d20: u32,
    damage_roll: i32,
    dtype: DamageType,
) -> AttackOutcome {
    let mut result = AttackResult::new(d20, attacker.attack_bonus, target.armor_class);
    let mut target_hp = target.hp;
    if result.hit {
        let damage = target
            .profile
            .calculate_damage(damage_roll, dtype, result.critical);
        target_hp = target_hp.saturating_sub(damage.final_damage).max(0);
        result = result.with_damage(damage);
    }
    AttackOutcome {
        killed: result.hit && target_hp <= 0,
        result,
        target_hp,
    }
}

/// The statements that make an attack's outcome durable, to commit as one
/// batch: the target's new HP and, if it died, its removal from the room
/// and the end of its status effects
pub fn attack_statements(
    universe_id: &str,
    target_id: &str,
    outcome: &AttackOutcome,
) -> anyhow::Result<Vec<(String, Vec<serde_json::Value>)>> {
    let mut statements = Vec::new();
    if outcome.result.hit {
        statements.push(ObjectStore::set_properties_statement(
            universe_id,
            target_id,
            &[("hp", serde_json::json!(outcome.target_hp))],
        )?);
    }
    if outcome.killed {
        statements.push(remove_from_room_statement(universe_id, target_id));
        statements.push(super::EffectRegistry::clear_statement(
            universe_id,
            target_id,
        ));
    }
    Ok(statements)
}

/// Take a dead combatant out of its room (parent_id = NULL)
pub fn remove_from_room_statement(universe_id: &str, id: &str) -> (String, Vec<serde_json::Value>) {
    (
        "UPDATE objects SET parent_id = NULL, updated_at = ? WHERE universe_id = ? AND id = ?"
            .to_string(),
        vec![
            serde_json::json!(chrono::Utc::now().to_rfc3339()),
            serde_json::json!(universe_id),
            serde_json::json!(id),
        ],
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn combatant(props: serde_json::Value) -> Combatant {
        let mut obj = Object::new("/npcs/x", "u", "npc").unwrap();
        for (k, v) in props.as_object().unwrap() {
            obj.set_property(k, v.clone());
        }
        Combatant::from_object(&obj, 10)
    }

    #[test]
    fn test_stats_from_object() {
        let c = combatant(serde_json::json!({
            "hp": 7, "max_hp": 20, "armor_class": 14, "attack_bonus": 3,
            "resistances": {"fire": "resistant", "cold": "immune", "bogus": "x"}
        }));
        assert_eq!(
            (c.hp, c.max_hp, c.armor_class, c.attack_bonus),
            (7, 20, 14, 3)
        );
        assert_eq!(c.profile.get(DamageType::Fire), DamageModifier::Resistant);
        assert_eq!(c.profile.get(DamageType::Cold), DamageModifier::Immune);
        let d = combatant(serde_json::json!({}));
        assert_eq!((d.hp, d.armor_class), (10, 10), "defaults");
    }

    #[test]
    fn test_resolve_attack() {
        let attacker = combatant(serde_json::json!({"attack_bonus": 0}));
        let target = combatant(serde_json::json!({"hp": 5, "armor_class": 10}));

        let miss = resolve_attack(&attacker, &target, 5, 4, DamageType::Physical);
        assert!(!miss.result.hit);
        assert_eq!(miss.target_hp, 5);

        let hit = resolve_attack(&attacker, &target, 12, 3, DamageType::Physical);
        assert!(hit.result.hit && !hit.killed);
        assert_eq!(hit.target_hp, 2);

        // A natural 20 doubles the damage and kills; HP floors at 0
        let crit = resolve_attack(&attacker, &target, 20, 4, DamageType::Physical);
        assert!(crit.result.critical && crit.killed);
        assert_eq!(crit.target_hp, 0);

        // Resistance halves fire damage
        let resistant =
            combatant(serde_json::json!({"hp": 10, "resistances": {"fire": "resistant"}}));
        let fire = resolve_attack(&attacker, &resistant, 15, 4, DamageType::Fire);
        assert_eq!(fire.target_hp, 8);
    }

    /// Builder-set stats are clamped: huge values neither wrap when
    /// narrowed nor overflow in resolution
    #[test]
    fn test_stats_are_clamped() {
        let c = combatant(serde_json::json!({
            "hp": 9_000_000_000_000i64, "max_hp": -5,
            "armor_class": 99_999_999_999i64, "attack_bonus": -99_999_999_999i64
        }));
        assert_eq!(c.hp, MAX_HP);
        assert_eq!(c.max_hp, 1);
        assert_eq!(c.armor_class, MAX_STAT);
        assert_eq!(c.attack_bonus, -MAX_STAT);

        let attacker = combatant(serde_json::json!({"attack_bonus": i64::MAX}));
        let target = combatant(serde_json::json!({"hp": i64::MIN}));
        assert_eq!(target.hp, 0);
        let outcome = resolve_attack(&attacker, &target, 20, i32::MAX, DamageType::Physical);
        assert_eq!(outcome.target_hp, 0);
    }
}
