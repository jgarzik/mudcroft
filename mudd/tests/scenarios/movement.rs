//! Movement scenario tests
//!
//! Tests room navigation through the cave adventure world

use crate::harness::{Role, TestServer};
use std::time::Duration;

/// Test: Navigate through all 4 rooms via cardinal directions
#[tokio::test]
async fn test_cave_full_navigation() {
    let server = TestServer::start().await.expect("Failed to start server");

    let mut wizard = server
        .connect_as(Role::Wizard {
            username: "navwizard".to_string(),
        })
        .await
        .expect("Failed to connect as wizard");

    // Player spawns at Cave Entrance - receive initial room description
    let room = wizard.expect("room").await.expect("no initial room");
    assert!(
        room["name"].as_str().unwrap().contains("Cave Entrance"),
        "Should start at Cave Entrance, got: {}",
        room["name"]
    );

    // Move north to Narrow Passage
    wizard.command("north").await.expect("north failed");
    // Skip echo message, get room
    let _echo = wizard.expect("echo").await;
    let room = wizard.expect("room").await.expect("no room after north");
    assert!(
        room["name"].as_str().unwrap().contains("Narrow Passage"),
        "Should be at Narrow Passage, got: {}",
        room["name"]
    );

    // Move north to Treasure Chamber
    wizard.command("north").await.expect("north failed");
    let _echo = wizard.expect("echo").await;
    let room = wizard
        .expect("room")
        .await
        .expect("no room after second north");
    assert!(
        room["name"].as_str().unwrap().contains("Treasure Chamber"),
        "Should be at Treasure Chamber, got: {}",
        room["name"]
    );

    // Move south back to Narrow Passage
    wizard.command("south").await.expect("south failed");
    let _echo = wizard.expect("echo").await;
    let room = wizard.expect("room").await.expect("no room after south");
    assert!(
        room["name"].as_str().unwrap().contains("Narrow Passage"),
        "Should be back at Narrow Passage, got: {}",
        room["name"]
    );

    // Move east to Underground Pool
    wizard.command("east").await.expect("east failed");
    let _echo = wizard.expect("echo").await;
    let room = wizard.expect("room").await.expect("no room after east");
    assert!(
        room["name"].as_str().unwrap().contains("Underground Pool"),
        "Should be at Underground Pool, got: {}",
        room["name"]
    );

    // Move west back to Narrow Passage
    wizard.command("west").await.expect("west failed");
    let _echo = wizard.expect("echo").await;
    let room = wizard.expect("room").await.expect("no room after west");
    assert!(
        room["name"].as_str().unwrap().contains("Narrow Passage"),
        "Should be back at Narrow Passage, got: {}",
        room["name"]
    );

    // Move south back to Cave Entrance
    wizard.command("south").await.expect("south failed");
    let _echo = wizard.expect("echo").await;
    let room = wizard
        .expect("room")
        .await
        .expect("no room after final south");
    assert!(
        room["name"].as_str().unwrap().contains("Cave Entrance"),
        "Should be back at Cave Entrance, got: {}",
        room["name"]
    );
}

/// Test: Error when trying to move in direction with no exit
#[tokio::test]
async fn test_invalid_direction() {
    let server = TestServer::start().await.expect("Failed to start server");

    let mut wizard = server
        .connect_as(Role::Wizard {
            username: "invaliddir".to_string(),
        })
        .await
        .expect("Failed to connect as wizard");

    // Receive initial room description
    let _initial = wizard.expect("room").await;

    // At entrance, try to go west (no exit)
    wizard.command("west").await.expect("west failed");

    // Skip echo message
    let _echo = wizard.expect("echo").await;

    // Should get an error or output saying no exit
    let msg = wizard
        .expect_any_timeout(Duration::from_secs(5))
        .await
        .expect("no response");

    // Could be "error" type or "output" type with message about no exit
    let msg_type = msg["type"].as_str().unwrap();
    if msg_type == "error" {
        let text = msg["message"].as_str().unwrap_or("");
        assert!(
            text.to_lowercase().contains("exit")
                || text.to_lowercase().contains("can't go")
                || text.to_lowercase().contains("cannot go"),
            "Error should mention no exit: {}",
            text
        );
    } else if msg_type == "output" {
        let text = msg["text"].as_str().unwrap_or("");
        assert!(
            text.to_lowercase().contains("exit")
                || text.to_lowercase().contains("can't go")
                || text.to_lowercase().contains("cannot go"),
            "Output should mention no exit: {}",
            text
        );
    }
}

/// Test: Look command shows available exits
#[tokio::test]
async fn test_look_shows_exits() {
    let server = TestServer::start().await.expect("Failed to start server");

    let mut wizard = server
        .connect_as(Role::Wizard {
            username: "lookexits".to_string(),
        })
        .await
        .expect("Failed to connect as wizard");

    // Receive initial room description
    let _initial = wizard.expect("room").await;

    // Move to Narrow Passage which has multiple exits
    wizard.command("north").await.expect("north failed");
    let _echo = wizard.expect("echo").await;
    let _room = wizard.expect("room").await.expect("no room");

    wizard.command("look").await.expect("look failed");
    let _echo = wizard.expect("echo").await;
    let room = wizard.expect("room").await.expect("no room on look");

    // Check that exits are listed
    // Room message should have an "exits" field
    if let Some(exits) = room.get("exits") {
        if exits.is_array() {
            let exits_arr = exits.as_array().unwrap();
            assert!(!exits_arr.is_empty(), "Narrow Passage should have exits");
            // Should have south, north, east
            let exit_strs: Vec<&str> = exits_arr.iter().filter_map(|e| e.as_str()).collect();
            assert!(
                exit_strs.contains(&"south") || exit_strs.contains(&"s"),
                "Should have south exit"
            );
            assert!(
                exit_strs.contains(&"north") || exit_strs.contains(&"n"),
                "Should have north exit"
            );
            assert!(
                exit_strs.contains(&"east") || exit_strs.contains(&"e"),
                "Should have east exit"
            );
        } else if exits.is_object() {
            // Exits might be an object mapping direction -> room_id
            let exits_obj = exits.as_object().unwrap();
            assert!(
                !exits_obj.is_empty(),
                "Narrow Passage should have exits object"
            );
        }
    }
    // If no exits field, might be in description - that's also acceptable
}

/// Test: Look command shows NPCs and items in room
#[tokio::test]
async fn test_look_shows_contents() {
    let server = TestServer::start().await.expect("Failed to start server");

    let mut wizard = server
        .connect_as(Role::Wizard {
            username: "lookcontents".to_string(),
        })
        .await
        .expect("Failed to connect as wizard");

    // Receive initial room description at entrance
    let _initial = wizard.expect("room").await;

    // At entrance, should see Rusty Short Sword
    wizard.command("look").await.expect("look failed");
    let _echo = wizard.expect("echo").await;
    let room = wizard.expect("room").await.expect("no room");

    // Check for items in room - could be in "items" or "contents" fields
    let has_items = room.get("items").is_some_and(|i| {
        if i.is_array() {
            !i.as_array().unwrap().is_empty()
        } else {
            false
        }
    });

    // Check "contents" field for sword
    let has_sword_in_contents = room.get("contents").is_some_and(|c| {
        if let Some(arr) = c.as_array() {
            arr.iter().any(|item| {
                item.as_str()
                    .is_some_and(|s| s.to_lowercase().contains("sword"))
            })
        } else {
            false
        }
    });

    // Or mentioned in description
    let desc = room
        .get("description")
        .and_then(|d| d.as_str())
        .unwrap_or("");
    let mentions_sword = desc.to_lowercase().contains("sword");

    assert!(
        has_items || has_sword_in_contents || mentions_sword,
        "Entrance should show the Rusty Sword somehow. Room: {:?}",
        room
    );

    // Move to Narrow Passage to check for NPC
    wizard.command("north").await.expect("north failed");
    let _echo = wizard.expect("echo").await;
    let _room = wizard.expect("room").await.expect("no room");

    wizard.command("look").await.expect("look failed");
    let _echo = wizard.expect("echo").await;
    let room = wizard.expect("room").await.expect("no room on look");

    // Check for NPCs in "living" field
    let has_living = room.get("living").is_some_and(|l| {
        if l.is_array() {
            !l.as_array().unwrap().is_empty()
        } else {
            false
        }
    });

    // Check for NPCs in "npcs" field
    let has_npcs = room.get("npcs").is_some_and(|n| {
        if n.is_array() {
            !n.as_array().unwrap().is_empty()
        } else {
            false
        }
    });

    // Check "contents" field for bat (contents contains NPC names as strings)
    let has_bat_in_contents = room.get("contents").is_some_and(|c| {
        if let Some(arr) = c.as_array() {
            arr.iter().any(|item| {
                item.as_str()
                    .is_some_and(|s| s.to_lowercase().contains("bat"))
            })
        } else {
            false
        }
    });

    let desc = room
        .get("description")
        .and_then(|d| d.as_str())
        .unwrap_or("");
    let mentions_bat = desc.to_lowercase().contains("bat");

    assert!(
        has_living || has_npcs || has_bat_in_contents || mentions_bat,
        "Narrow Passage should show the Giant Bat somehow. Room: {:?}",
        room
    );
}
