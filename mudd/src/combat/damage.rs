//! Damage types and modifiers
//!
//! Handles damage calculation with:
//! - Multiple damage types (fire, ice, poison, etc.)
//! - Immunity (0% damage)
//! - Resistance (50% damage)
//! - Vulnerability (200% damage)

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::str::FromStr;

/// Types of damage
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum DamageType {
    /// Physical damage (slashing, piercing, bludgeoning)
    Physical,
    /// Slashing damage (swords, claws)
    Slashing,
    /// Piercing damage (arrows, spears)
    Piercing,
    /// Bludgeoning damage (maces, hammers)
    Bludgeoning,
    /// Fire damage
    Fire,
    /// Cold/ice damage
    Cold,
    /// Lightning/electric damage
    Lightning,
    /// Acid damage
    Acid,
    /// Poison damage
    Poison,
    /// Necrotic/death damage
    Necrotic,
    /// Radiant/holy damage
    Radiant,
    /// Psychic/mental damage
    Psychic,
    /// Force/magic damage
    Force,
    /// Thunder/sonic damage
    Thunder,
}

impl DamageType {
    /// Get all damage types
    pub fn all() -> &'static [DamageType] {
        &[
            DamageType::Physical,
            DamageType::Slashing,
            DamageType::Piercing,
            DamageType::Bludgeoning,
            DamageType::Fire,
            DamageType::Cold,
            DamageType::Lightning,
            DamageType::Acid,
            DamageType::Poison,
            DamageType::Necrotic,
            DamageType::Radiant,
            DamageType::Psychic,
            DamageType::Force,
            DamageType::Thunder,
        ]
    }
}

impl FromStr for DamageType {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "physical" => Ok(DamageType::Physical),
            "slashing" => Ok(DamageType::Slashing),
            "piercing" => Ok(DamageType::Piercing),
            "bludgeoning" => Ok(DamageType::Bludgeoning),
            "fire" => Ok(DamageType::Fire),
            "cold" | "ice" => Ok(DamageType::Cold),
            "lightning" | "electric" => Ok(DamageType::Lightning),
            "acid" => Ok(DamageType::Acid),
            "poison" => Ok(DamageType::Poison),
            "necrotic" | "death" => Ok(DamageType::Necrotic),
            "radiant" | "holy" => Ok(DamageType::Radiant),
            "psychic" | "mental" => Ok(DamageType::Psychic),
            "force" | "magic" => Ok(DamageType::Force),
            "thunder" | "sonic" => Ok(DamageType::Thunder),
            _ => Err(()),
        }
    }
}

impl std::fmt::Display for DamageType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            DamageType::Physical => "physical",
            DamageType::Slashing => "slashing",
            DamageType::Piercing => "piercing",
            DamageType::Bludgeoning => "bludgeoning",
            DamageType::Fire => "fire",
            DamageType::Cold => "cold",
            DamageType::Lightning => "lightning",
            DamageType::Acid => "acid",
            DamageType::Poison => "poison",
            DamageType::Necrotic => "necrotic",
            DamageType::Radiant => "radiant",
            DamageType::Psychic => "psychic",
            DamageType::Force => "force",
            DamageType::Thunder => "thunder",
        };
        write!(f, "{}", s)
    }
}

/// Modifier for damage resistance/immunity/vulnerability
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum DamageModifier {
    /// Immune - takes 0% damage
    Immune,
    /// Resistant - takes 50% damage (rounded down)
    Resistant,
    /// Normal - takes 100% damage
    Normal,
    /// Vulnerable - takes 200% damage
    Vulnerable,
}

impl DamageModifier {
    /// Apply this modifier to damage amount
    pub fn apply(&self, damage: i32) -> i32 {
        match self {
            DamageModifier::Immune => 0,
            DamageModifier::Resistant => damage / 2,
            DamageModifier::Normal => damage,
            DamageModifier::Vulnerable => damage * 2,
        }
    }

    /// Get the multiplier as a percentage
    pub fn percentage(&self) -> u32 {
        match self {
            DamageModifier::Immune => 0,
            DamageModifier::Resistant => 50,
            DamageModifier::Normal => 100,
            DamageModifier::Vulnerable => 200,
        }
    }
}

/// Result of a damage calculation
#[derive(Debug, Clone)]
pub struct DamageResult {
    /// Original damage before modifiers
    pub base_damage: i32,
    /// Final damage after modifiers
    pub final_damage: i32,
    /// Type of damage dealt
    pub damage_type: DamageType,
    /// Modifier applied
    pub modifier: DamageModifier,
    /// Whether this was a critical hit
    pub is_critical: bool,
}

impl DamageResult {
    /// Create a new damage result
    pub fn new(base: i32, dtype: DamageType, modifier: DamageModifier, is_crit: bool) -> Self {
        let crit_damage = if is_crit { base * 2 } else { base };
        let final_damage = modifier.apply(crit_damage);

        Self {
            base_damage: base,
            final_damage,
            damage_type: dtype,
            modifier,
            is_critical: is_crit,
        }
    }
}

/// Damage profile for an entity (their resistances/immunities)
#[derive(Debug, Clone, Default)]
pub struct DamageProfile {
    modifiers: HashMap<DamageType, DamageModifier>,
}

impl DamageProfile {
    /// Create a new empty damage profile (all normal)
    pub fn new() -> Self {
        Self::default()
    }

    /// Set a damage modifier for a type
    pub fn set(&mut self, dtype: DamageType, modifier: DamageModifier) {
        if modifier == DamageModifier::Normal {
            self.modifiers.remove(&dtype);
        } else {
            self.modifiers.insert(dtype, modifier);
        }
    }

    /// Get the modifier for a damage type
    pub fn get(&self, dtype: DamageType) -> DamageModifier {
        self.modifiers
            .get(&dtype)
            .copied()
            .unwrap_or(DamageModifier::Normal)
    }

    /// Add an immunity
    pub fn add_immunity(&mut self, dtype: DamageType) {
        self.set(dtype, DamageModifier::Immune);
    }

    /// Add a resistance
    pub fn add_resistance(&mut self, dtype: DamageType) {
        self.set(dtype, DamageModifier::Resistant);
    }

    /// Add a vulnerability
    pub fn add_vulnerability(&mut self, dtype: DamageType) {
        self.set(dtype, DamageModifier::Vulnerable);
    }

    /// Calculate damage after applying modifiers
    pub fn calculate_damage(&self, base: i32, dtype: DamageType, is_crit: bool) -> DamageResult {
        let modifier = self.get(dtype);
        DamageResult::new(base, dtype, modifier, is_crit)
    }

    /// Get all immunities
    pub fn immunities(&self) -> Vec<DamageType> {
        self.modifiers
            .iter()
            .filter(|(_, m)| **m == DamageModifier::Immune)
            .map(|(t, _)| *t)
            .collect()
    }

    /// Get all resistances
    pub fn resistances(&self) -> Vec<DamageType> {
        self.modifiers
            .iter()
            .filter(|(_, m)| **m == DamageModifier::Resistant)
            .map(|(t, _)| *t)
            .collect()
    }

    /// Get all vulnerabilities
    pub fn vulnerabilities(&self) -> Vec<DamageType> {
        self.modifiers
            .iter()
            .filter(|(_, m)| **m == DamageModifier::Vulnerable)
            .map(|(t, _)| *t)
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_damage_modifier_apply() {
        assert_eq!(DamageModifier::Immune.apply(10), 0);
        assert_eq!(DamageModifier::Resistant.apply(10), 5);
        assert_eq!(DamageModifier::Normal.apply(10), 10);
        assert_eq!(DamageModifier::Vulnerable.apply(10), 20);
    }

    #[test]
    fn test_damage_profile() {
        let mut profile = DamageProfile::new();

        // Default is normal
        assert_eq!(profile.get(DamageType::Fire), DamageModifier::Normal);

        // Add immunity
        profile.add_immunity(DamageType::Fire);
        assert_eq!(profile.get(DamageType::Fire), DamageModifier::Immune);

        // Add resistance
        profile.add_resistance(DamageType::Cold);
        assert_eq!(profile.get(DamageType::Cold), DamageModifier::Resistant);

        // Add vulnerability
        profile.add_vulnerability(DamageType::Poison);
        assert_eq!(profile.get(DamageType::Poison), DamageModifier::Vulnerable);
    }

    #[test]
    fn test_calculate_damage() {
        let mut profile = DamageProfile::new();
        profile.add_immunity(DamageType::Fire);
        profile.add_resistance(DamageType::Cold);
        profile.add_vulnerability(DamageType::Lightning);

        // Immune = 0 damage
        let result = profile.calculate_damage(10, DamageType::Fire, false);
        assert_eq!(result.final_damage, 0);

        // Resistant = 50% damage
        let result = profile.calculate_damage(10, DamageType::Cold, false);
        assert_eq!(result.final_damage, 5);

        // Normal = 100% damage
        let result = profile.calculate_damage(10, DamageType::Acid, false);
        assert_eq!(result.final_damage, 10);

        // Vulnerable = 200% damage
        let result = profile.calculate_damage(10, DamageType::Lightning, false);
        assert_eq!(result.final_damage, 20);
    }

    #[test]
    fn test_critical_damage() {
        let profile = DamageProfile::new();

        // Normal crit = double damage
        let result = profile.calculate_damage(10, DamageType::Physical, true);
        assert!(result.is_critical);
        assert_eq!(result.final_damage, 20);

        // Crit with resistance = double then halve = normal
        let mut profile2 = DamageProfile::new();
        profile2.add_resistance(DamageType::Fire);
        let result2 = profile2.calculate_damage(10, DamageType::Fire, true);
        assert_eq!(result2.final_damage, 10); // 10 * 2 / 2 = 10
    }

    #[test]
    fn test_damage_type_parsing() {
        assert_eq!("fire".parse::<DamageType>(), Ok(DamageType::Fire));
        assert_eq!("FIRE".parse::<DamageType>(), Ok(DamageType::Fire));
        assert_eq!("ice".parse::<DamageType>(), Ok(DamageType::Cold));
        assert!("invalid".parse::<DamageType>().is_err());
    }

    #[test]
    fn test_list_modifiers() {
        let mut profile = DamageProfile::new();
        profile.add_immunity(DamageType::Fire);
        profile.add_immunity(DamageType::Cold);
        profile.add_resistance(DamageType::Lightning);
        profile.add_vulnerability(DamageType::Poison);

        let immunities = profile.immunities();
        assert_eq!(immunities.len(), 2);
        assert!(immunities.contains(&DamageType::Fire));
        assert!(immunities.contains(&DamageType::Cold));

        let resistances = profile.resistances();
        assert_eq!(resistances.len(), 1);
        assert!(resistances.contains(&DamageType::Lightning));

        let vulnerabilities = profile.vulnerabilities();
        assert_eq!(vulnerabilities.len(), 1);
        assert!(vulnerabilities.contains(&DamageType::Poison));
    }
}
