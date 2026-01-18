//! Integration tests using TestServer harness

mod harness;

use harness::{MuddTest, TestServer};
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
        .execute(mudd1.pool())
        .await
        .expect("Failed to insert");

    // Verify it exists in mudd1
    let result: Option<(String,)> =
        sqlx::query_as("SELECT username FROM accounts WHERE id = 'test1'")
            .fetch_optional(mudd1.pool())
            .await
            .expect("Failed to query");
    assert_eq!(result, Some(("alice".to_string(),)));

    // Verify it does NOT exist in mudd2 (database isolation)
    let result: Option<(String,)> =
        sqlx::query_as("SELECT username FROM accounts WHERE id = 'test1'")
            .fetch_optional(mudd2.pool())
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
    let universe_id = mudd
        .create_test_universe("Test Universe", &owner_id)
        .await
        .unwrap();

    let store = ObjectStore::new(mudd.pool().clone(), None);

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
    let universe_id = mudd
        .create_test_universe("Test Universe", &owner_id)
        .await
        .unwrap();

    let store = ObjectStore::new(mudd.pool().clone(), None);

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
    store
        .create(&shield)
        .await
        .expect("Failed to create shield");

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
    let store = ObjectStore::new(mudd.pool().clone(), None);

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
    let universe_id = mudd
        .create_test_universe("Test Universe", &owner_id)
        .await
        .unwrap();

    let store = ObjectStore::new(mudd.pool().clone(), None);

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
    let universe_id = mudd
        .create_test_universe("Test Universe", &owner_id)
        .await
        .unwrap();
    let store = ObjectStore::new(mudd.pool().clone(), None);

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
    let universe_id = mudd
        .create_test_universe("Test Universe", &owner_id)
        .await
        .unwrap();
    let store = ObjectStore::new(mudd.pool().clone(), None);

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
    let universe_id = mudd
        .create_test_universe("Test Universe", &owner_id)
        .await
        .unwrap();
    let store = ObjectStore::new(mudd.pool().clone(), None);

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

    let names: Vec<_> = living.iter().filter_map(|o| o.get_string("name")).collect();
    assert!(names.contains(&"Hero"));
    assert!(names.contains(&"Goblin"));
}

#[tokio::test]
async fn test_player_movement() {
    let mudd = MuddTest::start().await.expect("Failed to start server");
    let owner_id = mudd.create_test_account("testuser").await.unwrap();
    let universe_id = mudd
        .create_test_universe("Test Universe", &owner_id)
        .await
        .unwrap();
    let store = ObjectStore::new(mudd.pool().clone(), None);

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

    let mut ws = mudd
        .connect_ws()
        .await
        .expect("Failed to connect WebSocket");

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

    let mut ws = mudd
        .connect_ws()
        .await
        .expect("Failed to connect WebSocket");

    // Receive welcome
    let _welcome = ws
        .recv_json_timeout(std::time::Duration::from_secs(5))
        .await
        .unwrap();

    // Receive initial room description (player spawns at portal)
    let _room = ws
        .recv_json_timeout(std::time::Duration::from_secs(5))
        .await
        .unwrap();

    // Send a command
    ws.send_command("look")
        .await
        .expect("Failed to send command");

    // Should receive echo
    let echo = ws
        .recv_json_timeout(std::time::Duration::from_secs(5))
        .await
        .expect("Failed to receive echo");
    assert_eq!(echo["type"], "echo");
    assert_eq!(echo["command"], "look");

    // Should receive room description
    let output = ws
        .recv_json_timeout(std::time::Duration::from_secs(5))
        .await
        .expect("Failed to receive output");
    assert_eq!(output["type"], "room");
    assert!(output["name"].is_string());

    ws.close().await.ok();
}

#[tokio::test]
async fn test_websocket_help_command() {
    let mudd = MuddTest::start().await.expect("Failed to start server");

    let mut ws = mudd
        .connect_ws()
        .await
        .expect("Failed to connect WebSocket");

    // Receive welcome
    let _welcome = ws
        .recv_json_timeout(std::time::Duration::from_secs(5))
        .await
        .unwrap();

    // Receive initial room description (player spawns at portal)
    let _room = ws
        .recv_json_timeout(std::time::Duration::from_secs(5))
        .await
        .unwrap();

    // Send help command
    ws.send_command("help").await.expect("Failed to send help");

    // Skip echo
    let _echo = ws
        .recv_json_timeout(std::time::Duration::from_secs(5))
        .await
        .unwrap();

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

    let mut ws = mudd
        .connect_ws()
        .await
        .expect("Failed to connect WebSocket");

    // Receive welcome
    let _welcome = ws
        .recv_json_timeout(std::time::Duration::from_secs(5))
        .await
        .unwrap();

    // Receive initial room description (player spawns at portal)
    let _room = ws
        .recv_json_timeout(std::time::Duration::from_secs(5))
        .await
        .unwrap();

    // Send unknown command
    ws.send_command("xyzzy")
        .await
        .expect("Failed to send xyzzy");

    // Skip echo
    let _echo = ws
        .recv_json_timeout(std::time::Duration::from_secs(5))
        .await
        .unwrap();

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

// New Harness Tests - Phase 13

#[tokio::test]
async fn test_harness_testworld_creation() {
    // TestServer::start() automatically creates a TestWorld
    let server = TestServer::start().await.expect("Failed to start server");

    // Verify the world was created
    let world = server.world();
    assert!(!world.universe_id.is_empty(), "Universe should be created");
    assert!(!world.region_id.is_empty(), "Region should be created");
    assert!(
        !world.spawn_room_id.is_empty(),
        "Spawn room should be created"
    );
    assert!(
        !world.arena_room_id.is_empty(),
        "Arena room should be created"
    );
    assert!(
        !world.wizard_account_id.is_empty(),
        "Wizard account should be created"
    );
    assert!(
        !world.builder_account_id.is_empty(),
        "Builder account should be created"
    );
}

#[tokio::test]
async fn test_harness_connect_as_player() {
    let server = TestServer::start().await.expect("Failed to start server");

    // Connect as a player (should auto-register)
    let player = server
        .connect_as(harness::Role::Player {
            username: "testplayer".to_string(),
        })
        .await
        .expect("Failed to connect as player");

    // Verify we got a player ID from the welcome message
    assert!(player.player_id().is_some(), "Should have player ID");
    assert!(player.account_id().is_some(), "Should have account ID");
    assert!(player.auth_token().is_some(), "Should have auth token");
}

#[tokio::test]
async fn test_harness_connect_as_wizard() {
    let server = TestServer::start().await.expect("Failed to start server");

    // Connect as a wizard
    let wizard = server
        .connect_as(harness::Role::Wizard {
            username: "testwizard".to_string(),
        })
        .await
        .expect("Failed to connect as wizard");

    // Verify wizard account
    assert!(
        wizard.account_id().is_some(),
        "Wizard should have account ID"
    );

    // Verify access level was set to wizard
    let account_id = wizard.account_id().unwrap();
    let level: (String,) = sqlx::query_as("SELECT access_level FROM accounts WHERE id = ?")
        .bind(account_id)
        .fetch_one(server.pool())
        .await
        .expect("Failed to query account");
    assert_eq!(level.0, "wizard", "Account should have wizard access level");
}

#[tokio::test]
async fn test_harness_multiple_clients() {
    let server = TestServer::start().await.expect("Failed to start server");

    // Connect multiple players
    let mut player1 = server
        .connect_as(harness::Role::Player {
            username: "player1".to_string(),
        })
        .await
        .expect("Failed to connect player1");

    let mut player2 = server
        .connect_as(harness::Role::Player {
            username: "player2".to_string(),
        })
        .await
        .expect("Failed to connect player2");

    // Both should have different player IDs
    assert_ne!(
        player1.player_id(),
        player2.player_id(),
        "Players should have different IDs"
    );

    // Both can send commands
    player1
        .command("look")
        .await
        .expect("player1 failed to send command");
    player2
        .command("look")
        .await
        .expect("player2 failed to send command");

    // Both should receive room descriptions (players spawn in a room)
    let resp1 = player1
        .expect("room")
        .await
        .expect("player1 didn't get room");
    let resp2 = player2
        .expect("room")
        .await
        .expect("player2 didn't get room");

    assert_eq!(resp1["type"], "room");
    assert_eq!(resp2["type"], "room");
}

// Race Condition Tests

#[tokio::test]
async fn test_race_concurrent_commands() {
    // Test that multiple clients can send commands concurrently
    let server = TestServer::start().await.expect("Failed to start server");

    // Create two players
    let mut player1 = server
        .connect_as(harness::Role::Player {
            username: "racer1".to_string(),
        })
        .await
        .expect("Failed to connect player1");

    let mut player2 = server
        .connect_as(harness::Role::Player {
            username: "racer2".to_string(),
        })
        .await
        .expect("Failed to connect player2");

    // Race: both send commands simultaneously using tokio::join!
    let (r1, r2) = tokio::join!(player1.command("look"), player2.command("look"),);

    r1.expect("player1 command should succeed");
    r2.expect("player2 command should succeed");

    // Both should receive room responses (players spawn in a room)
    let (msg1, msg2) = tokio::join!(player1.expect("room"), player2.expect("room"),);

    assert!(msg1.is_ok(), "player1 should get room");
    assert!(msg2.is_ok(), "player2 should get room");
}

#[tokio::test]
async fn test_race_mixed_roles() {
    // Test that wizard and player can interact concurrently
    let server = TestServer::start().await.expect("Failed to start server");

    let mut wizard = server
        .connect_as(harness::Role::Wizard {
            username: "mixwiz".to_string(),
        })
        .await
        .expect("Failed to connect wizard");

    let mut player = server
        .connect_as(harness::Role::Player {
            username: "mixplayer".to_string(),
        })
        .await
        .expect("Failed to connect player");

    // Race: different roles, concurrent operations
    let (w, p) = tokio::join!(wizard.command("help"), player.command("look"),);

    w.expect("wizard command should work");
    p.expect("player command should work");

    // Wizard gets output (help command), player gets room (look command)
    let (wm, pm) = tokio::join!(wizard.expect("output"), player.expect("room"),);

    let wiz_output = wm.expect("wizard should get output");
    let player_room = pm.expect("player should get room");

    assert_eq!(wiz_output["type"], "output");
    assert_eq!(player_room["type"], "room");
}

/// Test: Wizard can execute Lua via eval command
#[tokio::test]
async fn test_eval_command_basic() {
    let server = TestServer::start().await.expect("Failed to start server");

    // Connect as wizard
    let mut wizard = server
        .connect_as(harness::Role::Wizard {
            username: "evalwizard".to_string(),
        })
        .await
        .expect("Failed to connect as wizard");

    // Execute simple Lua expression
    wizard
        .command("eval return 1 + 2")
        .await
        .expect("eval command should succeed");

    // Get output
    let output = wizard
        .expect("output")
        .await
        .expect("should receive output");
    assert_eq!(output["type"], "output");
    assert_eq!(output["text"], "3");
}

/// Test: Player cannot use eval command
#[tokio::test]
async fn test_eval_command_denied_for_player() {
    let server = TestServer::start().await.expect("Failed to start server");

    // Connect as player
    let mut player = server
        .connect_as(harness::Role::Player {
            username: "evalplayer".to_string(),
        })
        .await
        .expect("Failed to connect as player");

    // Try to execute Lua
    player
        .command("eval return 1")
        .await
        .expect("command should succeed");

    // Should get error
    let output = player.expect("error").await.expect("should receive error");
    assert_eq!(output["type"], "error");
    assert!(output["message"]
        .as_str()
        .unwrap()
        .contains("Permission denied"));
}

// =============================================================================
// Persistence Tests
// =============================================================================

/// Test: Timer persistence across server restarts
#[tokio::test]
async fn test_timer_persistence() {
    use mudd::timers::{Timer, TimerManager};

    // Create a shared temp file for the database
    let db_file = tempfile::NamedTempFile::new().expect("Failed to create temp file");
    let db_path = db_file.path().to_string_lossy().to_string();

    // First phase: Create timer
    {
        // Create database directly
        let db_url = format!("sqlite:{}?mode=rwc", db_path);
        let pool = sqlx::SqlitePool::connect(&db_url)
            .await
            .expect("Failed to connect");

        // Run migrations
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS timers (
                id TEXT PRIMARY KEY,
                universe_id TEXT NOT NULL,
                object_id TEXT NOT NULL,
                method TEXT NOT NULL,
                fire_at INTEGER NOT NULL,
                args TEXT,
                created_at TEXT NOT NULL DEFAULT (datetime('now'))
            )",
        )
        .execute(&pool)
        .await
        .expect("Failed to create timers table");

        let timer_mgr = TimerManager::new(Some(pool.clone()), None);
        let timer = Timer::new(
            "u1",
            "obj1",
            "on_timer",
            60000,
            Some("test_args".to_string()),
        );
        timer_mgr.add_timer(timer).await;

        assert_eq!(timer_mgr.timer_count().await, 1);
        // Pool dropped here
    }

    // Second phase: Verify timer loaded
    {
        let db_url = format!("sqlite:{}?mode=rwc", db_path);
        let pool = sqlx::SqlitePool::connect(&db_url)
            .await
            .expect("Failed to connect");

        let timer_mgr = TimerManager::new(Some(pool), None);
        timer_mgr
            .load_from_db()
            .await
            .expect("Failed to load timers");

        assert_eq!(
            timer_mgr.timer_count().await,
            1,
            "Timer should persist across restarts"
        );
    }
}

/// Test: Permission access level persistence
#[tokio::test]
async fn test_permission_persistence() {
    use mudd::permissions::{AccessLevel, PermissionManager};

    let db_file = tempfile::NamedTempFile::new().expect("Failed to create temp file");
    let db_path = db_file.path().to_string_lossy().to_string();

    let account_id: String;

    // First phase: Set access level
    {
        let db_url = format!("sqlite:{}?mode=rwc", db_path);
        let pool = sqlx::SqlitePool::connect(&db_url)
            .await
            .expect("Failed to connect");

        // Create accounts table
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS accounts (
                id TEXT PRIMARY KEY,
                username TEXT UNIQUE NOT NULL,
                password_hash TEXT,
                salt TEXT,
                token TEXT,
                access_level TEXT NOT NULL DEFAULT 'player',
                created_at TEXT NOT NULL DEFAULT (datetime('now'))
            )",
        )
        .execute(&pool)
        .await
        .expect("Failed to create accounts table");

        // Create builder_regions table
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS builder_regions (
                account_id TEXT NOT NULL,
                region_id TEXT NOT NULL,
                PRIMARY KEY (account_id, region_id)
            )",
        )
        .execute(&pool)
        .await
        .expect("Failed to create builder_regions table");

        // Create test account
        account_id = uuid::Uuid::new_v4().to_string();
        sqlx::query(
            "INSERT INTO accounts (id, username, access_level) VALUES (?, 'wizard_test', 'player')",
        )
        .bind(&account_id)
        .execute(&pool)
        .await
        .expect("Failed to create account");

        let perms = PermissionManager::with_db(pool, None);
        perms
            .set_access_level(&account_id, AccessLevel::Wizard)
            .await;

        assert_eq!(
            perms.get_access_level(&account_id).await,
            AccessLevel::Wizard
        );
    }

    // Second phase: Verify persistence
    {
        let db_url = format!("sqlite:{}?mode=rwc", db_path);
        let pool = sqlx::SqlitePool::connect(&db_url)
            .await
            .expect("Failed to connect");

        let perms = PermissionManager::with_db(pool, None);

        // Should load from DB since not in cache
        let level = perms.get_access_level(&account_id).await;
        assert_eq!(
            level,
            AccessLevel::Wizard,
            "Access level should persist to database"
        );
    }
}

/// Test: Builder region assignment persistence
#[tokio::test]
async fn test_builder_regions_persistence() {
    use mudd::permissions::PermissionManager;

    let db_file = tempfile::NamedTempFile::new().expect("Failed to create temp file");
    let db_path = db_file.path().to_string_lossy().to_string();

    let account_id = "builder_test_account";

    // First phase: Assign regions
    {
        let db_url = format!("sqlite:{}?mode=rwc", db_path);
        let pool = sqlx::SqlitePool::connect(&db_url)
            .await
            .expect("Failed to connect");

        sqlx::query(
            "CREATE TABLE IF NOT EXISTS builder_regions (
                account_id TEXT NOT NULL,
                region_id TEXT NOT NULL,
                PRIMARY KEY (account_id, region_id)
            )",
        )
        .execute(&pool)
        .await
        .expect("Failed to create builder_regions table");

        let perms = PermissionManager::with_db(pool, None);
        perms.assign_region(account_id, "forest_region").await;
        perms.assign_region(account_id, "dungeon_region").await;

        let regions = perms.get_assigned_regions(account_id).await;
        assert!(regions.contains("forest_region"));
        assert!(regions.contains("dungeon_region"));
    }

    // Second phase: Verify persistence
    {
        let db_url = format!("sqlite:{}?mode=rwc", db_path);
        let pool = sqlx::SqlitePool::connect(&db_url)
            .await
            .expect("Failed to connect");

        let perms = PermissionManager::with_db(pool, None);
        perms
            .load_builder_regions()
            .await
            .expect("Failed to load regions");

        let regions = perms.get_assigned_regions(account_id).await;
        assert!(
            regions.contains("forest_region"),
            "forest_region should persist"
        );
        assert!(
            regions.contains("dungeon_region"),
            "dungeon_region should persist"
        );
    }
}

/// Test: Combat state (HP) persistence
#[tokio::test]
async fn test_combat_state_persistence() {
    use mudd::combat::CombatManager;

    let db_file = tempfile::NamedTempFile::new().expect("Failed to create temp file");
    let db_path = db_file.path().to_string_lossy().to_string();

    // First phase: Take damage
    {
        let db_url = format!("sqlite:{}?mode=rwc", db_path);
        let pool = sqlx::SqlitePool::connect(&db_url)
            .await
            .expect("Failed to connect");

        // Create combat_state table
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS combat_state (
                entity_id TEXT PRIMARY KEY,
                universe_id TEXT NOT NULL,
                hp INTEGER NOT NULL,
                max_hp INTEGER NOT NULL,
                armor_class INTEGER NOT NULL DEFAULT 10,
                attack_bonus INTEGER NOT NULL DEFAULT 0
            )",
        )
        .execute(&pool)
        .await
        .expect("Failed to create combat_state table");

        let combat = CombatManager::with_db(pool);
        combat
            .init_entity_with_universe("player1", Some("universe1"), 100)
            .await;

        // Verify initial state
        let state = combat.get_state("player1").await.expect("Entity not found");
        assert_eq!(state.hp, 100);

        // Take damage
        combat
            .deal_damage("player1", 30, mudd::combat::DamageType::Physical, false)
            .await
            .expect("Failed to deal damage");

        let state = combat.get_state("player1").await.expect("Entity not found");
        assert_eq!(state.hp, 70, "HP should be 70 after 30 damage");
    }

    // Second phase: Verify HP persisted
    {
        let db_url = format!("sqlite:{}?mode=rwc", db_path);
        let pool = sqlx::SqlitePool::connect(&db_url)
            .await
            .expect("Failed to connect");

        let combat = CombatManager::with_db(pool);
        combat
            .load_from_db()
            .await
            .expect("Failed to load combat state");

        let state = combat.get_state("player1").await.expect("Entity not found");
        assert_eq!(state.hp, 70, "HP should persist across restarts");
        assert_eq!(state.max_hp, 100, "Max HP should persist");
    }
}

/// Test: Active effects persistence
#[tokio::test]
async fn test_effects_persistence() {
    use mudd::combat::{EffectRegistry, EffectType, StatusEffect};

    let db_file = tempfile::NamedTempFile::new().expect("Failed to create temp file");
    let db_path = db_file.path().to_string_lossy().to_string();

    // First phase: Apply effect
    {
        let db_url = format!("sqlite:{}?mode=rwc", db_path);
        let pool = sqlx::SqlitePool::connect(&db_url)
            .await
            .expect("Failed to connect");

        // Create active_effects table
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS active_effects (
                id TEXT PRIMARY KEY,
                entity_id TEXT NOT NULL,
                effect_type TEXT NOT NULL,
                remaining_ticks INTEGER NOT NULL,
                magnitude INTEGER NOT NULL DEFAULT 0,
                damage_type TEXT,
                source_id TEXT
            )",
        )
        .execute(&pool)
        .await
        .expect("Failed to create active_effects table");

        let effects = EffectRegistry::with_db(pool);
        effects
            .add_effect("player1", StatusEffect::new(EffectType::Poisoned, 10, 5))
            .await;
        effects
            .add_effect("player1", StatusEffect::new(EffectType::Stunned, 3, 0))
            .await;

        assert!(effects.has_effect("player1", EffectType::Poisoned).await);
        assert!(effects.has_effect("player1", EffectType::Stunned).await);
    }

    // Second phase: Verify persistence
    {
        let db_url = format!("sqlite:{}?mode=rwc", db_path);
        let pool = sqlx::SqlitePool::connect(&db_url)
            .await
            .expect("Failed to connect");

        let effects = EffectRegistry::with_db(pool);
        effects
            .load_from_db()
            .await
            .expect("Failed to load effects");

        assert!(
            effects.has_effect("player1", EffectType::Poisoned).await,
            "Poisoned effect should persist"
        );
        assert!(
            effects.has_effect("player1", EffectType::Stunned).await,
            "Stunned effect should persist"
        );
    }
}

// =============================================================================
// Lua Eval Tests
// =============================================================================

/// Test: Wizard can create objects via eval
#[tokio::test]
async fn test_eval_create_object() {
    let server = TestServer::start().await.expect("Failed to start server");
    // Note: TestWorld::create() now sets up the "default" universe with a portal

    // Connect as wizard
    let mut wizard = server
        .connect_as(harness::Role::Wizard {
            username: "createwizard".to_string(),
        })
        .await
        .expect("Failed to connect as wizard");

    // Create an object via Lua and return just the id
    wizard.command("eval local obj = game.create_object('item', nil, {name = 'Test Sword'}); return obj and obj.id or 'no object'").await.expect("eval should succeed");

    // Skip the echo message
    let echo = wizard.expect("echo").await.expect("should receive echo");
    assert_eq!(echo["type"], "echo");

    // Get output or error
    let output = wizard.expect_any().await.expect("should receive message");

    if output["type"] == "error" {
        panic!("Lua error: {}", output["message"]);
    }

    assert_eq!(
        output["type"], "output",
        "Expected output, got: {:?}",
        output
    );
    let obj_id = output["text"].as_str().expect("should have text");
    assert!(!obj_id.is_empty(), "Object ID should not be empty");
    // UUID is 36 chars (with dashes) or could be "no object" if failed
    assert!(
        obj_id != "no object",
        "Object creation failed: got '{}'",
        obj_id
    );
}
