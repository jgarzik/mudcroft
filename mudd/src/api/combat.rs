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
/// Longest respawn or corpse decay (builder-set values are clamped)
const MAX_DELAY_S: u64 = 30 * 24 * 60 * 60;
/// Longest pause between wanders
const MAX_WANDER_S: f64 = 24.0 * 60.0 * 60.0;

/// Longest sleep with nothing due (changes wake the scheduler sooner)
const IDLE_WAIT: Duration = Duration::from_secs(60);

/// A universe's combat settings (its config)
pub struct Settings {
    pub round_ms: u64,
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
    match settings(state, universe_id).await.pvp {
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

/// What one blow did, and what each party is told
pub struct Blow {
    pub killed: bool,
    pub damage: Option<i32>,
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
        .and_then(|n| crate::combat::parse_dice(n).ok())
        .filter(|roll| roll.count <= 100 && roll.sides <= 1000);
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
    let damage_roll = match dice {
        Some(roll) => roll.roll().max(0),
        None => rand::rng().random_range(1..=6),
    };
    let outcome = resolve_attack(&a, &d, &mods, d20, damage_roll, dtype, now);
    let statements = attack_statements(universe_id, &target.id, &outcome)?;
    if !statements.is_empty() {
        state.object_store.execute_batch(statements).await?;
    }
    if let Some(ref bleed) = outcome.inflicts {
        state
            .effects
            .cache_add(universe_id, &target.id, bleed.clone())
            .await;
    }

    let (an, tn) = (name_of(attacker), name_of(target));
    let result = &outcome.result;
    let damage = result.damage.as_ref().map(|d| d.final_damage);
    let mut mine = Vec::new();
    let target_text;
    if result.hit {
        let dmg = damage.unwrap_or(0);
        if result.critical {
            mine.push(format!(
                "CRITICAL HIT! You strike {} for {} damage!",
                tn, dmg
            ));
            if outcome.inflicts.is_some() {
                mine.push(format!("{} is bleeding!", tn));
            }
            target_text = format!("{} critically hits you for {} damage!", an, dmg);
        } else {
            mine.push(format!("You hit {} for {} damage.", tn, dmg));
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
        damage: if result.hit { damage } else { None },
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
    if let Err(e) = arrival_inner(state, universe_id, mover, room).await {
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
    let here = store.get_contents(universe_id, room).await?;
    if is_player(&mover) {
        for npc in here.iter().filter(|o| aggressive(o)) {
            if !in_combat(state, universe_id, &npc.id).await? {
                engage_at(state, universe_id, &npc.id, &mover.id, now, next).await?;
            }
        }
    } else if aggressive(&mover) && !in_combat(state, universe_id, &mover.id).await? {
        if let Some(player) = here.iter().find(|o| is_player(o) && alive(o)) {
            engage_at(state, universe_id, &mover.id, &player.id, now, next).await?;
        }
    }
    Ok(())
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
    let (dir, to) = choices.swap_remove(rand::rng().random_range(0..choices.len()));
    store.move_object(universe_id, &who.id, Some(&to)).await?;
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
    Ok(Some(dir))
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
                store.move_object(universe_id, &child.id, spill_to).await?;
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
        store
            .move_object(universe_id, &p.id, room.as_deref())
            .await?;
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
            // For good, with whatever it still holds (fixed things)
            delete_tree(state, universe_id, id, room.as_deref()).await?;
        } else {
            let home = victim
                .get_string("home_room")
                .map(str::to_string)
                .or_else(|| room.clone());
            let mut props = vec![("hp", json!(fighter.max_hp))];
            if let Some(ref home) = home {
                props.push(("home_room", json!(home)));
            }
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
    })
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
        let args = json!({ "object_id": id, "room_id": d.room, "killer_id": d.killer });
        if let Err(e) = super::scripting::invoke_handler(
            state,
            universe_id,
            id,
            d.room.clone(),
            "on_death",
            args,
        )
        .await
        {
            warn!("on_death of {} failed: {}", id, e);
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
        let done = super::scripting::in_run(state, async {
            round(state, now, u.clone(), entity.clone(), &mut mine)
                .await
                .map_err(|e| e.to_string())
        })
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
        // Player against player only while the policy allows it (a flag
        // switched off, the policy changed)
        if !pvp_allowed(state, &u, &attacker, &target).await {
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
        let done = super::scripting::in_run(state, async {
            deadline(state, now, u.clone(), id.clone(), kind.clone(), &mut mine)
                .await
                .map_err(|e| e.to_string())
        })
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
                let home = obj.get_string("home_room").map(str::to_string);
                let home = match home {
                    Some(h) if store.get(&u, &h).await?.is_some() => h,
                    _ => return Ok(()),
                };
                store.move_object(&u, &id, Some(&home)).await?;
                notes.push(Note::Room(
                    u.clone(),
                    home.clone(),
                    format!("{} appears.", name_of(&obj)),
                    None,
                ));
                arrival(state, &u, &id, &home).await;
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
                // Not while fighting, dead or nowhere; within its region
                if !is_player(&obj)
                    && alive(&obj)
                    && obj.parent_id.is_some()
                    && !in_combat(state, &u, &id).await?
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
