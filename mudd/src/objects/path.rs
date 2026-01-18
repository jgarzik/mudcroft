//! Object path validation and utilities
//!
//! Object paths follow filesystem-style naming:
//! - Must start with `/`
//! - Segments separated by `/`
//! - Each segment: `[a-z][a-z0-9-]*` (starts with letter, then lowercase alphanumeric and hyphens)
//! - Max 255 characters total
//! - Normalized to lowercase

use regex::Regex;
use std::fmt;
use std::sync::LazyLock;

/// Validation errors for object paths
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PathValidationError {
    /// Path is empty
    Empty,
    /// Path doesn't start with `/`
    MissingLeadingSlash,
    /// Path is too long (> 255 chars)
    TooLong,
    /// Path contains empty segment (consecutive slashes)
    EmptySegment,
    /// Segment contains invalid characters or format
    InvalidSegment(String),
    /// Segment doesn't start with a letter
    SegmentStartsWithNonLetter(String),
}

impl fmt::Display for PathValidationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PathValidationError::Empty => {
                write!(f, "Object path cannot be empty")
            }
            PathValidationError::MissingLeadingSlash => {
                write!(f, "Object path must start with '/'")
            }
            PathValidationError::TooLong => {
                write!(f, "Object path must be 255 characters or less")
            }
            PathValidationError::EmptySegment => {
                write!(
                    f,
                    "Object path cannot contain empty segments (consecutive slashes)"
                )
            }
            PathValidationError::InvalidSegment(seg) => {
                write!(
                    f,
                    "Segment '{}' contains invalid characters (allowed: lowercase letters, digits, hyphens)",
                    seg
                )
            }
            PathValidationError::SegmentStartsWithNonLetter(seg) => {
                write!(f, "Segment '{}' must start with a letter", seg)
            }
        }
    }
}

impl std::error::Error for PathValidationError {}

/// Segment pattern: starts with letter, then lowercase alphanumeric and hyphens
static SEGMENT_REGEX: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[a-z][a-z0-9-]*$").unwrap());

/// Validate and normalize an object path.
///
/// # Rules
/// - Must start with `/`
/// - Max 255 characters
/// - Segments separated by `/`
/// - Each segment: starts with letter, then `[a-z0-9-]*`
/// - Normalized to lowercase
///
/// # Returns
/// - `Ok(normalized_path)` - Lowercase normalized path
/// - `Err(PathValidationError)` - Specific validation failure
///
/// # Examples
/// ```
/// use mudd::objects::validate_object_path;
///
/// assert!(validate_object_path("/rooms/dark-cave").is_ok());
/// assert!(validate_object_path("/items/sword1").is_ok());
/// assert!(validate_object_path("/Rooms/DarkCave").is_ok()); // Normalized to "/rooms/darkcave"
///
/// assert!(validate_object_path("no-leading-slash").is_err());
/// assert!(validate_object_path("/bad//path").is_err());     // Empty segment
/// assert!(validate_object_path("/123-invalid").is_err());   // Starts with digit
/// ```
pub fn validate_object_path(path: &str) -> Result<String, PathValidationError> {
    // Empty check
    if path.is_empty() {
        return Err(PathValidationError::Empty);
    }

    // Normalize to lowercase
    let normalized = path.to_lowercase();

    // Must start with /
    if !normalized.starts_with('/') {
        return Err(PathValidationError::MissingLeadingSlash);
    }

    // Length check
    if normalized.len() > 255 {
        return Err(PathValidationError::TooLong);
    }

    // Split into segments (skip first empty segment from leading /)
    let segments: Vec<&str> = normalized[1..].split('/').collect();

    // Check for empty segments (consecutive slashes or trailing slash)
    for segment in &segments {
        if segment.is_empty() {
            return Err(PathValidationError::EmptySegment);
        }
    }

    // Validate each segment
    for segment in &segments {
        // Check if starts with letter
        if !segment
            .chars()
            .next()
            .map(|c| c.is_ascii_lowercase())
            .unwrap_or(false)
        {
            return Err(PathValidationError::SegmentStartsWithNonLetter(
                segment.to_string(),
            ));
        }

        // Check full segment pattern
        if !SEGMENT_REGEX.is_match(segment) {
            return Err(PathValidationError::InvalidSegment(segment.to_string()));
        }
    }

    Ok(normalized)
}

/// Extract the parent path from an object path.
///
/// # Examples
/// ```
/// use mudd::objects::parent_path;
///
/// assert_eq!(parent_path("/rooms/cave/big-room"), Some("/rooms/cave".to_string()));
/// assert_eq!(parent_path("/rooms"), None);
/// assert_eq!(parent_path("/a"), None);
/// ```
pub fn parent_path(path: &str) -> Option<String> {
    if let Some(last_slash) = path.rfind('/') {
        if last_slash > 0 {
            return Some(path[..last_slash].to_string());
        }
    }
    None
}

/// Extract the final segment (name) from an object path.
///
/// # Examples
/// ```
/// use mudd::objects::path_name;
///
/// assert_eq!(path_name("/rooms/cave/big-room"), "big-room");
/// assert_eq!(path_name("/rooms"), "rooms");
/// ```
pub fn path_name(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_valid_paths() {
        // Simple paths
        assert_eq!(validate_object_path("/rooms").unwrap(), "/rooms");
        assert_eq!(
            validate_object_path("/rooms/dark-cave").unwrap(),
            "/rooms/dark-cave"
        );
        assert_eq!(
            validate_object_path("/regions/area1/room2").unwrap(),
            "/regions/area1/room2"
        );

        // With numbers
        assert_eq!(validate_object_path("/room1").unwrap(), "/room1");
        assert_eq!(
            validate_object_path("/items/sword123").unwrap(),
            "/items/sword123"
        );

        // Case normalization
        assert_eq!(validate_object_path("/Rooms").unwrap(), "/rooms");
        assert_eq!(
            validate_object_path("/Rooms/DarkCave").unwrap(),
            "/rooms/darkcave"
        );
        assert_eq!(
            validate_object_path("/ITEMS/SWORD").unwrap(),
            "/items/sword"
        );
    }

    #[test]
    fn test_empty_path() {
        assert_eq!(validate_object_path(""), Err(PathValidationError::Empty));
    }

    #[test]
    fn test_missing_leading_slash() {
        assert_eq!(
            validate_object_path("rooms"),
            Err(PathValidationError::MissingLeadingSlash)
        );
        assert_eq!(
            validate_object_path("rooms/cave"),
            Err(PathValidationError::MissingLeadingSlash)
        );
    }

    #[test]
    fn test_too_long() {
        let long_path = format!("/{}", "a".repeat(255));
        assert_eq!(
            validate_object_path(&long_path),
            Err(PathValidationError::TooLong)
        );

        // Max length (255) should work
        let max_path = format!("/{}", "a".repeat(253));
        assert!(validate_object_path(&max_path).is_ok());
    }

    #[test]
    fn test_empty_segment() {
        // Consecutive slashes
        assert_eq!(
            validate_object_path("/rooms//cave"),
            Err(PathValidationError::EmptySegment)
        );
        // Trailing slash
        assert_eq!(
            validate_object_path("/rooms/"),
            Err(PathValidationError::EmptySegment)
        );
        // Just a slash
        assert_eq!(
            validate_object_path("/"),
            Err(PathValidationError::EmptySegment)
        );
    }

    #[test]
    fn test_segment_starts_with_non_letter() {
        assert_eq!(
            validate_object_path("/123"),
            Err(PathValidationError::SegmentStartsWithNonLetter(
                "123".to_string()
            ))
        );
        assert_eq!(
            validate_object_path("/rooms/1cave"),
            Err(PathValidationError::SegmentStartsWithNonLetter(
                "1cave".to_string()
            ))
        );
        assert_eq!(
            validate_object_path("/-invalid"),
            Err(PathValidationError::SegmentStartsWithNonLetter(
                "-invalid".to_string()
            ))
        );
    }

    #[test]
    fn test_invalid_segment_chars() {
        // Underscores not allowed
        assert_eq!(
            validate_object_path("/my_room"),
            Err(PathValidationError::InvalidSegment("my_room".to_string()))
        );
        // Spaces not allowed
        assert_eq!(
            validate_object_path("/my room"),
            Err(PathValidationError::InvalidSegment("my room".to_string()))
        );
        // Special chars not allowed
        assert_eq!(
            validate_object_path("/room@1"),
            Err(PathValidationError::InvalidSegment("room@1".to_string()))
        );
    }

    #[test]
    fn test_parent_path() {
        assert_eq!(
            parent_path("/rooms/cave/big-room"),
            Some("/rooms/cave".to_string())
        );
        assert_eq!(parent_path("/rooms/cave"), Some("/rooms".to_string()));
        assert_eq!(parent_path("/rooms"), None);
        assert_eq!(parent_path("/a"), None);
    }

    #[test]
    fn test_path_name() {
        assert_eq!(path_name("/rooms/cave/big-room"), "big-room");
        assert_eq!(path_name("/rooms/cave"), "cave");
        assert_eq!(path_name("/rooms"), "rooms");
    }
}
