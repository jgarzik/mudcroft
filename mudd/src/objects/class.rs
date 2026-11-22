//! Class system with inheritance

use std::collections::HashMap;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;
use tracing::{debug, warn};

use super::Properties;
use crate::raft::RaftWriter;

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
    /// Account that defined the class via game.define_class (None for
    /// built-in or loaded classes); only it (or an admin) may redefine it
    pub defined_by: Option<String>,
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
            defined_by: None,
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

/// Registry of class definitions: the global built-in classes plus each
/// universe's own classes (defined with game.define_class)
pub struct ClassRegistry {
    /// Built-in base classes, shared by every universe
    builtins: HashMap<String, ClassDef>,
    /// Classes defined per universe: (universe_id, name) -> class
    universe_classes: HashMap<(String, String), ClassDef>,
    db_pool: Option<SqlitePool>,
    raft_writer: Option<Arc<RaftWriter>>,
    /// Serializes defines (check, persist, insert) without blocking lookups
    define_lock: Arc<tokio::sync::Mutex<()>>,
}

impl Default for ClassRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl ClassRegistry {
    /// Create a new registry with base classes
    pub fn new() -> Self {
        let mut registry = Self {
            builtins: HashMap::new(),
            universe_classes: HashMap::new(),
            db_pool: None,
            raft_writer: None,
            define_lock: Arc::default(),
        };
        registry.register_base_classes();
        registry
    }

    /// Create a new registry with database pool and raft writer
    pub fn with_db(pool: SqlitePool, raft_writer: Arc<RaftWriter>) -> Self {
        let mut registry = Self {
            builtins: HashMap::new(),
            universe_classes: HashMap::new(),
            db_pool: Some(pool),
            raft_writer: Some(raft_writer),
            define_lock: Arc::default(),
        };
        registry.register_base_classes();
        registry
    }

    /// Register the built-in base classes (see mudd/docs/design.md, Classes)
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

    /// Register a built-in (global) class definition
    pub fn register(&mut self, class: ClassDef) {
        self.builtins.insert(class.name.clone(), class);
    }

    /// Get a class definition visible in a universe (built-in or the
    /// universe's own)
    pub fn get(&self, universe_id: &str, name: &str) -> Option<&ClassDef> {
        self.builtins.get(name).or_else(|| {
            self.universe_classes
                .get(&(universe_id.to_string(), name.to_string()))
        })
    }

    /// Check if a class exists in a universe
    pub fn exists(&self, universe_id: &str, name: &str) -> bool {
        self.get(universe_id, name).is_some()
    }

    /// Get the inheritance chain for a class (child -> ... -> root)
    pub fn get_chain(&self, universe_id: &str, name: &str) -> Vec<String> {
        let mut chain = Vec::new();
        let mut current = name.to_string();

        while let Some(class) = self.get(universe_id, &current) {
            // Guard against a cycle in user-defined parents
            if chain.contains(&class.name) {
                break;
            }
            chain.push(class.name.clone());
            match &class.parent {
                Some(parent) => current = parent.clone(),
                None => break,
            }
        }

        chain
    }

    /// Names of the built-in base classes, which scripts may not redefine
    pub const BUILTIN: &'static [&'static str] = &[
        "thing",
        "item",
        "living",
        "room",
        "region",
        "weapon",
        "armor",
        "container",
        "player",
        "npc",
    ];

    /// Whether `name` is a built-in base class
    pub fn is_builtin(name: &str) -> bool {
        Self::BUILTIN.contains(&name)
    }

    /// Check if a class is a descendant of another class
    pub fn is_a(&self, universe_id: &str, child: &str, ancestor: &str) -> bool {
        self.get_chain(universe_id, child)
            .contains(&ancestor.to_string())
    }

    /// Resolve all properties for a class (includes inherited)
    pub fn resolve_properties(&self, universe_id: &str, name: &str) -> Properties {
        let mut props = Properties::new();

        // Apply from root to child (so child overrides parent)
        for class_name in self.get_chain(universe_id, name).into_iter().rev() {
            if let Some(class) = self.get(universe_id, &class_name) {
                for (k, v) in &class.properties {
                    props.insert(k.clone(), v.clone());
                }
            }
        }

        props
    }

    /// Resolve all handlers for a class (includes inherited)
    pub fn resolve_handlers(&self, universe_id: &str, name: &str) -> Vec<String> {
        let mut handlers = Vec::new();

        for class_name in self.get_chain(universe_id, name) {
            if let Some(class) = self.get(universe_id, &class_name) {
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
    pub fn get_class(&self, universe_id: &str, name: &str) -> Option<&ClassDef> {
        self.get(universe_id, name)
    }

    /// Get inheritance chain (alias for get_chain, for Lua API)
    pub fn get_inheritance_chain(&self, universe_id: &str, name: &str) -> Vec<String> {
        self.get_chain(universe_id, name)
    }

    /// Define (or redefine) a class in a universe: persist it, then make it
    /// visible. Callers sharing the registry behind a lock should instead
    /// persist without holding the lock (`build_class`, `persister`,
    /// `ClassPersister::persist`, then `insert`), as game.define_class does.
    pub async fn define_class(
        &mut self,
        universe_id: &str,
        name: &str,
        parent: Option<&str>,
        props_map: std::collections::HashMap<String, (String, serde_json::Value)>,
        defined_by: Option<&str>,
    ) -> anyhow::Result<()> {
        let class = Self::build_class(name, parent, props_map, defined_by);
        self.persister().persist(universe_id, &class).await?;
        self.insert(universe_id, class);
        Ok(())
    }

    /// Build a class definition from Lua's typed properties.
    /// props_map contains (property_name -> (type_name, default_value)).
    pub fn build_class(
        name: &str,
        parent: Option<&str>,
        props_map: std::collections::HashMap<String, (String, serde_json::Value)>,
        defined_by: Option<&str>,
    ) -> ClassDef {
        let mut class = ClassDef::new(name, parent);
        class.defined_by = defined_by.map(str::to_string);
        for (prop_name, (_type_name, default_val)) in props_map {
            class.set_property(&prop_name, default_val);
        }
        class
    }

    /// Lock held across a whole define (check, persist, insert) so defines
    /// cannot interleave; lookups only use the registry's own lock
    pub fn define_lock(&self) -> Arc<tokio::sync::Mutex<()>> {
        self.define_lock.clone()
    }

    /// A handle that persists classes without borrowing the registry
    pub fn persister(&self) -> ClassPersister {
        ClassPersister {
            db_pool: self.db_pool.clone(),
            raft_writer: self.raft_writer.clone(),
        }
    }

    /// Make a (persisted) class visible in its universe
    pub fn insert(&mut self, universe_id: &str, class: ClassDef) {
        self.universe_classes
            .insert((universe_id.to_string(), class.name.clone()), class);
    }

    /// Load universe-defined classes from database on startup
    pub async fn load_from_db(&mut self) -> anyhow::Result<()> {
        let Some(ref pool) = self.db_pool else {
            return Ok(());
        };

        type ClassRow = (
            String,
            String,
            Option<String>,
            Option<String>,
            Option<String>,
        );
        let class_rows: Vec<ClassRow> =
            sqlx::query_as("SELECT universe_id, name, parent, code_hash, defined_by FROM classes")
                .fetch_all(pool)
                .await?;
        let prop_rows: Vec<(String, String, String, String)> =
            sqlx::query_as("SELECT universe_id, class_name, key, value FROM class_properties")
                .fetch_all(pool)
                .await?;
        let handler_rows: Vec<(String, String, String)> =
            sqlx::query_as("SELECT universe_id, class_name, handler FROM class_handlers")
                .fetch_all(pool)
                .await?;

        // Replaces the universe classes: also used to rebuild the cache on
        // becoming primary
        self.universe_classes.clear();
        let mut props_by_class: HashMap<(String, String), Properties> = HashMap::new();
        for (universe_id, class_name, key, value) in prop_rows {
            match serde_json::from_str(&value) {
                Ok(v) => {
                    props_by_class
                        .entry((universe_id, class_name))
                        .or_default()
                        .insert(key, v);
                }
                Err(e) => warn!("Failed to parse property value: {}", e),
            }
        }
        let mut handlers_by_class: HashMap<(String, String), Vec<String>> = HashMap::new();
        for (universe_id, class_name, handler) in handler_rows {
            handlers_by_class
                .entry((universe_id, class_name))
                .or_default()
                .push(handler);
        }

        for (universe_id, name, parent, code_hash, defined_by) in class_rows {
            let key = (universe_id, name);
            let mut class = ClassDef::new(&key.1, parent.as_deref());
            class.code_hash = code_hash;
            class.defined_by = defined_by;
            if let Some(props) = props_by_class.remove(&key) {
                class.properties = props;
            }
            if let Some(handlers) = handlers_by_class.remove(&key) {
                class.handlers = handlers;
            }
            self.universe_classes.insert(key, class);
        }

        debug!(
            "Loaded {} universe classes from database",
            self.universe_classes.len()
        );
        Ok(())
    }
}

/// Persists class definitions (one atomic Raft batch per class)
pub struct ClassPersister {
    db_pool: Option<SqlitePool>,
    raft_writer: Option<Arc<RaftWriter>>,
}

impl ClassPersister {
    /// Write a class and its properties and handlers, replacing any earlier
    /// definition, all or nothing
    pub async fn persist(&self, universe_id: &str, class: &ClassDef) -> anyhow::Result<()> {
        let name = &class.name;
        let mut statements: Vec<(String, Vec<serde_json::Value>)> = vec![
            (
                "INSERT OR REPLACE INTO classes (universe_id, name, parent, code_hash, defined_by)
                 VALUES (?, ?, ?, ?, ?)"
                    .to_string(),
                vec![
                    serde_json::json!(universe_id),
                    serde_json::json!(name),
                    serde_json::json!(class.parent),
                    serde_json::json!(class.code_hash),
                    serde_json::json!(class.defined_by),
                ],
            ),
            (
                "DELETE FROM class_properties WHERE universe_id = ? AND class_name = ?".to_string(),
                vec![serde_json::json!(universe_id), serde_json::json!(name)],
            ),
            (
                "DELETE FROM class_handlers WHERE universe_id = ? AND class_name = ?".to_string(),
                vec![serde_json::json!(universe_id), serde_json::json!(name)],
            ),
        ];
        for (key, value) in &class.properties {
            statements.push((
                "INSERT INTO class_properties (universe_id, class_name, key, value) VALUES (?, ?, ?, ?)"
                    .to_string(),
                vec![
                    serde_json::json!(universe_id),
                    serde_json::json!(name),
                    serde_json::json!(key),
                    serde_json::json!(serde_json::to_string(value)?),
                ],
            ));
        }
        for handler in &class.handlers {
            statements.push((
                "INSERT INTO class_handlers (universe_id, class_name, handler) VALUES (?, ?, ?)"
                    .to_string(),
                vec![
                    serde_json::json!(universe_id),
                    serde_json::json!(name),
                    serde_json::json!(handler),
                ],
            ));
        }

        if let Some(ref raft_writer) = self.raft_writer {
            raft_writer.execute_batch(statements).await?;
        } else if let Some(ref pool) = self.db_pool {
            // Direct SQL fallback (unit tests without Raft), also atomic
            let mut tx = pool.begin().await?;
            for (sql, params) in &statements {
                // The statements above are SQL literals; all values are bound
                let mut query = sqlx::query(sqlx::AssertSqlSafe(sql.as_str()));
                for param in params {
                    query = match param {
                        serde_json::Value::Null => query.bind(Option::<String>::None),
                        serde_json::Value::String(v) => query.bind(v.clone()),
                        other => query.bind(other.to_string()),
                    };
                }
                query.execute(&mut *tx).await?;
            }
            tx.commit().await?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Classes defined in one universe are invisible in another
    #[tokio::test]
    async fn test_classes_are_per_universe() {
        let mut registry = ClassRegistry::new();
        let mut props = std::collections::HashMap::new();
        props.insert(
            "lit".to_string(),
            ("boolean".to_string(), serde_json::json!(true)),
        );
        registry
            .define_class("a", "lantern", Some("item"), props, Some("wiz"))
            .await
            .unwrap();
        assert!(registry.is_a("a", "lantern", "item"));
        assert!(!registry.exists("b", "lantern"));
        assert_eq!(registry.resolve_properties("a", "lantern")["lit"], true);
        assert_eq!(
            registry.get("a", "lantern").unwrap().defined_by.as_deref(),
            Some("wiz")
        );
        // Built-ins are visible everywhere
        assert!(registry.exists("b", "item"));
    }

    #[test]
    fn test_class_creation() {
        let class = ClassDef::new("sword", Some("weapon"));
        assert_eq!(class.name, "sword");
        assert_eq!(class.parent, Some("weapon".to_string()));
    }

    #[test]
    fn test_registry_base_classes() {
        let registry = ClassRegistry::new();

        assert!(registry.exists("u", "thing"));
        assert!(registry.exists("u", "item"));
        assert!(registry.exists("u", "living"));
        assert!(registry.exists("u", "room"));
        assert!(registry.exists("u", "weapon"));
        assert!(registry.exists("u", "player"));
        assert!(registry.exists("u", "npc"));
    }

    #[test]
    fn test_inheritance_chain() {
        let registry = ClassRegistry::new();

        let chain = registry.get_chain("u", "weapon");
        assert_eq!(chain, vec!["weapon", "item", "thing"]);

        let chain = registry.get_chain("u", "player");
        assert_eq!(chain, vec!["player", "living", "thing"]);
    }

    #[test]
    fn test_is_a() {
        let registry = ClassRegistry::new();

        assert!(registry.is_a("u", "weapon", "item"));
        assert!(registry.is_a("u", "weapon", "thing"));
        assert!(registry.is_a("u", "player", "living"));
        assert!(registry.is_a("u", "player", "thing"));
        assert!(!registry.is_a("u", "weapon", "living"));
        assert!(!registry.is_a("u", "player", "item"));
    }

    #[test]
    fn test_resolve_properties() {
        let registry = ClassRegistry::new();

        let props = registry.resolve_properties("u", "weapon");

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

        let handlers = registry.resolve_handlers("u", "player");

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

        let chain = registry.get_chain("u", "fire_sword");
        assert_eq!(
            chain,
            vec!["fire_sword", "sword", "weapon", "item", "thing"]
        );

        assert!(registry.is_a("u", "fire_sword", "weapon"));
        assert!(registry.is_a("u", "fire_sword", "thing"));

        let props = registry.resolve_properties("u", "fire_sword");
        assert!(props.contains_key("elemental_damage"));
        assert!(props.contains_key("blade_type"));
        assert!(props.contains_key("damage_dice"));
        assert!(props.contains_key("weight"));
        assert!(props.contains_key("name"));
    }
}
