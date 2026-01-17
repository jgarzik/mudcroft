//! Dice rolling system
//!
//! Parses and rolls dice notation like "2d6+3", "1d20", "4d6-2"

use rand::Rng;
use std::str::FromStr;

/// A parsed dice roll specification
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiceRoll {
    /// Number of dice to roll
    pub count: u32,
    /// Number of sides per die
    pub sides: u32,
    /// Modifier to add/subtract
    pub modifier: i32,
}

impl DiceRoll {
    /// Create a new dice roll
    pub fn new(count: u32, sides: u32, modifier: i32) -> Self {
        Self { count, sides, modifier }
    }

    /// Roll the dice and return the total
    pub fn roll(&self) -> i32 {
        roll_dice(self.count, self.sides, self.modifier)
    }

    /// Roll and return individual die results plus total
    pub fn roll_detailed(&self) -> (Vec<u32>, i32) {
        let mut rng = rand::rng();
        let mut results = Vec::with_capacity(self.count as usize);

        for _ in 0..self.count {
            let roll = rng.random_range(1..=self.sides);
            results.push(roll);
        }

        let sum: u32 = results.iter().sum();
        let total = sum as i32 + self.modifier;

        (results, total)
    }

    /// Get the minimum possible result
    pub fn min(&self) -> i32 {
        self.count as i32 + self.modifier
    }

    /// Get the maximum possible result
    pub fn max(&self) -> i32 {
        (self.count * self.sides) as i32 + self.modifier
    }

    /// Get the expected average (rounded down)
    pub fn average(&self) -> i32 {
        let avg_per_die = (1.0 + self.sides as f64) / 2.0;
        (self.count as f64 * avg_per_die + self.modifier as f64) as i32
    }
}

impl FromStr for DiceRoll {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        parse_dice(s)
    }
}

impl std::fmt::Display for DiceRoll {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.modifier > 0 {
            write!(f, "{}d{}+{}", self.count, self.sides, self.modifier)
        } else if self.modifier < 0 {
            write!(f, "{}d{}{}", self.count, self.sides, self.modifier)
        } else {
            write!(f, "{}d{}", self.count, self.sides)
        }
    }
}

/// Parse a dice notation string like "2d6+3"
pub fn parse_dice(notation: &str) -> Result<DiceRoll, String> {
    let notation = notation.trim().to_lowercase();

    // Find the 'd' separator
    let d_pos = notation.find('d').ok_or("Missing 'd' in dice notation")?;

    // Parse count (before 'd')
    let count_str = &notation[..d_pos];
    let count: u32 = if count_str.is_empty() {
        1 // "d6" means "1d6"
    } else {
        count_str.parse().map_err(|_| format!("Invalid dice count: {}", count_str))?
    };

    if count == 0 {
        return Err("Dice count must be at least 1".to_string());
    }

    // Parse sides and modifier (after 'd')
    let rest = &notation[d_pos + 1..];

    // Find modifier position
    let (sides_str, modifier) = if let Some(plus_pos) = rest.find('+') {
        let sides = &rest[..plus_pos];
        let mod_str = &rest[plus_pos + 1..];
        let modifier: i32 = mod_str.parse().map_err(|_| format!("Invalid modifier: {}", mod_str))?;
        (sides, modifier)
    } else if let Some(minus_pos) = rest.rfind('-') {
        // Use rfind to handle negative modifier
        if minus_pos == 0 {
            // No modifier, just sides
            (rest, 0)
        } else {
            let sides = &rest[..minus_pos];
            let mod_str = &rest[minus_pos..]; // includes the minus sign
            let modifier: i32 = mod_str.parse().map_err(|_| format!("Invalid modifier: {}", mod_str))?;
            (sides, modifier)
        }
    } else {
        (rest, 0)
    };

    let sides: u32 = sides_str.parse().map_err(|_| format!("Invalid die sides: {}", sides_str))?;

    if sides == 0 {
        return Err("Die sides must be at least 1".to_string());
    }

    Ok(DiceRoll { count, sides, modifier })
}

/// Roll dice with the given parameters
pub fn roll_dice(count: u32, sides: u32, modifier: i32) -> i32 {
    let mut rng = rand::rng();
    let mut total: i32 = 0;

    for _ in 0..count {
        total += rng.random_range(1..=sides) as i32;
    }

    total + modifier
}

/// Roll a single d20
pub fn roll_d20() -> u32 {
    rand::rng().random_range(1..=20)
}

/// Check if a d20 roll is a natural 20 (critical hit)
pub fn is_critical(roll: u32) -> bool {
    roll == 20
}

/// Check if a d20 roll is a natural 1 (critical fail)
pub fn is_fumble(roll: u32) -> bool {
    roll == 1
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_basic() {
        let roll = parse_dice("2d6").unwrap();
        assert_eq!(roll.count, 2);
        assert_eq!(roll.sides, 6);
        assert_eq!(roll.modifier, 0);
    }

    #[test]
    fn test_parse_with_plus() {
        let roll = parse_dice("1d20+5").unwrap();
        assert_eq!(roll.count, 1);
        assert_eq!(roll.sides, 20);
        assert_eq!(roll.modifier, 5);
    }

    #[test]
    fn test_parse_with_minus() {
        let roll = parse_dice("3d8-2").unwrap();
        assert_eq!(roll.count, 3);
        assert_eq!(roll.sides, 8);
        assert_eq!(roll.modifier, -2);
    }

    #[test]
    fn test_parse_implicit_one() {
        let roll = parse_dice("d6").unwrap();
        assert_eq!(roll.count, 1);
        assert_eq!(roll.sides, 6);
    }

    #[test]
    fn test_parse_whitespace() {
        let roll = parse_dice("  2d10+3  ").unwrap();
        assert_eq!(roll.count, 2);
        assert_eq!(roll.sides, 10);
        assert_eq!(roll.modifier, 3);
    }

    #[test]
    fn test_parse_case_insensitive() {
        let roll = parse_dice("2D6+1").unwrap();
        assert_eq!(roll.count, 2);
        assert_eq!(roll.sides, 6);
    }

    #[test]
    fn test_parse_invalid() {
        assert!(parse_dice("abc").is_err());
        assert!(parse_dice("2d").is_err());
        assert!(parse_dice("d").is_err());
        assert!(parse_dice("0d6").is_err());
        assert!(parse_dice("2d0").is_err());
    }

    #[test]
    fn test_roll_bounds() {
        let roll = DiceRoll::new(2, 6, 0);

        // Roll many times and check bounds
        for _ in 0..100 {
            let result = roll.roll();
            assert!(result >= 2, "Roll {} below minimum 2", result);
            assert!(result <= 12, "Roll {} above maximum 12", result);
        }
    }

    #[test]
    fn test_roll_with_modifier() {
        let roll = DiceRoll::new(1, 6, 5);

        for _ in 0..100 {
            let result = roll.roll();
            assert!(result >= 6, "Roll {} below minimum 6", result);
            assert!(result <= 11, "Roll {} above maximum 11", result);
        }
    }

    #[test]
    fn test_min_max_average() {
        let roll = DiceRoll::new(2, 6, 3);
        assert_eq!(roll.min(), 5);  // 2 + 3
        assert_eq!(roll.max(), 15); // 12 + 3
        assert_eq!(roll.average(), 10); // 7 + 3
    }

    #[test]
    fn test_display() {
        assert_eq!(DiceRoll::new(2, 6, 0).to_string(), "2d6");
        assert_eq!(DiceRoll::new(1, 20, 5).to_string(), "1d20+5");
        assert_eq!(DiceRoll::new(3, 8, -2).to_string(), "3d8-2");
    }

    #[test]
    fn test_detailed_roll() {
        let roll = DiceRoll::new(3, 6, 2);
        let (dice, total) = roll.roll_detailed();

        assert_eq!(dice.len(), 3);
        for d in &dice {
            assert!(*d >= 1 && *d <= 6);
        }

        let sum: u32 = dice.iter().sum();
        assert_eq!(total, sum as i32 + 2);
    }

    #[test]
    fn test_critical_fumble() {
        assert!(is_critical(20));
        assert!(!is_critical(19));
        assert!(is_fumble(1));
        assert!(!is_fumble(2));
    }
}
