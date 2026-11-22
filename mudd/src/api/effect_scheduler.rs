//! The effect scheduler: deadline-driven, not a periodic sweep
//!
//! Effects store absolute deadlines (`expires_at`, `next_pulse_at`). The
//! scheduler sleeps until the earliest one (or until effects change), then
//! commits everything due as **one atomic batch**: pulses (HP), pulse
//! clocks, ended effects and deaths. A plain effect such as a stun costs no
//! writes until it ends; a pulsing one costs one per pulse. With nothing
//! due, nothing runs. Caches and messages follow only after the commit; if
//! it fails, nothing changed and the next wake retries.

use std::time::Duration;

use tracing::warn;

use super::{AppState, ServerMessage};
use crate::combat::engine::{process_due, tick_statements, Combatant, TickEvent, TickOutcome};
use crate::combat::now_ms;
use crate::objects::Object;

/// Longest sleep with nothing due (a safety net; changes wake it sooner)
const IDLE_WAIT: Duration = Duration::from_secs(60);

/// Process due effects forever
pub async fn run_effect_scheduler(state: AppState) {
    loop {
        // Register for change notification before reading the deadline,
        // so an effect applied in between is not missed
        let changed = state.effects.changed();
        tokio::pin!(changed);
        let now = now_ms();
        let wait = match state.effects.next_deadline().await {
            Some(due) if due <= now => Duration::ZERO,
            Some(due) => Duration::from_millis(due - now).min(IDLE_WAIT),
            None => IDLE_WAIT,
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

/// What one processed entity needs reported after the commit
struct Processed {
    universe: String,
    entity: String,
    obj: Object,
    is_player: bool,
    outcome: TickOutcome,
    /// Who caused a death: the source of the last effect that dealt damage
    killer: Option<String>,
    /// Where a player who died respawns (already committed)
    respawn: Option<String>,
}

/// Commit everything due at `now`
pub async fn process(state: &AppState, now: u64) {
    let mut had_errors = false;
    let processed = {
        // Serializes against attacks and effect writes, which also read
        // and write HP and effect rows. Released before reporting: hooks
        // that run then may write effects themselves.
        let _writes = state.effects.write_guard().await;
        let due = state.effects.due(now).await;
        if due.is_empty() {
            return;
        }

        let mut statements = Vec::new();
        let mut processed = Vec::new();
        for ((universe, entity), _) in due {
            // Work from the table, the source of truth, not the cache: the
            // object may have been deleted (rows cascaded) and even recreated
            let effects = match crate::combat::EffectRegistry::load_entity(
                state.db.pool(),
                &universe,
                &entity,
            )
            .await
            {
                Ok(effects) if effects.is_empty() => {
                    state.effects.cache_clear(&universe, &entity).await;
                    continue;
                }
                Ok(effects) => effects,
                Err(e) => {
                    warn!("effects: loading {} failed: {}", entity, e);
                    had_errors = true;
                    continue;
                }
            };
            let obj = match state.object_store.get(&universe, &entity).await {
                Ok(Some(obj)) => obj,
                Ok(None) => {
                    // The object is gone, and its rows with it
                    state.effects.cache_clear(&universe, &entity).await;
                    continue;
                }
                Err(e) => {
                    warn!("effects: lookup of {} failed: {}", entity, e);
                    had_errors = true;
                    continue;
                }
            };
            let is_player = obj.class == "player";
            let target = Combatant::from_object(&obj, if is_player { 100 } else { 10 });
            let outcome = process_due(&target, &effects, now);
            let mut entity_statements =
                match tick_statements(&universe, &entity, is_player, &effects, &outcome) {
                    Ok(s) => s,
                    Err(e) => {
                        warn!("effects: {}: {}", entity, e);
                        had_errors = true;
                        continue;
                    }
                };
            let killer = outcome.events.iter().rev().find_map(|event| match event {
                TickEvent::Damage { effect, .. } => effects
                    .all()
                    .iter()
                    .find(|e| e.effect_type == *effect)
                    .and_then(|e| e.source_id.clone()),
                _ => None,
            });
            // A player who dies drops what they carry and respawns in the
            // same batch: HP restored and moved (or, with no respawn room
            // yet, revived where they fell), never left dead at 0 HP
            let mut respawn = None;
            if outcome.killed && is_player {
                let found = state.player_manager.respawn_room(&obj, &universe).await;
                let drops = match obj.parent_id.as_deref() {
                    Some(room) => {
                        state
                            .player_manager
                            .drop_statements(&universe, &entity, room)
                            .await
                    }
                    None => Ok(Vec::new()),
                };
                match (found, drops) {
                    (Ok(room), Ok(drops)) => {
                        let room = room.or_else(|| {
                            warn!("effects: no respawn room for {}; reviving in place", entity);
                            obj.parent_id.clone()
                        });
                        entity_statements.extend(drops);
                        entity_statements.extend(respawn_statements(
                            &universe,
                            &entity,
                            &target,
                            room.as_deref(),
                        ));
                        respawn = room;
                    }
                    (Err(e), _) | (_, Err(e)) => {
                        warn!("effects: respawn of {} not prepared: {}", entity, e);
                        had_errors = true;
                        continue;
                    }
                }
            }
            statements.extend(entity_statements);
            processed.push(Processed {
                universe,
                entity,
                obj,
                is_player,
                outcome,
                killer,
                respawn,
            });
        }
        if !statements.is_empty() {
            if let Err(e) = state.object_store.execute_batch(statements).await {
                warn!("due effects not committed (retrying): {}", e);
                drop(_writes);
                tokio::time::sleep(Duration::from_millis(500)).await;
                return;
            }
        }
        for p in &processed {
            if p.outcome.killed {
                state.effects.cache_clear(&p.universe, &p.entity).await;
            } else {
                state
                    .effects
                    .cache_set(&p.universe, &p.entity, p.outcome.effects.clone())
                    .await;
            }
        }
        processed
    };

    for p in &processed {
        report(state, p).await;
    }
    // Entities skipped for an error stay due: back off rather than spin
    if had_errors {
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

/// Statements restoring a dead player: full HP, at the respawn room
fn respawn_statements(
    universe: &str,
    entity: &str,
    target: &Combatant,
    room: Option<&str>,
) -> Vec<(String, Vec<serde_json::Value>)> {
    let mut statements = Vec::new();
    if let Ok(hp) = crate::objects::ObjectStore::set_properties_statement(
        universe,
        entity,
        &[("hp", serde_json::json!(target.max_hp))],
    ) {
        statements.push(hp);
    }
    if let Some(room) = room {
        statements.push(crate::objects::ObjectStore::set_parent_statement(
            universe,
            entity,
            Some(room),
        ));
    }
    statements
}

/// Tell a player what their effects did; announce and handle deaths
async fn report(state: &AppState, p: &Processed) {
    let (universe, entity) = (p.universe.as_str(), p.entity.as_str());
    if p.is_player {
        for event in &p.outcome.events {
            let text = match event {
                TickEvent::Damage {
                    effect,
                    amount,
                    dtype,
                } => format!("You take {} {} damage ({}).", amount, dtype, effect),
                TickEvent::Heal { amount } => format!("You regenerate {} HP.", amount),
                TickEvent::Expired(effect) => format!("You are no longer {}.", effect),
            };
            state
                .connections
                .send_to_player(universe, entity, ServerMessage::Output { text })
                .await;
        }
    }
    if !p.outcome.killed {
        return;
    }
    let name = p
        .obj
        .properties
        .get("name")
        .and_then(|v| v.as_str())
        .unwrap_or("Something")
        .to_string();
    let death_room = p.obj.parent_id.clone();
    if let Some(ref room) = death_room {
        state
            .connections
            .broadcast_room_except(
                universe,
                room,
                p.is_player.then_some(entity),
                ServerMessage::Output {
                    text: format!("{} dies.", name),
                },
            )
            .await;
    }

    if !p.is_player {
        // As for a kill by attack: the NPC's on_death runs (as its owner)
        let args = serde_json::json!({
            "object_id": entity,
            "room_id": death_room,
            "killer_id": p.killer,
        });
        if let Err(e) = super::scripting::invoke_handler(
            state,
            universe,
            entity,
            death_room.clone(),
            "on_death",
            args,
        )
        .await
        {
            warn!("on_death of {} failed: {}", entity, e);
        }
        return;
    }

    // A player: inventory dropped and respawned (committed above). Show them
    // where they are.
    state
        .connections
        .send_to_player(
            universe,
            entity,
            ServerMessage::Output {
                text: "You have died.".to_string(),
            },
        )
        .await;
    if let Some(ref respawn) = p.respawn {
        state
            .connections
            .update_room(universe, entity, Some(respawn.clone()))
            .await;
        if let Some(room_msg) =
            super::websocket::build_room_message(state, universe, respawn, None).await
        {
            state
                .connections
                .send_to_player(universe, entity, room_msg)
                .await;
        }
    }
}
