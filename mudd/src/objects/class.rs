//! Class system with inheritance

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use super::Properties;

/// A class definition with properties and handlers
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClassDef {
    /// Class name
    pub name: String,
    /// Parent class name (None for root "thing" class)
    pub parent: Option<String>,
    /// Default properties for instances of this class
    pub properties: Properties,
    /// Handler names defined by this class (e.g., ["on_init", "on_use"])
    pub handlers: Vec<String>,
    /// Code hash for this class's Lua implementation
    pub code_hash: Option<String>,
}

impl ClassDef {
    /// Create a new class definition
    pub fn new(name: &str, parent: Option<&str>) -> Self {
        Self {
            name: name.to_string(),
            parent: parent.map(|s| s.to_string()),
            properties: Properties::new(),
            handlers: Vec::new(),
            code_hash: None,
        }
    }

    /// Set a default property
    pub fn set_property(&mut self, key: &str, value: serde_json::Value) {
        self.properties.insert(key.to_string(), value);
    }

    /// Add a handler name
    pub fn add_handler(&mut self, name: &str) {
        if !self.handlers.contains(&name.to_string()) {
            self.handlers.push(name.to_string());
        }
    }

    /// Check if class has a handler
    pub fn has_handler(&self, name: &str) -> bool {
        self.handlers.contains(&name.to_string())
    }
}

/// Registry of all class definitions
#[derive(Debug, Default)]
pub struct ClassRegistry {
    classes: HashMap<String, ClassDef>,
}

impl ClassRegistry {
    /// Create a new registry with base classes
    pub fn new() -> Self {
        let mut registry = Self {
            classes: HashMap::new(),
        };
        registry.register_base_classes();
        registry
    }

    /// Register the base classes from design.md
    fn register_base_classes(&mut self) {
        // thing - root class
        let mut thing = ClassDef::new("thing", None);
        thing.set_property("name", serde_json::json!(""));
        thing.set_property("description", serde_json::json!(""));
        thing.add_handler("on_create");
        thing.add_handler("on_destroy");
        thing.add_handler("on_init");
        self.register(thing);

        // item - inherits from thing
        let mut item = ClassDef::new("item", Some("thing"));
        item.set_property("weight", serde_json::json!(0));
        item.set_property("value", serde_json::json!(0));
        item.set_property("fixed", serde_json::json!(false));
        item.add_handler("on_move");
        item.add_handler("on_use");
        self.register(item);

        // living - mixin for combat
        let mut living = ClassDef::new("living", Some("thing"));
        living.set_property("hp", serde_json::json!(100));
        living.set_property("max_hp", serde_json::json!(100));
        living.set_property("attack_bonus", serde_json::json!(0));
        living.set_property("armor_class", serde_json::json!(10));
        living.set_property("in_combat", serde_json::json!(false));
        living.add_handler("heart_beat");
        living.add_handler("on_damage");
        living.add_handler("on_death");
        self.register(living);

        // room - inherits from thing
        let mut room = ClassDef::new("room", Some("thing"));
        room.set_property("exits", serde_json::json!({}));
        room.set_property("lighting", serde_json::json!("normal"));
        room.set_property("region_id", serde_json::json!(null));
        room.add_handler("on_enter");
        room.add_handler("on_leave");
        self.register(room);

        // region - inherits from thing
        let mut region = ClassDef::new("region", Some("thing"));
        region.set_property("environment_type", serde_json::json!("dungeon"));
        region.set_property("danger_level", serde_json::json!(1));
        region.set_property("ambient_sounds", serde_json::json!([]));
        self.register(region);

        // weapon - inherits from item
        let mut weapon = ClassDef::new("weapon", Some("item"));
        weapon.set_property("damage_dice", serde_json::json!("1d6"));
        weapon.set_property("damage_bonus", serde_json::json!(0));
        weapon.set_property("damage_type", serde_json::json!("physical"));
        self.register(weapon);

        // armor - inherits from item
        let mut armor = ClassDef::new("armor", Some("item"));
        armor.set_property("armor_value", serde_json::json!(0));
        armor.set_property("slot", serde_json::json!("body"));
        self.register(armor);

        // container - inherits from item
        let mut container = ClassDef::new("container", Some("item"));
        container.set_property("capacity", serde_json::json!(10));
        container.set_property("locked", serde_json::json!(false));
        self.register(container);

        // player - inherits from living
        let mut player = ClassDef::new("player", Some("living"));
        player.set_property("wallet_address", serde_json::json!(null));
        player.set_property("access_level", serde_json::json!("player"));
        self.register(player);

        // npc - inherits from living
        let mut npc = ClassDef::new("npc", Some("living"));
        npc.set_property("aggro", serde_json::json!(false));
        npc.set_property("respawn_time", serde_json::json!(null));
        npc.add_handler("ai_idle_tick");
        npc.add_handler("ai_combat_tick");
        self.register(npc);
    }

    /// Register a class definition
    pub fn register(&mut self, class: ClassDef) {
        self.classes.insert(class.name.clone(), class);
    }

    /// Get a class definition by name
    pub fn get(&self, name: &str) -> Option<&ClassDef> {
        self.classes.get(name)
    }

    /// Check if a class exists
    pub fn exists(&self, name: &str) -> bool {
        self.classes.contains_key(name)
    }

    /// Get the inheritance chain for a class (child -> ... -> root)
    pub fn get_chain(&self, name: &str) -> Vec<String> {
        let mut chain = Vec::new();
        let mut current = name.to_string();

        while let Some(class) = self.classes.get(&current) {
            chain.push(class.name.clone());
            match &class.parent {
                Some(parent) => current = parent.clone(),
                None => break,
            }
        }

        chain
    }

    /// Check if a class is a descendant of another class
    pub fn is_a(&self, child: &str, ancestor: &str) -> bool {
        let chain = self.get_chain(child);
        chain.contains(&ancestor.to_string())
    }

    /// Resolve all properties for a class (includes inherited)
    pub fn resolve_properties(&self, name: &str) -> Properties {
        let chain = self.get_chain(name);
        let mut props = Properties::new();

        // Apply from root to child (so child overrides parent)
        for class_name in chain.into_iter().rev() {
            if let Some(class) = self.classes.get(&class_name) {
                for (k, v) in &class.properties {
                    props.insert(k.clone(), v.clone());
                }
            }
        }

        props
    }

    /// Resolve all handlers for a class (includes inherited)
    pub fn resolve_handlers(&self, name: &str) -> Vec<String> {
        let chain = self.get_chain(name);
        let mut handlers = Vec::new();

        for class_name in chain {
            if let Some(class) = self.classes.get(&class_name) {
                for h in &class.handlers {
                    if !handlers.contains(h) {
                        handlers.push(h.clone());
                    }
                }
            }
        }

        handlers
    }

    /// Get a class definition (alias for get, for Lua API)
    pub fn get_class(&self, name: &str) -> Option<&ClassDef> {
        self.get(name)
    }

    /// Get inheritance chain (alias for get_chain, for Lua API)
    pub fn get_inheritance_chain(&self, name: &str) -> Vec<String> {
        self.get_chain(name)
    }

    /// Define a class from Lua with typed properties
    /// props_map contains (property_name -> (type_name, default_value))
    pub fn define_class(
        &mut self,
        name: &str,
        parent: Option<&str>,
        props_map: std::collections::HashMap<String, (String, serde_json::Value)>,
    ) {
        let mut class = ClassDef::new(name, parent);
        for (prop_name, (_type_name, default_val)) in props_map {
            class.set_property(&prop_name, default_val);
        }
        self.register(class);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_class_creation() {
        let class = ClassDef::new("sword", Some("weapon"));
        assert_eq!(class.name, "sword");
        assert_eq!(class.parent, Some("weapon".to_string()));
    }

    #[test]
    fn test_registry_base_classes() {
        let registry = ClassRegistry::new();

        assert!(registry.exists("thing"));
        assert!(registry.exists("item"));
        assert!(registry.exists("living"));
        assert!(registry.exists("room"));
        assert!(registry.exists("weapon"));
        assert!(registry.exists("player"));
        assert!(registry.exists("npc"));
    }

    #[test]
    fn test_inheritance_chain() {
        let registry = ClassRegistry::new();

        let chain = registry.get_chain("weapon");
        assert_eq!(chain, vec!["weapon", "item", "thing"]);

        let chain = registry.get_chain("player");
        assert_eq!(chain, vec!["player", "living", "thing"]);
    }

    #[test]
    fn test_is_a() {
        let registry = ClassRegistry::new();

        assert!(registry.is_a("weapon", "item"));
        assert!(registry.is_a("weapon", "thing"));
        assert!(registry.is_a("player", "living"));
        assert!(registry.is_a("player", "thing"));
        assert!(!registry.is_a("weapon", "living"));
        assert!(!registry.is_a("player", "item"));
    }

    #[test]
    fn test_resolve_properties() {
        let registry = ClassRegistry::new();

        let props = registry.resolve_properties("weapon");

        // Should have thing properties
        assert!(props.contains_key("name"));
        assert!(props.contains_key("description"));

        // Should have item properties
        assert!(props.contains_key("weight"));
        assert!(props.contains_key("value"));
        assert!(props.contains_key("fixed"));

        // Should have weapon properties
        assert!(props.contains_key("damage_dice"));
        assert!(props.contains_key("damage_bonus"));
    }

    #[test]
    fn test_resolve_handlers() {
        let registry = ClassRegistry::new();

        let handlers = registry.resolve_handlers("player");

        // Should have living handlers
        assert!(handlers.contains(&"heart_beat".to_string()));
        assert!(handlers.contains(&"on_damage".to_string()));
        assert!(handlers.contains(&"on_death".to_string()));

        // Should have thing handlers
        assert!(handlers.contains(&"on_create".to_string()));
        assert!(handlers.contains(&"on_init".to_string()));
    }

    #[test]
    fn test_deep_inheritance() {
        let mut registry = ClassRegistry::new();

        // Create deep chain: sword -> weapon -> item -> thing
        let mut sword = ClassDef::new("sword", Some("weapon"));
        sword.set_property("blade_type", serde_json::json!("longsword"));
        registry.register(sword);

        // fire_sword -> sword
        let mut fire_sword = ClassDef::new("fire_sword", Some("sword"));
        fire_sword.set_property("elemental_damage", serde_json::json!("fire"));
        fire_sword.set_property("fire_damage", serde_json::json!("1d6"));
        registry.register(fire_sword);

        let chain = registry.get_chain("fire_sword");
        assert_eq!(
            chain,
            vec!["fire_sword", "sword", "weapon", "item", "thing"]
        );

        assert!(registry.is_a("fire_sword", "weapon"));
        assert!(registry.is_a("fire_sword", "thing"));

        let props = registry.resolve_properties("fire_sword");
        assert!(props.contains_key("elemental_damage"));
        assert!(props.contains_key("blade_type"));
        assert!(props.contains_key("damage_dice"));
        assert!(props.contains_key("weight"));
        assert!(props.contains_key("name"));
    }
}
