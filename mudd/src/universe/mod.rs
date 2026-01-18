//! Universe ID validation and utilities
//!
//! Universe IDs follow DNS subdomain-style naming:
//! - 3-64 characters
//! - Lowercase alphanumeric and hyphens
//! - Must start and end with alphanumeric
//! - No consecutive hyphens

use regex::Regex;
use std::fmt;
use std::sync::LazyLock;

/// Validation errors for universe IDs
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ValidationError {
    /// ID is too short (< 3 chars) or too long (> 64 chars)
    Length,
    /// ID contains invalid characters or format
    InvalidFormat,
    /// ID contains consecutive hyphens (--)
    ConsecutiveHyphens,
}

impl fmt::Display for ValidationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ValidationError::Length => {
                write!(f, "Universe ID must be 3-64 characters")
            }
            ValidationError::InvalidFormat => {
                write!(
                    f,
                    "Universe ID must be lowercase alphanumeric, may contain hyphens, and must start/end with alphanumeric"
                )
            }
            ValidationError::ConsecutiveHyphens => {
                write!(f, "Universe ID cannot contain consecutive hyphens (--)")
            }
        }
    }
}

impl std::error::Error for ValidationError {}

/// DNS subdomain-style pattern
static UNIVERSE_ID_REGEX: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[a-z0-9]([a-z0-9-]*[a-z0-9])?$").unwrap());

/// Validate and normalize a universe ID.
///
/// # Rules
/// - Length: 3-64 characters
/// - Pattern: DNS subdomain style (lowercase alphanumeric and hyphens)
/// - Must start and end with alphanumeric character
/// - No consecutive hyphens (--)
///
/// # Returns
/// - `Ok(normalized_id)` - Lowercase normalized ID
/// - `Err(ValidationError)` - Specific validation failure
///
/// # Examples
/// ```
/// use mudd::universe::validate_universe_id;
///
/// assert!(validate_universe_id("my-game").is_ok());
/// assert!(validate_universe_id("test123").is_ok());
/// assert!(validate_universe_id("My-Game").is_ok()); // Normalized to "my-game"
///
/// assert!(validate_universe_id("ab").is_err());           // Too short
/// assert!(validate_universe_id("-invalid").is_err());     // Starts with hyphen
/// assert!(validate_universe_id("test--bad").is_err());    // Consecutive hyphens
/// ```
pub fn validate_universe_id(id: &str) -> Result<String, ValidationError> {
    let normalized = id.to_lowercase();

    // Length check
    if normalized.len() < 3 || normalized.len() > 64 {
        return Err(ValidationError::Length);
    }

    // Regex: DNS subdomain style
    if !UNIVERSE_ID_REGEX.is_match(&normalized) {
        return Err(ValidationError::InvalidFormat);
    }

    // No consecutive hyphens
    if normalized.contains("--") {
        return Err(ValidationError::ConsecutiveHyphens);
    }

    Ok(normalized)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_valid_ids() {
        // Valid simple IDs
        assert_eq!(validate_universe_id("abc").unwrap(), "abc");
        assert_eq!(validate_universe_id("my-game").unwrap(), "my-game");
        assert_eq!(validate_universe_id("test123").unwrap(), "test123");
        assert_eq!(validate_universe_id("rpg-world-2").unwrap(), "rpg-world-2");

        // Case normalization
        assert_eq!(validate_universe_id("My-Game").unwrap(), "my-game");
        assert_eq!(validate_universe_id("TEST").unwrap(), "test");

        // Edge cases
        assert_eq!(validate_universe_id("a1b").unwrap(), "a1b"); // Minimum length
        let long_id = "a".repeat(64);
        assert_eq!(validate_universe_id(&long_id).unwrap(), long_id); // Maximum length
    }

    #[test]
    fn test_length_errors() {
        // Too short
        assert_eq!(validate_universe_id(""), Err(ValidationError::Length));
        assert_eq!(validate_universe_id("a"), Err(ValidationError::Length));
        assert_eq!(validate_universe_id("ab"), Err(ValidationError::Length));

        // Too long
        let too_long = "a".repeat(65);
        assert_eq!(
            validate_universe_id(&too_long),
            Err(ValidationError::Length)
        );
    }

    #[test]
    fn test_format_errors() {
        // Starts with hyphen
        assert_eq!(
            validate_universe_id("-invalid"),
            Err(ValidationError::InvalidFormat)
        );

        // Ends with hyphen
        assert_eq!(
            validate_universe_id("invalid-"),
            Err(ValidationError::InvalidFormat)
        );

        // Contains spaces
        assert_eq!(
            validate_universe_id("my game"),
            Err(ValidationError::InvalidFormat)
        );

        // Contains underscores
        assert_eq!(
            validate_universe_id("my_game"),
            Err(ValidationError::InvalidFormat)
        );

        // Contains special characters
        assert_eq!(
            validate_universe_id("my@game"),
            Err(ValidationError::InvalidFormat)
        );

        // Ends with hyphen (valid length)
        assert_eq!(
            validate_universe_id("abc-"),
            Err(ValidationError::InvalidFormat)
        );
    }

    #[test]
    fn test_consecutive_hyphens() {
        assert_eq!(
            validate_universe_id("test--bad"),
            Err(ValidationError::ConsecutiveHyphens)
        );
        assert_eq!(
            validate_universe_id("a---b"),
            Err(ValidationError::ConsecutiveHyphens)
        );
    }
}
