//! TestWorld - Pre-configured test universe with regions, rooms, and accounts
//!
//! Creates a standard test environment with:
//! - A test universe
//! - A test region
//! - A spawn room and arena room
//! - Wizard and builder accounts

#![allow(dead_code)]

use anyhow::Result;
use mudd::db::Database;
use mudd::objects::{Object, ObjectStore};

/// Pre-configured test world with universe, region, rooms, and accounts
pub struct TestWorld {
    /// The test universe ID
    pub universe_id: String,
    /// The test region ID
    pub region_id: String,
    /// The spawn room where players start
    pub spawn_room_id: String,
    /// The arena room (PvP-enabled)
    pub arena_room_id: String,
    /// The wizard account ID
    pub wizard_account_id: String,
    /// The builder account ID
    pub builder_account_id: String,

    // Store for creating additional objects
    store: ObjectStore,
}

impl TestWorld {
    /// Create a fully populated test world using Database reference
    pub async fn create(db: &Database) -> Result<Self> {
        Self::create_with_pool(db.pool()).await
    }

    /// Create a fully populated test world using raw pool
    pub async fn create_with_pool(pool: &sqlx::SqlitePool) -> Result<Self> {
        let store = ObjectStore::new(pool.clone());

        // Create wizard account
        let wizard_account_id = uuid::Uuid::new_v4().to_string();
        sqlx::query("INSERT INTO accounts (id, username, access_level) VALUES (?, ?, ?)")
            .bind(&wizard_account_id)
            .bind("test_wizard")
            .bind("wizard")
            .execute(pool)
            .await?;

        // Create builder account
        let builder_account_id = uuid::Uuid::new_v4().to_string();
        sqlx::query("INSERT INTO accounts (id, username, access_level) VALUES (?, ?, ?)")
            .bind(&builder_account_id)
            .bind("test_builder")
            .bind("builder")
            .execute(pool)
            .await?;

        // Create test universe
        let universe_id = uuid::Uuid::new_v4().to_string();
        sqlx::query("INSERT INTO universes (id, name, owner_id) VALUES (?, ?, ?)")
            .bind(&universe_id)
            .bind("Test Universe")
            .bind(&wizard_account_id)
            .execute(pool)
            .await?;

        // Create test region
        let mut region = Object::new(&universe_id, "region");
        region.set_property("name", serde_json::json!("Test Region"));
        region.set_property("description", serde_json::json!("A region for testing"));
        store.create(&region).await?;
        let region_id = region.id.clone();

        // Create spawn room
        let mut spawn_room = Object::new(&universe_id, "room");
        spawn_room.parent_id = Some(region_id.clone());
        spawn_room.set_property("name", serde_json::json!("Spawn Room"));
        spawn_room.set_property(
            "description",
            serde_json::json!("You stand in the spawn room. The arena is to the north."),
        );
        store.create(&spawn_room).await?;
        let spawn_room_id = spawn_room.id.clone();

        // Create arena room (PvP enabled)
        let mut arena_room = Object::new(&universe_id, "room");
        arena_room.parent_id = Some(region_id.clone());
        arena_room.set_property("name", serde_json::json!("Arena"));
        arena_room.set_property(
            "description",
            serde_json::json!("A combat arena where PvP is permitted."),
        );
        arena_room.set_property("is_arena", serde_json::json!(true));
        store.create(&arena_room).await?;
        let arena_room_id = arena_room.id.clone();

        // Link rooms with exits
        store
            .set_exit(&spawn_room_id, "north", &arena_room_id)
            .await?;
        store
            .set_exit(&arena_room_id, "south", &spawn_room_id)
            .await?;

        Ok(Self {
            universe_id,
            region_id,
            spawn_room_id,
            arena_room_id,
            wizard_account_id,
            builder_account_id,
            store,
        })
    }

    /// Create a new room in the test region
    pub async fn create_room(&self, name: &str, description: &str) -> Result<String> {
        let mut room = Object::new(&self.universe_id, "room");
        room.parent_id = Some(self.region_id.clone());
        room.set_property("name", serde_json::json!(name));
        room.set_property("description", serde_json::json!(description));
        self.store.create(&room).await?;
        Ok(room.id)
    }

    /// Create an item in a specific location
    pub async fn create_item(&self, class: &str, name: &str, location: &str) -> Result<String> {
        let mut item = Object::new(&self.universe_id, class);
        item.parent_id = Some(location.to_string());
        item.set_property("name", serde_json::json!(name));
        self.store.create(&item).await?;
        Ok(item.id)
    }

    /// Create an NPC in a specific location
    pub async fn create_npc(&self, name: &str, location: &str) -> Result<String> {
        let mut npc = Object::new(&self.universe_id, "npc");
        npc.parent_id = Some(location.to_string());
        npc.set_property("name", serde_json::json!(name));
        npc.set_property("health", serde_json::json!(100));
        npc.set_property("max_health", serde_json::json!(100));
        self.store.create(&npc).await?;
        Ok(npc.id)
    }

    /// Link two rooms with exits
    pub async fn link_rooms(&self, room1: &str, dir1: &str, room2: &str, dir2: &str) -> Result<()> {
        self.store.set_exit(room1, dir1, room2).await?;
        self.store.set_exit(room2, dir2, room1).await?;
        Ok(())
    }

    /// Get the object store for direct access
    pub fn store(&self) -> &ObjectStore {
        &self.store
    }
}
