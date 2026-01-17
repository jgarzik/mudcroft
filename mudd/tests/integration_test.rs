//! Integration tests using MuddTest harness

mod common;

use common::MuddTest;
use mudd::objects::{ClassRegistry, Object, ObjectStore};

#[tokio::test]
async fn test_server_starts_and_stops() {
    let mudd = MuddTest::start().await.expect("Failed to start server");
    // Server shuts down automatically when mudd is dropped
    drop(mudd);
}

#[tokio::test]
async fn test_health_endpoint() {
    let mudd = MuddTest::start().await.expect("Failed to start server");

    let resp = mudd.get("/health").await.expect("Failed to get health");
    assert_eq!(resp.status(), 200);

    let body: serde_json::Value = resp.json().await.expect("Failed to parse JSON");
    assert_eq!(body["status"], "healthy");
    assert_eq!(body["database"], "ok");
}

#[tokio::test]
async fn test_root_endpoint() {
    let mudd = MuddTest::start().await.expect("Failed to start server");

    let resp = mudd.get("/").await.expect("Failed to get root");
    assert_eq!(resp.status(), 200);

    let body: serde_json::Value = resp.json().await.expect("Failed to parse JSON");
    assert_eq!(body["name"], "mudd");
}

#[tokio::test]
async fn test_parallel_servers() {
    // Start multiple servers to verify port isolation
    let mudd1 = MuddTest::start().await.expect("Failed to start server 1");
    let mudd2 = MuddTest::start().await.expect("Failed to start server 2");

    assert_ne!(mudd1.addr, mudd2.addr);

    // Both should respond
    let resp1 = mudd1.get("/health").await.expect("Failed to get health 1");
    let resp2 = mudd2.get("/health").await.expect("Failed to get health 2");

    assert_eq!(resp1.status(), 200);
    assert_eq!(resp2.status(), 200);
}

#[tokio::test]
async fn test_database_isolation() {
    let mudd1 = MuddTest::start().await.expect("Failed to start server 1");
    let mudd2 = MuddTest::start().await.expect("Failed to start server 2");

    // Insert into mudd1's database
    sqlx::query("INSERT INTO accounts (id, username) VALUES ('test1', 'alice')")
        .execute(mudd1.db().pool())
        .await
        .expect("Failed to insert");

    // Verify it exists in mudd1
    let result: Option<(String,)> =
        sqlx::query_as("SELECT username FROM accounts WHERE id = 'test1'")
            .fetch_optional(mudd1.db().pool())
            .await
            .expect("Failed to query");
    assert_eq!(result, Some(("alice".to_string(),)));

    // Verify it does NOT exist in mudd2 (database isolation)
    let result: Option<(String,)> =
        sqlx::query_as("SELECT username FROM accounts WHERE id = 'test1'")
            .fetch_optional(mudd2.db().pool())
            .await
            .expect("Failed to query");
    assert_eq!(result, None);
}

// Object System Tests

#[tokio::test]
async fn test_object_crud() {
    let mudd = MuddTest::start().await.expect("Failed to start server");

    // Create test account and universe first
    let owner_id = mudd.create_test_account("testuser").await.unwrap();
    let universe_id = mudd.create_test_universe("Test Universe", &owner_id).await.unwrap();

    let store = ObjectStore::new(mudd.db().pool().clone());

    // Create
    let mut obj = Object::new(&universe_id, "sword");
    obj.set_property("name", serde_json::json!("Excalibur"));
    obj.set_property("damage_dice", serde_json::json!("2d6"));

    store.create(&obj).await.expect("Failed to create object");

    // Read
    let loaded = store
        .get(&obj.id)
        .await
        .expect("Failed to get object")
        .expect("Object not found");

    assert_eq!(loaded.class, "sword");
    assert_eq!(loaded.get_string("name"), Some("Excalibur"));

    // Update
    let mut updated = loaded.clone();
    updated.set_property("damage_dice", serde_json::json!("3d6"));
    store.update(&updated).await.expect("Failed to update");

    let reloaded = store.get(&obj.id).await.unwrap().unwrap();
    assert_eq!(reloaded.get_string("damage_dice"), Some("3d6"));

    // Delete
    let deleted = store.delete(&obj.id).await.expect("Failed to delete");
    assert!(deleted);

    let gone = store.get(&obj.id).await.expect("Failed to check deletion");
    assert!(gone.is_none());
}

#[tokio::test]
async fn test_object_hierarchy() {
    let mudd = MuddTest::start().await.expect("Failed to start server");

    // Create test account and universe first
    let owner_id = mudd.create_test_account("testuser").await.unwrap();
    let universe_id = mudd.create_test_universe("Test Universe", &owner_id).await.unwrap();

    let store = ObjectStore::new(mudd.db().pool().clone());

    // Create a room
    let mut room = Object::new(&universe_id, "room");
    room.set_property("name", serde_json::json!("Town Square"));
    store.create(&room).await.expect("Failed to create room");

    // Create items in the room
    let mut sword = Object::new(&universe_id, "sword");
    sword.set_property("name", serde_json::json!("Rusty Sword"));
    sword.parent_id = Some(room.id.clone());
    store.create(&sword).await.expect("Failed to create sword");

    let mut shield = Object::new(&universe_id, "armor");
    shield.set_property("name", serde_json::json!("Wooden Shield"));
    shield.parent_id = Some(room.id.clone());
    store.create(&shield).await.expect("Failed to create shield");

    // Get room contents
    let contents = store
        .get_contents(&room.id)
        .await
        .expect("Failed to get contents");
    assert_eq!(contents.len(), 2);

    // Move sword out of room
    store
        .move_object(&sword.id, None)
        .await
        .expect("Failed to move");

    let contents_after = store.get_contents(&room.id).await.unwrap();
    assert_eq!(contents_after.len(), 1);
}

#[tokio::test]
async fn test_code_store() {
    let mudd = MuddTest::start().await.expect("Failed to start server");
    let store = ObjectStore::new(mudd.db().pool().clone());

    let code = r#"
        function on_init(self)
            self.ready = true
        end
    "#;

    // Store code
    let hash = store.store_code(code).await.expect("Failed to store code");
    assert_eq!(hash.len(), 64); // SHA-256

    // Retrieve code
    let retrieved = store
        .get_code(&hash)
        .await
        .expect("Failed to get code")
        .expect("Code not found");
    assert_eq!(retrieved, code);

    // Store same code again - should return same hash
    let hash2 = store.store_code(code).await.unwrap();
    assert_eq!(hash, hash2);
}

#[tokio::test]
async fn test_class_registry() {
    let registry = ClassRegistry::new();

    // Verify inheritance chain
    assert!(registry.is_a("weapon", "item"));
    assert!(registry.is_a("weapon", "thing"));
    assert!(registry.is_a("player", "living"));

    // Verify property resolution
    let props = registry.resolve_properties("weapon");
    assert!(props.contains_key("damage_dice")); // from weapon
    assert!(props.contains_key("weight")); // from item
    assert!(props.contains_key("name")); // from thing

    // Verify handlers resolution
    let handlers = registry.resolve_handlers("npc");
    assert!(handlers.contains(&"heart_beat".to_string())); // from living
    assert!(handlers.contains(&"ai_idle_tick".to_string())); // from npc
    assert!(handlers.contains(&"on_init".to_string())); // from thing
}

#[tokio::test]
async fn test_find_by_name() {
    let mudd = MuddTest::start().await.expect("Failed to start server");

    // Create test account and universe first
    let owner_id = mudd.create_test_account("testuser").await.unwrap();
    let universe_id = mudd.create_test_universe("Test Universe", &owner_id).await.unwrap();

    let store = ObjectStore::new(mudd.db().pool().clone());

    // Create a room
    let mut room = Object::new(&universe_id, "room");
    room.set_property("name", serde_json::json!("Test Room"));
    store.create(&room).await.unwrap();

    // Create objects in room
    let mut sword = Object::new(&universe_id, "sword");
    sword.set_property("name", serde_json::json!("Magic Sword"));
    sword.parent_id = Some(room.id.clone());
    store.create(&sword).await.unwrap();

    let mut key = Object::new(&universe_id, "item");
    key.set_property("name", serde_json::json!("Golden Key"));
    key.parent_id = Some(room.id.clone());
    store.create(&key).await.unwrap();

    // Find by name
    let found = store
        .find_by_name(&room.id, "Magic Sword")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(found.id, sword.id);

    // Not found
    let not_found = store.find_by_name(&room.id, "Missing Item").await.unwrap();
    assert!(not_found.is_none());
}

// Rooms & Movement Tests

#[tokio::test]
async fn test_room_exits() {
    let mudd = MuddTest::start().await.expect("Failed to start server");
    let owner_id = mudd.create_test_account("testuser").await.unwrap();
    let universe_id = mudd.create_test_universe("Test Universe", &owner_id).await.unwrap();
    let store = ObjectStore::new(mudd.db().pool().clone());

    // Create two rooms
    let mut room1 = Object::new(&universe_id, "room");
    room1.set_property("name", serde_json::json!("Town Square"));
    store.create(&room1).await.unwrap();

    let mut room2 = Object::new(&universe_id, "room");
    room2.set_property("name", serde_json::json!("Market"));
    store.create(&room2).await.unwrap();

    // Set exits
    store.set_exit(&room1.id, "north", &room2.id).await.unwrap();
    store.set_exit(&room2.id, "south", &room1.id).await.unwrap();

    // Verify exits
    let north_dest = store.get_exit(&room1.id, "north").await.unwrap();
    assert_eq!(north_dest, Some(room2.id.clone()));

    let south_dest = store.get_exit(&room2.id, "south").await.unwrap();
    assert_eq!(south_dest, Some(room1.id.clone()));

    // No exit in that direction
    let no_exit = store.get_exit(&room1.id, "west").await.unwrap();
    assert!(no_exit.is_none());

    // Remove exit
    store.remove_exit(&room1.id, "north").await.unwrap();
    let removed = store.get_exit(&room1.id, "north").await.unwrap();
    assert!(removed.is_none());
}

#[tokio::test]
async fn test_environment_query() {
    let mudd = MuddTest::start().await.expect("Failed to start server");
    let owner_id = mudd.create_test_account("testuser").await.unwrap();
    let universe_id = mudd.create_test_universe("Test Universe", &owner_id).await.unwrap();
    let store = ObjectStore::new(mudd.db().pool().clone());

    // Create room
    let mut room = Object::new(&universe_id, "room");
    room.set_property("name", serde_json::json!("Test Room"));
    store.create(&room).await.unwrap();

    // Create player in room
    let mut player = Object::new(&universe_id, "player");
    player.set_property("name", serde_json::json!("Alice"));
    player.parent_id = Some(room.id.clone());
    store.create(&player).await.unwrap();

    // Get environment
    let env = store.get_environment(&player.id).await.unwrap();
    assert!(env.is_some());
    assert_eq!(env.unwrap().id, room.id);

    // Room has no environment (top level)
    let room_env = store.get_environment(&room.id).await.unwrap();
    assert!(room_env.is_none());
}

#[tokio::test]
async fn test_get_living_in() {
    let mudd = MuddTest::start().await.expect("Failed to start server");
    let owner_id = mudd.create_test_account("testuser").await.unwrap();
    let universe_id = mudd.create_test_universe("Test Universe", &owner_id).await.unwrap();
    let store = ObjectStore::new(mudd.db().pool().clone());

    // Create room
    let mut room = Object::new(&universe_id, "room");
    room.set_property("name", serde_json::json!("Arena"));
    store.create(&room).await.unwrap();

    // Create player in room
    let mut player = Object::new(&universe_id, "player");
    player.set_property("name", serde_json::json!("Hero"));
    player.parent_id = Some(room.id.clone());
    store.create(&player).await.unwrap();

    // Create NPC in room
    let mut npc = Object::new(&universe_id, "npc");
    npc.set_property("name", serde_json::json!("Goblin"));
    npc.parent_id = Some(room.id.clone());
    store.create(&npc).await.unwrap();

    // Create item in room (not living)
    let mut sword = Object::new(&universe_id, "sword");
    sword.set_property("name", serde_json::json!("Rusty Sword"));
    sword.parent_id = Some(room.id.clone());
    store.create(&sword).await.unwrap();

    // Get living entities
    let living = store.get_living_in(&room.id).await.unwrap();
    assert_eq!(living.len(), 2);

    let names: Vec<_> = living
        .iter()
        .filter_map(|o| o.get_string("name"))
        .collect();
    assert!(names.contains(&"Hero"));
    assert!(names.contains(&"Goblin"));
}

#[tokio::test]
async fn test_player_movement() {
    let mudd = MuddTest::start().await.expect("Failed to start server");
    let owner_id = mudd.create_test_account("testuser").await.unwrap();
    let universe_id = mudd.create_test_universe("Test Universe", &owner_id).await.unwrap();
    let store = ObjectStore::new(mudd.db().pool().clone());

    // Create two connected rooms
    let mut room1 = Object::new(&universe_id, "room");
    room1.set_property("name", serde_json::json!("Start"));
    store.create(&room1).await.unwrap();

    let mut room2 = Object::new(&universe_id, "room");
    room2.set_property("name", serde_json::json!("End"));
    store.create(&room2).await.unwrap();

    store.set_exit(&room1.id, "east", &room2.id).await.unwrap();

    // Create player in room1
    let mut player = Object::new(&universe_id, "player");
    player.set_property("name", serde_json::json!("Adventurer"));
    player.parent_id = Some(room1.id.clone());
    store.create(&player).await.unwrap();

    // Verify player is in room1
    let contents1 = store.get_living_in(&room1.id).await.unwrap();
    assert_eq!(contents1.len(), 1);

    // Get exit destination
    let dest = store.get_exit(&room1.id, "east").await.unwrap().unwrap();
    assert_eq!(dest, room2.id);

    // Move player to room2
    store.move_object(&player.id, Some(&dest)).await.unwrap();

    // Verify player moved
    let contents1_after = store.get_living_in(&room1.id).await.unwrap();
    assert_eq!(contents1_after.len(), 0);

    let contents2 = store.get_living_in(&room2.id).await.unwrap();
    assert_eq!(contents2.len(), 1);
    assert_eq!(contents2[0].get_string("name"), Some("Adventurer"));
}

// WebSocket Tests

#[tokio::test]
async fn test_websocket_connect() {
    let mudd = MuddTest::start().await.expect("Failed to start server");

    let mut ws = mudd.connect_ws().await.expect("Failed to connect WebSocket");

    // Should receive welcome message
    let msg = ws
        .recv_json_timeout(std::time::Duration::from_secs(5))
        .await
        .expect("Failed to receive welcome");

    assert_eq!(msg["type"], "welcome");
    assert!(msg["player_id"].is_string());

    ws.close().await.ok();
}

#[tokio::test]
async fn test_websocket_command() {
    let mudd = MuddTest::start().await.expect("Failed to start server");

    let mut ws = mudd.connect_ws().await.expect("Failed to connect WebSocket");

    // Receive welcome
    let _welcome = ws
        .recv_json_timeout(std::time::Duration::from_secs(5))
        .await
        .unwrap();

    // Send a command
    ws.send_command("look").await.expect("Failed to send command");

    // Should receive echo
    let echo = ws
        .recv_json_timeout(std::time::Duration::from_secs(5))
        .await
        .expect("Failed to receive echo");
    assert_eq!(echo["type"], "echo");
    assert_eq!(echo["command"], "look");

    // Should receive output
    let output = ws
        .recv_json_timeout(std::time::Duration::from_secs(5))
        .await
        .expect("Failed to receive output");
    assert_eq!(output["type"], "output");
    assert!(output["text"].is_string());

    ws.close().await.ok();
}

#[tokio::test]
async fn test_websocket_help_command() {
    let mudd = MuddTest::start().await.expect("Failed to start server");

    let mut ws = mudd.connect_ws().await.expect("Failed to connect WebSocket");

    // Receive welcome
    let _welcome = ws
        .recv_json_timeout(std::time::Duration::from_secs(5))
        .await
        .unwrap();

    // Send help command
    ws.send_command("help").await.expect("Failed to send help");

    // Skip echo
    let _echo = ws.recv_json_timeout(std::time::Duration::from_secs(5)).await.unwrap();

    // Get help output
    let output = ws
        .recv_json_timeout(std::time::Duration::from_secs(5))
        .await
        .expect("Failed to receive help output");

    assert_eq!(output["type"], "output");
    let text = output["text"].as_str().unwrap();
    assert!(text.contains("Commands:"));
    assert!(text.contains("look"));

    ws.close().await.ok();
}

#[tokio::test]
async fn test_websocket_multiple_connections() {
    let mudd = MuddTest::start().await.expect("Failed to start server");

    // Connect two clients
    let mut ws1 = mudd.connect_ws().await.expect("Failed to connect ws1");
    let mut ws2 = mudd.connect_ws().await.expect("Failed to connect ws2");

    // Both should receive welcome messages with different player IDs
    let welcome1 = ws1
        .recv_json_timeout(std::time::Duration::from_secs(5))
        .await
        .expect("Failed to receive welcome1");
    let welcome2 = ws2
        .recv_json_timeout(std::time::Duration::from_secs(5))
        .await
        .expect("Failed to receive welcome2");

    assert_eq!(welcome1["type"], "welcome");
    assert_eq!(welcome2["type"], "welcome");
    assert_ne!(welcome1["player_id"], welcome2["player_id"]);

    ws1.close().await.ok();
    ws2.close().await.ok();
}

#[tokio::test]
async fn test_websocket_unknown_command() {
    let mudd = MuddTest::start().await.expect("Failed to start server");

    let mut ws = mudd.connect_ws().await.expect("Failed to connect WebSocket");

    // Receive welcome
    let _welcome = ws.recv_json_timeout(std::time::Duration::from_secs(5)).await.unwrap();

    // Send unknown command
    ws.send_command("xyzzy").await.expect("Failed to send xyzzy");

    // Skip echo
    let _echo = ws.recv_json_timeout(std::time::Duration::from_secs(5)).await.unwrap();

    // Get output
    let output = ws
        .recv_json_timeout(std::time::Duration::from_secs(5))
        .await
        .expect("Failed to receive output");

    assert_eq!(output["type"], "output");
    let text = output["text"].as_str().unwrap();
    assert!(text.contains("Unknown command"));

    ws.close().await.ok();
}
