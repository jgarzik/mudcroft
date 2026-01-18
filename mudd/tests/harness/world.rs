//! TestWorld - Pre-configured test universe with cave adventure content
//!
//! Creates a standard test environment with:
//! - A test universe with dark-caves region
//! - 4 rooms: entrance, passage, chamber, pool
//! - 2 NPCs: Giant Bat, Cave Troll
//! - Items: Rusty Sword, Glowing Mushroom
//! - Wizard and builder accounts

#![allow(dead_code)]

use anyhow::Result;
use mudd::objects::{Object, ObjectStore};
use serde_json::json;

use super::server::TestServer;

/// Pre-configured test world with cave adventure content
pub struct TestWorld {
    /// The test universe ID
    pub universe_id: String,
    /// The dark-caves region ID
    pub region_id: String,

    // Room IDs
    /// Cave entrance - starting room with rusty sword
    pub entrance_id: String,
    /// Narrow passage - contains Giant Bat
    pub passage_id: String,
    /// Treasure chamber - contains Cave Troll
    pub chamber_id: String,
    /// Underground pool - contains Glowing Mushroom
    pub pool_id: String,

    // NPC IDs
    /// Giant Bat NPC (15 HP) in passage
    pub bat_id: String,
    /// Cave Troll NPC (50 HP) in chamber
    pub troll_id: String,

    // Item IDs
    /// Rusty Sword weapon in entrance
    pub sword_id: String,
    /// Glowing Mushroom item in pool
    pub mushroom_id: String,

    // Account IDs
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

        // Create test universe directly via store (with core lib hashes)
        let universe_id = format!("test-{}", &uuid::Uuid::new_v4().to_string()[..8]);

        // Get core lib hashes to include in universe config
        let core_lib_hashes = store.get_core_lib_hashes().await?;
        let config = json!({
            "lib_hashes": core_lib_hashes
        });

        store
            .create_universe(&universe_id, "Test Universe", &wizard_account_id, config)
            .await?;

        // Create dark-caves region
        let mut region = Object::new("/regions/dark-caves", &universe_id, "region")?;
        region.set_property("name", json!("Dark Caves"));
        region.set_property("environment_type", json!("cave"));
        store.create(&region).await?;
        let region_id = region.id.clone();

        // Create cave entrance room
        let mut entrance = Object::new("/rooms/cave-entrance", &universe_id, "room")?;
        entrance.parent_id = Some(region_id.clone());
        entrance.set_property("name", json!("Cave Entrance"));
        entrance.set_property(
            "description",
            json!("A dark opening in the mountainside. Cold air flows from within."),
        );
        store.create(&entrance).await?;
        let entrance_id = entrance.id.clone();

        // Create narrow passage room
        let mut passage = Object::new("/rooms/narrow-passage", &universe_id, "room")?;
        passage.parent_id = Some(region_id.clone());
        passage.set_property("name", json!("Narrow Passage"));
        passage.set_property(
            "description",
            json!("A tight passage barely wide enough for one person. Water drips from stalactites above."),
        );
        store.create(&passage).await?;
        let passage_id = passage.id.clone();

        // Create treasure chamber room
        let mut chamber = Object::new("/rooms/treasure-chamber", &universe_id, "room")?;
        chamber.parent_id = Some(region_id.clone());
        chamber.set_property("name", json!("Treasure Chamber"));
        chamber.set_property(
            "description",
            json!("A vast underground chamber. Gold coins and gems glitter in the dim light."),
        );
        store.create(&chamber).await?;
        let chamber_id = chamber.id.clone();

        // Create underground pool room
        let mut pool = Object::new("/rooms/underground-pool", &universe_id, "room")?;
        pool.parent_id = Some(region_id.clone());
        pool.set_property("name", json!("Underground Pool"));
        pool.set_property(
            "description",
            json!("A serene underground pool fed by a small waterfall. Bioluminescent fungi cast an eerie blue glow."),
        );
        store.create(&pool).await?;
        let pool_id = pool.id.clone();

        // Link rooms with exits (matching cave_adventure.lua)
        // entrance -> north -> passage
        store.set_exit(&entrance_id, "north", &passage_id).await?;
        // passage -> south -> entrance, north -> chamber, east -> pool
        store.set_exit(&passage_id, "south", &entrance_id).await?;
        store.set_exit(&passage_id, "north", &chamber_id).await?;
        store.set_exit(&passage_id, "east", &pool_id).await?;
        // chamber -> south -> passage
        store.set_exit(&chamber_id, "south", &passage_id).await?;
        // pool -> west -> passage
        store.set_exit(&pool_id, "west", &passage_id).await?;

        // Create Giant Bat NPC in passage (15 HP)
        let mut bat = Object::new("/npcs/giant-bat", &universe_id, "npc")?;
        bat.parent_id = Some(passage_id.clone());
        bat.set_property("name", json!("Giant Bat"));
        bat.set_property(
            "description",
            json!("A bat the size of a dog with razor-sharp fangs."),
        );
        bat.set_property("hp", json!(15));
        bat.set_property("max_hp", json!(15));
        bat.set_property("attack_bonus", json!(1));
        bat.set_property("armor_class", json!(12));
        store.create(&bat).await?;
        let bat_id = bat.id.clone();

        // Create Cave Troll NPC in chamber (50 HP)
        let mut troll = Object::new("/npcs/cave-troll", &universe_id, "npc")?;
        troll.parent_id = Some(chamber_id.clone());
        troll.set_property("name", json!("Cave Troll"));
        troll.set_property(
            "description",
            json!("A massive troll with mottled grey skin and beady red eyes."),
        );
        troll.set_property("hp", json!(50));
        troll.set_property("max_hp", json!(50));
        troll.set_property("attack_bonus", json!(4));
        troll.set_property("armor_class", json!(14));
        store.create(&troll).await?;
        let troll_id = troll.id.clone();

        // Create Rusty Sword weapon in entrance
        let mut sword = Object::new("/weapons/rusty-sword", &universe_id, "weapon")?;
        sword.parent_id = Some(entrance_id.clone());
        sword.set_property("name", json!("Rusty Short Sword"));
        sword.set_property(
            "description",
            json!("A battered but serviceable short sword."),
        );
        sword.set_property("damage_dice", json!("1d6"));
        sword.set_property("damage_bonus", json!(0));
        sword.set_property("weight", json!(2));
        store.create(&sword).await?;
        let sword_id = sword.id.clone();

        // Create Glowing Mushroom item in pool
        let mut mushroom = Object::new("/items/glowing-mushroom", &universe_id, "item")?;
        mushroom.parent_id = Some(pool_id.clone());
        mushroom.set_property("name", json!("Glowing Mushroom"));
        mushroom.set_property(
            "description",
            json!("A softly glowing blue mushroom. It pulses with an inner light."),
        );
        mushroom.set_property("value", json!(25));
        mushroom.set_property("weight", json!(0.5));
        store.create(&mushroom).await?;
        let mushroom_id = mushroom.id.clone();

        // Set portal to entrance so players spawn there
        store.set_portal(&universe_id, &entrance_id).await?;

        // Force WAL checkpoint so server can see our writes
        sqlx::query("PRAGMA wal_checkpoint(FULL)")
            .execute(server.pool())
            .await?;

        Ok(Self {
            universe_id,
            region_id,
            entrance_id,
            passage_id,
            chamber_id,
            pool_id,
            bat_id,
            troll_id,
            sword_id,
            mushroom_id,
            wizard_account_id,
            builder_account_id,
            store,
        })
    }

    /// Create a new room in the test region
    pub async fn create_room(&self, name: &str, description: &str) -> Result<String> {
        // Generate path from name (lowercase, spaces to hyphens)
        let path_name = name.to_lowercase().replace(' ', "-");
        let path = format!("/rooms/{}", path_name);
        let mut room = Object::new(&path, &self.universe_id, "room")?;
        room.parent_id = Some(self.region_id.clone());
        room.set_property("name", json!(name));
        room.set_property("description", json!(description));
        self.store.create(&room).await?;
        Ok(room.id)
    }

    /// Create an item in a specific location
    pub async fn create_item(&self, class: &str, name: &str, location: &str) -> Result<String> {
        // Generate path from class and name
        let path_name = name.to_lowercase().replace(' ', "-");
        let path = format!("/items/{}", path_name);
        let mut item = Object::new(&path, &self.universe_id, class)?;
        item.parent_id = Some(location.to_string());
        item.set_property("name", json!(name));
        self.store.create(&item).await?;
        Ok(item.id)
    }

    /// Create an NPC in a specific location
    pub async fn create_npc(&self, name: &str, location: &str) -> Result<String> {
        // Generate path from name
        let path_name = name.to_lowercase().replace(' ', "-");
        let path = format!("/npcs/{}", path_name);
        let mut npc = Object::new(&path, &self.universe_id, "npc")?;
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
