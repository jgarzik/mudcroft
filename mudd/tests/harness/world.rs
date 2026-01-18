//! TestWorld - Pre-configured test universe with regions, rooms, and accounts
//!
//! Creates a standard test environment with:
//! - A test universe
//! - A test region
//! - A spawn room and arena room
//! - Wizard and builder accounts

#![allow(dead_code)]

use anyhow::Result;
use mudd::objects::{Object, ObjectStore};
use serde_json::json;

use super::server::TestServer;

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
    /// Create a fully populated test world using TestServer reference
    /// Uses HTTP API for account and universe creation
    pub async fn create(server: &TestServer) -> Result<Self> {
        let store = ObjectStore::new(server.pool().clone(), None);

        // Create wizard account via API
        let wizard_resp = server
            .post(
                "/auth/register",
                &json!({
                    "username": "test_wizard",
                    "password": "test123"
                }),
            )
            .await?;

        let wizard_account_id = if wizard_resp.status().is_success() {
            let body: serde_json::Value = wizard_resp.json().await?;
            body["account_id"].as_str().unwrap().to_string()
        } else {
            // Account may already exist (unlikely in fresh test), try to get it
            let row: (String,) = sqlx::query_as("SELECT id FROM accounts WHERE username = ?")
                .bind("test_wizard")
                .fetch_one(server.pool())
                .await?;
            row.0
        };

        // Promote to wizard level
        sqlx::query("UPDATE accounts SET access_level = ? WHERE id = ?")
            .bind("wizard")
            .bind(&wizard_account_id)
            .execute(server.pool())
            .await?;

        // Create builder account via API
        let builder_resp = server
            .post(
                "/auth/register",
                &json!({
                    "username": "test_builder",
                    "password": "test123"
                }),
            )
            .await?;

        let builder_account_id = if builder_resp.status().is_success() {
            let body: serde_json::Value = builder_resp.json().await?;
            body["account_id"].as_str().unwrap().to_string()
        } else {
            // Account may already exist (unlikely in fresh test), try to get it
            let row: (String,) = sqlx::query_as("SELECT id FROM accounts WHERE username = ?")
                .bind("test_builder")
                .fetch_one(server.pool())
                .await?;
            row.0
        };

        // Promote to builder level
        sqlx::query("UPDATE accounts SET access_level = ? WHERE id = ?")
            .bind("builder")
            .bind(&builder_account_id)
            .execute(server.pool())
            .await?;

        // Create test universe via API with DNS-style ID
        let universe_id = format!("test-{}", uuid::Uuid::new_v4().to_string()[..8].to_string());
        let universe_resp = server
            .post(
                "/universe/create",
                &json!({
                    "id": universe_id,
                    "name": "Test Universe",
                    "owner_id": wizard_account_id
                }),
            )
            .await?;

        if !universe_resp.status().is_success() {
            let body = universe_resp.text().await?;
            anyhow::bail!("Failed to create test universe: {}", body);
        }

        // Create test region
        let mut region = Object::new(&universe_id, "region");
        region.set_property("name", json!("Test Region"));
        region.set_property("description", json!("A region for testing"));
        store.create(&region).await?;
        let region_id = region.id.clone();

        // Create spawn room
        let mut spawn_room = Object::new(&universe_id, "room");
        spawn_room.parent_id = Some(region_id.clone());
        spawn_room.set_property("name", json!("Spawn Room"));
        spawn_room.set_property(
            "description",
            json!("You stand in the spawn room. The arena is to the north."),
        );
        store.create(&spawn_room).await?;
        let spawn_room_id = spawn_room.id.clone();

        // Create arena room (PvP enabled)
        let mut arena_room = Object::new(&universe_id, "room");
        arena_room.parent_id = Some(region_id.clone());
        arena_room.set_property("name", json!("Arena"));
        arena_room.set_property(
            "description",
            json!("A combat arena where PvP is permitted."),
        );
        arena_room.set_property("is_arena", json!(true));
        store.create(&arena_room).await?;
        let arena_room_id = arena_room.id.clone();

        // Link rooms with exits
        store
            .set_exit(&spawn_room_id, "north", &arena_room_id)
            .await?;
        store
            .set_exit(&arena_room_id, "south", &spawn_room_id)
            .await?;

        // Set portal to spawn room so players spawn there
        store.set_portal(&universe_id, &spawn_room_id).await?;

        // Force WAL checkpoint so server can see our writes
        sqlx::query("PRAGMA wal_checkpoint(FULL)")
            .execute(server.pool())
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
        room.set_property("name", json!(name));
        room.set_property("description", json!(description));
        self.store.create(&room).await?;
        Ok(room.id)
    }

    /// Create an item in a specific location
    pub async fn create_item(&self, class: &str, name: &str, location: &str) -> Result<String> {
        let mut item = Object::new(&self.universe_id, class);
        item.parent_id = Some(location.to_string());
        item.set_property("name", json!(name));
        self.store.create(&item).await?;
        Ok(item.id)
    }

    /// Create an NPC in a specific location
    pub async fn create_npc(&self, name: &str, location: &str) -> Result<String> {
        let mut npc = Object::new(&self.universe_id, "npc");
        npc.parent_id = Some(location.to_string());
        npc.set_property("name", json!(name));
        npc.set_property("health", json!(100));
        npc.set_property("max_health", json!(100));
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
