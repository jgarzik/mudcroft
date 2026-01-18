//! Object types and core structures

use super::path::{validate_object_path, PathValidationError};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Unique object identifier (path-based, e.g., "/rooms/dark-cave")
pub type ObjectId = String;

/// Properties are stored as JSON-compatible key-value pairs
pub type Properties = HashMap<String, serde_json::Value>;

/// An in-game object instance
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Object {
    /// Path-based identifier (e.g., "/rooms/dark-cave")
    pub id: ObjectId,
    /// Universe this object belongs to
    pub universe_id: String,
    /// Class name (e.g., "sword", "room", "player")
    pub class: String,
    /// Parent object (container) ID, if any
    pub parent_id: Option<ObjectId>,
    /// Instance properties (override class defaults)
    pub properties: Properties,
    /// SHA-256 hash of attached code, if any
    pub code_hash: Option<String>,
    /// Account ID of the creator (for ownership-based permissions)
    pub owner_id: Option<String>,
    /// Creation timestamp
    pub created_at: String,
    /// Last update timestamp
    pub updated_at: String,
}

impl Object {
    /// Create a new object with the given path and class
    ///
    /// # Arguments
    /// * `id` - Path-based identifier (e.g., "/rooms/dark-cave")
    /// * `universe_id` - Universe this object belongs to
    /// * `class` - Class name (e.g., "sword", "room", "player")
    ///
    /// # Returns
    /// * `Ok(Object)` - New object with validated path
    /// * `Err(PathValidationError)` - If path is invalid
    pub fn new(id: &str, universe_id: &str, class: &str) -> Result<Self, PathValidationError> {
        let validated_id = validate_object_path(id)?;
        let now = chrono::Utc::now().to_rfc3339();
        Ok(Self {
            id: validated_id,
            universe_id: universe_id.to_string(),
            class: class.to_string(),
            parent_id: None,
            properties: Properties::new(),
            code_hash: None,
            owner_id: None,
            created_at: now.clone(),
            updated_at: now,
        })
    }

    /// Create a new object with an owner
    pub fn new_with_owner(
        id: &str,
        universe_id: &str,
        class: &str,
        owner_id: &str,
    ) -> Result<Self, PathValidationError> {
        let mut obj = Self::new(id, universe_id, class)?;
        obj.owner_id = Some(owner_id.to_string());
        Ok(obj)
    }

    /// Get a property value, returning None if not set
    pub fn get_property(&self, key: &str) -> Option<&serde_json::Value> {
        self.properties.get(key)
    }

    /// Set a property value
    pub fn set_property(&mut self, key: &str, value: serde_json::Value) {
        self.properties.insert(key.to_string(), value);
        self.updated_at = chrono::Utc::now().to_rfc3339();
    }

    /// Remove a property
    pub fn remove_property(&mut self, key: &str) -> Option<serde_json::Value> {
        let result = self.properties.remove(key);
        if result.is_some() {
            self.updated_at = chrono::Utc::now().to_rfc3339();
        }
        result
    }

    /// Check if object has a property set
    pub fn has_property(&self, key: &str) -> bool {
        self.properties.contains_key(key)
    }

    /// Get property as string
    pub fn get_string(&self, key: &str) -> Option<&str> {
        self.properties.get(key).and_then(|v| v.as_str())
    }

    /// Get property as i64
    pub fn get_i64(&self, key: &str) -> Option<i64> {
        self.properties.get(key).and_then(|v| v.as_i64())
    }

    /// Get property as f64
    pub fn get_f64(&self, key: &str) -> Option<f64> {
        self.properties.get(key).and_then(|v| v.as_f64())
    }

    /// Get property as bool
    pub fn get_bool(&self, key: &str) -> Option<bool> {
        self.properties.get(key).and_then(|v| v.as_bool())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_object_creation() {
        let obj = Object::new("/items/sword", "test-universe", "sword").unwrap();
        assert_eq!(obj.id, "/items/sword");
        assert_eq!(obj.class, "sword");
        assert_eq!(obj.universe_id, "test-universe");
        assert!(obj.parent_id.is_none());
        assert!(obj.properties.is_empty());
    }

    #[test]
    fn test_object_creation_invalid_path() {
        // Missing leading slash
        assert!(Object::new("invalid", "test-universe", "item").is_err());
        // Empty path
        assert!(Object::new("", "test-universe", "item").is_err());
        // Invalid characters
        assert!(Object::new("/bad_path", "test-universe", "item").is_err());
    }

    #[test]
    fn test_object_path_normalization() {
        let obj = Object::new("/Items/Sword", "test-universe", "sword").unwrap();
        assert_eq!(obj.id, "/items/sword"); // Normalized to lowercase
    }

    #[test]
    fn test_properties() {
        let mut obj = Object::new("/items/magic-sword", "test-universe", "item").unwrap();

        obj.set_property("name", serde_json::json!("Magic Sword"));
        obj.set_property("damage", serde_json::json!(10));
        obj.set_property("magical", serde_json::json!(true));

        assert_eq!(obj.get_string("name"), Some("Magic Sword"));
        assert_eq!(obj.get_i64("damage"), Some(10));
        assert_eq!(obj.get_bool("magical"), Some(true));
        assert!(obj.has_property("name"));
        assert!(!obj.has_property("nonexistent"));
    }

    #[test]
    fn test_remove_property() {
        let mut obj = Object::new("/items/temp-item", "test-universe", "item").unwrap();
        obj.set_property("temp", serde_json::json!(42));

        let removed = obj.remove_property("temp");
        assert!(removed.is_some());
        assert!(!obj.has_property("temp"));
    }
}
