//! Combat state tracking
//!
//! Manages combat sessions between entities:
//! - Who is fighting whom
//! - Attack queues
//! - Combat initiation and ending

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use tokio::sync::RwLock;

use sqlx::SqlitePool;
use tracing::{debug, warn};

use super::damage::{DamageProfile, DamageResult, DamageType};
use super::dice::{is_critical, is_fumble, roll_d20};

/// PvP policy for a universe
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PvpPolicy {
    /// PvP is completely disabled
    #[default]
    Disabled,
    /// PvP only in designated arenas
    ArenaOnly,
    /// Players must flag themselves for PvP
    Flagged,
    /// Open PvP everywhere
    Open,
}

impl PvpPolicy {
    /// Parse from string
    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> Option<PvpPolicy> {
        match s.to_lowercase().as_str() {
            "disabled" | "off" | "none" => Some(PvpPolicy::Disabled),
            "arena" | "arena_only" | "arenaonly" => Some(PvpPolicy::ArenaOnly),
            "flagged" | "flag" => Some(PvpPolicy::Flagged),
            "open" | "enabled" | "on" => Some(PvpPolicy::Open),
            _ => None,
        }
    }
}

/// Combat state for a single entity
#[derive(Debug, Clone, Default)]
pub struct CombatState {
    /// Universe this entity belongs to (for persistence)
    pub universe_id: Option<String>,
    /// Whether entity is in combat (ephemeral, not persisted)
    pub in_combat: bool,
    /// Entity currently being attacked (ephemeral, not persisted)
    pub attacking: Option<String>,
    /// Set of entities attacking this entity (ephemeral, not persisted)
    pub attackers: BTreeSet<String>,
    /// Whether entity is flagged for PvP (ephemeral, not persisted)
    pub pvp_flagged: bool,
    /// Damage profile (resistances/immunities)
    pub damage_profile: DamageProfile,
    /// Current hit points
    pub hp: i32,
    /// Maximum hit points
    pub max_hp: i32,
    /// Base attack bonus
    pub attack_bonus: i32,
    /// Armor class
    pub armor_class: i32,
}

impl CombatState {
    /// Create a new combat state with default values
    pub fn new(max_hp: i32) -> Self {
        Self {
            universe_id: None,
            in_combat: false,
            attacking: None,
            attackers: BTreeSet::new(),
            pvp_flagged: false,
            damage_profile: DamageProfile::new(),
            hp: max_hp,
            max_hp,
            attack_bonus: 0,
            armor_class: 10,
        }
    }

    /// Create a new combat state with universe ID
    pub fn new_with_universe(max_hp: i32, universe_id: &str) -> Self {
        Self {
            universe_id: Some(universe_id.to_string()),
            in_combat: false,
            attacking: None,
            attackers: BTreeSet::new(),
            pvp_flagged: false,
            damage_profile: DamageProfile::new(),
            hp: max_hp,
            max_hp,
            attack_bonus: 0,
            armor_class: 10,
        }
    }

    /// Check if entity is dead
    pub fn is_dead(&self) -> bool {
        self.hp <= 0
    }

    /// Take damage (returns actual damage taken after modifiers)
    pub fn take_damage(&mut self, amount: i32, dtype: DamageType, is_crit: bool) -> DamageResult {
        let result = self.damage_profile.calculate_damage(amount, dtype, is_crit);
        self.hp -= result.final_damage;
        result
    }

    /// Heal (cannot exceed max_hp)
    pub fn heal(&mut self, amount: i32) -> i32 {
        let actual = amount.min(self.max_hp - self.hp);
        self.hp += actual;
        actual
    }

    /// Start attacking a target
    pub fn start_attacking(&mut self, target_id: &str) {
        self.in_combat = true;
        self.attacking = Some(target_id.to_string());
    }

    /// Stop attacking
    pub fn stop_attacking(&mut self) {
        self.attacking = None;
        if self.attackers.is_empty() {
            self.in_combat = false;
        }
    }

    /// Add an attacker
    pub fn add_attacker(&mut self, attacker_id: &str) {
        self.in_combat = true;
        self.attackers.insert(attacker_id.to_string());
    }

    /// Remove an attacker
    pub fn remove_attacker(&mut self, attacker_id: &str) {
        self.attackers.remove(attacker_id);
        if self.attackers.is_empty() && self.attacking.is_none() {
            self.in_combat = false;
        }
    }

    /// Leave combat entirely
    pub fn leave_combat(&mut self) {
        self.in_combat = false;
        self.attacking = None;
        self.attackers.clear();
    }
}

/// Result of an attack roll
#[derive(Debug, Clone)]
pub struct AttackResult {
    /// The d20 roll
    pub roll: u32,
    /// Total attack value (roll + bonus)
    pub attack_total: i32,
    /// Target's AC
    pub target_ac: i32,
    /// Whether the attack hit
    pub hit: bool,
    /// Whether it was a critical hit
    pub critical: bool,
    /// Whether it was a fumble
    pub fumble: bool,
    /// Damage result if hit
    pub damage: Option<DamageResult>,
}

impl AttackResult {
    /// Create a new attack result
    pub fn new(roll: u32, attack_bonus: i32, target_ac: i32) -> Self {
        let critical = is_critical(roll);
        let fumble = is_fumble(roll);
        let attack_total = roll as i32 + attack_bonus;

        // Critical always hits, fumble always misses
        let hit = critical || (!fumble && attack_total >= target_ac);

        Self {
            roll,
            attack_total,
            target_ac,
            hit,
            critical,
            fumble,
            damage: None,
        }
    }

    /// Add damage to the attack result
    pub fn with_damage(mut self, damage: DamageResult) -> Self {
        self.damage = Some(damage);
        self
    }
}

/// Combat manager for a universe
#[derive(Debug)]
pub struct CombatManager {
    /// Combat states by entity ID
    states: RwLock<BTreeMap<String, CombatState>>,
    /// Universe PvP policy
    pvp_policy: RwLock<PvpPolicy>,
    /// Database pool for persistence
    db_pool: Option<SqlitePool>,
}

impl Default for CombatManager {
    fn default() -> Self {
        Self {
            states: RwLock::new(BTreeMap::new()),
            pvp_policy: RwLock::new(PvpPolicy::default()),
            db_pool: None,
        }
    }
}

impl CombatManager {
    /// Create a new combat manager
    pub fn new() -> Self {
        Self::default()
    }

    /// Create a new combat manager with database pool
    pub fn with_db(pool: SqlitePool) -> Self {
        Self {
            states: RwLock::new(BTreeMap::new()),
            pvp_policy: RwLock::new(PvpPolicy::default()),
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

    /// Set the PvP policy
    pub async fn set_pvp_policy(&self, policy: PvpPolicy) {
        *self.pvp_policy.write().await = policy;
    }

    /// Get the PvP policy
    pub async fn get_pvp_policy(&self) -> PvpPolicy {
        *self.pvp_policy.read().await
    }

    /// Initialize combat state for an entity
    pub async fn init_entity(&self, entity_id: &str, max_hp: i32) {
        self.init_entity_with_universe(entity_id, None, max_hp)
            .await;
    }

    /// Initialize combat state for an entity with universe ID
    pub async fn init_entity_with_universe(
        &self,
        entity_id: &str,
        universe_id: Option<&str>,
        max_hp: i32,
    ) {
        let state = match universe_id {
            Some(uid) => CombatState::new_with_universe(max_hp, uid),
            None => CombatState::new(max_hp),
        };

        // Persist to database
        if let Some(ref pool) = self.db_pool {
            if let Some(uid) = universe_id {
                if let Err(e) = self.persist_state(entity_id, uid, &state, pool).await {
                    warn!("Failed to persist combat state for {}: {}", entity_id, e);
                }
            }
        }

        self.states
            .write()
            .await
            .insert(entity_id.to_string(), state);
    }

    /// Persist combat state to database
    async fn persist_state(
        &self,
        entity_id: &str,
        universe_id: &str,
        state: &CombatState,
        pool: &SqlitePool,
    ) -> anyhow::Result<()> {
        sqlx::query(
            r#"
            INSERT OR REPLACE INTO combat_state
            (entity_id, universe_id, hp, max_hp, armor_class, attack_bonus)
            VALUES (?, ?, ?, ?, ?, ?)
            "#,
        )
        .bind(entity_id)
        .bind(universe_id)
        .bind(state.hp)
        .bind(state.max_hp)
        .bind(state.armor_class)
        .bind(state.attack_bonus)
        .execute(pool)
        .await?;
        Ok(())
    }

    /// Save current state to database (for HP changes)
    async fn save_entity_state(&self, entity_id: &str, state: &CombatState) {
        if let Some(ref pool) = self.db_pool {
            if let Some(ref universe_id) = state.universe_id {
                if let Err(e) = self
                    .persist_state(entity_id, universe_id, state, pool)
                    .await
                {
                    warn!("Failed to save combat state for {}: {}", entity_id, e);
                }
            }
        }
    }

    /// Load combat states from database on startup
    pub async fn load_from_db(&self) -> anyhow::Result<()> {
        let Some(ref pool) = self.db_pool else {
            return Ok(());
        };

        let rows: Vec<(String, String, i32, i32, i32, i32)> = sqlx::query_as(
            "SELECT entity_id, universe_id, hp, max_hp, armor_class, attack_bonus FROM combat_state",
        )
        .fetch_all(pool)
        .await?;

        let mut states = self.states.write().await;
        for (entity_id, universe_id, hp, max_hp, armor_class, attack_bonus) in rows {
            let mut state = CombatState::new_with_universe(max_hp, &universe_id);
            state.hp = hp;
            state.armor_class = armor_class;
            state.attack_bonus = attack_bonus;
            states.insert(entity_id, state);
        }

        debug!("Loaded {} combat states from database", states.len());
        Ok(())
    }

    /// Get combat state for an entity
    pub async fn get_state(&self, entity_id: &str) -> Option<CombatState> {
        let states = self.states.read().await;
        states.get(entity_id).cloned()
    }

    /// Update combat state for an entity
    pub async fn update_state(&self, entity_id: &str, state: CombatState) {
        let mut states = self.states.write().await;
        states.insert(entity_id.to_string(), state);
    }

    /// Check if entity is in combat
    pub async fn is_in_combat(&self, entity_id: &str) -> bool {
        let states = self.states.read().await;
        states.get(entity_id).is_some_and(|s| s.in_combat)
    }

    /// Check if entity is dead
    pub async fn is_dead(&self, entity_id: &str) -> bool {
        let states = self.states.read().await;
        states.get(entity_id).is_some_and(|s| s.is_dead())
    }

    /// Initiate combat between attacker and defender
    pub async fn initiate(&self, attacker_id: &str, defender_id: &str) -> Result<(), String> {
        // Check PvP policy if both are players
        // (In a real implementation, we'd check if they're players)

        let mut states = self.states.write().await;

        // Ensure both entities have combat states
        if !states.contains_key(attacker_id) {
            states.insert(attacker_id.to_string(), CombatState::new(100));
        }
        if !states.contains_key(defender_id) {
            states.insert(defender_id.to_string(), CombatState::new(100));
        }

        // Set up combat relationship
        if let Some(attacker) = states.get_mut(attacker_id) {
            attacker.start_attacking(defender_id);
        }
        if let Some(defender) = states.get_mut(defender_id) {
            defender.add_attacker(attacker_id);
        }

        Ok(())
    }

    /// Perform an attack
    pub async fn attack(
        &self,
        attacker_id: &str,
        defender_id: &str,
        damage_dice: i32,
        damage_type: DamageType,
    ) -> Result<AttackResult, String> {
        let result = {
            let mut states = self.states.write().await;

            let attacker = states.get(attacker_id).ok_or("Attacker not found")?;
            let defender = states.get(defender_id).ok_or("Defender not found")?;

            // Roll to hit
            let roll = roll_d20();
            let mut result = AttackResult::new(roll, attacker.attack_bonus, defender.armor_class);

            if result.hit {
                // Calculate and apply damage
                let is_crit = result.critical;
                let defender_mut = states.get_mut(defender_id).unwrap();
                let damage_result = defender_mut.take_damage(damage_dice, damage_type, is_crit);
                result = result.with_damage(damage_result);
            }

            result
        };

        // Persist HP change if damage was dealt
        if result.hit {
            if let Some(state) = self.get_state(defender_id).await {
                self.save_entity_state(defender_id, &state).await;
            }
        }

        Ok(result)
    }

    /// Deal direct damage (bypassing attack roll)
    pub async fn deal_damage(
        &self,
        target_id: &str,
        amount: i32,
        damage_type: DamageType,
        is_crit: bool,
    ) -> Result<DamageResult, String> {
        let result = {
            let mut states = self.states.write().await;
            let target = states.get_mut(target_id).ok_or("Target not found")?;
            target.take_damage(amount, damage_type, is_crit)
        };

        // Persist HP change
        if let Some(state) = self.get_state(target_id).await {
            self.save_entity_state(target_id, &state).await;
        }

        Ok(result)
    }

    /// Heal an entity
    pub async fn heal(&self, target_id: &str, amount: i32) -> Result<i32, String> {
        let healed = {
            let mut states = self.states.write().await;
            let target = states.get_mut(target_id).ok_or("Target not found")?;
            target.heal(amount)
        };

        // Persist HP change
        if let Some(state) = self.get_state(target_id).await {
            self.save_entity_state(target_id, &state).await;
        }

        Ok(healed)
    }

    /// End combat for an entity
    pub async fn end_combat(&self, entity_id: &str) {
        let mut states = self.states.write().await;

        // Remove this entity from all attackers lists
        let attacker_ids: Vec<String> = states
            .get(entity_id)
            .map(|s| s.attackers.iter().cloned().collect())
            .unwrap_or_default();

        for attacker_id in &attacker_ids {
            if let Some(attacker) = states.get_mut(attacker_id) {
                if attacker.attacking.as_deref() == Some(entity_id) {
                    attacker.stop_attacking();
                }
            }
        }

        // Get who this entity was attacking
        let target_id = states.get(entity_id).and_then(|s| s.attacking.clone());

        if let Some(ref tid) = target_id {
            if let Some(target) = states.get_mut(tid) {
                target.remove_attacker(entity_id);
            }
        }

        // Clear this entity's combat state
        if let Some(state) = states.get_mut(entity_id) {
            state.leave_combat();
        }
    }

    /// Remove entity from combat system (on death/disconnect)
    pub async fn remove_entity(&self, entity_id: &str) {
        self.end_combat(entity_id).await;

        // Remove from database (cascades to active_effects)
        if let Some(ref pool) = self.db_pool {
            if let Err(e) = sqlx::query("DELETE FROM combat_state WHERE entity_id = ?")
                .bind(entity_id)
                .execute(pool)
                .await
            {
                warn!("Failed to remove combat state for {}: {}", entity_id, e);
            }
        }

        let mut states = self.states.write().await;
        states.remove(entity_id);
    }

    /// Set damage modifier for an entity
    pub async fn set_damage_modifier(
        &self,
        entity_id: &str,
        damage_type: DamageType,
        modifier: super::damage::DamageModifier,
    ) {
        let mut states = self.states.write().await;
        if let Some(state) = states.get_mut(entity_id) {
            state.damage_profile.set(damage_type, modifier);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_combat_state() {
        let mut state = CombatState::new(100);

        assert!(!state.in_combat);
        assert!(!state.is_dead());
        assert_eq!(state.hp, 100);

        // Take damage
        let result = state.take_damage(30, DamageType::Physical, false);
        assert_eq!(result.final_damage, 30);
        assert_eq!(state.hp, 70);

        // Heal
        let healed = state.heal(20);
        assert_eq!(healed, 20);
        assert_eq!(state.hp, 90);

        // Can't overheal
        let healed = state.heal(50);
        assert_eq!(healed, 10);
        assert_eq!(state.hp, 100);
    }

    #[test]
    fn test_combat_relationships() {
        let mut attacker = CombatState::new(100);
        let mut defender = CombatState::new(100);

        // Initiate combat
        attacker.start_attacking("defender");
        defender.add_attacker("attacker");

        assert!(attacker.in_combat);
        assert!(defender.in_combat);
        assert_eq!(attacker.attacking, Some("defender".to_string()));
        assert!(defender.attackers.contains("attacker"));

        // End combat
        attacker.stop_attacking();
        defender.remove_attacker("attacker");

        assert!(!attacker.in_combat);
        assert!(!defender.in_combat);
    }

    #[test]
    fn test_attack_result() {
        // Critical hit (roll 20)
        let result = AttackResult::new(20, 5, 15);
        assert!(result.hit);
        assert!(result.critical);
        assert!(!result.fumble);

        // Fumble (roll 1)
        let result = AttackResult::new(1, 5, 5);
        assert!(!result.hit);
        assert!(!result.critical);
        assert!(result.fumble);

        // Normal hit
        let result = AttackResult::new(15, 5, 18);
        assert!(result.hit); // 15 + 5 = 20 >= 18
        assert!(!result.critical);

        // Normal miss
        let result = AttackResult::new(10, 3, 18);
        assert!(!result.hit); // 10 + 3 = 13 < 18
    }

    #[tokio::test]
    async fn test_combat_manager() {
        let manager = CombatManager::new();

        // Init entities
        manager.init_entity("player1", 100).await;
        manager.init_entity("goblin", 20).await;

        // Initiate combat
        manager.initiate("player1", "goblin").await.unwrap();

        assert!(manager.is_in_combat("player1").await);
        assert!(manager.is_in_combat("goblin").await);

        // Deal damage
        let result = manager
            .deal_damage("goblin", 15, DamageType::Slashing, false)
            .await
            .unwrap();
        assert_eq!(result.final_damage, 15);

        let state = manager.get_state("goblin").await.unwrap();
        assert_eq!(state.hp, 5);

        // End combat
        manager.end_combat("player1").await;
        assert!(!manager.is_in_combat("player1").await);
    }

    #[test]
    fn test_pvp_policy_parsing() {
        assert_eq!(PvpPolicy::from_str("disabled"), Some(PvpPolicy::Disabled));
        assert_eq!(PvpPolicy::from_str("ARENA"), Some(PvpPolicy::ArenaOnly));
        assert_eq!(PvpPolicy::from_str("open"), Some(PvpPolicy::Open));
        assert_eq!(PvpPolicy::from_str("invalid"), None);
    }
}
