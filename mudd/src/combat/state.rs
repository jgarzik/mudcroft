//! Combat state tracking
//!
//! Manages combat sessions between entities:
//! - Who is fighting whom
//! - Attack queues
//! - Combat initiation and ending

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use tokio::sync::RwLock;

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
    /// Whether entity is in combat
    pub in_combat: bool,
    /// Entity currently being attacked
    pub attacking: Option<String>,
    /// Set of entities attacking this entity
    pub attackers: HashSet<String>,
    /// Whether entity is flagged for PvP
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
            in_combat: false,
            attacking: None,
            attackers: HashSet::new(),
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
#[derive(Debug, Default)]
pub struct CombatManager {
    /// Combat states by entity ID
    states: RwLock<HashMap<String, CombatState>>,
    /// Universe PvP policy
    pvp_policy: RwLock<PvpPolicy>,
}

impl CombatManager {
    /// Create a new combat manager
    pub fn new() -> Self {
        Self::default()
    }

    /// Create a shared instance
    pub fn shared() -> Arc<Self> {
        Arc::new(Self::new())
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
        let mut states = self.states.write().await;
        states.insert(entity_id.to_string(), CombatState::new(max_hp));
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
        let mut states = self.states.write().await;
        let target = states.get_mut(target_id).ok_or("Target not found")?;
        Ok(target.take_damage(amount, damage_type, is_crit))
    }

    /// Heal an entity
    pub async fn heal(&self, target_id: &str, amount: i32) -> Result<i32, String> {
        let mut states = self.states.write().await;
        let target = states.get_mut(target_id).ok_or("Target not found")?;
        Ok(target.heal(amount))
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
