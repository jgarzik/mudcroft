//! Attack resolution over object rows
//!
//! HP and combat stats have one home: the combatant's object row
//! (`hp`, `max_hp`, `armor_class`, `attack_bonus`, `resistances`), the same
//! properties Lua sees. Resolution is a pure function; its result becomes
//! one atomic batch of statements, so a round is all-or-nothing.

use std::str::FromStr;

use super::damage::{DamageModifier, DamageProfile};
use super::{AttackResult, DamageType, EffectRegistry, EffectType, EntityEffects, StatusEffect};
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
    /// An effect the hit inflicts on the target (bleeding on a critical)
    pub inflicts: Option<StatusEffect>,
}

/// Bleeding from a critical hit: damage per second, and how long it lasts
pub const BLEED_DAMAGE: i32 = 2;
pub const BLEED_MS: u64 = 3_000;

/// How status effects change an attack
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct AttackMods {
    /// Added to the d20 roll
    pub to_hit: i32,
    /// Added to the damage roll (before criticals and resistances)
    pub damage: i32,
}

/// The attack modifiers from both sides' effects in force at `now`:
/// blinded attacker -4 to hit, invisible target -4, helpless (stunned or
/// paralyzed) target +4; strengthened/weakened attacker and exposed/
/// protected target add or subtract their magnitude from the damage
pub fn attack_mods(attacker: &EntityEffects, target: &EntityEffects, now: u64) -> AttackMods {
    let magnitude = |effects: &EntityEffects, t| effects.get(t, now).map_or(0, |e| e.magnitude);
    let mut mods = AttackMods::default();
    if attacker.has(EffectType::Blinded, now) {
        mods.to_hit -= 4;
    }
    if target.has(EffectType::Invisible, now) {
        mods.to_hit -= 4;
    }
    if !target.can_act(now) {
        mods.to_hit += 4;
    }
    mods.damage = magnitude(attacker, EffectType::Strengthened)
        .saturating_sub(magnitude(attacker, EffectType::Weakened))
        .saturating_add(magnitude(target, EffectType::Exposed))
        .saturating_sub(magnitude(target, EffectType::Protected));
    mods
}

/// Resolve one attack: `d20` to hit against the target's armor class, and
/// on a hit `damage_roll` of `dtype` (doubled on a critical, then the
/// target's resistances)
pub fn resolve_attack(
    attacker: &Combatant,
    target: &Combatant,
    mods: &AttackMods,
    d20: u32,
    damage_roll: i32,
    dtype: DamageType,
    now: u64,
) -> AttackOutcome {
    let mut result = AttackResult::new(
        d20,
        attacker.attack_bonus.saturating_add(mods.to_hit),
        target.armor_class,
    );
    let mut target_hp = target.hp;
    if result.hit {
        let base = damage_roll.saturating_add(mods.damage).max(0);
        let damage = target
            .profile
            .calculate_damage(base, dtype, result.critical);
        target_hp = target_hp.saturating_sub(damage.final_damage).max(0);
        result = result.with_damage(damage);
    }
    let killed = result.hit && target_hp <= 0;
    let inflicts = (result.critical && !killed).then(|| {
        StatusEffect::periodic(
            EffectType::Bleeding,
            now,
            BLEED_MS,
            1_000,
            BLEED_DAMAGE,
            Some(DamageType::Physical),
        )
        .with_source(&attacker.id)
    });
    AttackOutcome {
        killed,
        result,
        target_hp,
        inflicts,
    }
}

/// The statements that make an attack's outcome durable: the target's new
/// HP and any effect it inflicted (a death is handled by api::combat::death)
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
    if let Some(ref effect) = outcome.inflicts {
        statements.push(EffectRegistry::upsert_statement(
            universe_id,
            target_id,
            effect,
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

/// Something processing an entity's due effects did
#[derive(Debug, Clone, PartialEq)]
pub enum TickEvent {
    /// Damage from an effect's pulses (after resistances)
    Damage {
        effect: EffectType,
        amount: i32,
        dtype: DamageType,
    },
    /// Healing from regeneration, capped at max HP
    Heal { amount: i32 },
    /// The effect ended
    Expired(EffectType),
}

/// The result of processing one entity's due effects
#[derive(Debug, Clone)]
pub struct TickOutcome {
    /// The effects left afterwards (pulse clocks advanced)
    pub effects: EntityEffects,
    pub hp: i32,
    pub hp_changed: bool,
    /// HP reached 0 now
    pub killed: bool,
    pub events: Vec<TickEvent>,
}

/// Process everything due on `target` at `now`: every pulse whose time has
/// come (several at once if the server fell behind, counted arithmetically,
/// not one by one), then any effect that has ended. Pure: the caller
/// commits the result.
pub fn process_due(target: &Combatant, effects: &EntityEffects, now: u64) -> TickOutcome {
    // First count every effect's due pulses (advancing its clock), then
    // apply all damage before any healing: the outcome must not depend on
    // the order effects happen to be stored in, and healing cannot revive
    // a target that the damage killed
    let mut advanced = Vec::new();
    let mut damage = Vec::new();
    let mut heals = Vec::new();
    for effect in effects.all() {
        let mut effect = effect.clone();
        if let (Some(first), Some(period)) = (effect.pending_pulse(), effect.period_ms) {
            let last = now.min(effect.expires_at);
            if first <= last {
                let pulses = (last - first) / period + 1;
                effect.next_pulse_at = Some(first + pulses * period);
                let pulses = i32::try_from(pulses).unwrap_or(i32::MAX);
                match effect.pulse_amount() {
                    Some((amount, dtype)) if amount > 0 => {
                        let each = target
                            .profile
                            .calculate_damage(amount, dtype, false)
                            .final_damage;
                        damage.push((effect.effect_type, each.saturating_mul(pulses), dtype));
                    }
                    Some((amount, _)) => heals.push(amount.saturating_neg().saturating_mul(pulses)),
                    None => {}
                }
            }
        }
        advanced.push(effect);
    }

    let mut hp = target.hp;
    let mut events = Vec::new();
    for (effect, dealt, dtype) in damage {
        if dealt > 0 {
            hp = hp.saturating_sub(dealt).max(0);
            events.push(TickEvent::Damage {
                effect,
                amount: dealt,
                dtype,
            });
        }
    }
    let killed = hp <= 0 && target.hp > 0;
    if !killed {
        for amount in heals {
            let healed = amount.min(target.max_hp.saturating_sub(hp)).max(0);
            if healed > 0 {
                hp += healed;
                events.push(TickEvent::Heal { amount: healed });
            }
        }
    }

    let mut kept = EntityEffects::new();
    for effect in advanced {
        if now >= effect.expires_at && effect.pending_pulse().is_none() {
            events.push(TickEvent::Expired(effect.effect_type));
        } else {
            kept.add(effect);
        }
    }
    TickOutcome {
        effects: kept,
        hp_changed: hp != target.hp,
        killed,
        hp,
        events,
    }
}

/// The statements that commit one entity's processed effects: pulse clocks
/// moved, ended effects deleted, the new HP, and on a death the end of all
/// its effects (and, unless it is a player, who respawns, its removal from
/// the room)
pub fn tick_statements(
    universe_id: &str,
    entity_id: &str,
    is_player: bool,
    before: &EntityEffects,
    outcome: &TickOutcome,
) -> anyhow::Result<Vec<(String, Vec<serde_json::Value>)>> {
    let _ = is_player;
    let mut statements = Vec::new();
    // A death (effects cleared, removal or respawn) is api::combat::death's
    if !outcome.killed {
        for effect in before.all() {
            let after = outcome
                .effects
                .all()
                .iter()
                .find(|e| e.effect_type == effect.effect_type);
            match after {
                Some(after) if after.next_pulse_at != effect.next_pulse_at => {
                    if let Some(next) = after.next_pulse_at {
                        statements.push(EffectRegistry::set_next_pulse_statement(
                            universe_id,
                            entity_id,
                            effect.effect_type,
                            next,
                        ));
                    }
                }
                Some(_) => {}
                None => statements.push(EffectRegistry::delete_statement(
                    universe_id,
                    entity_id,
                    effect.effect_type,
                )),
            }
        }
    }
    if outcome.hp_changed {
        statements.push(ObjectStore::set_properties_statement(
            universe_id,
            entity_id,
            &[("hp", serde_json::json!(outcome.hp))],
        )?);
    }
    Ok(statements)
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

        let miss = resolve_attack(
            &attacker,
            &target,
            &AttackMods::default(),
            5,
            4,
            DamageType::Physical,
            0,
        );
        assert!(!miss.result.hit);
        assert_eq!(miss.target_hp, 5);

        let hit = resolve_attack(
            &attacker,
            &target,
            &AttackMods::default(),
            12,
            3,
            DamageType::Physical,
            0,
        );
        assert!(hit.result.hit && !hit.killed);
        assert_eq!(hit.target_hp, 2);

        // A natural 20 doubles the damage and kills; HP floors at 0
        let crit = resolve_attack(
            &attacker,
            &target,
            &AttackMods::default(),
            20,
            4,
            DamageType::Physical,
            0,
        );
        assert!(crit.result.critical && crit.killed);
        assert_eq!(crit.target_hp, 0);

        // Resistance halves fire damage
        let resistant =
            combatant(serde_json::json!({"hp": 10, "resistances": {"fire": "resistant"}}));
        let fire = resolve_attack(
            &attacker,
            &resistant,
            &AttackMods::default(),
            15,
            4,
            DamageType::Fire,
            0,
        );
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
        let outcome = resolve_attack(
            &attacker,
            &target,
            &AttackMods::default(),
            20,
            i32::MAX,
            DamageType::Physical,
            0,
        );
        assert_eq!(outcome.target_hp, 0);
    }

    #[test]
    fn test_process_due() {
        use crate::combat::StatusEffect;
        let target = combatant(serde_json::json!({
            "hp": 20, "max_hp": 22, "resistances": {"fire": "resistant"}
        }));
        let mut effects = EntityEffects::new();
        effects.add(StatusEffect::periodic(
            EffectType::Poisoned,
            0,
            3_000,
            1_000,
            3,
            Some(DamageType::Poison),
        ));
        effects.add(StatusEffect::periodic(
            EffectType::Burning,
            0,
            1_000,
            1_000,
            4,
            Some(DamageType::Fire),
        ));
        effects.add(StatusEffect::new(EffectType::Stunned, 0, 5_000, 0));

        // Nothing is due before the first pulse
        let out = process_due(&target, &effects, 999);
        assert!(out.events.is_empty() && !out.hp_changed);

        // At 1 s: one poison pulse (3) and the burn's only pulse (4, halved
        // to 2), and the burn ends; the stun is untouched
        let out = process_due(&target, &effects, 1_000);
        assert_eq!(out.hp, 15);
        assert!(out
            .events
            .contains(&TickEvent::Expired(EffectType::Burning)));
        assert!(!out
            .events
            .contains(&TickEvent::Expired(EffectType::Stunned)));
        let statements = tick_statements("u", "/npcs/x", false, &effects, &out).unwrap();
        // poison clock moved, burn deleted, hp set: the stun costs nothing
        assert_eq!(statements.len(), 3, "{:?}", statements);

        // Far behind (10 s): poison catches up on all three pulses at once
        let out = process_due(&target, &effects, 10_000);
        assert!(out.events.contains(&TickEvent::Damage {
            effect: EffectType::Poisoned,
            amount: 9,
            dtype: DamageType::Poison
        }));
        assert!(out
            .events
            .contains(&TickEvent::Expired(EffectType::Poisoned)));
        assert!(out
            .events
            .contains(&TickEvent::Expired(EffectType::Stunned)));
        assert!(out.effects.is_empty());

        // Lethal damage kills once, ending the effects and leaving the room
        let weak = combatant(serde_json::json!({"hp": 2}));
        let mut lethal = EntityEffects::new();
        lethal.add(StatusEffect::periodic(
            EffectType::Bleeding,
            0,
            5_000,
            1_000,
            9,
            Some(DamageType::Physical),
        ));
        let out = process_due(&weak, &lethal, 1_000);
        assert!(out.killed && out.hp == 0);
        // A death writes only the HP here (api::combat::death does the rest)
        let statements = tick_statements("u", "/npcs/x", false, &lethal, &out).unwrap();
        assert_eq!(statements.len(), 1);
        assert!(statements[0].0.contains("json_set"));
    }

    #[test]
    fn test_attack_mods_from_effects() {
        use crate::combat::StatusEffect;
        let now = 10_000;
        let mut attacker = EntityEffects::new();
        attacker.add(StatusEffect::new(EffectType::Blinded, 0, 60_000, 0));
        attacker.add(StatusEffect::new(EffectType::Strengthened, 0, 60_000, 3));
        let mut target = EntityEffects::new();
        target.add(StatusEffect::new(EffectType::Stunned, 0, 60_000, 0));
        target.add(StatusEffect::new(EffectType::Protected, 0, 60_000, 1));
        // An effect that has ended does nothing
        target.add(StatusEffect::new(EffectType::Invisible, 0, 5_000, 0));
        let mods = attack_mods(&attacker, &target, now);
        assert_eq!(
            mods,
            AttackMods {
                to_hit: -4 + 4,
                damage: 3 - 1
            }
        );
    }

    #[test]
    fn test_resolve_with_mods_and_bleed() {
        let attacker = combatant(serde_json::json!({}));
        let target = combatant(serde_json::json!({"hp": 30, "armor_class": 12}));
        let now = 1_000;

        // +2 to hit turns an 11 into a hit; +3 damage on a roll of 2
        let mods = AttackMods {
            to_hit: 2,
            damage: 3,
        };
        let out = resolve_attack(&attacker, &target, &mods, 11, 2, DamageType::Physical, now);
        assert!(out.result.hit);
        assert_eq!(out.target_hp, 25);
        assert!(out.inflicts.is_none(), "no bleed without a critical");

        // Damage modifiers never make damage negative
        let weak = AttackMods {
            to_hit: 0,
            damage: -10,
        };
        let out = resolve_attack(&attacker, &target, &weak, 15, 2, DamageType::Physical, now);
        assert_eq!(out.target_hp, 30);

        // A critical that does not kill makes the target bleed, in the
        // same batch as the hit
        let out = resolve_attack(
            &attacker,
            &target,
            &AttackMods::default(),
            20,
            3,
            DamageType::Physical,
            now,
        );
        let bleed = out.inflicts.clone().expect("bleeding");
        assert_eq!(bleed.effect_type, EffectType::Bleeding);
        assert_eq!(
            bleed.pulse_amount(),
            Some((BLEED_DAMAGE, DamageType::Physical))
        );
        let statements = attack_statements("u", "/npcs/x", &out).unwrap();
        assert!(statements
            .iter()
            .any(|(sql, _)| sql.contains("INSERT INTO active_effects")));
    }

    /// Review fixes: no zero-damage events; regenerating always heals;
    /// death is decided when HP first reaches 0, whatever the effect order
    #[test]
    fn test_process_due_edge_cases() {
        use crate::combat::StatusEffect;
        let immune = combatant(serde_json::json!({"hp": 10, "resistances": {"poison": "immune"}}));
        let mut poison = EntityEffects::new();
        poison.add(StatusEffect::periodic(
            EffectType::Poisoned,
            0,
            5_000,
            1_000,
            3,
            Some(DamageType::Poison),
        ));
        let out = process_due(&immune, &poison, 1_000);
        assert!(out.events.is_empty(), "{:?}", out.events);

        let regen = StatusEffect::periodic(
            EffectType::Regenerating,
            0,
            5_000,
            1_000,
            5,
            Some(DamageType::Radiant),
        );
        assert_eq!(regen.pulse_amount(), Some((-5, DamageType::Physical)));

        let weak = combatant(serde_json::json!({"hp": 5, "max_hp": 30}));
        for heal_first in [false, true] {
            let mut e = EntityEffects::new();
            let bleed = StatusEffect::periodic(
                EffectType::Bleeding,
                0,
                5_000,
                1_000,
                9,
                Some(DamageType::Physical),
            );
            let heal = StatusEffect::periodic(EffectType::Regenerating, 0, 5_000, 1_000, 20, None);
            if heal_first {
                e.add(heal.clone());
                e.add(bleed.clone());
            } else {
                e.add(bleed);
                e.add(heal);
            }
            let out = process_due(&weak, &e, 1_000);
            assert!(out.killed, "order {} must not decide death", heal_first);
            assert_eq!(out.hp, 0);
        }
    }
}
