//! Combat rounds, deaths and world deadlines (event-driven)
//!
//! A fight is rows in the `combat` table: each fighter's current target and
//! the absolute time of its next round (`combat_round_ms` apart, a universe
//! setting). Attacking starts a fight (the first blow lands at once) and
//! the target fights back. The scheduler sleeps until the earliest round or
//! deadline is due, then runs everything due as one run (one batch):
//! blows, HP, deaths, the next rounds. A fight ends when either side dies
//! or they are no longer in the same room; a fighter whose target is gone
//! turns on someone still fighting it.
//!
//! A death follows the universe's policy (`player_death`: corpse, keep or
//! drop; `npc_death`: corpse, drop or destroy). Players respawn at once; an
//! NPC respawns at its home room after its `respawn_seconds` (default 300,
//! 0 = never) and its killer gains its `xp_value` (default its max HP).
//! Corpses decay after `corpse_decay_s` (default 300), spilling what is
//! left into the room. Respawns and decay are rows in `deadlines`.

use std::time::Duration;

use rand::RngExt;
use serde_json::json;
use tracing::warn;

use super::{AppState, ServerMessage};
use crate::combat::engine::{attack_mods, attack_statements, resolve_attack, Combatant};
use crate::combat::{now_ms, DamageType};
use crate::objects::Object;

/// Round length when the universe sets none
pub const DEFAULT_ROUND_MS: u64 = 3000;
const MIN_ROUND_MS: u64 = 1000;
const MAX_ROUND_MS: u64 = 10_000;
const DEFAULT_CORPSE_DECAY_S: u64 = 300;
const DEFAULT_RESPAWN_S: u64 = 300;
/// When an NPC gone for good is deleted if its death report never ran
const DELETE_FALLBACK_MS: u64 = 60_000;
/// Longest respawn or corpse decay (builder-set values are clamped)
const MAX_DELAY_S: u64 = 30 * 24 * 60 * 60;
/// Longest pause between wanders
const MAX_WANDER_S: f64 = 24.0 * 60.0 * 60.0;

/// Longest sleep with nothing due (changes wake the scheduler sooner)
const IDLE_WAIT: Duration = Duration::from_secs(60);

/// A universe's combat settings (its config)
pub struct Settings {
    pub round_ms: u64,
    /// "on" (default), "npc_only" (no player against player), "off" (no
    /// fighting at all: social universes)
    pub combat: String,
    /// Chance (0-100) that a player's flee fails (default 0)
    pub flee_fail_percent: u32,
    pub pvp: crate::combat::PvpPolicy,
    pub player_death: String,
    pub npc_death: String,
    pub corpse_decay_ms: u64,
}

pub async fn settings(state: &AppState, universe_id: &str) -> Settings {
    let config = state
        .object_store
        .get_universe(universe_id)
        .await
        .ok()
        .flatten()
        .map(|u| u.config)
        .unwrap_or_default();
    let choice = |key: &str, allowed: &[&str]| {
        config
            .get(key)
            .and_then(|v| v.as_str())
            .filter(|v| allowed.contains(v))
            .unwrap_or(allowed[0])
            .to_string()
    };
    Settings {
        combat: choice("combat", &["on", "npc_only", "off"]),
        flee_fail_percent: config
            .get("flee_fail_percent")
            .and_then(|v| v.as_u64())
            .unwrap_or(0)
            .min(100) as u32,
        pvp: config
            .get("pvp_policy")
            .and_then(|v| v.as_str())
            .and_then(crate::combat::PvpPolicy::from_str)
            .unwrap_or_default(),
        round_ms: config
            .get("combat_round_ms")
            .and_then(|v| v.as_u64())
            .unwrap_or(DEFAULT_ROUND_MS)
            .clamp(MIN_ROUND_MS, MAX_ROUND_MS),
        player_death: choice("player_death", &["corpse", "keep", "drop"]),
        npc_death: choice("npc_death", &["corpse", "drop", "destroy"]),
        corpse_decay_ms: config
            .get("corpse_decay_s")
            .and_then(|v| v.as_u64())
            .unwrap_or(DEFAULT_CORPSE_DECAY_S)
            .min(MAX_DELAY_S)
            * 1000,
    }
}

fn name_of(obj: &Object) -> String {
    obj.properties
        .get("name")
        .and_then(|v| v.as_str())
        .unwrap_or("something")
        .to_string()
}

fn is_player(obj: &Object) -> bool {
    obj.class == "player"
}

fn alive(obj: &Object) -> bool {
    obj.properties
        .get("hp")
        .and_then(|v| v.as_i64())
        .is_none_or(|hp| hp > 0)
}

/// Whether `attacker` may fight `target` here: anything but player against
/// player is always allowed; that follows the universe's `pvp_policy`
/// (arena, the default: only in a room with `is_arena`; flagged: both
/// players have `pvp` on; open; disabled)
pub async fn pvp_allowed(
    state: &AppState,
    universe_id: &str,
    attacker: &Object,
    target: &Object,
) -> bool {
    use crate::combat::PvpPolicy;
    if !(is_player(attacker) && is_player(target)) {
        return true;
    }
    let flagged = |o: &Object| o.get_bool("pvp").unwrap_or(false);
    let settings = settings(state, universe_id).await;
    if settings.combat != "on" {
        return false;
    }
    match settings.pvp {
        PvpPolicy::Disabled => false,
        PvpPolicy::Open => true,
        PvpPolicy::Flagged => flagged(attacker) && flagged(target),
        PvpPolicy::ArenaOnly => match attacker.parent_id.as_deref() {
            Some(room) => state
                .object_store
                .get(universe_id, room)
                .await
                .ok()
                .flatten()
                .is_some_and(|r| r.get_bool("is_arena").unwrap_or(false)),
            None => false,
        },
    }
}

/// A weapon's damage dice if within bounds (at most 100 dice of 1 to 1000
/// sides, modifier within +-1000); else None (the bare 1d6)
pub fn weapon_dice(notation: &str) -> Option<crate::combat::DiceRoll> {
    crate::combat::parse_dice(notation).ok().filter(|roll| {
        roll.count <= 100
            && (1..=1000).contains(&roll.sides)
            && roll.modifier.unsigned_abs() <= 1000
    })
}

/// A creature: anything living that is not a player (an npc, a class
/// derived from npc or living), or a monster
pub async fn is_creature(state: &AppState, universe_id: &str, obj: &Object) -> bool {
    obj.class == "monster"
        || (!is_player(obj)
            && state
                .classes
                .read()
                .await
                .is_a(universe_id, &obj.class, "living"))
}

/// A dead NPC waiting to respawn (or to be deleted): not in play
async fn awaiting_respawn(state: &AppState, universe_id: &str, id: &str) -> Result<bool, String> {
    let row: Option<i64> = sqlx::query_scalar(
        "SELECT 1 FROM deadlines WHERE universe_id = ? AND object_id = ? AND kind IN ('respawn', 'delete')",
    )
    .bind(universe_id)
    .bind(id)
    .fetch_optional(
        &mut *crate::db::run::reader(state.db.pool())
            .await
            .map_err(|e| e.to_string())?,
    )
    .await
    .map_err(|e| e.to_string())?;
    Ok(row.is_some())
}

/// Whether `attacker` may fight `target` now: the universe allows fighting
/// (`combat` not "off"), the target is a creature (an npc, a subclass, a
/// monster) or a player the PvP rules allow, both are alive and together.
/// Err is the reason, for the player.
pub async fn fight_allowed(
    state: &AppState,
    universe_id: &str,
    attacker: &Object,
    target: &Object,
) -> Result<(), String> {
    let tn = name_of(target);
    if settings(state, universe_id).await.combat == "off" {
        return Err("There is no fighting in this world.".to_string());
    }
    if attacker.id == target.id {
        return Err("You can't attack yourself.".to_string());
    }
    if attacker.parent_id.is_none() || attacker.parent_id != target.parent_id {
        return Err(format!("You don't see {} here.", tn));
    }
    if !alive(target) {
        return Err(format!("{} is already dead.", tn));
    }
    // Only fighters fight: players and creatures, alive
    if !alive(attacker) || !(is_player(attacker) || is_creature(state, universe_id, attacker).await)
    {
        return Err(format!("{} can't fight.", name_of(attacker)));
    }
    let creature = is_creature(state, universe_id, target).await;
    let allowed =
        creature || (is_player(target) && pvp_allowed(state, universe_id, attacker, target).await);
    if !allowed {
        return Err(format!("You can't attack {}.", tn));
    }
    Ok(())
}

/// Whether `attacker` may open a fight with a blow now: a full round since
/// its last blow (stopping and restarting a fight cannot add blows)
pub async fn ready_to_strike(
    state: &AppState,
    universe_id: &str,
    attacker: &Object,
    now: u64,
) -> bool {
    let last = attacker
        .properties
        .get("last_blow_at")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    now >= last.saturating_add(settings(state, universe_id).await.round_ms)
}

/// What one blow did, and what each party is told
pub struct Blow {
    pub killed: bool,
    pub critical: bool,
    pub fumble: bool,
    /// Total damage dealt on a hit (all parts, after resistances)
    pub damage: Option<i32>,
    /// Each damage part: type, as rolled, as applied
    pub parts: Vec<crate::combat::engine::DamagePart>,
    /// For the attacker (a reply when the attacker typed the command)
    pub attacker_text: String,
    pub target_text: String,
    pub room_text: String,
}

/// One blow from `attacker` at `target`, written in the current run: new HP
/// and, on a critical hit, bleeding. A death is not handled here (see
/// [`death`]). The caller holds the effects write guard.
pub async fn strike(
    state: &AppState,
    universe_id: &str,
    attacker: &Object,
    target: &Object,
    now: u64,
) -> anyhow::Result<Blow> {
    let mut a = Combatant::from_object(attacker, if is_player(attacker) { 100 } else { 10 });
    let mut d = Combatant::from_object(target, if is_player(target) { 100 } else { 10 });
    // Gear: a wielded weapon's dice, type and bonus; worn armor class
    let gear = super::equipment::equipped(state, universe_id, &attacker.id).await;
    let weapon = super::equipment::weapon(&gear);
    let dice = weapon
        .and_then(|w| w.get_string("damage"))
        .and_then(weapon_dice);
    let dtype = weapon
        .and_then(|w| w.get_string("damage_type"))
        .and_then(|t| t.parse::<DamageType>().ok())
        .unwrap_or(DamageType::Physical);
    if let Some(bonus) = weapon
        .and_then(|w| w.properties.get("attack_bonus"))
        .and_then(|v| v.as_i64())
    {
        a.attack_bonus = a
            .attack_bonus
            .saturating_add(bonus.clamp(-1000, 1000) as i32);
    }
    let worn = super::equipment::equipped(state, universe_id, &target.id).await;
    d.armor_class = d
        .armor_class
        .saturating_add(super::equipment::armor_bonus(&worn));
    let attacker_effects = state.effects.get(universe_id, &attacker.id).await;
    let target_effects = state.effects.get(universe_id, &target.id).await;
    let mods = attack_mods(&attacker_effects, &target_effects, now);
    let d20 = crate::combat::roll_d20();
    // The weapon's damage parts (damage_parts: at most 8 {dice, type}),
    // else its one damage/damage_type, else a bare 1d6
    let mut specs: Vec<(Option<crate::combat::DiceRoll>, DamageType)> = weapon
        .and_then(|w| w.properties.get("damage_parts"))
        .and_then(|v| v.as_array())
        .map(|list| {
            list.iter()
                .take(8)
                .filter_map(|p| {
                    let dice = p
                        .get("dice")
                        .and_then(|d| d.as_str())
                        .and_then(weapon_dice)?;
                    // No type: physical; a type that is not one is dropped
                    // (as bad dice are), never silently physical
                    let t = match p.get("type").and_then(|t| t.as_str()) {
                        None => DamageType::Physical,
                        Some(name) => name.parse::<DamageType>().ok()?,
                    };
                    Some((Some(dice), t))
                })
                .collect()
        })
        .unwrap_or_default();
    if specs.is_empty() {
        specs.push((dice, dtype));
    }
    let roll = |spec: &Option<crate::combat::DiceRoll>| match spec {
        Some(r) => r.roll().max(0),
        None => rand::rng().random_range(1..=6),
    };
    let first = specs[0].1;
    let damage_roll = roll(&specs[0].0);
    let extra: Vec<(i32, DamageType)> = specs[1..].iter().map(|(d, t)| (roll(d), *t)).collect();
    let mut outcome = resolve_attack(&a, &d, &mods, d20, damage_roll, first, now);
    let parts = crate::combat::engine::add_damage_parts(&mut outcome, &d, first, &extra);
    let mut statements = attack_statements(universe_id, &target.id, &outcome)?;
    // When it last struck: a fight's first blow waits a full round after
    // the last one, whatever happened to the fight meanwhile
    statements.push(crate::objects::ObjectStore::set_properties_statement(
        universe_id,
        &attacker.id,
        &[("last_blow_at", json!(now))],
    )?);
    state.object_store.execute_batch(statements).await?;
    if let Some(ref bleed) = outcome.inflicts {
        state
            .effects
            .cache_add(universe_id, &target.id, bleed.clone())
            .await;
    }

    let (an, tn) = (name_of(attacker), name_of(target));
    let result = &outcome.result;
    let damage = result
        .damage
        .as_ref()
        .map(|_| parts.iter().fold(0i32, |t, p| t.saturating_add(p.applied)));
    // Several parts: each one's share, and the target's resistances
    let breakdown = if parts.len() > 1 {
        let shares: Vec<String> = parts
            .iter()
            .map(|p| {
                use crate::combat::DamageModifier;
                match p.modifier {
                    DamageModifier::Immune => format!("{}: {} is immune", p.dtype, tn),
                    DamageModifier::Resistant => format!("{} {}, resisted", p.applied, p.dtype),
                    DamageModifier::Vulnerable => format!("{} {}, vulnerable", p.applied, p.dtype),
                    _ => format!("{} {}", p.applied, p.dtype),
                }
            })
            .collect();
        format!(" ({})", shares.join("; "))
    } else {
        String::new()
    };
    let mut mine = Vec::new();
    let target_text;
    if result.hit {
        let dmg = damage.unwrap_or(0);
        if result.critical {
            mine.push(format!(
                "CRITICAL HIT! You strike {} for {} damage{}!",
                tn, dmg, breakdown
            ));
            if outcome.inflicts.is_some() {
                mine.push(format!("{} is bleeding!", tn));
            }
            target_text = format!("{} critically hits you for {} damage!", an, dmg);
        } else {
            mine.push(format!("You hit {} for {} damage{}.", tn, dmg, breakdown));
            target_text = format!("{} hits you for {} damage.", an, dmg);
        }
        if outcome.killed {
            mine.push(format!("{} is slain!", tn));
        } else {
            mine.push(format!("{} has {} HP remaining.", tn, outcome.target_hp));
        }
    } else if result.fumble {
        mine.push(format!("You fumble your attack against {}!", tn));
        target_text = format!("{} fumbles an attack against you.", an);
    } else {
        mine.push(format!(
            "You miss {} (rolled {} vs AC {}).",
            tn, result.attack_total, result.target_ac
        ));
        target_text = format!("{} misses you.", an);
    }
    let mut room_text = format!("{} attacks {}.", an, tn);
    if outcome.killed {
        room_text.push_str(&format!(" {} is slain!", tn));
    }
    Ok(Blow {
        killed: outcome.killed,
        critical: result.critical,
        fumble: result.fumble,
        damage: if result.hit { damage } else { None },
        parts,
        attacker_text: mine.join("\n"),
        target_text,
        room_text,
    })
}

/// Start (or switch) a fight: `attacker` targets `target`, and `target`
/// fights back unless it is already fighting someone. Both next rounds are
/// a round from now (the attacker's first blow is struck by the caller).
pub async fn engage(
    state: &AppState,
    universe_id: &str,
    attacker: &str,
    target: &str,
    now: u64,
) -> anyhow::Result<()> {
    let next = now + settings(state, universe_id).await.round_ms;
    engage_at(state, universe_id, attacker, target, next, next).await?;
    assist(state, universe_id, target, attacker, now).await
}

/// [`engage`] with explicit first rounds for each side (no assist)
async fn engage_at(
    state: &AppState,
    universe_id: &str,
    attacker: &str,
    target: &str,
    attacker_at: u64,
    target_at: u64,
) -> anyhow::Result<()> {
    state
        .object_store
        .execute_batch(vec![
            (
                "INSERT INTO combat (universe_id, entity_id, target_id, next_round_at)
                 VALUES (?, ?, ?, ?)
                 ON CONFLICT (universe_id, entity_id) DO UPDATE SET target_id = excluded.target_id"
                    .to_string(),
                vec![
                    json!(universe_id),
                    json!(attacker),
                    json!(target),
                    json!(attacker_at),
                ],
            ),
            (
                "INSERT OR IGNORE INTO combat (universe_id, entity_id, target_id, next_round_at)
                 VALUES (?, ?, ?, ?)"
                    .to_string(),
                vec![
                    json!(universe_id),
                    json!(target),
                    json!(attacker),
                    json!(target_at),
                ],
            ),
        ])
        .await?;
    crate::db::run::after_commit({
        let changed = state.combat_changed.clone();
        move || async move { changed.notify_one() }
    })
    .await;
    Ok(())
}

/// Whom `id` is fighting, if anyone
pub async fn current_target(
    state: &AppState,
    universe_id: &str,
    id: &str,
) -> anyhow::Result<Option<String>> {
    Ok(
        sqlx::query_scalar("SELECT target_id FROM combat WHERE universe_id = ? AND entity_id = ?")
            .bind(universe_id)
            .bind(id)
            .fetch_optional(&mut *crate::db::run::reader(state.db.pool()).await?)
            .await?,
    )
}

/// Switch a fighter's target (its round keeps its time); the new target
/// fights back and its allies assist
pub async fn retarget(
    state: &AppState,
    universe_id: &str,
    id: &str,
    target: &str,
    now: u64,
) -> anyhow::Result<()> {
    let next = now + settings(state, universe_id).await.round_ms;
    state
        .object_store
        .execute_batch(vec![
            statement(
                "UPDATE combat SET target_id = ? WHERE universe_id = ? AND entity_id = ?",
                vec![json!(target), json!(universe_id), json!(id)],
            ),
            statement(
                "INSERT OR IGNORE INTO combat (universe_id, entity_id, target_id, next_round_at)
                 VALUES (?, ?, ?, ?)",
                vec![json!(universe_id), json!(target), json!(id), json!(next)],
            ),
        ])
        .await?;
    assist(state, universe_id, target, id, now).await
}

async fn in_combat(state: &AppState, universe_id: &str, id: &str) -> anyhow::Result<bool> {
    let found: Option<i64> =
        sqlx::query_scalar("SELECT 1 FROM combat WHERE universe_id = ? AND entity_id = ?")
            .bind(universe_id)
            .bind(id)
            .fetch_optional(&mut *crate::db::run::reader(state.db.pool()).await?)
            .await?;
    Ok(found.is_some())
}

/// NPCs of the victim's `faction` in its room, alive and not fighting
/// already, join in against the attacker (their first round a round away)
pub async fn assist(
    state: &AppState,
    universe_id: &str,
    victim: &str,
    attacker: &str,
    now: u64,
) -> anyhow::Result<()> {
    let store = &state.object_store;
    let Some(victim) = store.get(universe_id, victim).await? else {
        return Ok(());
    };
    if is_player(&victim) {
        return Ok(());
    }
    let Some(faction) = victim
        .get_string("faction")
        .filter(|f| !f.is_empty())
        .map(str::to_string)
    else {
        return Ok(());
    };
    let Some(ref room) = victim.parent_id else {
        return Ok(());
    };
    let next = now + settings(state, universe_id).await.round_ms;
    for ally in store.get_contents(universe_id, room).await? {
        if ally.id == victim.id
            || ally.id == attacker
            || is_player(&ally)
            || ally.get_string("faction") != Some(faction.as_str())
            || !alive(&ally)
            || in_combat(state, universe_id, &ally.id).await?
        {
            continue;
        }
        engage_at(state, universe_id, &ally.id, attacker, next, next).await?;
    }
    Ok(())
}

/// Someone arrived in `room`: an aggressive NPC there attacks a player who
/// walked in, and an aggressive NPC that walked in attacks a player there
/// (at once; the player fights back a round later)
pub async fn arrival(state: &AppState, universe_id: &str, mover: &str, room: &str) {
    if let Err(e) = with_ai_budget(arrival_inner(state, universe_id, mover, room)).await {
        warn!("combat: arrival of {} in {}: {}", mover, room, e);
    }
}

async fn arrival_inner(
    state: &AppState,
    universe_id: &str,
    mover: &str,
    room: &str,
) -> anyhow::Result<()> {
    let store = &state.object_store;
    let Some(mover) = store.get(universe_id, mover).await? else {
        return Ok(());
    };
    if !alive(&mover) {
        return Ok(());
    }
    let now = now_ms();
    let next = now + settings(state, universe_id).await.round_ms;
    let aggressive = |o: &Object| !is_player(o) && o.get_bool("aggro").unwrap_or(false) && alive(o);
    // (fight_allowed below also requires the aggressor to be a creature)
    let here = store.get_contents(universe_id, room).await?;
    if is_player(&mover) {
        // Each NPC there not fighting may react with its own ai_idle_tick
        // (a non-nil result replaces the built-in aggro)
        let mut handled = std::collections::HashSet::new();
        for npc in here
            .iter()
            .filter(|o| !is_player(o) && o.id != mover.id && alive(o))
        {
            if !in_combat(state, universe_id, &npc.id).await?
                && ai_override(
                    state,
                    universe_id,
                    &npc.id,
                    Some(room.to_string()),
                    "ai_idle_tick",
                    json!({ "reason": "arrival", "arrived_id": mover.id }),
                )
                .await
            {
                handled.insert(npc.id.clone());
            }
        }
        // Read again: the AI handlers may have moved or killed anyone
        let Some(mover) = store.get(universe_id, &mover.id).await? else {
            return Ok(());
        };
        for id in here
            .iter()
            .filter(|o| aggressive(o) && !handled.contains(&o.id))
            .map(|o| o.id.clone())
        {
            let Some(npc) = store.get(universe_id, &id).await? else {
                continue;
            };
            if aggressive(&npc)
                && !in_combat(state, universe_id, &npc.id).await?
                && fight_allowed(state, universe_id, &npc, &mover)
                    .await
                    .is_ok()
            {
                engage_at(state, universe_id, &npc.id, &mover.id, now, next).await?;
            }
        }
    } else if aggressive(&mover) && !in_combat(state, universe_id, &mover.id).await? {
        if let Some(player) = here.iter().find(|o| is_player(o) && alive(o)) {
            if fight_allowed(state, universe_id, &mover, player)
                .await
                .is_ok()
            {
                engage_at(state, universe_id, &mover.id, &player.id, now, next).await?;
            }
        }
    }
    Ok(())
}

/// Deepest nesting of engine-invoked handlers (AI and lifecycle: a
/// handler's move or create setting off another handler, ...) and most
/// such calls per top-level action
const MAX_AI_DEPTH: u32 = 4;
const MAX_AI_CALLS: u32 = 32;

tokio::task_local! {
    /// The AI handler budget of the action being processed: (depth, calls
    /// left). Task-scoped, so it follows nested scripts and their moves.
    static AI_BUDGET: (u32, std::sync::Arc<std::sync::atomic::AtomicU32>);
}

/// Run `f` with a fresh engine hook budget of its own (an NPC's turn)
async fn fresh_ai_budget<F: std::future::Future>(f: F) -> F::Output {
    let fresh = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(MAX_AI_CALLS));
    AI_BUDGET.scope((0, fresh), f).await
}

/// Run `f` as one action for the engine hook budget: inside an action
/// already (a run, a command), it shares that action's budget; else it
/// starts a fresh one
pub(crate) async fn with_ai_budget<F: std::future::Future>(f: F) -> F::Output {
    if AI_BUDGET.try_with(|_| ()).is_ok() {
        return f.await;
    }
    let fresh = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(MAX_AI_CALLS));
    AI_BUDGET.scope((0, fresh), f).await
}

/// Run an NPC's AI handler (as its owner): true if it handled the moment
/// (returned something other than nil), false to use the built-in
/// behaviour (no code, no such handler, nil, an error, or the budget spent)
async fn ai_override(
    state: &AppState,
    universe_id: &str,
    npc: &str,
    room: Option<String>,
    handler: &str,
    mut args: serde_json::Value,
) -> bool {
    if let Some(map) = args.as_object_mut() {
        map.insert("room_id".into(), room.clone().into());
    }
    engine_hook(state, universe_id, npc, room, handler, args)
        .await
        .is_some_and(|reply| reply != "nil" && !reply.is_empty())
}

/// Run an engine-invoked handler (AI, lifecycle) of `object`'s code as its
/// owner, within the action's hook budget: at most MAX_AI_DEPTH deep (a
/// handler's doing setting off more handlers) and MAX_AI_CALLS in all.
/// Returns the handler's reply; None if it did not run (no code or
/// handler, the budget spent) or failed (logged).
pub async fn engine_hook(
    state: &AppState,
    universe_id: &str,
    object: &str,
    room: Option<String>,
    handler: &str,
    mut args: serde_json::Value,
) -> Option<String> {
    if let Some(map) = args.as_object_mut() {
        map.insert("object_id".into(), object.into());
    }
    let (depth, left) = AI_BUDGET
        .try_with(|(d, left)| (*d, left.clone()))
        .unwrap_or_else(|_| {
            (
                0,
                std::sync::Arc::new(std::sync::atomic::AtomicU32::new(MAX_AI_CALLS)),
            )
        });
    let allowed = depth < MAX_AI_DEPTH
        && left
            .try_update(
                std::sync::atomic::Ordering::Relaxed,
                std::sync::atomic::Ordering::Relaxed,
                |n| n.checked_sub(1),
            )
            .is_ok();
    if !allowed {
        warn!(
            "{} of {} not run: engine hook budget spent",
            handler, object
        );
        return None;
    }
    let call = super::scripting::invoke_handler(state, universe_id, object, room, handler, args);
    match AI_BUDGET.scope((depth + 1, left), call).await {
        Ok(reply) => reply,
        Err(e) => {
            warn!("{} of {} failed: {}", handler, object, e);
            None
        }
    }
}

/// After a move (walking, goto, spawning, a Lua move, fleeing,
/// wandering): the regions' on_leave/on_enter if the move crossed into
/// another region, on_init on each object with code already in `to` (and
/// on the newcomer, per object, if it has code), and the moved object's
/// on_move. Engine hooks, as their objects' owners, within the action's
/// hook budget.
pub async fn move_extras(
    state: &AppState,
    universe_id: &str,
    moved: &str,
    from: Option<&str>,
    to: Option<&str>,
    actor: Option<&str>,
) {
    with_ai_budget(move_extras_inner(
        state,
        universe_id,
        moved,
        from,
        to,
        actor,
    ))
    .await
}

async fn move_extras_inner(
    state: &AppState,
    universe_id: &str,
    moved: &str,
    from: Option<&str>,
    to: Option<&str>,
    actor: Option<&str>,
) {
    let store = &state.object_store;
    let get = |id: Option<&str>| {
        let id = id.map(str::to_string);
        async move {
            match id {
                Some(id) => store.get(universe_id, &id).await.ok().flatten(),
                None => None,
            }
        }
    };
    let (from_room, to_room) = (get(from).await, get(to).await);
    // For hooks a room's region is its parent region object only: the
    // region_id property (a style hint anyone may set on their room) must
    // not let a room fire someone else's region hooks
    let region = |room: &Option<Object>| room.as_ref().and_then(|r| r.parent_id.clone());
    let (rf, rt) = (region(&from_room), region(&to_room));
    let args = json!({ "moved_id": moved, "from": from, "to": to, "actor_id": actor });
    if rf != rt {
        for (region_id, handler) in [(rf, "on_leave"), (rt, "on_enter")] {
            let Some(region_id) = region_id else { continue };
            let is_region = match store.get(universe_id, &region_id).await {
                Ok(Some(r)) => {
                    r.code_hash.is_some()
                        && state
                            .classes
                            .read()
                            .await
                            .is_a(universe_id, &r.class, "region")
                }
                _ => false,
            };
            if is_region {
                engine_hook(
                    state,
                    universe_id,
                    &region_id,
                    to.map(str::to_string),
                    handler,
                    args.clone(),
                )
                .await;
            }
        }
    }
    let moved_obj = get(Some(moved)).await;
    if let Some(to) = to {
        let present = store
            .get_contents(universe_id, to)
            .await
            .unwrap_or_default();
        for p in present.iter().filter(|p| p.id != moved) {
            if p.code_hash.is_some() {
                engine_hook(
                    state,
                    universe_id,
                    &p.id,
                    Some(to.to_string()),
                    "on_init",
                    json!({ "arrived_id": moved, "actor_id": actor }),
                )
                .await;
            }
            if moved_obj.as_ref().is_some_and(|m| m.code_hash.is_some()) {
                engine_hook(
                    state,
                    universe_id,
                    moved,
                    Some(to.to_string()),
                    "on_init",
                    json!({ "other_id": p.id, "actor_id": actor }),
                )
                .await;
            }
        }
    }
    if moved_obj.as_ref().is_some_and(|m| m.code_hash.is_some()) {
        engine_hook(
            state,
            universe_id,
            moved,
            to.map(str::to_string),
            "on_move",
            args,
        )
        .await;
    }
}

/// The exits of a room as (direction, destination)
fn exits(room: &Object) -> Vec<(String, String)> {
    room.properties
        .get("exits")
        .and_then(|v| v.as_object())
        .map(|m| {
            m.iter()
                .filter_map(|(dir, to)| to.as_str().map(|t| (dir.clone(), t.to_string())))
                .collect()
        })
        .unwrap_or_default()
}

/// A room's region: its `region_id`, else its parent (a region object)
fn region_of(room: &Object) -> Option<String> {
    room.get_string("region_id")
        .map(str::to_string)
        .or_else(|| room.parent_id.clone())
}

/// Move someone out through a random exit of its room (`same_region`: only
/// to rooms of the same region), telling both rooms. Returns the direction
/// taken, or None if there was nowhere to go.
async fn leave_randomly(
    state: &AppState,
    universe_id: &str,
    who: &Object,
    same_region: bool,
    verb: &str,
    notes: &mut Vec<Note>,
) -> anyhow::Result<Option<String>> {
    let Some((dir, to)) = pick_exit(state, universe_id, who, same_region).await? else {
        return Ok(None);
    };
    leave_via(state, universe_id, who, &dir, &to, verb, notes).await?;
    Ok(Some(dir))
}

/// A random usable exit of `who`'s room (`same_region`: only into rooms of
/// the same region): (direction, destination)
async fn pick_exit(
    state: &AppState,
    universe_id: &str,
    who: &Object,
    same_region: bool,
) -> anyhow::Result<Option<(String, String)>> {
    let store = &state.object_store;
    let Some(ref from) = who.parent_id else {
        return Ok(None);
    };
    let Some(room) = store.get(universe_id, from).await? else {
        return Ok(None);
    };
    let mut choices = Vec::new();
    for (dir, to) in exits(&room) {
        if let Some(dest) = store.get(universe_id, &to).await? {
            if dest.class == "room" && (!same_region || region_of(&dest) == region_of(&room)) {
                choices.push((dir, to));
            }
        }
    }
    if choices.is_empty() {
        return Ok(None);
    }
    Ok(Some(
        choices.swap_remove(rand::rng().random_range(0..choices.len())),
    ))
}

/// Move `who` out through exit `dir` to `to`, telling both rooms
async fn leave_via(
    state: &AppState,
    universe_id: &str,
    who: &Object,
    dir: &str,
    to: &str,
    verb: &str,
    notes: &mut Vec<Note>,
) -> anyhow::Result<()> {
    let store = &state.object_store;
    let from = who.parent_id.clone().unwrap_or_default();
    let to = to.to_string();
    if is_player(who) {
        move_player(state, universe_id, &who.id, Some(&to)).await?;
    } else {
        store.move_object(universe_id, &who.id, Some(&to)).await?;
    }
    let name = name_of(who);
    notes.push(Note::Room(
        universe_id.to_string(),
        from.clone(),
        format!("{} {} {}.", name, verb, dir),
        None,
    ));
    notes.push(Note::Room(
        universe_id.to_string(),
        to.clone(),
        format!("{} arrives.", name),
        None,
    ));
    // An aggressive NPC arriving attacks a player there (in this run)
    arrival(state, universe_id, &who.id, &to).await;
    Ok(())
}

/// What a death did, for the messages and hooks after the commit
pub struct Death {
    pub victim: Object,
    pub killer: Option<String>,
    /// Where it died
    pub room: Option<String>,
    /// A player's respawn room (already moved there)
    pub respawn: Option<String>,
    /// XP the killer gained
    pub xp: Option<i64>,
    /// Gone for good: deleted by the report, after its on_death
    pub delete_after: bool,
}

/// Move a player (in the current run) and, once it commits, put their
/// session there and show them the room
async fn move_player(
    state: &AppState,
    universe_id: &str,
    player: &str,
    to: Option<&str>,
) -> anyhow::Result<()> {
    state
        .object_store
        .move_object(universe_id, player, to)
        .await?;
    let (st, u, p, to) = (
        state.clone(),
        universe_id.to_string(),
        player.to_string(),
        to.map(str::to_string),
    );
    crate::db::run::after_commit(move || async move {
        st.connections.update_room(&u, &p, to.clone()).await;
        if let Some(room) = to {
            if let Some(msg) = super::websocket::build_room_message(&st, &u, &room, None).await {
                st.connections.send_to_player(&u, &p, msg).await;
            }
        }
    })
    .await;
    Ok(())
}

/// Most objects one deletion may take with it
const MAX_TREE: usize = 10_000;

/// Delete an object and everything inside it, deepest first (an object
/// that still contains anything cannot be deleted). Players inside are
/// never deleted: they are moved to `spill_to` first (or out of the world).
/// Each object is visited once (a containment cycle cannot loop), and a
/// tree larger than MAX_TREE is refused.
pub async fn delete_tree(
    state: &AppState,
    universe_id: &str,
    id: &str,
    spill_to: Option<&str>,
) -> anyhow::Result<()> {
    let store = &state.object_store;
    let mut order = vec![id.to_string()];
    let mut seen = std::collections::HashSet::from([id.to_string()]);
    let mut i = 0;
    while i < order.len() {
        for child in store.get_contents(universe_id, &order[i]).await? {
            if is_player(&child) {
                move_player(state, universe_id, &child.id, spill_to).await?;
                continue;
            }
            if seen.insert(child.id.clone()) {
                order.push(child.id);
                if order.len() > MAX_TREE {
                    anyhow::bail!("{} holds more than {} objects", id, MAX_TREE);
                }
            }
        }
        i += 1;
    }
    // Every on_destroy first (while everything still exists), deepest
    // first; a failing or skipped hook does not stop the deletion
    for victim in order.iter().rev() {
        if let Some(obj) = store.get(universe_id, victim).await? {
            if obj.code_hash.is_some() {
                engine_hook(
                    state,
                    universe_id,
                    victim,
                    obj.parent_id.clone(),
                    "on_destroy",
                    json!({}),
                )
                .await;
            }
        }
    }
    // Then list the tree again (the hooks may have added to it or moved a
    // player in), and delete it all, players spilled out
    let mut order = vec![id.to_string()];
    let mut seen = std::collections::HashSet::from([id.to_string()]);
    let mut i = 0;
    while i < order.len() {
        for child in store.get_contents(universe_id, &order[i]).await? {
            if is_player(&child) {
                move_player(state, universe_id, &child.id, spill_to).await?;
                continue;
            }
            if seen.insert(child.id.clone()) {
                order.push(child.id);
                if order.len() > MAX_TREE {
                    anyhow::bail!("{} holds more than {} objects", id, MAX_TREE);
                }
            }
        }
        i += 1;
    }
    for victim in order.iter().rev() {
        store.delete(universe_id, victim).await?;
    }
    Ok(())
}

fn statement(sql: &str, params: Vec<serde_json::Value>) -> crate::db::run::Statement {
    (sql.to_string(), params)
}

/// Handle `victim`'s death in the current run, by the universe's policy:
/// its fights and effects end, its belongings go to a corpse, the floor or
/// nowhere (or stay, for a player under `keep`); a player respawns, an NPC
/// leaves the world until its respawn (or for good), and its killer gains
/// XP. The caller holds the effects write guard.
pub async fn death(
    state: &AppState,
    universe_id: &str,
    victim: &Object,
    killer: Option<&Object>,
    now: u64,
) -> anyhow::Result<Death> {
    let store = &state.object_store;
    let id = victim.id.as_str();
    let settings = settings(state, universe_id).await;
    let room = victim.parent_id.clone();
    let player = is_player(victim);
    let fighter = Combatant::from_object(victim, if player { 100 } else { 10 });

    store
        .execute_batch(vec![
            crate::combat::EffectRegistry::clear_statement(universe_id, id),
            statement(
                "DELETE FROM combat WHERE universe_id = ? AND (entity_id = ? OR target_id = ?)",
                vec![json!(universe_id), json!(id), json!(id)],
            ),
        ])
        .await?;
    crate::db::run::after_commit({
        let effects = state.effects.clone();
        let (u, v) = (universe_id.to_string(), id.to_string());
        move || async move { effects.cache_clear(&u, &v).await }
    })
    .await;

    // Belongings
    let policy = if player {
        settings.player_death.as_str()
    } else {
        settings.npc_death.as_str()
    };
    // Players inside are not belongings: they go to the room
    let contents = store.get_contents(universe_id, id).await?;
    for p in contents.iter().filter(|o| is_player(o)) {
        move_player(state, universe_id, &p.id, room.as_deref()).await?;
    }
    let items: Vec<Object> = contents
        .into_iter()
        .filter(|item| !is_player(item) && !item.get_bool("fixed").unwrap_or(false))
        .collect();
    if !items.is_empty() {
        match (policy, room.as_deref()) {
            ("corpse", Some(room)) => {
                let corpse_id = format!(
                    "/corpses/c-{}",
                    &uuid::Uuid::new_v4().simple().to_string()[..12]
                );
                let mut corpse = Object::new(&corpse_id, universe_id, "item")?;
                corpse.parent_id = Some(room.to_string());
                corpse.set_property("name", json!(format!("corpse of {}", name_of(victim))));
                corpse.set_property(
                    "description",
                    json!(format!("The remains of {}.", name_of(victim))),
                );
                corpse.set_property("corpse", json!(true));
                corpse.set_property("fixed", json!(true));
                store.create(&corpse).await?;
                for item in &items {
                    store
                        .move_object(universe_id, &item.id, Some(&corpse_id))
                        .await?;
                }
                store
                    .execute_batch(vec![statement(
                        "INSERT OR REPLACE INTO deadlines (universe_id, object_id, kind, due_at)
                         VALUES (?, ?, 'decay', ?)",
                        vec![
                            json!(universe_id),
                            json!(corpse_id),
                            json!(now + settings.corpse_decay_ms),
                        ],
                    )])
                    .await?;
                crate::db::run::after_commit({
                    let changed = state.combat_changed.clone();
                    move || async move { changed.notify_one() }
                })
                .await;
            }
            ("destroy", _) => {
                for item in &items {
                    delete_tree(state, universe_id, &item.id, room.as_deref()).await?;
                }
            }
            ("keep", _) => {}
            // drop, or a corpse with nowhere to lie: on the floor
            (_, Some(room)) => {
                for item in &items {
                    store.move_object(universe_id, &item.id, Some(room)).await?;
                }
            }
            (_, None) => {}
        }
    }

    let mut respawn = None;
    let mut xp = None;
    let mut delete_after = false;
    if player {
        // Back at full HP, at the respawn room (or where they fell)
        respawn = state
            .player_manager
            .respawn_room(victim, universe_id)
            .await?;
        store
            .execute_batch(vec![crate::objects::ObjectStore::set_properties_statement(
                universe_id,
                id,
                &[("hp", json!(fighter.max_hp))],
            )?])
            .await?;
        if let Some(ref to) = respawn {
            store.move_object(universe_id, id, Some(to)).await?;
        }
    } else {
        if let Some(k) = killer.filter(|k| is_player(k)) {
            let value = victim
                .properties
                .get("xp_value")
                .and_then(|v| v.as_i64())
                .unwrap_or(fighter.max_hp as i64)
                .max(0);
            let had = k.properties.get("xp").and_then(|v| v.as_i64()).unwrap_or(0);
            store
                .execute_batch(vec![crate::objects::ObjectStore::set_properties_statement(
                    universe_id,
                    &k.id,
                    &[("xp", json!(had.saturating_add(value)))],
                )?])
                .await?;
            xp = Some(value);
        }
        let respawn_s = victim
            .properties
            .get("respawn_seconds")
            .and_then(|v| v.as_u64())
            .unwrap_or(DEFAULT_RESPAWN_S)
            .min(MAX_DELAY_S);
        if respawn_s == 0 {
            // Gone for good, but only once its on_death has run: out of the
            // world now; the death report deletes it after on_death. A
            // `delete` deadline a minute away is the fallback, should the
            // report never run (the server stopped in between).
            delete_after = true;
            store
                .execute_batch(vec![
                    crate::combat::engine::remove_from_room_statement(universe_id, id),
                    statement(
                        "INSERT OR REPLACE INTO deadlines (universe_id, object_id, kind, due_at)
                         VALUES (?, ?, 'delete', ?)",
                        vec![
                            json!(universe_id),
                            json!(id),
                            json!(now + DELETE_FALLBACK_MS),
                        ],
                    ),
                ])
                .await?;
        } else {
            let home = victim
                .get_string("home_room")
                .map(str::to_string)
                .or_else(|| room.clone());
            let mut props = vec![("hp", json!(fighter.max_hp))];
            if let Some(ref home) = home {
                props.push(("home_room", json!(home)));
            }
            // Fallback if the home room is gone by the respawn (this
            // death's room, or none)
            props.push(("died_in", json!(room)));
            store
                .execute_batch(vec![
                    crate::objects::ObjectStore::set_properties_statement(universe_id, id, &props)?,
                    crate::combat::engine::remove_from_room_statement(universe_id, id),
                    statement(
                        "INSERT OR REPLACE INTO deadlines (universe_id, object_id, kind, due_at)
                         VALUES (?, ?, 'respawn', ?)",
                        vec![
                            json!(universe_id),
                            json!(id),
                            json!(now + respawn_s.saturating_mul(1000)),
                        ],
                    ),
                ])
                .await?;
            crate::db::run::after_commit({
                let changed = state.combat_changed.clone();
                move || async move { changed.notify_one() }
            })
            .await;
        }
    }
    Ok(Death {
        victim: victim.clone(),
        killer: killer.map(|k| k.id.clone()),
        room,
        respawn,
        xp,
        delete_after,
    })
}

/// An NPC's on_death (as its owner); failures are logged
async fn run_on_death(
    state: &AppState,
    universe_id: &str,
    id: &str,
    room: Option<&str>,
    killer: Option<&str>,
) {
    let args = json!({ "object_id": id, "room_id": room, "killer_id": killer });
    if let Err(e) = super::scripting::invoke_handler(
        state,
        universe_id,
        id,
        room.map(str::to_string),
        "on_death",
        args,
    )
    .await
    {
        warn!("on_death of {} failed: {}", id, e);
    }
}

/// After the commit: tell everyone about a death, run the NPC's on_death,
/// show a respawned player where they are
pub async fn report_death(state: &AppState, universe_id: &str, d: &Death) {
    let id = d.victim.id.as_str();
    let name = name_of(&d.victim);
    if let Some(ref room) = d.room {
        state
            .connections
            .broadcast_room_except(
                universe_id,
                room,
                is_player(&d.victim).then_some(id),
                ServerMessage::Output {
                    text: format!("{} dies.", name),
                },
            )
            .await;
    }
    if let (Some(killer), Some(xp)) = (d.killer.as_deref(), d.xp) {
        if xp > 0 {
            state
                .connections
                .send_to_player(
                    universe_id,
                    killer,
                    ServerMessage::Output {
                        text: format!("You gain {} experience.", xp),
                    },
                )
                .await;
        }
    }
    if !is_player(&d.victim) {
        run_on_death(
            state,
            universe_id,
            id,
            d.room.as_deref(),
            d.killer.as_deref(),
        )
        .await;
        if d.delete_after {
            // Now, after its on_death (with whatever it still holds)
            let deleted = super::scripting::in_run(state, async {
                delete_tree(state, universe_id, id, None)
                    .await
                    .map_err(|e| e.to_string())
            })
            .await;
            if let Err(e) = deleted {
                warn!("deleting {} after its death: {} (the fallback will)", id, e);
            }
        }
        return;
    }
    state
        .connections
        .send_to_player(
            universe_id,
            id,
            ServerMessage::Output {
                text: "You have died.".to_string(),
            },
        )
        .await;
    let at = match d.respawn {
        Some(ref room) => Some(room.clone()),
        None => d.room.clone(),
    };
    if let Some(room) = at {
        state
            .connections
            .update_room(universe_id, id, Some(room.clone()))
            .await;
        if let Some(msg) =
            super::websocket::build_room_message(state, universe_id, &room, None).await
        {
            state.connections.send_to_player(universe_id, id, msg).await;
        }
    }
}

/// The scheduler: sleep until the earliest round or deadline, then run
/// everything due
pub async fn run_combat_scheduler(state: AppState) {
    loop {
        let changed = state.combat_changed.notified();
        tokio::pin!(changed);
        let now = now_ms();
        let wait = match next_due(&state).await {
            Ok(Some(due)) if due <= now => Duration::ZERO,
            Ok(Some(due)) => Duration::from_millis(due - now).min(IDLE_WAIT),
            Ok(None) => IDLE_WAIT,
            Err(e) => {
                warn!("combat: reading deadlines failed: {}", e);
                Duration::from_millis(500)
            }
        };
        if !wait.is_zero() {
            tokio::select! {
                _ = tokio::time::sleep(wait) => {}
                _ = &mut changed => continue,
            }
        }
        process(&state, now_ms()).await;
    }
}

async fn next_due(state: &AppState) -> anyhow::Result<Option<u64>> {
    let due: Option<i64> = sqlx::query_scalar(
        "SELECT MIN(t) FROM (SELECT MIN(next_round_at) AS t FROM combat
                             UNION ALL SELECT MIN(due_at) FROM deadlines)",
    )
    .fetch_one(state.db.pool())
    .await?;
    Ok(due.map(|t| t.max(0) as u64))
}

/// A message for after the commit
enum Note {
    Player(String, String, String),
    Room(String, String, String, Option<String>),
    Death(String, Box<Death>),
    Hook(String, String, Option<String>, serde_json::Value),
}

/// Run every round and deadline due at `now`, as one run
pub async fn process(state: &AppState, now: u64) {
    let notes = super::scripting::in_run(state, async {
        // HP and effects are read then written: serialized with every writer
        let _writes = state.effects.write_guard().await;
        let mut notes = Vec::new();
        rounds(state, now, &mut notes)
            .await
            .map_err(|e| e.to_string())?;
        deadlines(state, now, &mut notes)
            .await
            .map_err(|e| e.to_string())?;
        Ok::<_, String>(notes)
    })
    .await;
    let notes = match notes {
        Ok(notes) => notes,
        Err(e) => {
            warn!("combat: due rounds not committed (retrying): {}", e);
            tokio::time::sleep(Duration::from_millis(500)).await;
            return;
        }
    };
    deliver(state, notes).await;
}

/// Messages and hooks for after a commit
async fn deliver(state: &AppState, notes: Vec<Note>) {
    for note in notes {
        match note {
            Note::Player(u, p, text) => {
                state
                    .connections
                    .send_to_player(&u, &p, ServerMessage::Output { text })
                    .await
            }
            Note::Room(u, room, text, except) => {
                state
                    .connections
                    .broadcast_room_except(
                        &u,
                        &room,
                        except.as_deref(),
                        ServerMessage::Output { text },
                    )
                    .await
            }
            Note::Death(u, d) => report_death(state, &u, &d).await,
            Note::Hook(u, object, room, args) => {
                if let Err(e) =
                    super::scripting::invoke_handler(state, &u, &object, room, "on_damage", args)
                        .await
                {
                    warn!("on_damage of {} failed: {}", object, e);
                }
            }
        }
    }
}

async fn rounds(state: &AppState, now: u64, notes: &mut Vec<Note>) -> anyhow::Result<()> {
    let due: Vec<(String, String, String)> = sqlx::query_as(
        "SELECT universe_id, entity_id, target_id FROM combat
         WHERE next_round_at <= ? ORDER BY next_round_at",
    )
    .bind(now as i64)
    .fetch_all(&mut *crate::db::run::reader(state.db.pool()).await?)
    .await?;
    for (u, entity, _) in due {
        // Each round is a nested part: one that fails (a bad object) is
        // undone and its fight ended, and the other rounds go on
        let mut mine = Vec::new();
        // Each fighter's turn has its own AI handler budget
        let done = fresh_ai_budget(super::scripting::in_run(state, async {
            round(state, now, u.clone(), entity.clone(), &mut mine)
                .await
                .map_err(|e| e.to_string())
        }))
        .await;
        match done {
            Ok(()) => notes.extend(mine),
            Err(e) => {
                warn!("combat: a round of {} failed (fight ended): {}", entity, e);
                state
                    .object_store
                    .execute_batch(vec![statement(
                        "DELETE FROM combat WHERE universe_id = ? AND entity_id = ?",
                        vec![json!(u), json!(entity)],
                    )])
                    .await?;
            }
        }
    }
    Ok(())
}

/// One fighter's round
async fn round(
    state: &AppState,
    now: u64,
    u: String,
    entity: String,
    notes: &mut Vec<Note>,
) -> anyhow::Result<()> {
    let store = &state.object_store;
    {
        // Read again: an earlier round in this batch may have ended this
        // fight (a death removes its rows)
        let still: Option<String> = sqlx::query_scalar(
            "SELECT target_id FROM combat WHERE universe_id = ? AND entity_id = ?",
        )
        .bind(&u)
        .bind(&entity)
        .fetch_optional(&mut *crate::db::run::reader(state.db.pool()).await?)
        .await?;
        let Some(mut target_id) = still else {
            return Ok(());
        };
        let end = |u: &str, e: &str| {
            statement(
                "DELETE FROM combat WHERE universe_id = ? AND entity_id = ?",
                vec![json!(u), json!(e)],
            )
        };
        let Some(attacker) = store.get(&u, &entity).await? else {
            return Ok(());
        };
        if !alive(&attacker) || attacker.parent_id.is_none() {
            store.execute_batch(vec![end(&u, &entity)]).await?;
            return Ok(());
        }
        // The target must still be here and alive; else someone still
        // fighting us, or the fight is over
        let here = |t: &Option<Object>| {
            t.as_ref()
                .is_some_and(|t| t.parent_id == attacker.parent_id && alive(t))
        };
        let mut target = store.get(&u, &target_id).await?;
        if !here(&target) {
            let foes: Vec<String> = sqlx::query_scalar(
                "SELECT entity_id FROM combat WHERE universe_id = ? AND target_id = ?",
            )
            .bind(&u)
            .bind(&entity)
            .fetch_all(&mut *crate::db::run::reader(state.db.pool()).await?)
            .await?;
            target = None;
            for foe in foes {
                let candidate = store.get(&u, &foe).await?;
                if here(&candidate) {
                    target_id = foe;
                    target = candidate;
                    break;
                }
            }
            if target.is_none() {
                store.execute_batch(vec![end(&u, &entity)]).await?;
                if is_player(&attacker) {
                    notes.push(Note::Player(
                        u.clone(),
                        entity.clone(),
                        "You stop fighting.".into(),
                    ));
                }
                return Ok(());
            }
        }
        let target = target.expect("found above");
        // Only while the rules allow it (combat switched off, a PvP flag
        // switched off, the policy changed)
        if fight_allowed(state, &u, &attacker, &target).await.is_err() {
            store.execute_batch(vec![end(&u, &entity)]).await?;
            if is_player(&attacker) {
                notes.push(Note::Player(
                    u.clone(),
                    entity.clone(),
                    "You stop fighting.".into(),
                ));
            }
            return Ok(());
        }
        let round_ms = settings(state, &u).await.round_ms;
        store
            .execute_batch(vec![statement(
                "UPDATE combat SET target_id = ?, next_round_at = ? WHERE universe_id = ? AND entity_id = ?",
                vec![json!(target_id), json!(now + round_ms), json!(u), json!(entity)],
            )])
            .await?;
        // A stunned or paralyzed fighter loses its turn
        if !state.effects.get(&u, &entity).await.can_act(now) {
            if is_player(&attacker) {
                notes.push(Note::Player(
                    u.clone(),
                    entity.clone(),
                    "You are stunned and cannot act.".into(),
                ));
            }
            return Ok(());
        }
        // An NPC's own ai_combat_tick (as its owner) may take its turn
        // instead: a non-nil result replaces the built-in behaviour
        if !is_player(&attacker)
            && ai_override(
                state,
                &u,
                &entity,
                attacker.parent_id.clone(),
                "ai_combat_tick",
                json!({ "target_id": target_id, "hp": Combatant::from_object(&attacker, 10).hp }),
            )
            .await
        {
            return Ok(());
        }
        // A hurt NPC below its flee_threshold (a fraction of max HP) runs
        // through a random exit instead (if there is one)
        if !is_player(&attacker) {
            let threshold = attacker
                .properties
                .get("flee_threshold")
                .and_then(|v| v.as_f64())
                .unwrap_or(0.0);
            let me = Combatant::from_object(&attacker, 10);
            if threshold > 0.0
                && (me.hp as f64) < threshold * me.max_hp as f64
                && leave_randomly(state, &u, &attacker, false, "flees", notes)
                    .await?
                    .is_some()
            {
                store.execute_batch(vec![end(&u, &entity)]).await?;
                return Ok(());
            }
        }
        let blow = strike(state, &u, &attacker, &target, now).await?;
        let room = attacker.parent_id.clone().expect("checked above");
        if is_player(&attacker) {
            notes.push(Note::Player(
                u.clone(),
                entity.clone(),
                blow.attacker_text.clone(),
            ));
        }
        if is_player(&target) {
            notes.push(Note::Player(
                u.clone(),
                target_id.clone(),
                blow.target_text.clone(),
            ));
        }
        // Onlookers: everyone else in the room (attacker and target
        // excepted by name in the text's sense; the target hears its own
        // line above)
        notes.push(Note::Room(
            u.clone(),
            room.clone(),
            blow.room_text.clone(),
            Some(entity.clone()),
        ));
        if let Some(damage) = blow.damage {
            notes.push(Note::Hook(
                u.clone(),
                target_id.clone(),
                Some(room.clone()),
                json!({ "attacker_id": entity, "damage": damage }),
            ));
        }
        // The target fights back if it is not fighting yet
        store
            .execute_batch(vec![statement(
                "INSERT OR IGNORE INTO combat (universe_id, entity_id, target_id, next_round_at)
                 VALUES (?, ?, ?, ?)",
                vec![
                    json!(u),
                    json!(target_id),
                    json!(entity),
                    json!(now + round_ms),
                ],
            )])
            .await?;
        if blow.killed {
            let Some(dead) = store.get(&u, &target_id).await? else {
                return Ok(());
            };
            let d = death(state, &u, &dead, Some(&attacker), now).await?;
            notes.push(Note::Death(u.clone(), Box::new(d)));
        }
    }
    Ok(())
}

async fn deadlines(state: &AppState, now: u64, notes: &mut Vec<Note>) -> anyhow::Result<()> {
    let due: Vec<(String, String, String)> = sqlx::query_as(
        "SELECT universe_id, object_id, kind FROM deadlines WHERE due_at <= ? ORDER BY due_at",
    )
    .bind(now as i64)
    .fetch_all(&mut *crate::db::run::reader(state.db.pool()).await?)
    .await?;
    let store = &state.object_store;
    for (u, id, kind) in due {
        // Isolated like rounds: a failing one is dropped, the rest go on
        let mut mine = Vec::new();
        let done = fresh_ai_budget(super::scripting::in_run(state, async {
            deadline(state, now, u.clone(), id.clone(), kind.clone(), &mut mine)
                .await
                .map_err(|e| e.to_string())
        }))
        .await;
        match done {
            Ok(()) => notes.extend(mine),
            Err(e) => {
                warn!("combat: {} of {} failed (dropped): {}", kind, id, e);
                store
                    .execute_batch(vec![statement(
                        "DELETE FROM deadlines WHERE universe_id = ? AND object_id = ? AND kind = ?",
                        vec![json!(u), json!(id), json!(kind)],
                    )])
                    .await?;
            }
        }
    }
    Ok(())
}

/// One due deadline
async fn deadline(
    state: &AppState,
    now: u64,
    u: String,
    id: String,
    kind: String,
    notes: &mut Vec<Note>,
) -> anyhow::Result<()> {
    let store = &state.object_store;
    {
        store
            .execute_batch(vec![statement(
                "DELETE FROM deadlines WHERE universe_id = ? AND object_id = ? AND kind = ?",
                vec![json!(u), json!(id), json!(kind)],
            )])
            .await?;
        let Some(obj) = store.get(&u, &id).await? else {
            return Ok(());
        };
        match kind.as_str() {
            "decay" => {
                // What is left spills into the room
                let room = obj.parent_id.clone();
                for item in store.get_contents(&u, &id).await? {
                    store.move_object(&u, &item.id, room.as_deref()).await?;
                }
                delete_tree(state, &u, &id, room.as_deref()).await?;
                if let Some(room) = room {
                    notes.push(Note::Room(
                        u.clone(),
                        room,
                        format!("The {} crumbles to dust.", name_of(&obj)),
                        None,
                    ));
                }
            }
            "respawn" => {
                // Home, else where it died; with neither left, it stays
                // out of the world (logged)
                let mut home = None;
                for key in ["home_room", "died_in"] {
                    if let Some(room) = obj.get_string(key) {
                        if store.get(&u, room).await?.is_some() {
                            home = Some(room.to_string());
                            break;
                        }
                    }
                }
                let Some(home) = home else {
                    warn!("combat: {} has no room to respawn in", id);
                    return Ok(());
                };
                store.move_object(&u, &id, Some(&home)).await?;
                store
                    .execute_batch(vec![crate::objects::ObjectStore::set_properties_statement(
                        &u,
                        &id,
                        &[("died_in", json!(null))],
                    )?])
                    .await?;
                notes.push(Note::Room(
                    u.clone(),
                    home.clone(),
                    format!("{} appears.", name_of(&obj)),
                    None,
                ));
                arrival(state, &u, &id, &home).await;
            }
            "delete" => {
                // An NPC gone for good, after its death was reported
                delete_tree(state, &u, &id, None).await?;
            }
            "wander" => {
                // Opt-in (wander_seconds), and only while it lasts
                let Some(every) = obj
                    .properties
                    .get("wander_seconds")
                    .and_then(|v| v.as_f64())
                    .filter(|s| s.is_finite() && *s > 0.0)
                    .map(|s| s.clamp(1.0, MAX_WANDER_S))
                else {
                    return Ok(());
                };
                // Not while fighting, dead or nowhere; within its region.
                // Its own ai_idle_tick may act instead (non-nil result).
                if !is_player(&obj)
                    && alive(&obj)
                    && obj.parent_id.is_some()
                    && !in_combat(state, &u, &id).await?
                    && !ai_override(
                        state,
                        &u,
                        &id,
                        obj.parent_id.clone(),
                        "ai_idle_tick",
                        json!({ "reason": "wander" }),
                    )
                    .await
                {
                    leave_randomly(state, &u, &obj, true, "leaves", notes).await?;
                }
                let next_ms = (every * 1000.0 * rand::rng().random_range(0.5..1.5)) as u64;
                store
                    .execute_batch(vec![statement(
                        "INSERT OR REPLACE INTO deadlines (universe_id, object_id, kind, due_at)
                         VALUES (?, ?, 'wander', ?)",
                        vec![json!(u), json!(id), json!(now + next_ms.max(1000))],
                    )])
                    .await?;
            }
            _ => {}
        }
    }
    Ok(())
}

/// The engine as Lua sees it (game.attack and friends; lua::engine)
pub struct CombatEngine {
    pub state: AppState,
}

use crate::lua::engine::{BlowReport, BoxFuture, CombatState, DamageReport, Engine};

impl CombatEngine {
    async fn object(&self, universe_id: &str, id: &str) -> Result<Object, String> {
        self.state
            .object_store
            .get(universe_id, id)
            .await
            .map_err(|e| e.to_string())?
            .ok_or_else(|| format!("no such object: {}", id))
    }

    /// Deliver notes and a death report once the current run commits
    async fn after(&self, notes: Vec<Note>, death: Option<Death>) {
        let st = self.state.clone();
        crate::db::run::after_commit(move || async move {
            deliver(&st, notes).await;
            if let Some(d) = death {
                let u = d.victim.universe_id.clone();
                report_death(&st, &u, &d).await;
            }
        })
        .await;
    }

    async fn attack_inner(
        &self,
        u: &str,
        attacker_id: &str,
        target_id: &str,
    ) -> Result<BlowReport, String> {
        let state = &self.state;
        let _writes = state.effects.write_guard().await;
        let attacker = self.object(u, attacker_id).await?;
        let target = self.object(u, target_id).await?;
        fight_allowed(state, u, &attacker, &target).await?;
        let now = now_ms();
        if !state.effects.get(u, attacker_id).await.can_act(now) {
            return Err(format!("{} is stunned and cannot act.", name_of(&attacker)));
        }
        // Rounds set the pace: no extra blow during a fight
        match current_target(state, u, attacker_id)
            .await
            .map_err(|e| e.to_string())?
        {
            Some(current) if current == target_id => {
                return Err(format!("already fighting {}", name_of(&target)));
            }
            Some(_) => {
                retarget(state, u, attacker_id, target_id, now)
                    .await
                    .map_err(|e| e.to_string())?;
                return Ok(BlowReport {
                    text: format!(
                        "{} turns to fight {}.",
                        name_of(&attacker),
                        name_of(&target)
                    ),
                    ..Default::default()
                });
            }
            None => {}
        }
        if !ready_to_strike(state, u, &attacker, now).await {
            return Err(format!(
                "{} is not ready to strike again",
                name_of(&attacker)
            ));
        }
        let blow = strike(state, u, &attacker, &target, now)
            .await
            .map_err(|e| e.to_string())?;
        let room = attacker.parent_id.clone().unwrap_or_default();
        let mut notes = Vec::new();
        if is_player(&attacker) {
            notes.push(Note::Player(
                u.to_string(),
                attacker_id.to_string(),
                blow.attacker_text.clone(),
            ));
        }
        if is_player(&target) {
            notes.push(Note::Player(
                u.to_string(),
                target_id.to_string(),
                blow.target_text.clone(),
            ));
        }
        notes.push(Note::Room(
            u.to_string(),
            room.clone(),
            blow.room_text.clone(),
            Some(attacker_id.to_string()),
        ));
        if let Some(damage) = blow.damage {
            notes.push(Note::Hook(
                u.to_string(),
                target_id.to_string(),
                Some(room),
                json!({ "attacker_id": attacker_id, "damage": damage }),
            ));
        }
        let death = if blow.killed {
            let dead = self.object(u, target_id).await?;
            Some(
                death(state, u, &dead, Some(&attacker), now)
                    .await
                    .map_err(|e| e.to_string())?,
            )
        } else {
            engage(state, u, attacker_id, target_id, now)
                .await
                .map_err(|e| e.to_string())?;
            None
        };
        self.after(notes, death).await;
        Ok(BlowReport {
            hit: blow.damage.is_some(),
            critical: blow.critical,
            fumble: blow.fumble,
            damage: blow.damage.unwrap_or(0) as i64,
            killed: blow.killed,
            parts: blow
                .parts
                .iter()
                .map(|p| (p.dtype.to_string(), p.rolled as i64, p.applied as i64))
                .collect(),
            text: blow.attacker_text,
        })
    }

    async fn damage_inner(
        &self,
        u: &str,
        target_id: &str,
        amount: i64,
        damage_type: &str,
        source: Option<&str>,
    ) -> Result<DamageReport, String> {
        let state = &self.state;
        let _writes = state.effects.write_guard().await;
        let target = self.object(u, target_id).await?;
        // A dead NPC waiting to respawn: not again
        if awaiting_respawn(state, u, target_id).await? {
            return Err(format!("{} is not in the world", name_of(&target)));
        }
        let dtype = damage_type
            .parse::<DamageType>()
            .map_err(|_| format!("unknown damage type: {}", damage_type))?;
        let amount = amount.clamp(0, 1_000_000) as i32;
        let who = Combatant::from_object(&target, if is_player(&target) { 100 } else { 10 });
        let dealt = who
            .profile
            .calculate_damage(amount, dtype, false)
            .final_damage
            .max(0);
        let was_alive = who.hp > 0;
        let hp = who.hp.saturating_sub(dealt).max(0);
        state
            .object_store
            .execute_batch(vec![crate::objects::ObjectStore::set_properties_statement(
                u,
                target_id,
                &[("hp", json!(hp))],
            )
            .map_err(|e| e.to_string())?])
            .await
            .map_err(|e| e.to_string())?;
        let killed = was_alive && hp == 0;
        let death = if killed {
            let killer = match source {
                Some(s) => state.object_store.get(u, s).await.ok().flatten(),
                None => None,
            };
            let dead = self.object(u, target_id).await?;
            Some(
                death(state, u, &dead, killer.as_ref(), now_ms())
                    .await
                    .map_err(|e| e.to_string())?,
            )
        } else {
            None
        };
        self.after(Vec::new(), death).await;
        Ok(DamageReport {
            damage: dealt as i64,
            hp: hp as i64,
            killed,
        })
    }

    async fn flee_inner(&self, u: &str, id: &str) -> Result<String, String> {
        let state = &self.state;
        let who = self.object(u, id).await?;
        let now = now_ms();
        if !state.effects.get(u, id).await.can_act(now) {
            return Err("You are stunned and cannot flee.".to_string());
        }
        let fail = settings(state, u).await.flee_fail_percent;
        if fail > 0 && rand::rng().random_range(0..100) < fail {
            return Err("You try to flee but fail to get away!".to_string());
        }
        // An exit first (nowhere to run: the fight goes on); then its own
        // fight ends (so a fight it walks into can start) and it moves, in
        // this run; its attackers' end on their next round (no longer
        // together)
        let (dir, to) = pick_exit(state, u, &who, false)
            .await
            .map_err(|e| e.to_string())?
            .ok_or_else(|| "There is nowhere to run.".to_string())?;
        state
            .object_store
            .execute_batch(vec![statement(
                "DELETE FROM combat WHERE universe_id = ? AND entity_id = ?",
                vec![json!(u), json!(id)],
            )])
            .await
            .map_err(|e| e.to_string())?;
        let mut notes = Vec::new();
        leave_via(state, u, &who, &dir, &to, "flees", &mut notes)
            .await
            .map_err(|e| e.to_string())?;
        self.after(notes, None).await;
        Ok(dir)
    }
}

impl Engine for CombatEngine {
    fn can_attack<'a>(
        &'a self,
        universe_id: &'a str,
        attacker: &'a str,
        target: &'a str,
    ) -> BoxFuture<'a, Result<(), String>> {
        Box::pin(async move {
            let a = self.object(universe_id, attacker).await?;
            let t = self.object(universe_id, target).await?;
            fight_allowed(&self.state, universe_id, &a, &t).await
        })
    }

    fn attack<'a>(
        &'a self,
        universe_id: &'a str,
        attacker: &'a str,
        target: &'a str,
    ) -> BoxFuture<'a, Result<BlowReport, String>> {
        Box::pin(self.attack_inner(universe_id, attacker, target))
    }

    fn combat_state<'a>(
        &'a self,
        universe_id: &'a str,
        id: &'a str,
    ) -> BoxFuture<'a, Result<CombatState, String>> {
        Box::pin(async move {
            let pool = self.state.db.pool();
            let mine: Option<(String, i64)> = sqlx::query_as(
                "SELECT target_id, next_round_at FROM combat WHERE universe_id = ? AND entity_id = ?",
            )
            .bind(universe_id)
            .bind(id)
            .fetch_optional(&mut *crate::db::run::reader(pool).await.map_err(|e| e.to_string())?)
            .await
            .map_err(|e| e.to_string())?;
            let attackers: Vec<String> = sqlx::query_scalar(
                "SELECT entity_id FROM combat WHERE universe_id = ? AND target_id = ? ORDER BY entity_id",
            )
            .bind(universe_id)
            .bind(id)
            .fetch_all(&mut *crate::db::run::reader(pool).await.map_err(|e| e.to_string())?)
            .await
            .map_err(|e| e.to_string())?;
            Ok(CombatState {
                in_combat: mine.is_some(),
                attacking: mine.as_ref().map(|(t, _)| t.clone()),
                next_round_at: mine.map(|(_, at)| at.max(0) as u64),
                attackers,
            })
        })
    }

    fn damage<'a>(
        &'a self,
        universe_id: &'a str,
        target: &'a str,
        amount: i64,
        damage_type: &'a str,
        source: Option<&'a str>,
    ) -> BoxFuture<'a, Result<DamageReport, String>> {
        Box::pin(self.damage_inner(universe_id, target, amount, damage_type, source))
    }

    fn heal<'a>(
        &'a self,
        universe_id: &'a str,
        target: &'a str,
        amount: i64,
    ) -> BoxFuture<'a, Result<i64, String>> {
        Box::pin(async move {
            let _writes = self.state.effects.write_guard().await;
            let t = self.object(universe_id, target).await?;
            if awaiting_respawn(&self.state, universe_id, target).await? {
                return Err(format!("{} is not in the world", name_of(&t)));
            }
            let who = Combatant::from_object(&t, if is_player(&t) { 100 } else { 10 });
            let hp = (who.hp as i64)
                .saturating_add(amount.clamp(0, 1_000_000))
                .min(who.max_hp as i64);
            self.state
                .object_store
                .execute_batch(vec![crate::objects::ObjectStore::set_properties_statement(
                    universe_id,
                    target,
                    &[("hp", json!(hp))],
                )
                .map_err(|e| e.to_string())?])
                .await
                .map_err(|e| e.to_string())?;
            Ok(hp)
        })
    }

    fn hook<'a>(
        &'a self,
        universe_id: &'a str,
        object: &'a str,
        handler: &'a str,
        args: serde_json::Value,
    ) -> BoxFuture<'a, ()> {
        Box::pin(async move {
            let room = self
                .state
                .object_store
                .get(universe_id, object)
                .await
                .ok()
                .flatten()
                .and_then(|o| o.parent_id);
            engine_hook(&self.state, universe_id, object, room, handler, args).await;
        })
    }

    fn stop<'a>(&'a self, universe_id: &'a str, id: &'a str) -> BoxFuture<'a, Result<u64, String>> {
        Box::pin(async move {
            let response = self
                .state
                .raft_writer
                .execute(
                    "DELETE FROM combat WHERE universe_id = ? AND (entity_id = ? OR target_id = ?)",
                    vec![json!(universe_id), json!(id), json!(id)],
                )
                .await
                .map_err(|e| e.to_string())?;
            Ok(response.rows_affected)
        })
    }

    fn flee<'a>(
        &'a self,
        universe_id: &'a str,
        id: &'a str,
    ) -> BoxFuture<'a, Result<String, String>> {
        Box::pin(self.flee_inner(universe_id, id))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_weapon_dice_bounds() {
        assert!(weapon_dice("2d4+1").is_some());
        assert!(
            weapon_dice("1d6-2147483648").is_none(),
            "no overflow on i32::MIN"
        );
        assert!(weapon_dice("1d6+1001").is_none());
        assert!(weapon_dice("101d6").is_none());
        assert!(weapon_dice("1d0").is_none());
        assert!(weapon_dice("1d1001").is_none());
    }
}
