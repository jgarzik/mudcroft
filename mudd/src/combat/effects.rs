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

/// Global effect registry for tracking effects on all entities
#[derive(Debug)]
pub struct EffectRegistry {
    entities: RwLock<HashMap<String, EntityEffects>>,
    db_pool: Option<SqlitePool>,
}

impl Default for EffectRegistry {
    fn default() -> Self {
        Self {
            entities: RwLock::new(HashMap::new()),
            db_pool: None,
        }
    }
}

impl EffectRegistry {
    /// Create a new effect registry
    pub fn new() -> Self {
        Self::default()
    }

    /// Create a new effect registry with database pool
    pub fn with_db(pool: SqlitePool) -> Self {
        Self {
            entities: RwLock::new(HashMap::new()),
            db_pool: Some(pool),
        }
    }

    /// Create a shared instance
    pub fn shared() -> Arc<Self> {
        Arc::new(Self::new())
    }

    /// Create a shared instance with database pool
    pub fn shared_with_db(pool: SqlitePool) -> Arc<Self> {
        Arc::new(Self::with_db(pool))
    }

    /// Add an effect to an entity
    pub async fn add_effect(&self, entity_id: &str, effect: StatusEffect) {
        // Persist to database
        if let Some(ref pool) = self.db_pool {
            if let Err(e) = self.persist_effect(entity_id, &effect, pool).await {
                warn!("Failed to persist effect for {}: {}", entity_id, e);
            }
        }

        let mut entities = self.entities.write().await;
        entities
            .entry(entity_id.to_string())
            .or_default()
            .add(effect);
    }

    /// Persist an effect to database
    async fn persist_effect(
        &self,
        entity_id: &str,
        effect: &StatusEffect,
        pool: &SqlitePool,
    ) -> anyhow::Result<()> {
        let effect_type_str = effect.effect_type.to_string();
        let damage_type_str = effect.damage_type.map(|dt| format!("{:?}", dt));
        let id = uuid::Uuid::new_v4().to_string();

        sqlx::query(
            r#"
            INSERT INTO active_effects
            (id, entity_id, effect_type, remaining_ticks, magnitude, damage_type, source_id)
            VALUES (?, ?, ?, ?, ?, ?, ?)
            "#,
        )
        .bind(&id)
        .bind(entity_id)
        .bind(&effect_type_str)
        .bind(effect.remaining_ticks as i32)
        .bind(effect.magnitude)
        .bind(&damage_type_str)
        .bind(&effect.source_id)
        .execute(pool)
        .await?;
        Ok(())
    }

    /// Remove an effect from an entity
    pub async fn remove_effect(&self, entity_id: &str, effect_type: EffectType) {
        // Remove from database
        if let Some(ref pool) = self.db_pool {
            let effect_type_str = effect_type.to_string();
            if let Err(e) =
                sqlx::query("DELETE FROM active_effects WHERE entity_id = ? AND effect_type = ?")
                    .bind(entity_id)
                    .bind(&effect_type_str)
                    .execute(pool)
                    .await
            {
                warn!(
                    "Failed to remove effect {} for {}: {}",
                    effect_type, entity_id, e
                );
            }
        }

        let mut entities = self.entities.write().await;
        if let Some(effects) = entities.get_mut(entity_id) {
            effects.remove(effect_type);
        }
    }

    /// Check if entity has an effect
    pub async fn has_effect(&self, entity_id: &str, effect_type: EffectType) -> bool {
        let entities = self.entities.read().await;
        entities.get(entity_id).is_some_and(|e| e.has(effect_type))
    }

    /// Check if entity can act
    pub async fn can_act(&self, entity_id: &str) -> bool {
        let entities = self.entities.read().await;
        entities.get(entity_id).is_none_or(|e| e.can_act())
    }

    /// Tick effects for an entity
    pub async fn tick(&self, entity_id: &str) -> Vec<(i32, DamageType)> {
        let results = {
            let mut entities = self.entities.write().await;
            if let Some(effects) = entities.get_mut(entity_id) {
                effects.tick_all()
            } else {
                Vec::new()
            }
        };

        // Update remaining ticks in database
        if let Some(ref pool) = self.db_pool {
            if let Err(e) = self.sync_effects_to_db(entity_id, pool).await {
                warn!("Failed to sync effects for {}: {}", entity_id, e);
            }
        }

        results
    }

    /// Sync effects to database after tick
    async fn sync_effects_to_db(&self, entity_id: &str, pool: &SqlitePool) -> anyhow::Result<()> {
        // Delete all effects for entity and re-insert active ones
        sqlx::query("DELETE FROM active_effects WHERE entity_id = ?")
            .bind(entity_id)
            .execute(pool)
            .await?;

        let entities = self.entities.read().await;
        if let Some(entity_effects) = entities.get(entity_id) {
            for effect in entity_effects.active_effects() {
                self.persist_effect(entity_id, effect, pool).await?;
            }
        }
        Ok(())
    }

    /// Load effects from database on startup
    pub async fn load_from_db(&self) -> anyhow::Result<()> {
        let Some(ref pool) = self.db_pool else {
            return Ok(());
        };

        #[allow(clippy::type_complexity)]
        let rows: Vec<(String, String, String, i32, i32, Option<String>, Option<String>)> =
            sqlx::query_as(
                "SELECT id, entity_id, effect_type, remaining_ticks, magnitude, damage_type, source_id FROM active_effects",
            )
            .fetch_all(pool)
            .await?;

        let mut entities = self.entities.write().await;
        for (
            _id,
            entity_id,
            effect_type_str,
            remaining_ticks,
            magnitude,
            damage_type_str,
            source_id,
        ) in rows
        {
            let effect_type = match EffectType::from_str(&effect_type_str) {
                Ok(et) => et,
                Err(_) => continue,
            };

            let damage_type = damage_type_str.and_then(|s| match s.as_str() {
                "Physical" => Some(DamageType::Physical),
                "Slashing" => Some(DamageType::Slashing),
                "Piercing" => Some(DamageType::Piercing),
                "Bludgeoning" => Some(DamageType::Bludgeoning),
                "Fire" => Some(DamageType::Fire),
                "Cold" => Some(DamageType::Cold),
                "Lightning" => Some(DamageType::Lightning),
                "Acid" => Some(DamageType::Acid),
                "Poison" => Some(DamageType::Poison),
                "Psychic" => Some(DamageType::Psychic),
                "Radiant" => Some(DamageType::Radiant),
                "Necrotic" => Some(DamageType::Necrotic),
                "Force" => Some(DamageType::Force),
                "Thunder" => Some(DamageType::Thunder),
                _ => None,
            });

            let mut effect = StatusEffect::new(effect_type, remaining_ticks as u32, magnitude);
            effect.damage_type = damage_type;
            effect.source_id = source_id;

            entities.entry(entity_id).or_default().add(effect);
        }

        debug!("Loaded effects for {} entities", entities.len());
        Ok(())
    }

    /// Clear effects for an entity
    pub async fn clear(&self, entity_id: &str) {
        // Remove from database
        if let Some(ref pool) = self.db_pool {
            if let Err(e) = sqlx::query("DELETE FROM active_effects WHERE entity_id = ?")
                .bind(entity_id)
                .execute(pool)
                .await
            {
                warn!("Failed to clear effects for {}: {}", entity_id, e);
            }
        }

        let mut entities = self.entities.write().await;
        entities.remove(entity_id);
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

    #[tokio::test]
    async fn test_effect_registry() {
        let registry = EffectRegistry::new();

        // Add effect
        registry
            .add_effect("player1", StatusEffect::new(EffectType::Stunned, 2, 0))
            .await;
        assert!(registry.has_effect("player1", EffectType::Stunned).await);
        assert!(!registry.can_act("player1").await);

        // Remove effect
        registry.remove_effect("player1", EffectType::Stunned).await;
        assert!(!registry.has_effect("player1", EffectType::Stunned).await);
        assert!(registry.can_act("player1").await);
    }
}
