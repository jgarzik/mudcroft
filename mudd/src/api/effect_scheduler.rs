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

/// Commit everything due at `now`
pub async fn process(state: &AppState, now: u64) {
    // Serializes against attacks and effect writes, which also read and
    // write HP and effect rows
    let _writes = state.effects.write_guard().await;
    let due = state.effects.due(now).await;
    if due.is_empty() {
        return;
    }

    let mut statements = Vec::new();
    let mut processed: Vec<(String, String, Object, bool, TickOutcome)> = Vec::new();
    for ((universe, entity), _) in due {
        // Work from the table, the source of truth, not the cache: the
        // object may have been deleted (rows cascaded) and even recreated
        let effects =
            match crate::combat::EffectRegistry::load_entity(state.db.pool(), &universe, &entity)
                .await
            {
                Ok(effects) if effects.is_empty() => {
                    state.effects.cache_clear(&universe, &entity).await;
                    continue;
                }
                Ok(effects) => effects,
                Err(e) => {
                    warn!("effects: loading {} failed: {}", entity, e);
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
                continue;
            }
        };
        let is_player = obj.class == "player";
        let target = Combatant::from_object(&obj, if is_player { 100 } else { 10 });
        let outcome = process_due(&target, &effects, now);
        match tick_statements(&universe, &entity, is_player, &effects, &outcome) {
            Ok(s) => statements.extend(s),
            Err(e) => {
                warn!("effects: {}: {}", entity, e);
                continue;
            }
        }
        processed.push((universe, entity, obj, is_player, outcome));
    }
    if !statements.is_empty() {
        if let Err(e) = state.object_store.execute_batch(statements).await {
            warn!("due effects not committed (retrying): {}", e);
            // Back off briefly so a persistent failure does not spin
            tokio::time::sleep(Duration::from_millis(500)).await;
            return;
        }
    }

    for (universe, entity, obj, is_player, outcome) in processed {
        if outcome.killed {
            state.effects.cache_clear(&universe, &entity).await;
        } else {
            state
                .effects
                .cache_set(&universe, &entity, outcome.effects.clone())
                .await;
        }
        report(state, &universe, &entity, &obj, is_player, &outcome).await;
    }
}

/// Tell a player what their effects did; announce deaths
async fn report(
    state: &AppState,
    universe: &str,
    entity: &str,
    obj: &Object,
    is_player: bool,
    outcome: &TickOutcome,
) {
    if is_player {
        for event in &outcome.events {
            let text = match event {
                TickEvent::Damage {
                    effect,
                    amount,
                    dtype,
                } => format!("You take {} {} damage ({}).", amount, dtype, effect),
                TickEvent::Heal { amount } if *amount > 0 => {
                    format!("You regenerate {} HP.", amount)
                }
                TickEvent::Heal { .. } => continue,
                TickEvent::Expired(effect) => format!("You are no longer {}.", effect),
            };
            state
                .connections
                .send_to_player(universe, entity, ServerMessage::Output { text })
                .await;
        }
    }
    if !outcome.killed {
        return;
    }
    let name = obj
        .properties
        .get("name")
        .and_then(|v| v.as_str())
        .unwrap_or("Something")
        .to_string();
    if let Some(ref room) = obj.parent_id {
        state
            .connections
            .broadcast_room_except(
                universe,
                room,
                is_player.then_some(entity),
                ServerMessage::Output {
                    text: format!("{} dies.", name),
                },
            )
            .await;
    }
    if is_player {
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
        match state.player_manager.handle_death(entity, universe).await {
            Ok(respawn) => {
                state
                    .connections
                    .update_room(universe, entity, Some(respawn))
                    .await;
            }
            Err(e) => warn!("respawn of {} failed: {}", entity, e),
        }
    }
}
