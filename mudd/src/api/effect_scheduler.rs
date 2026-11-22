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
    is_player: bool,
    outcome: TickOutcome,
    /// Its death (already handled in the run), killed by the source of the
    /// last effect that dealt damage
    death: Option<super::combat::Death>,
}

/// Commit everything due at `now`
pub async fn process(state: &AppState, now: u64) {
    let mut had_errors = false;
    // One run: everything due commits as one batch (hooks and messages
    // follow the commit)
    let processed = super::scripting::in_run(state, async {
        // Serializes against attacks and effect writes, which also read
        // and write HP and effect rows. Released before reporting: hooks
        // that run then may write effects themselves.
        let _writes = state.effects.write_guard().await;
        let due = state.effects.due(now).await;
        if due.is_empty() {
            return Ok(Vec::new());
        }

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
            let entity_statements =
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
            // Written now (in this run), so a death below works from them
            state
                .object_store
                .execute_batch(entity_statements)
                .await
                .map_err(|e| e.to_string())?;
            // A death follows the universe's policy, as for any death
            let death = if outcome.killed {
                let killer_obj = match killer {
                    Some(ref k) => state.object_store.get(&universe, k).await.ok().flatten(),
                    None => None,
                };
                let dead = state
                    .object_store
                    .get(&universe, &entity)
                    .await
                    .map_err(|e| e.to_string())?
                    .unwrap_or_else(|| obj.clone());
                Some(
                    super::combat::death(state, &universe, &dead, killer_obj.as_ref(), now)
                        .await
                        .map_err(|e| e.to_string())?,
                )
            } else {
                None
            };
            processed.push(Processed {
                universe,
                entity,
                is_player,
                outcome,
                death,
            });
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
        Ok::<_, String>(processed)
    })
    .await;
    let processed = match processed {
        Ok(processed) => processed,
        Err(e) => {
            warn!("due effects not committed (retrying): {}", e);
            tokio::time::sleep(Duration::from_millis(500)).await;
            return;
        }
    };

    for p in &processed {
        report(state, p).await;
    }
    // Entities skipped for an error stay due: back off rather than spin
    if had_errors {
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
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
    if let Some(ref death) = p.death {
        super::combat::report_death(state, universe, death).await;
    }
}
