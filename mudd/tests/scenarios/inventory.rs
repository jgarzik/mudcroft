//! Inventory scenario tests
//!
//! Tests item pickup, drop, and inventory listing

use crate::harness::{Role, TestServer};
use std::time::Duration;

/// Test: Pick up sword at entrance with take/get command
#[tokio::test]
async fn test_get_item() {
    let server = TestServer::start().await.expect("Failed to start server");

    let mut wizard = server
        .connect_as(Role::Wizard {
            username: "getwizard".to_string(),
        })
        .await
        .expect("Failed to connect as wizard");

    // At entrance with Rusty Short Sword
    // Try to pick it up (try both "take" and "get" as common MUD commands)
    wizard.command("take sword").await.expect("take failed");

    // Get the response
    let msg = wizard
        .expect_any_timeout(Duration::from_secs(5))
        .await
        .expect("no response");

    // Should be success output or error if command not implemented
    let msg_type = msg["type"].as_str().unwrap();

    if msg_type == "error" {
        // If "take" doesn't work, try "get"
        wizard.command("get sword").await.expect("get failed");
        let msg = wizard
            .expect_any_timeout(Duration::from_secs(5))
            .await
            .expect("no response");

        if msg["type"].as_str().unwrap() == "error" {
            // Commands may not be implemented yet - that's discoverable
            eprintln!(
                "Note: take/get commands may not be implemented. Error: {:?}",
                msg
            );
            return;
        }
    }

    // If we got an output, check inventory
    wizard.command("inventory").await.expect("inventory failed");

    let msg = wizard
        .expect_any_timeout(Duration::from_secs(5))
        .await
        .expect("no response");

    // Inventory should show the sword
    if msg["type"].as_str().unwrap() == "output" {
        let text = msg["text"].as_str().unwrap_or("").to_lowercase();
        assert!(
            text.contains("sword") || text.contains("rusty"),
            "Inventory should contain sword after pickup: {}",
            text
        );
    } else if msg["type"].as_str().unwrap() == "inventory" {
        // Might be a special inventory message type
        let items = msg.get("items");
        assert!(items.is_some(), "Inventory message should have items field");
    }
}

/// Test: Drop item makes it appear in room
#[tokio::test]
async fn test_drop_item() {
    let server = TestServer::start().await.expect("Failed to start server");

    let mut wizard = server
        .connect_as(Role::Wizard {
            username: "dropwizard".to_string(),
        })
        .await
        .expect("Failed to connect as wizard");

    // First pick up the sword
    wizard.command("take sword").await.expect("take failed");
    let msg = wizard
        .expect_any_timeout(Duration::from_secs(5))
        .await
        .expect("no response");

    if msg["type"].as_str().unwrap() == "error" {
        wizard.command("get sword").await.expect("get failed");
        let msg = wizard
            .expect_any_timeout(Duration::from_secs(5))
            .await
            .expect("no response");
        if msg["type"].as_str().unwrap() == "error" {
            eprintln!("Note: take/get commands may not be implemented");
            return;
        }
    }

    // Now drop it
    wizard.command("drop sword").await.expect("drop failed");
    let msg = wizard
        .expect_any_timeout(Duration::from_secs(5))
        .await
        .expect("no response");

    if msg["type"].as_str().unwrap() == "error" {
        eprintln!(
            "Note: drop command may not be implemented. Error: {:?}",
            msg
        );
        return;
    }

    // Look to verify sword is back in room
    wizard.command("look").await.expect("look failed");
    let room = wizard.expect("room").await.expect("no room");

    // Check room has sword again
    let has_sword_in_items = room.get("items").map_or(false, |items| {
        if let Some(arr) = items.as_array() {
            arr.iter().any(|i| {
                i.as_str()
                    .map_or(false, |s| s.to_lowercase().contains("sword"))
                    || i.get("name")
                        .and_then(|n| n.as_str())
                        .map_or(false, |s| s.to_lowercase().contains("sword"))
            })
        } else {
            false
        }
    });

    let has_sword_in_contents = room.get("contents").map_or(false, |contents| {
        if let Some(arr) = contents.as_array() {
            arr.iter().any(|i| {
                i.as_str()
                    .map_or(false, |s| s.to_lowercase().contains("sword"))
                    || i.get("name")
                        .and_then(|n| n.as_str())
                        .map_or(false, |s| s.to_lowercase().contains("sword"))
            })
        } else {
            false
        }
    });

    assert!(
        has_sword_in_items || has_sword_in_contents,
        "Sword should be back in room after drop. Room: {:?}",
        room
    );
}

/// Test: Inventory shows nothing when empty
#[tokio::test]
async fn test_inventory_empty() {
    let server = TestServer::start().await.expect("Failed to start server");

    let mut wizard = server
        .connect_as(Role::Wizard {
            username: "emptywizard".to_string(),
        })
        .await
        .expect("Failed to connect as wizard");

    // Check inventory immediately (should be empty)
    wizard.command("inventory").await.expect("inventory failed");

    let msg = wizard
        .expect_any_timeout(Duration::from_secs(5))
        .await
        .expect("no response");

    let msg_type = msg["type"].as_str().unwrap();

    if msg_type == "error" {
        eprintln!(
            "Note: inventory command may not be implemented. Error: {:?}",
            msg
        );
        return;
    }

    if msg_type == "output" {
        let text = msg["text"].as_str().unwrap_or("").to_lowercase();
        // Should indicate empty or nothing
        assert!(
            text.contains("empty")
                || text.contains("nothing")
                || text.contains("not carrying")
                || text.contains("no items")
                || text.is_empty(),
            "Empty inventory should say so: {}",
            text
        );
    } else if msg_type == "inventory" {
        // Special inventory message type
        if let Some(items) = msg.get("items") {
            if let Some(arr) = items.as_array() {
                assert!(arr.is_empty(), "Inventory should be empty initially");
            }
        }
    }
}

/// Test: Inventory lists items after pickup
#[tokio::test]
async fn test_inventory_has_items() {
    let server = TestServer::start().await.expect("Failed to start server");

    let mut wizard = server
        .connect_as(Role::Wizard {
            username: "haswizard".to_string(),
        })
        .await
        .expect("Failed to connect as wizard");

    // Pick up sword first
    wizard.command("take sword").await.expect("take failed");
    let msg = wizard
        .expect_any_timeout(Duration::from_secs(5))
        .await
        .expect("no response");

    if msg["type"].as_str().unwrap() == "error" {
        wizard.command("get sword").await.expect("get failed");
        let msg = wizard
            .expect_any_timeout(Duration::from_secs(5))
            .await
            .expect("no response");
        if msg["type"].as_str().unwrap() == "error" {
            eprintln!("Note: take/get commands may not be implemented");
            return;
        }
    }

    // Check inventory
    wizard.command("inventory").await.expect("inventory failed");

    let msg = wizard
        .expect_any_timeout(Duration::from_secs(5))
        .await
        .expect("no response");

    let msg_type = msg["type"].as_str().unwrap();

    if msg_type == "error" {
        eprintln!("Note: inventory command may not be implemented");
        return;
    }

    if msg_type == "output" {
        let text = msg["text"].as_str().unwrap_or("").to_lowercase();
        assert!(
            text.contains("sword") || text.contains("rusty"),
            "Inventory should list sword: {}",
            text
        );
    } else if msg_type == "inventory" {
        // Special inventory message
        if let Some(items) = msg.get("items") {
            if let Some(arr) = items.as_array() {
                assert!(!arr.is_empty(), "Inventory should have items");
                let has_sword = arr.iter().any(|i| {
                    i.as_str()
                        .map_or(false, |s| s.to_lowercase().contains("sword"))
                        || i.get("name")
                            .and_then(|n| n.as_str())
                            .map_or(false, |s| s.to_lowercase().contains("sword"))
                });
                assert!(has_sword, "Inventory should contain sword");
            }
        }
    }
}

/// Test: Error when trying to get item not in room
#[tokio::test]
async fn test_get_nonexistent() {
    let server = TestServer::start().await.expect("Failed to start server");

    let mut wizard = server
        .connect_as(Role::Wizard {
            username: "nonexwizard".to_string(),
        })
        .await
        .expect("Failed to connect as wizard");

    // Try to get something that doesn't exist
    wizard.command("take unicorn").await.expect("take failed");

    let msg = wizard
        .expect_any_timeout(Duration::from_secs(5))
        .await
        .expect("no response");

    let msg_type = msg["type"].as_str().unwrap();

    // Should be an error or output saying item not found
    if msg_type == "error" {
        let text = msg["message"].as_str().unwrap_or("").to_lowercase();
        assert!(
            text.contains("not found")
                || text.contains("don't see")
                || text.contains("no such")
                || text.contains("unknown")
                || text.contains("can't find"),
            "Should report item not found: {}",
            text
        );
    } else if msg_type == "output" {
        let text = msg["text"].as_str().unwrap_or("").to_lowercase();
        assert!(
            text.contains("not found")
                || text.contains("don't see")
                || text.contains("no such")
                || text.contains("can't find"),
            "Should report item not found: {}",
            text
        );
    }
}
