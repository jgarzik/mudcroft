//! Attack results and PvP policy
//!
//! HP and combat stats live on the object rows (see `engine`); nothing here
//! holds per-entity state.

use super::damage::DamageResult;
use super::dice::{is_critical, is_fumble};

/// PvP policy for a universe
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PvpPolicy {
    /// PvP is completely disabled
    Disabled,
    /// PvP only in designated arenas (rooms with `is_arena`): the default
    #[default]
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

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn test_pvp_policy_parsing() {
        assert_eq!(PvpPolicy::from_str("disabled"), Some(PvpPolicy::Disabled));
        assert_eq!(PvpPolicy::from_str("ARENA"), Some(PvpPolicy::ArenaOnly));
        assert_eq!(PvpPolicy::from_str("open"), Some(PvpPolicy::Open));
        assert_eq!(PvpPolicy::from_str("invalid"), None);
    }
}
