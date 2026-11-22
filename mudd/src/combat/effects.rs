//! Status effects system
//!
//! Manages temporary effects on entities like:
//! - Poisoned, stunned, blinded
//! - Buffs and debuffs
//! - Damage over time

use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;
use std::collections::HashMap;
use std::str::FromStr;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{debug, warn};

use super::DamageType;
use crate::objects::ObjectStore;

/// Types of status effects
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum EffectType {
    /// Poisoned - takes damage over time
    Poisoned,
    /// Stunned - cannot act
    Stunned,
    /// Blinded - reduced accuracy
    Blinded,
    /// Burning - takes fire damage over time
    Burning,
    /// Frozen - reduced movement, vulnerable to bludgeoning
    Frozen,
    /// Paralyzed - cannot move or act
    Paralyzed,
    /// Slowed - reduced movement speed
    Slowed,
    /// Hasted - increased movement speed
    Hasted,
    /// Strengthened - increased damage
    Strengthened,
    /// Weakened - reduced damage
    Weakened,
    /// Protected - reduced damage taken
    Protected,
    /// Vulnerable - increased damage taken
    Exposed,
    /// Invisible - harder to hit
    Invisible,
    /// Regenerating - heals over time
    Regenerating,
    /// Silenced - cannot cast spells
    Silenced,
    /// Bleeding - takes physical damage over time (e.g. after a critical hit)
    Bleeding,
}

impl FromStr for EffectType {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "poisoned" | "poison" => Ok(EffectType::Poisoned),
            "stunned" | "stun" => Ok(EffectType::Stunned),
            "blinded" | "blind" => Ok(EffectType::Blinded),
            "burning" | "burn" => Ok(EffectType::Burning),
            "frozen" | "freeze" => Ok(EffectType::Frozen),
            "paralyzed" | "paralyze" => Ok(EffectType::Paralyzed),
            "slowed" | "slow" => Ok(EffectType::Slowed),
            "hasted" | "haste" => Ok(EffectType::Hasted),
            "strengthened" | "strength" => Ok(EffectType::Strengthened),
            "weakened" | "weak" => Ok(EffectType::Weakened),
            "protected" | "protect" => Ok(EffectType::Protected),
            "exposed" | "expose" => Ok(EffectType::Exposed),
            "invisible" | "invis" => Ok(EffectType::Invisible),
            "regenerating" | "regen" => Ok(EffectType::Regenerating),
            "silenced" | "silence" => Ok(EffectType::Silenced),
            "bleeding" | "bleed" => Ok(EffectType::Bleeding),
            _ => Err(()),
        }
    }
}

impl EffectType {
    /// Whether this effect prevents actions
    pub fn prevents_action(&self) -> bool {
        matches!(self, EffectType::Stunned | EffectType::Paralyzed)
    }

    /// Whether this effect is negative (a debuff)
    pub fn is_debuff(&self) -> bool {
        matches!(
            self,
            EffectType::Poisoned
                | EffectType::Stunned
                | EffectType::Blinded
                | EffectType::Burning
                | EffectType::Frozen
                | EffectType::Paralyzed
                | EffectType::Slowed
                | EffectType::Weakened
                | EffectType::Exposed
                | EffectType::Silenced
                | EffectType::Bleeding
        )
    }
}

impl std::fmt::Display for EffectType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            EffectType::Poisoned => "poisoned",
            EffectType::Stunned => "stunned",
            EffectType::Blinded => "blinded",
            EffectType::Burning => "burning",
            EffectType::Frozen => "frozen",
            EffectType::Paralyzed => "paralyzed",
            EffectType::Slowed => "slowed",
            EffectType::Hasted => "hasted",
            EffectType::Strengthened => "strengthened",
            EffectType::Weakened => "weakened",
            EffectType::Protected => "protected",
            EffectType::Exposed => "exposed",
            EffectType::Invisible => "invisible",
            EffectType::Regenerating => "regenerating",
            EffectType::Silenced => "silenced",
            EffectType::Bleeding => "bleeding",
        };
        write!(f, "{}", s)
    }
}

/// Shortest pulse period: each pulse is a replicated write
pub const MIN_PERIOD_MS: u64 = 500;

/// Current time in milliseconds since the Unix epoch. Effect deadlines are
/// absolute times computed on the primary before proposing, so replicas
/// store the same values.
pub fn now_ms() -> u64 {
    chrono::Utc::now().timestamp_millis().max(0) as u64
}

/// A status effect instance. Times are absolute (Unix ms): a plain effect
/// costs no writes while it lasts, and a periodic one (damage or healing
/// over time) one write per pulse.
#[derive(Debug, Clone, PartialEq)]
pub struct StatusEffect {
    /// Type of effect
    pub effect_type: EffectType,
    /// Damage or healing per pulse, or the strength of a buff/debuff
    pub magnitude: i32,
    /// Source object ID (who applied this effect)
    pub source_id: Option<String>,
    /// Damage type for damage over time
    pub damage_type: Option<DamageType>,
    /// When the effect ends
    pub expires_at: u64,
    /// Periodic effects: milliseconds between pulses
    pub period_ms: Option<u64>,
    /// Periodic effects: when the next pulse is due
    pub next_pulse_at: Option<u64>,
}

impl StatusEffect {
    /// A plain effect lasting `duration_ms` from `now`
    pub fn new(effect_type: EffectType, now: u64, duration_ms: u64, magnitude: i32) -> Self {
        Self {
            effect_type,
            magnitude,
            source_id: None,
            damage_type: None,
            expires_at: now.saturating_add(duration_ms),
            period_ms: None,
            next_pulse_at: None,
        }
    }

    /// A periodic effect: a pulse every `period_ms` until it ends (a pulse
    /// due exactly at the end still happens)
    pub fn periodic(
        effect_type: EffectType,
        now: u64,
        duration_ms: u64,
        period_ms: u64,
        magnitude: i32,
        damage_type: Option<DamageType>,
    ) -> Self {
        let period_ms = period_ms.max(MIN_PERIOD_MS);
        Self {
            effect_type,
            magnitude,
            source_id: None,
            damage_type,
            expires_at: now.saturating_add(duration_ms),
            period_ms: Some(period_ms),
            next_pulse_at: Some(now.saturating_add(period_ms)),
        }
    }

    /// Set the source of this effect
    pub fn with_source(mut self, source_id: &str) -> Self {
        self.source_id = Some(source_id.to_string());
        self
    }

    /// Whether the effect is in force at `now`
    pub fn is_active(&self, now: u64) -> bool {
        now < self.expires_at
    }

    /// What one pulse does: damage of a type (positive) or healing
    /// (negative). None for effects that do not pulse.
    pub fn pulse_amount(&self) -> Option<(i32, DamageType)> {
        self.period_ms?;
        // Regenerating always heals, whatever damage type it was given
        if self.effect_type == EffectType::Regenerating {
            return (self.magnitude > 0).then_some((-self.magnitude, DamageType::Physical));
        }
        let dtype = self.damage_type?;
        (self.magnitude > 0).then_some((self.magnitude, dtype))
    }

    /// The next pulse still to come, if any (pulses run up to the end)
    pub fn pending_pulse(&self) -> Option<u64> {
        self.next_pulse_at.filter(|t| *t <= self.expires_at)
    }

    /// When the scheduler next has something to do for this effect: its
    /// next pulse, or else its end
    pub fn next_deadline(&self) -> u64 {
        self.pending_pulse().unwrap_or(self.expires_at)
    }
}

/// Effects on a single entity
#[derive(Debug, Clone, Default)]
pub struct EntityEffects {
    effects: Vec<StatusEffect>,
}

impl EntityEffects {
    /// Create new empty effects
    pub fn new() -> Self {
        Self::default()
    }

    /// Add an effect, or refresh one of the same type: the later end and
    /// the stronger magnitude win; a periodic effect keeps its pulse clock
    pub fn add(&mut self, effect: StatusEffect) {
        if let Some(existing) = self
            .effects
            .iter_mut()
            .find(|e| e.effect_type == effect.effect_type)
        {
            existing.expires_at = existing.expires_at.max(effect.expires_at);
            existing.magnitude = existing.magnitude.max(effect.magnitude);
            if existing.period_ms.is_none() {
                existing.period_ms = effect.period_ms;
                existing.next_pulse_at = effect.next_pulse_at;
            }
            if effect.damage_type.is_some() {
                existing.damage_type = effect.damage_type;
            }
            if effect.source_id.is_some() {
                existing.source_id = effect.source_id;
            }
        } else {
            self.effects.push(effect);
        }
    }

    /// Remove an effect by type
    pub fn remove(&mut self, effect_type: EffectType) {
        self.effects.retain(|e| e.effect_type != effect_type);
    }

    /// Whether the entity has an effect in force at `now`
    pub fn has(&self, effect_type: EffectType, now: u64) -> bool {
        self.get(effect_type, now).is_some()
    }

    /// An effect in force at `now`
    pub fn get(&self, effect_type: EffectType, now: u64) -> Option<&StatusEffect> {
        self.effects
            .iter()
            .find(|e| e.effect_type == effect_type && e.is_active(now))
    }

    /// Whether the entity can act at `now` (not stunned or paralyzed)
    pub fn can_act(&self, now: u64) -> bool {
        !self
            .effects
            .iter()
            .any(|e| e.effect_type.prevents_action() && e.is_active(now))
    }

    /// Every stored effect (including any past its end the scheduler has
    /// not removed yet)
    pub fn all(&self) -> &[StatusEffect] {
        &self.effects
    }

    /// Effects in force at `now`
    pub fn active(&self, now: u64) -> Vec<&StatusEffect> {
        self.effects.iter().filter(|e| e.is_active(now)).collect()
    }

    /// Whether nothing is stored
    pub fn is_empty(&self) -> bool {
        self.effects.is_empty()
    }

    /// The earliest deadline among the effects
    pub fn next_deadline(&self) -> Option<u64> {
        self.effects.iter().map(StatusEffect::next_deadline).min()
    }
}

/// Effects of every entity, a cache of the `active_effects` table (the
/// source of truth, written through Raft), keyed by (universe, entity).
///
/// Writes go to the table first, as statements that can share an atomic
/// batch with other changes (an attack, a pulse); the cache is updated only
/// after the batch commits. `load_from_db` rebuilds the cache at startup.
/// The effect scheduler sleeps until `next_deadline` or until `changed`.
#[derive(Debug, Default)]
pub struct EffectRegistry {
    cache: RwLock<HashMap<EntityKey, EntityEffects>>,
    /// Serializes everything that reads then writes effect rows or HP (the
    /// scheduler, attacks, apply/remove), so none of them overwrites
    /// another's change from a stale read
    writes: tokio::sync::Mutex<()>,
    /// Wakes the scheduler when a new deadline may be earlier
    changed: tokio::sync::Notify,
}

/// (universe_id, entity_id)
pub type EntityKey = (String, String);

/// An active_effects row, in column order
type Row = (
    String,
    String,
    String,
    i64,
    Option<i64>,
    Option<i64>,
    i64,
    Option<String>,
    Option<String>,
);

fn row_to_effect(row: Row) -> Option<(EntityKey, StatusEffect)> {
    let (universe_id, entity_id, effect_type, expires, period, next, magnitude, dtype, source) =
        row;
    let Ok(effect_type) = EffectType::from_str(&effect_type) else {
        warn!("Unknown effect type in database: {}", effect_type);
        return None;
    };
    let effect = StatusEffect {
        effect_type,
        magnitude: magnitude as i32,
        source_id: source,
        damage_type: dtype.and_then(|d| DamageType::from_str(&d).ok()),
        expires_at: expires.max(0) as u64,
        period_ms: period.map(|p| (p.max(0) as u64).max(MIN_PERIOD_MS)),
        next_pulse_at: next.map(|n| n.max(0) as u64),
    };
    Some(((universe_id, entity_id), effect))
}

fn key(universe_id: &str, entity_id: &str) -> EntityKey {
    (universe_id.to_string(), entity_id.to_string())
}

type Statement = (String, Vec<serde_json::Value>);

impl EffectRegistry {
    /// An empty registry
    pub fn new() -> Self {
        Self::default()
    }

    /// Wrap in Arc for sharing
    pub fn shared() -> Arc<Self> {
        Arc::new(Self::new())
    }

    /// Statement adding an effect, or refreshing one of the same type (as
    /// `EntityEffects::add`: later end, stronger magnitude, pulse clock kept)
    pub fn upsert_statement(
        universe_id: &str,
        entity_id: &str,
        effect: &StatusEffect,
    ) -> Statement {
        (
            "INSERT INTO active_effects
               (universe_id, entity_id, effect_type, expires_at, period_ms, next_pulse_at,
                magnitude, damage_type, source_id)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)
             ON CONFLICT (universe_id, entity_id, effect_type) DO UPDATE SET
               expires_at = max(active_effects.expires_at, excluded.expires_at),
               magnitude = max(active_effects.magnitude, excluded.magnitude),
               period_ms = coalesce(active_effects.period_ms, excluded.period_ms),
               next_pulse_at = coalesce(active_effects.next_pulse_at, excluded.next_pulse_at),
               damage_type = coalesce(excluded.damage_type, active_effects.damage_type),
               source_id = coalesce(excluded.source_id, active_effects.source_id)"
                .to_string(),
            vec![
                serde_json::json!(universe_id),
                serde_json::json!(entity_id),
                serde_json::json!(effect.effect_type.to_string()),
                serde_json::json!(effect.expires_at),
                serde_json::json!(effect.period_ms),
                serde_json::json!(effect.next_pulse_at),
                serde_json::json!(effect.magnitude),
                serde_json::json!(effect.damage_type.map(|d| d.to_string())),
                serde_json::json!(effect.source_id),
            ],
        )
    }

    /// Statement moving a periodic effect's next pulse (after pulsing)
    pub fn set_next_pulse_statement(
        universe_id: &str,
        entity_id: &str,
        effect_type: EffectType,
        next_pulse_at: u64,
    ) -> Statement {
        (
            "UPDATE active_effects SET next_pulse_at = ?
             WHERE universe_id = ? AND entity_id = ? AND effect_type = ?"
                .to_string(),
            vec![
                serde_json::json!(next_pulse_at),
                serde_json::json!(universe_id),
                serde_json::json!(entity_id),
                serde_json::json!(effect_type.to_string()),
            ],
        )
    }

    /// Statement removing one effect
    pub fn delete_statement(
        universe_id: &str,
        entity_id: &str,
        effect_type: EffectType,
    ) -> Statement {
        (
            "DELETE FROM active_effects WHERE universe_id = ? AND entity_id = ? AND effect_type = ?"
                .to_string(),
            vec![
                serde_json::json!(universe_id),
                serde_json::json!(entity_id),
                serde_json::json!(effect_type.to_string()),
            ],
        )
    }

    /// Statement removing all of an entity's effects (e.g. on death)
    pub fn clear_statement(universe_id: &str, entity_id: &str) -> Statement {
        (
            "DELETE FROM active_effects WHERE universe_id = ? AND entity_id = ?".to_string(),
            vec![serde_json::json!(universe_id), serde_json::json!(entity_id)],
        )
    }

    /// Hold while reading then writing effect rows or HP (see `writes`)
    pub async fn write_guard(&self) -> tokio::sync::MutexGuard<'_, ()> {
        self.writes.lock().await
    }

    /// Add (or refresh) an effect: write it, then cache it
    pub async fn apply(
        &self,
        store: &ObjectStore,
        universe_id: &str,
        entity_id: &str,
        effect: StatusEffect,
    ) -> anyhow::Result<()> {
        let _writes = self.write_guard().await;
        store
            .execute_batch(vec![Self::upsert_statement(
                universe_id,
                entity_id,
                &effect,
            )])
            .await?;
        self.cache_add(universe_id, entity_id, effect).await;
        Ok(())
    }

    /// Remove an effect: write it, then uncache it
    pub async fn remove(
        &self,
        store: &ObjectStore,
        universe_id: &str,
        entity_id: &str,
        effect_type: EffectType,
    ) -> anyhow::Result<()> {
        let _writes = self.write_guard().await;
        store
            .execute_batch(vec![Self::delete_statement(
                universe_id,
                entity_id,
                effect_type,
            )])
            .await?;
        self.cache_remove(universe_id, entity_id, effect_type).await;
        Ok(())
    }

    /// After a committed upsert: cache the effect (and wake the scheduler)
    pub async fn cache_add(&self, universe_id: &str, entity_id: &str, effect: StatusEffect) {
        self.cache
            .write()
            .await
            .entry(key(universe_id, entity_id))
            .or_default()
            .add(effect);
        self.changed.notify_one();
    }

    /// After a committed delete: uncache the effect
    pub async fn cache_remove(&self, universe_id: &str, entity_id: &str, effect_type: EffectType) {
        let mut cache = self.cache.write().await;
        let k = key(universe_id, entity_id);
        if let Some(effects) = cache.get_mut(&k) {
            effects.remove(effect_type);
            if effects.is_empty() {
                cache.remove(&k);
            }
        }
    }

    /// After a committed clear (or when the entity is gone): uncache all
    pub async fn cache_clear(&self, universe_id: &str, entity_id: &str) {
        self.cache
            .write()
            .await
            .remove(&key(universe_id, entity_id));
    }

    /// After a committed scheduler batch: replace an entity's effects
    pub async fn cache_set(&self, universe_id: &str, entity_id: &str, effects: EntityEffects) {
        let mut cache = self.cache.write().await;
        if effects.is_empty() {
            cache.remove(&key(universe_id, entity_id));
        } else {
            cache.insert(key(universe_id, entity_id), effects);
        }
    }

    /// An entity's stored effects (empty if none)
    pub async fn get(&self, universe_id: &str, entity_id: &str) -> EntityEffects {
        self.cache
            .read()
            .await
            .get(&key(universe_id, entity_id))
            .cloned()
            .unwrap_or_default()
    }

    /// The earliest deadline of any effect, if there are any
    pub async fn next_deadline(&self) -> Option<u64> {
        self.cache
            .read()
            .await
            .values()
            .filter_map(EntityEffects::next_deadline)
            .min()
    }

    /// Entities with a deadline at or before `now`, with their effects
    pub async fn due(&self, now: u64) -> Vec<(EntityKey, EntityEffects)> {
        self.cache
            .read()
            .await
            .iter()
            .filter(|(_, e)| e.next_deadline().is_some_and(|t| t <= now))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect()
    }

    /// Resolves when effects change (a new effect may be due sooner)
    pub async fn changed(&self) {
        self.changed.notified().await
    }

    /// One entity's effects as stored in the table (the source of truth)
    pub async fn load_entity(
        pool: &SqlitePool,
        universe_id: &str,
        entity_id: &str,
    ) -> anyhow::Result<EntityEffects> {
        let rows: Vec<Row> = sqlx::query_as(
            "SELECT universe_id, entity_id, effect_type, expires_at, period_ms, next_pulse_at,
                    magnitude, damage_type, source_id
             FROM active_effects WHERE universe_id = ? AND entity_id = ?",
        )
        .bind(universe_id)
        .bind(entity_id)
        .fetch_all(&mut *crate::db::run::reader(pool).await?)
        .await?;
        let mut effects = EntityEffects::new();
        for row in rows {
            if let Some((_, effect)) = row_to_effect(row) {
                effects.add(effect);
            }
        }
        Ok(effects)
    }

    /// Rebuild the cache from the table (at startup)
    pub async fn load_from_db(&self, pool: &SqlitePool) -> anyhow::Result<()> {
        let rows: Vec<Row> = sqlx::query_as(
            "SELECT universe_id, entity_id, effect_type, expires_at, period_ms, next_pulse_at,
                    magnitude, damage_type, source_id
             FROM active_effects",
        )
        .fetch_all(&mut *crate::db::run::reader(pool).await?)
        .await?;

        let mut cache = HashMap::<EntityKey, EntityEffects>::new();
        for row in rows {
            if let Some((key, effect)) = row_to_effect(row) {
                cache.entry(key).or_default().add(effect);
            }
        }
        debug!("Loaded effects for {} entities", cache.len());
        *self.cache.write().await = cache;
        self.changed.notify_one();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_effect_type_parsing() {
        assert_eq!("poisoned".parse::<EffectType>(), Ok(EffectType::Poisoned));
        assert_eq!("STUN".parse::<EffectType>(), Ok(EffectType::Stunned));
        assert!("invalid".parse::<EffectType>().is_err());
    }

    #[test]
    fn test_effect_prevents_action() {
        assert!(EffectType::Stunned.prevents_action());
        assert!(EffectType::Paralyzed.prevents_action());
        assert!(!EffectType::Poisoned.prevents_action());
        assert!(!EffectType::Blinded.prevents_action());
    }

    #[test]
    fn test_plain_effect_lasts_until_its_end() {
        let stun = StatusEffect::new(EffectType::Stunned, 1_000, 2_000, 0);
        let mut effects = EntityEffects::new();
        effects.add(stun);
        assert!(!effects.can_act(1_500));
        assert!(effects.has(EffectType::Stunned, 2_999));
        assert!(effects.can_act(3_000), "over at its end time");
        assert_eq!(effects.next_deadline(), Some(3_000));
        assert_eq!(
            StatusEffect::new(EffectType::Stunned, 0, 1, 0).pulse_amount(),
            None
        );
    }

    #[test]
    fn test_periodic_effect_deadlines() {
        let poison = StatusEffect::periodic(
            EffectType::Poisoned,
            0,
            3_000,
            1_000,
            2,
            Some(DamageType::Poison),
        );
        assert_eq!(poison.next_deadline(), 1_000, "first pulse");
        assert_eq!(poison.pulse_amount(), Some((2, DamageType::Poison)));
        let regen = StatusEffect::periodic(EffectType::Regenerating, 0, 3_000, 1_000, 4, None);
        assert_eq!(regen.pulse_amount(), Some((-4, DamageType::Physical)));
        // Past the last pulse, the deadline is the end
        let mut late = poison.clone();
        late.next_pulse_at = Some(4_000);
        assert_eq!(late.next_deadline(), 3_000);
    }

    #[test]
    fn test_effect_refresh() {
        let mut effects = EntityEffects::new();
        effects.add(StatusEffect::new(EffectType::Stunned, 0, 2_000, 0));
        effects.add(StatusEffect::new(EffectType::Stunned, 0, 5_000, 1));
        let stun = effects.get(EffectType::Stunned, 0).unwrap();
        assert_eq!((stun.expires_at, stun.magnitude), (5_000, 1));

        // Refreshing a periodic effect keeps its pulse clock
        effects.add(StatusEffect::periodic(
            EffectType::Burning,
            0,
            2_000,
            500,
            3,
            Some(DamageType::Fire),
        ));
        effects.add(StatusEffect::periodic(
            EffectType::Burning,
            400,
            4_000,
            500,
            1,
            Some(DamageType::Fire),
        ));
        let burn = effects.get(EffectType::Burning, 0).unwrap();
        assert_eq!(
            (burn.expires_at, burn.magnitude, burn.next_pulse_at),
            (4_400, 3, Some(500))
        );
    }

    async fn universe_with(pool: &SqlitePool, universe: &str, objects: &[&str]) -> ObjectStore {
        sqlx::query(
            "INSERT OR IGNORE INTO accounts (id, username, password_hash, access_level, created_at)
             VALUES ('acct', 'acct', 'x', 'admin', '2026-01-01')",
        )
        .execute(pool)
        .await
        .unwrap();
        let store = ObjectStore::new(pool.clone(), None);
        store
            .create_universe(universe, universe, "acct", serde_json::json!({}))
            .await
            .unwrap();
        for id in objects {
            let obj = crate::objects::Object::new(id, universe, "npc").unwrap();
            store.create(&obj).await.unwrap();
        }
        store
    }

    /// The table is the truth: effects written through the registry come
    /// back in a fresh registry loaded from it, per universe
    #[tokio::test]
    async fn test_effect_registry_persists_per_universe() {
        let pool = crate::db::test_utils::test_pool().await;
        let store = universe_with(&pool, "a", &["/npcs/rat"]).await;
        universe_with(&pool, "b", &["/npcs/rat"]).await;
        let registry = EffectRegistry::new();

        let now = 1_000_000;
        registry
            .apply(
                &store,
                "a",
                "/npcs/rat",
                StatusEffect::new(EffectType::Stunned, now, 2_000, 0),
            )
            .await
            .unwrap();
        // Refreshing keeps the later end, the stronger magnitude and the
        // pulse clock
        let poison = |start, duration, magnitude| {
            StatusEffect::periodic(
                EffectType::Poisoned,
                start,
                duration,
                1_000,
                magnitude,
                Some(DamageType::Poison),
            )
        };
        registry
            .apply(&store, "a", "/npcs/rat", poison(now, 3_000, 2))
            .await
            .unwrap();
        registry
            .apply(&store, "a", "/npcs/rat", poison(now + 500, 1_000, 5))
            .await
            .unwrap();
        assert!(!registry.get("a", "/npcs/rat").await.can_act(now));
        assert!(
            registry.get("b", "/npcs/rat").await.can_act(now),
            "per universe"
        );
        assert_eq!(registry.next_deadline().await, Some(now + 1_000));

        let fresh = EffectRegistry::new();
        fresh.load_from_db(&pool).await.unwrap();
        let effects = fresh.get("a", "/npcs/rat").await;
        let poison = effects
            .get(EffectType::Poisoned, now)
            .expect("poison persisted");
        assert_eq!(
            (poison.expires_at, poison.magnitude, poison.next_pulse_at),
            (now + 3_000, 5, Some(now + 1_000))
        );
        assert_eq!(poison.damage_type, Some(DamageType::Poison));
        assert!(effects.has(EffectType::Stunned, now));
        assert!(fresh.get("b", "/npcs/rat").await.is_empty());

        registry
            .remove(&store, "a", "/npcs/rat", EffectType::Stunned)
            .await
            .unwrap();
        assert!(registry.get("a", "/npcs/rat").await.can_act(now));
        let fresh = EffectRegistry::new();
        fresh.load_from_db(&pool).await.unwrap();
        assert!(!fresh
            .get("a", "/npcs/rat")
            .await
            .has(EffectType::Stunned, now));
    }

    /// Effects belong to an object: deleting it deletes them
    #[tokio::test]
    async fn test_effects_cascade_with_object() {
        let pool = crate::db::test_utils::test_pool().await;
        let store = universe_with(&pool, "a", &["/npcs/rat"]).await;
        EffectRegistry::new()
            .apply(
                &store,
                "a",
                "/npcs/rat",
                StatusEffect::new(EffectType::Stunned, 0, 2_000, 0),
            )
            .await
            .unwrap();
        store.delete("a", "/npcs/rat").await.unwrap();
        let left: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM active_effects")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(left, 0);
    }
}
