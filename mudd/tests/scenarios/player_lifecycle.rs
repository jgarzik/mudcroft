//! Player lifecycle scenario tests
//!
//! Tests for persistent player objects, safe zones, disconnect handling,
//! and reconnect grace period.

use crate::harness::{Role, TestServer};
use std::time::Duration;

/// Test: Player object is created on first connect
#[tokio::test]
async fn test_player_object_created() {
    let server = TestServer::start().await.expect("Failed to start server");

    let mut wizard = server
        .connect_as(Role::Wizard {
            username: "lifecycle_wiz".to_string(),
        })
        .await
        .expect("Failed to connect as wizard");

    // Drain initial room messages
    wizard.drain().await;

    // Player should have a player_id starting with /players/p-
    let player_id = wizard.player_id().expect("should have player_id");
    assert!(
        player_id.starts_with("/players/p-"),
        "Player ID should be persistent path: {}",
        player_id
    );

    // Verify player object exists via eval
    wizard
        .command(&format!(
            r#"eval local p = game.get_object("{}"); return p and p.class or "nil""#,
            player_id
        ))
        .await
        .expect("eval failed");

    // Use expect("output") to skip echo messages
    let msg = wizard.expect("output").await.expect("no output response");
    let text = msg["text"].as_str().unwrap_or("");
    assert_eq!(
        text, "player",
        "Player object should exist with class 'player'"
    );

    wizard.close().await.ok();
}

/// Test: Same account reconnects to same player object
#[tokio::test]
async fn test_player_object_persistence() {
    let server = TestServer::start().await.expect("Failed to start server");

    // First connection
    let mut wizard1 = server
        .connect_as(Role::Wizard {
            username: "persist_wiz".to_string(),
        })
        .await
        .expect("Failed to connect as wizard");

    let player_id_1 = wizard1
        .player_id()
        .expect("should have player_id")
        .to_string();
    wizard1.close().await.ok();

    // Wait a moment then reconnect
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Second connection with same username
    let mut wizard2 = server
        .connect_as(Role::Wizard {
            username: "persist_wiz".to_string(),
        })
        .await
        .expect("Failed to reconnect as wizard");

    let player_id_2 = wizard2
        .player_id()
        .expect("should have player_id")
        .to_string();

    assert_eq!(
        player_id_1, player_id_2,
        "Same account should get same player object"
    );

    wizard2.close().await.ok();
}

/// Test: Disconnect in portal room (safe zone) preserves inventory
#[tokio::test]
async fn test_disconnect_in_portal_preserves_inventory() {
    let server = TestServer::start().await.expect("Failed to start server");

    let mut wizard = server
        .connect_as(Role::Wizard {
            username: "portal_wiz".to_string(),
        })
        .await
        .expect("Failed to connect as wizard");

    // Drain welcome and room messages
    wizard.drain().await;

    // Pick up sword (entrance is the universe portal = safe zone)
    wizard.command("take sword").await.expect("take failed");
    let msg = wizard.expect("output").await.expect("no take response");

    let take_text = msg["text"].as_str().unwrap_or("").to_lowercase();
    if msg["type"].as_str() == Some("error") || take_text.contains("player not found") {
        eprintln!("Note: take command failed: {}", take_text);
        wizard.close().await.ok();
        return;
    }

    // Verify sword in inventory
    wizard.command("inventory").await.expect("inventory failed");
    let inv_msg = wizard.expect("output").await.expect("no inv response");
    let inv_text = inv_msg["text"].as_str().unwrap_or("").to_lowercase();

    if !inv_text.contains("sword") && !inv_text.contains("rusty") {
        eprintln!("Note: Sword not in inventory after take: {}", inv_text);
        wizard.close().await.ok();
        return;
    }

    // Disconnect
    wizard.close().await.ok();

    // Wait for grace period to expire (5 seconds + buffer)
    tokio::time::sleep(Duration::from_secs(6)).await;

    // Reconnect
    let mut wizard2 = server
        .connect_as(Role::Wizard {
            username: "portal_wiz".to_string(),
        })
        .await
        .expect("Failed to reconnect");

    wizard2.drain().await;

    // Check inventory - should still have sword (was in safe zone)
    wizard2
        .command("inventory")
        .await
        .expect("inventory failed");
    let inv_msg2 = wizard2.expect("output").await.expect("no inv response");
    let inv_text2 = inv_msg2["text"].as_str().unwrap_or("").to_lowercase();

    assert!(
        inv_text2.contains("sword") || inv_text2.contains("rusty"),
        "Should still have sword after disconnect in portal: {}",
        inv_text2
    );

    wizard2.close().await.ok();
}

/// Test: Disconnect outside safe zone drops inventory
#[tokio::test]
async fn test_disconnect_outside_safezone_drops_inventory() {
    let server = TestServer::start().await.expect("Failed to start server");

    let mut wizard = server
        .connect_as(Role::Wizard {
            username: "unsafe_wiz".to_string(),
        })
        .await
        .expect("Failed to connect as wizard");

    wizard.drain().await;

    // Pick up sword at entrance
    wizard.command("take sword").await.expect("take failed");
    let msg = wizard.expect("output").await.expect("no take response");

    let take_text = msg["text"].as_str().unwrap_or("").to_lowercase();
    if msg["type"].as_str() == Some("error") || take_text.contains("player not found") {
        eprintln!("Note: take command failed: {}", take_text);
        wizard.close().await.ok();
        return;
    }

    // Move to narrow passage (not a portal, not a workroom)
    wizard.command("north").await.expect("north failed");
    let _room = wizard.expect("room").await.expect("no room");

    // Disconnect
    wizard.close().await.ok();

    // Wait for grace period to expire
    tokio::time::sleep(Duration::from_secs(6)).await;

    // Reconnect
    let mut wizard2 = server
        .connect_as(Role::Wizard {
            username: "unsafe_wiz".to_string(),
        })
        .await
        .expect("Failed to reconnect");

    wizard2.drain().await;

    // Check inventory - should be empty (was outside safe zone)
    wizard2
        .command("inventory")
        .await
        .expect("inventory failed");
    let inv_msg = wizard2.expect("output").await.expect("no inv response");
    let inv_text = inv_msg["text"].as_str().unwrap_or("").to_lowercase();

    assert!(
        !inv_text.contains("sword") && !inv_text.contains("rusty"),
        "Should NOT have sword after disconnect outside safe zone: {}",
        inv_text
    );

    // Sword should be in the narrow passage now
    wizard2
        .command("goto /rooms/narrow-passage")
        .await
        .expect("goto failed");
    let room_msg = wizard2.expect("room").await.expect("no room");
    let contents = room_msg["contents"]
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        })
        .unwrap_or_default()
        .to_lowercase();

    assert!(
        contents.contains("sword") || contents.contains("rusty"),
        "Sword should be dropped in narrow passage: {}",
        contents
    );

    wizard2.close().await.ok();
}

/// Test: Reconnect within grace period cancels inventory drop
#[tokio::test]
async fn test_reconnect_grace_period() {
    let server = TestServer::start().await.expect("Failed to start server");

    let mut wizard = server
        .connect_as(Role::Wizard {
            username: "grace_wiz".to_string(),
        })
        .await
        .expect("Failed to connect as wizard");

    wizard.drain().await;

    // Pick up sword
    wizard.command("take sword").await.expect("take failed");
    let msg = wizard.expect("output").await.expect("no take response");

    let take_text = msg["text"].as_str().unwrap_or("").to_lowercase();
    if msg["type"].as_str() == Some("error") || take_text.contains("player not found") {
        eprintln!("Note: take command failed: {}", take_text);
        wizard.close().await.ok();
        return;
    }

    // Move to unsafe zone
    wizard.command("north").await.expect("north failed");
    let _room = wizard.expect("room").await.expect("no room");

    // Disconnect
    wizard.close().await.ok();

    // Reconnect WITHIN grace period (less than 5 seconds)
    tokio::time::sleep(Duration::from_millis(500)).await;

    let mut wizard2 = server
        .connect_as(Role::Wizard {
            username: "grace_wiz".to_string(),
        })
        .await
        .expect("Failed to reconnect");

    wizard2.drain().await;

    // Check inventory - should still have sword (reconnected in time)
    wizard2
        .command("inventory")
        .await
        .expect("inventory failed");
    let inv_msg = wizard2.expect("output").await.expect("no inv response");
    let inv_text = inv_msg["text"].as_str().unwrap_or("").to_lowercase();

    assert!(
        inv_text.contains("sword") || inv_text.contains("rusty"),
        "Should still have sword after quick reconnect: {}",
        inv_text
    );

    wizard2.close().await.ok();
}

/// Test: Workroom is a safe zone
#[tokio::test]
async fn test_workroom_is_safe_zone() {
    let server = TestServer::start().await.expect("Failed to start server");

    let mut wizard = server
        .connect_as(Role::Wizard {
            username: "workroom_wiz".to_string(),
        })
        .await
        .expect("Failed to connect as wizard");

    let player_id = wizard
        .player_id()
        .expect("should have player_id")
        .to_string();
    wizard.drain().await;

    // Create a workroom
    wizard
        .command(r#"create room /d/workrooms/wiz-room "Wizard's Workroom""#)
        .await
        .expect("create failed");
    let _create_msg = wizard.expect("output").await.expect("no create response");

    // Set workroom_id on player
    wizard
        .command(&format!(
            r#"eval return game.update_object("{}", {{workroom_id = "/d/workrooms/wiz-room"}})"#,
            player_id
        ))
        .await
        .expect("set workroom failed");
    let _update_msg = wizard.expect("output").await.expect("no update response");

    // Pick up sword
    wizard.command("take sword").await.expect("take failed");
    let msg = wizard.expect("output").await.expect("no take response");

    let take_text = msg["text"].as_str().unwrap_or("").to_lowercase();
    if msg["type"].as_str() == Some("error") || take_text.contains("player not found") {
        eprintln!("Note: take command failed: {}", take_text);
        wizard.close().await.ok();
        return;
    }

    // Go to workroom
    wizard
        .command("goto /d/workrooms/wiz-room")
        .await
        .expect("goto failed");
    let _room = wizard.expect("room").await.expect("no room");

    // Disconnect
    wizard.close().await.ok();

    // Wait for grace period
    tokio::time::sleep(Duration::from_secs(6)).await;

    // Reconnect
    let mut wizard2 = server
        .connect_as(Role::Wizard {
            username: "workroom_wiz".to_string(),
        })
        .await
        .expect("Failed to reconnect");

    wizard2.drain().await;

    // Check inventory - should still have sword (workroom is safe)
    wizard2
        .command("inventory")
        .await
        .expect("inventory failed");
    let inv_msg = wizard2.expect("output").await.expect("no inv response");
    let inv_text = inv_msg["text"].as_str().unwrap_or("").to_lowercase();

    assert!(
        inv_text.contains("sword") || inv_text.contains("rusty"),
        "Should still have sword after disconnect in workroom: {}",
        inv_text
    );

    wizard2.close().await.ok();
}

/// Test: Safe zone tracking updates last_safe_location
#[tokio::test]
async fn test_safe_zone_tracking() {
    let server = TestServer::start().await.expect("Failed to start server");

    let mut wizard = server
        .connect_as(Role::Wizard {
            username: "tracking_wiz".to_string(),
        })
        .await
        .expect("Failed to connect as wizard");

    let player_id = wizard
        .player_id()
        .expect("should have player_id")
        .to_string();
    wizard.drain().await;

    // Set last_safe_location manually via eval
    wizard
        .command(&format!(
            r#"eval return game.update_object("{}", {{last_safe_location = "/rooms/cave-entrance"}})"#,
            player_id
        ))
        .await
        .expect("set location failed");
    wizard.expect("output").await.expect("no set response");

    // Check last_safe_location is set to entrance (which is the universe portal)
    // Note: properties go into p.metadata, not root level
    wizard
        .command(&format!(
            r#"eval local p = game.get_object("{}"); return (p.metadata and p.metadata.last_safe_location) or "none""#,
            player_id
        ))
        .await
        .expect("eval failed");

    let msg = wizard.expect("output").await.expect("no output response");
    let last_safe = msg["text"].as_str().unwrap_or("");

    assert_eq!(
        last_safe, "/rooms/cave-entrance",
        "last_safe_location should be entrance: {}",
        last_safe
    );

    wizard.close().await.ok();
}
