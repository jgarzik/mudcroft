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
use crate::raft::RaftWriter;

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

/// (universe_id, entity_id): object ids are scoped per universe
type EntityKey = (String, String);

fn ek(universe_id: &str, entity_id: &str) -> EntityKey {
    (universe_id.to_string(), entity_id.to_string())
}

/// Combat manager for all universes
pub struct CombatManager {
    /// Combat states by (universe, entity)
    states: RwLock<BTreeMap<EntityKey, CombatState>>,
    /// Universe PvP policy
    pvp_policy: RwLock<PvpPolicy>,
    /// Database pool for persistence
    db_pool: Option<SqlitePool>,
    /// Raft writer: in the server every write goes through it
    raft_writer: Option<Arc<RaftWriter>>,
}

impl std::fmt::Debug for CombatManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CombatManager")
            .field("db_pool", &self.db_pool.is_some())
            .field("raft_writer", &self.raft_writer.is_some())
            .finish()
    }
}

impl Default for CombatManager {
    fn default() -> Self {
        Self {
            states: RwLock::new(BTreeMap::new()),
            pvp_policy: RwLock::new(PvpPolicy::default()),
            db_pool: None,
            raft_writer: None,
        }
    }
}

impl CombatManager {
    /// Create a new combat manager
    pub fn new() -> Self {
        Self::default()
    }

    /// Create a combat manager that persists state: through Raft when a
    /// writer is given (the server), otherwise directly (tests)
    pub fn with_db(pool: SqlitePool, raft_writer: Option<Arc<RaftWriter>>) -> Self {
        Self {
            states: RwLock::new(BTreeMap::new()),
            pvp_policy: RwLock::new(PvpPolicy::default()),
            db_pool: Some(pool),
            raft_writer,
        }
    }

    /// Run one write, through Raft when available
    async fn write(&self, sql: &str, params: Vec<serde_json::Value>) -> anyhow::Result<()> {
        if let Some(ref raft_writer) = self.raft_writer {
            raft_writer.execute(sql, params).await?;
        } else if let Some(ref pool) = self.db_pool {
            let mut query = sqlx::query(sql);
            for param in params {
                query = match param {
                    serde_json::Value::String(v) => query.bind(v),
                    serde_json::Value::Number(n) => query.bind(n.as_i64()),
                    other => query.bind(other.to_string()),
                };
            }
            query.execute(pool).await?;
        }
        Ok(())
    }

    /// Create a shared instance
    pub fn shared() -> Arc<Self> {
        Arc::new(Self::new())
    }

    /// Create a shared instance with database pool
    pub fn shared_with_db(pool: SqlitePool, raft_writer: Option<Arc<RaftWriter>>) -> Arc<Self> {
        Arc::new(Self::with_db(pool, raft_writer))
    }

    /// Set the PvP policy
    pub async fn set_pvp_policy(&self, policy: PvpPolicy) {
        *self.pvp_policy.write().await = policy;
    }

    /// Get the PvP policy
    pub async fn get_pvp_policy(&self) -> PvpPolicy {
        *self.pvp_policy.read().await
    }

    /// Initialize combat state for an entity in a universe
    pub async fn init_entity(&self, universe_id: &str, entity_id: &str, max_hp: i32) {
        let state = CombatState::new_with_universe(max_hp, universe_id);

        if let Err(e) = self.persist_state(universe_id, entity_id, &state).await {
            warn!("Failed to persist combat state for {}: {}", entity_id, e);
        }

        self.states
            .write()
            .await
            .insert(ek(universe_id, entity_id), state);
    }

    /// Persist combat state to database
    async fn persist_state(
        &self,
        universe_id: &str,
        entity_id: &str,
        state: &CombatState,
    ) -> anyhow::Result<()> {
        self.write(
            "INSERT OR REPLACE INTO combat_state
             (universe_id, entity_id, hp, max_hp, armor_class, attack_bonus)
             VALUES (?, ?, ?, ?, ?, ?)",
            vec![
                serde_json::json!(universe_id),
                serde_json::json!(entity_id),
                serde_json::json!(state.hp),
                serde_json::json!(state.max_hp),
                serde_json::json!(state.armor_class),
                serde_json::json!(state.attack_bonus),
            ],
        )
        .await
    }

    /// Save current state to database (for HP changes)
    async fn save_entity_state(&self, universe_id: &str, entity_id: &str) {
        let Some(state) = self.get_state(universe_id, entity_id).await else {
            return;
        };
        if let Err(e) = self.persist_state(universe_id, entity_id, &state).await {
            warn!("Failed to save combat state for {}: {}", entity_id, e);
        }
    }

    /// Load combat states from database on startup
    pub async fn load_from_db(&self) -> anyhow::Result<()> {
        let Some(ref pool) = self.db_pool else {
            return Ok(());
        };

        let rows: Vec<(String, String, i32, i32, i32, i32)> = sqlx::query_as(
            "SELECT universe_id, entity_id, hp, max_hp, armor_class, attack_bonus FROM combat_state",
        )
        .fetch_all(pool)
        .await?;

        let mut states = self.states.write().await;
        for (universe_id, entity_id, hp, max_hp, armor_class, attack_bonus) in rows {
            let mut state = CombatState::new_with_universe(max_hp, &universe_id);
            state.hp = hp;
            state.armor_class = armor_class;
            state.attack_bonus = attack_bonus;
            states.insert((universe_id, entity_id), state);
        }

        debug!("Loaded {} combat states from database", states.len());
        Ok(())
    }

    /// Get combat state for an entity
    pub async fn get_state(&self, universe_id: &str, entity_id: &str) -> Option<CombatState> {
        self.states
            .read()
            .await
            .get(&ek(universe_id, entity_id))
            .cloned()
    }

    /// Update combat state for an entity
    pub async fn update_state(&self, universe_id: &str, entity_id: &str, state: CombatState) {
        self.states
            .write()
            .await
            .insert(ek(universe_id, entity_id), state);
    }

    /// Check if entity is in combat
    pub async fn is_in_combat(&self, universe_id: &str, entity_id: &str) -> bool {
        self.states
            .read()
            .await
            .get(&ek(universe_id, entity_id))
            .is_some_and(|s| s.in_combat)
    }

    /// Check if entity is dead
    pub async fn is_dead(&self, universe_id: &str, entity_id: &str) -> bool {
        self.states
            .read()
            .await
            .get(&ek(universe_id, entity_id))
            .is_some_and(|s| s.is_dead())
    }

    /// Initiate combat between attacker and defender (same universe)
    pub async fn initiate(
        &self,
        universe_id: &str,
        attacker_id: &str,
        defender_id: &str,
    ) -> Result<(), String> {
        let mut states = self.states.write().await;
        let attacker_key = ek(universe_id, attacker_id);
        let defender_key = ek(universe_id, defender_id);

        // Ensure both entities have combat states
        states
            .entry(attacker_key.clone())
            .or_insert_with(|| CombatState::new_with_universe(100, universe_id));
        states
            .entry(defender_key.clone())
            .or_insert_with(|| CombatState::new_with_universe(100, universe_id));

        // Set up combat relationship
        if let Some(attacker) = states.get_mut(&attacker_key) {
            attacker.start_attacking(defender_id);
        }
        if let Some(defender) = states.get_mut(&defender_key) {
            defender.add_attacker(attacker_id);
        }

        Ok(())
    }

    /// Perform an attack: roll to hit, and on a hit deal `damage` (already
    /// rolled by the caller; doubled on a critical) of `damage_type`
    pub async fn attack(
        &self,
        universe_id: &str,
        attacker_id: &str,
        defender_id: &str,
        damage: i32,
        damage_type: DamageType,
    ) -> Result<AttackResult, String> {
        let result = {
            let mut states = self.states.write().await;
            let defender_key = ek(universe_id, defender_id);

            let attacker = states
                .get(&ek(universe_id, attacker_id))
                .ok_or("Attacker not found")?;
            let defender = states.get(&defender_key).ok_or("Defender not found")?;

            // Roll to hit
            let roll = roll_d20();
            let mut result = AttackResult::new(roll, attacker.attack_bonus, defender.armor_class);

            if result.hit {
                // Calculate and apply damage
                let is_crit = result.critical;
                let defender_mut = states.get_mut(&defender_key).unwrap();
                let damage_result = defender_mut.take_damage(damage, damage_type, is_crit);
                result = result.with_damage(damage_result);
            }

            result
        };

        // Persist HP change if damage was dealt
        if result.hit {
            self.save_entity_state(universe_id, defender_id).await;
        }

        Ok(result)
    }

    /// Deal direct damage (bypassing attack roll)
    pub async fn deal_damage(
        &self,
        universe_id: &str,
        target_id: &str,
        amount: i32,
        damage_type: DamageType,
        is_crit: bool,
    ) -> Result<DamageResult, String> {
        let result = {
            let mut states = self.states.write().await;
            let target = states
                .get_mut(&ek(universe_id, target_id))
                .ok_or("Target not found")?;
            target.take_damage(amount, damage_type, is_crit)
        };

        self.save_entity_state(universe_id, target_id).await;
        Ok(result)
    }

    /// Heal an entity
    pub async fn heal(
        &self,
        universe_id: &str,
        target_id: &str,
        amount: i32,
    ) -> Result<i32, String> {
        let healed = {
            let mut states = self.states.write().await;
            let target = states
                .get_mut(&ek(universe_id, target_id))
                .ok_or("Target not found")?;
            target.heal(amount)
        };

        self.save_entity_state(universe_id, target_id).await;
        Ok(healed)
    }

    /// End combat for an entity
    pub async fn end_combat(&self, universe_id: &str, entity_id: &str) {
        let mut states = self.states.write().await;
        let key = ek(universe_id, entity_id);

        // Remove this entity from all attackers lists
        let attacker_ids: Vec<String> = states
            .get(&key)
            .map(|s| s.attackers.iter().cloned().collect())
            .unwrap_or_default();

        for attacker_id in &attacker_ids {
            if let Some(attacker) = states.get_mut(&ek(universe_id, attacker_id)) {
                if attacker.attacking.as_deref() == Some(entity_id) {
                    attacker.stop_attacking();
                }
            }
        }

        // Get who this entity was attacking
        let target_id = states.get(&key).and_then(|s| s.attacking.clone());

        if let Some(ref tid) = target_id {
            if let Some(target) = states.get_mut(&ek(universe_id, tid)) {
                target.remove_attacker(entity_id);
            }
        }

        // Clear this entity's combat state
        if let Some(state) = states.get_mut(&key) {
            state.leave_combat();
        }
    }

    /// Remove entity from combat system (on death/disconnect)
    pub async fn remove_entity(&self, universe_id: &str, entity_id: &str) {
        self.end_combat(universe_id, entity_id).await;

        if let Err(e) = self
            .write(
                "DELETE FROM combat_state WHERE universe_id = ? AND entity_id = ?",
                vec![serde_json::json!(universe_id), serde_json::json!(entity_id)],
            )
            .await
        {
            warn!("Failed to remove combat state for {}: {}", entity_id, e);
        }

        self.states
            .write()
            .await
            .remove(&ek(universe_id, entity_id));
    }

    /// Set damage modifier for an entity
    pub async fn set_damage_modifier(
        &self,
        universe_id: &str,
        entity_id: &str,
        damage_type: DamageType,
        modifier: super::damage::DamageModifier,
    ) {
        let mut states = self.states.write().await;
        if let Some(state) = states.get_mut(&ek(universe_id, entity_id)) {
            state.damage_profile.set(damage_type, modifier);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The same entity id in two universes has independent combat state
    #[tokio::test]
    async fn test_combat_state_is_per_universe() {
        let manager = CombatManager::new();
        manager.init_entity("a", "/npcs/rat", 10).await;
        manager.init_entity("b", "/npcs/rat", 10).await;
        manager.init_entity("a", "/players/p-x", 100).await;
        manager
            .deal_damage("a", "/npcs/rat", 7, DamageType::Physical, false)
            .await
            .unwrap();
        assert_eq!(manager.get_state("a", "/npcs/rat").await.unwrap().hp, 3);
        assert_eq!(manager.get_state("b", "/npcs/rat").await.unwrap().hp, 10);
        manager.remove_entity("b", "/npcs/rat").await;
        assert!(manager.get_state("a", "/npcs/rat").await.is_some());
    }

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
        manager.init_entity("u", "player1", 100).await;
        manager.init_entity("u", "goblin", 20).await;

        // Initiate combat
        manager.initiate("u", "player1", "goblin").await.unwrap();

        assert!(manager.is_in_combat("u", "player1").await);
        assert!(manager.is_in_combat("u", "goblin").await);

        // Deal damage
        let result = manager
            .deal_damage("u", "goblin", 15, DamageType::Slashing, false)
            .await
            .unwrap();
        assert_eq!(result.final_damage, 15);

        let state = manager.get_state("u", "goblin").await.unwrap();
        assert_eq!(state.hp, 5);

        // End combat
        manager.end_combat("u", "player1").await;
        assert!(!manager.is_in_combat("u", "player1").await);
    }

    #[test]
    fn test_pvp_policy_parsing() {
        assert_eq!(PvpPolicy::from_str("disabled"), Some(PvpPolicy::Disabled));
        assert_eq!(PvpPolicy::from_str("ARENA"), Some(PvpPolicy::ArenaOnly));
        assert_eq!(PvpPolicy::from_str("open"), Some(PvpPolicy::Open));
        assert_eq!(PvpPolicy::from_str("invalid"), None);
    }
}
