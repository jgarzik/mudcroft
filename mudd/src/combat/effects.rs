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

/// A status effect instance
#[derive(Debug, Clone)]
pub struct StatusEffect {
    /// Type of effect
    pub effect_type: EffectType,
    /// Remaining duration in ticks (heartbeats)
    pub remaining_ticks: u32,
    /// Magnitude/power of the effect (e.g., damage per tick)
    pub magnitude: i32,
    /// Source object ID (who applied this effect)
    pub source_id: Option<String>,
    /// Damage type for DoT effects
    pub damage_type: Option<DamageType>,
}

impl StatusEffect {
    /// Create a new status effect
    pub fn new(effect_type: EffectType, duration_ticks: u32, magnitude: i32) -> Self {
        Self {
            effect_type,
            remaining_ticks: duration_ticks,
            magnitude,
            source_id: None,
            damage_type: None,
        }
    }

    /// Create a DoT (damage over time) effect
    pub fn dot(effect_type: EffectType, duration: u32, damage: i32, dtype: DamageType) -> Self {
        Self {
            effect_type,
            remaining_ticks: duration,
            magnitude: damage,
            source_id: None,
            damage_type: Some(dtype),
        }
    }

    /// Set the source of this effect
    pub fn with_source(mut self, source_id: &str) -> Self {
        self.source_id = Some(source_id.to_string());
        self
    }

    /// Tick the effect, returning damage dealt if applicable
    pub fn tick(&mut self) -> Option<(i32, DamageType)> {
        if self.remaining_ticks > 0 {
            self.remaining_ticks -= 1;
        }

        // Return damage for DoT effects
        if self.magnitude > 0 {
            if let Some(dtype) = self.damage_type {
                return Some((self.magnitude, dtype));
            }
        }

        // Return healing for regen
        if self.effect_type == EffectType::Regenerating && self.magnitude > 0 {
            return Some((-self.magnitude, DamageType::Physical)); // Negative = healing
        }

        None
    }

    /// Check if effect has expired
    pub fn is_expired(&self) -> bool {
        self.remaining_ticks == 0
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

    /// Add an effect (stacks or refreshes based on type)
    pub fn add(&mut self, effect: StatusEffect) {
        // For most effects, just refresh duration if already present
        if let Some(existing) = self
            .effects
            .iter_mut()
            .find(|e| e.effect_type == effect.effect_type)
        {
            existing.remaining_ticks = existing.remaining_ticks.max(effect.remaining_ticks);
            existing.magnitude = existing.magnitude.max(effect.magnitude);
        } else {
            self.effects.push(effect);
        }
    }

    /// Remove an effect by type
    pub fn remove(&mut self, effect_type: EffectType) {
        self.effects.retain(|e| e.effect_type != effect_type);
    }

    /// Check if entity has a specific effect
    pub fn has(&self, effect_type: EffectType) -> bool {
        self.effects
            .iter()
            .any(|e| e.effect_type == effect_type && !e.is_expired())
    }

    /// Get an effect if present
    pub fn get(&self, effect_type: EffectType) -> Option<&StatusEffect> {
        self.effects
            .iter()
            .find(|e| e.effect_type == effect_type && !e.is_expired())
    }

    /// Check if entity can act (not stunned/paralyzed)
    pub fn can_act(&self) -> bool {
        !self
            .effects
            .iter()
            .any(|e| e.effect_type.prevents_action() && !e.is_expired())
    }

    /// Tick all effects and return damage/healing to apply
    pub fn tick_all(&mut self) -> Vec<(i32, DamageType)> {
        let mut results = Vec::new();

        for effect in &mut self.effects {
            if let Some(result) = effect.tick() {
                results.push(result);
            }
        }

        // Remove expired effects
        self.effects.retain(|e| !e.is_expired());

        results
    }

    /// Get all active effects
    pub fn active_effects(&self) -> Vec<&StatusEffect> {
        self.effects.iter().filter(|e| !e.is_expired()).collect()
    }

    /// Clear all effects
    pub fn clear(&mut self) {
        self.effects.clear();
    }

    /// Clear only debuffs
    pub fn clear_debuffs(&mut self) {
        self.effects.retain(|e| !e.effect_type.is_debuff());
    }
}

/// Effects of every entity, a cache of the `active_effects` table (the
/// source of truth, written through Raft), keyed by (universe, entity).
///
/// Writes go to the table first, as statements that can share an atomic
/// batch with other changes (an attack, a tick); the cache is updated only
/// after the batch commits. `load_from_db` rebuilds the cache at startup.
#[derive(Debug, Default)]
pub struct EffectRegistry {
    cache: RwLock<HashMap<EntityKey, EntityEffects>>,
}

/// (universe_id, entity_id)
pub type EntityKey = (String, String);

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

    /// Statement adding an effect, or refreshing one of the same type to
    /// the longer duration and stronger magnitude (as `EntityEffects::add`)
    pub fn upsert_statement(
        universe_id: &str,
        entity_id: &str,
        effect: &StatusEffect,
    ) -> Statement {
        (
            "INSERT INTO active_effects
               (universe_id, entity_id, effect_type, remaining_ticks, magnitude, damage_type, source_id)
             VALUES (?, ?, ?, ?, ?, ?, ?)
             ON CONFLICT (universe_id, entity_id, effect_type) DO UPDATE SET
               remaining_ticks = max(active_effects.remaining_ticks, excluded.remaining_ticks),
               magnitude = max(active_effects.magnitude, excluded.magnitude),
               damage_type = excluded.damage_type,
               source_id = excluded.source_id"
                .to_string(),
            vec![
                serde_json::json!(universe_id),
                serde_json::json!(entity_id),
                serde_json::json!(effect.effect_type.to_string()),
                serde_json::json!(effect.remaining_ticks),
                serde_json::json!(effect.magnitude),
                serde_json::json!(effect.damage_type.map(|d| d.to_string())),
                serde_json::json!(effect.source_id),
            ],
        )
    }

    /// Statement setting an effect's remaining ticks (after a tick)
    pub fn set_remaining_statement(
        universe_id: &str,
        entity_id: &str,
        effect_type: EffectType,
        remaining_ticks: u32,
    ) -> Statement {
        (
            "UPDATE active_effects SET remaining_ticks = ?
             WHERE universe_id = ? AND entity_id = ? AND effect_type = ?"
                .to_string(),
            vec![
                serde_json::json!(remaining_ticks),
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

    /// Add (or refresh) an effect: write it, then cache it
    pub async fn apply(
        &self,
        store: &ObjectStore,
        universe_id: &str,
        entity_id: &str,
        effect: StatusEffect,
    ) -> anyhow::Result<()> {
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

    /// After a committed upsert: cache the effect
    pub async fn cache_add(&self, universe_id: &str, entity_id: &str, effect: StatusEffect) {
        self.cache
            .write()
            .await
            .entry(key(universe_id, entity_id))
            .or_default()
            .add(effect);
    }

    /// After a committed delete: uncache the effect
    pub async fn cache_remove(&self, universe_id: &str, entity_id: &str, effect_type: EffectType) {
        let mut cache = self.cache.write().await;
        let k = key(universe_id, entity_id);
        if let Some(effects) = cache.get_mut(&k) {
            effects.remove(effect_type);
            if effects.active_effects().is_empty() {
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

    /// After a committed tick: replace an entity's cached effects
    pub async fn cache_set(&self, universe_id: &str, entity_id: &str, effects: EntityEffects) {
        let mut cache = self.cache.write().await;
        if effects.active_effects().is_empty() {
            cache.remove(&key(universe_id, entity_id));
        } else {
            cache.insert(key(universe_id, entity_id), effects);
        }
    }

    /// An entity's current effects (empty if none)
    pub async fn get(&self, universe_id: &str, entity_id: &str) -> EntityEffects {
        self.cache
            .read()
            .await
            .get(&key(universe_id, entity_id))
            .cloned()
            .unwrap_or_default()
    }

    /// Every entity that has effects, with them (for the tick)
    pub async fn snapshot(&self) -> Vec<(EntityKey, EntityEffects)> {
        self.cache
            .read()
            .await
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect()
    }

    /// Rebuild the cache from the table (at startup)
    pub async fn load_from_db(&self, pool: &SqlitePool) -> anyhow::Result<()> {
        type Row = (
            String,
            String,
            String,
            i64,
            i64,
            Option<String>,
            Option<String>,
        );
        let rows: Vec<Row> = sqlx::query_as(
            "SELECT universe_id, entity_id, effect_type, remaining_ticks, magnitude,
                    damage_type, source_id
             FROM active_effects",
        )
        .fetch_all(pool)
        .await?;

        let mut cache = HashMap::<EntityKey, EntityEffects>::new();
        for (universe_id, entity_id, effect_type, remaining, magnitude, dtype, source) in rows {
            let Ok(effect_type) = EffectType::from_str(&effect_type) else {
                warn!("Unknown effect type in database: {}", effect_type);
                continue;
            };
            let effect = StatusEffect {
                effect_type,
                remaining_ticks: remaining.max(0) as u32,
                magnitude: magnitude as i32,
                source_id: source,
                damage_type: dtype.and_then(|d| DamageType::from_str(&d).ok()),
            };
            cache
                .entry((universe_id, entity_id))
                .or_default()
                .add(effect);
        }
        debug!("Loaded effects for {} entities", cache.len());
        *self.cache.write().await = cache;
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
    fn test_effect_tick() {
        let mut effect = StatusEffect::dot(EffectType::Poisoned, 3, 5, DamageType::Poison);

        // Tick returns damage
        let result = effect.tick();
        assert_eq!(result, Some((5, DamageType::Poison)));
        assert_eq!(effect.remaining_ticks, 2);

        // Tick again
        effect.tick();
        assert_eq!(effect.remaining_ticks, 1);

        // Last tick
        effect.tick();
        assert!(effect.is_expired());
    }

    #[test]
    fn test_entity_effects() {
        let mut effects = EntityEffects::new();

        // Add effect
        effects.add(StatusEffect::new(EffectType::Stunned, 2, 0));
        assert!(effects.has(EffectType::Stunned));
        assert!(!effects.can_act());

        // Tick
        effects.tick_all();
        assert!(effects.has(EffectType::Stunned));
        assert!(!effects.can_act());

        // Tick again - expires
        effects.tick_all();
        assert!(!effects.has(EffectType::Stunned));
        assert!(effects.can_act());
    }

    #[test]
    fn test_dot_damage() {
        let mut effects = EntityEffects::new();
        effects.add(StatusEffect::dot(
            EffectType::Burning,
            2,
            10,
            DamageType::Fire,
        ));

        let damage = effects.tick_all();
        assert_eq!(damage.len(), 1);
        assert_eq!(damage[0], (10, DamageType::Fire));

        let damage2 = effects.tick_all();
        assert_eq!(damage2.len(), 1); // Last tick still deals damage

        // Effect now expired
        assert!(!effects.has(EffectType::Burning));
    }

    #[test]
    fn test_effect_refresh() {
        let mut effects = EntityEffects::new();

        effects.add(StatusEffect::new(EffectType::Stunned, 2, 0));
        effects.add(StatusEffect::new(EffectType::Stunned, 5, 0)); // Refresh to longer duration

        let effect = effects.get(EffectType::Stunned).unwrap();
        assert_eq!(effect.remaining_ticks, 5);
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

        registry
            .apply(
                &store,
                "a",
                "/npcs/rat",
                StatusEffect::new(EffectType::Stunned, 2, 0),
            )
            .await
            .unwrap();
        // Refreshing keeps the longer duration and stronger magnitude
        registry
            .apply(
                &store,
                "a",
                "/npcs/rat",
                StatusEffect::dot(EffectType::Poisoned, 3, 2, DamageType::Poison),
            )
            .await
            .unwrap();
        registry
            .apply(
                &store,
                "a",
                "/npcs/rat",
                StatusEffect::dot(EffectType::Poisoned, 1, 5, DamageType::Poison),
            )
            .await
            .unwrap();
        assert!(!registry.get("a", "/npcs/rat").await.can_act());
        assert!(
            registry.get("b", "/npcs/rat").await.can_act(),
            "per universe"
        );

        let fresh = EffectRegistry::new();
        fresh.load_from_db(&pool).await.unwrap();
        let effects = fresh.get("a", "/npcs/rat").await;
        let poison = effects.get(EffectType::Poisoned).expect("poison persisted");
        assert_eq!((poison.remaining_ticks, poison.magnitude), (3, 5));
        assert_eq!(poison.damage_type, Some(DamageType::Poison));
        assert!(effects.has(EffectType::Stunned));
        assert!(fresh
            .get("b", "/npcs/rat")
            .await
            .active_effects()
            .is_empty());

        registry
            .remove(&store, "a", "/npcs/rat", EffectType::Stunned)
            .await
            .unwrap();
        assert!(registry.get("a", "/npcs/rat").await.can_act());
        let fresh = EffectRegistry::new();
        fresh.load_from_db(&pool).await.unwrap();
        assert!(!fresh.get("a", "/npcs/rat").await.has(EffectType::Stunned));
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
                StatusEffect::new(EffectType::Stunned, 2, 0),
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
