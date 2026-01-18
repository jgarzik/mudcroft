//! Multi-user scenario tests
//!
//! Tests builder permissions, path grants, and multi-user interactions
//! including room creation, NPC creation, and combat

use crate::harness::{Role, TestServer};
use std::time::Duration;

/// Test: Wizard grants builder access, builder creates content, player adventures
#[tokio::test]
async fn test_multiuser_builder_creates_room() {
    let server = TestServer::start().await.expect("Failed to start server");

    // Phase 1: Connect 3 users with different roles
    let mut wizard = server
        .connect_as(Role::Wizard {
            username: "admin_wiz".to_string(),
        })
        .await
        .expect("Failed to connect as wizard");

    let mut builder = server
        .connect_as(Role::Builder {
            username: "zone_builder".to_string(),
            regions: vec![],
        })
        .await
        .expect("Failed to connect as builder");

    let mut player = server
        .connect_as(Role::Player {
            username: "adventurer".to_string(),
        })
        .await
        .expect("Failed to connect as player");

    // Get account IDs
    let builder_account_id = builder
        .account_id()
        .expect("builder has account")
        .to_string();

    // Drain initial room messages for all
    wizard.drain().await;
    builder.drain().await;
    player.drain().await;

    // Phase 2: Wizard grants path access to builder via eval
    let grant_cmd = format!(
        r#"eval return game.grant_path("{}", "/d/builder-zone", true)"#,
        builder_account_id
    );
    wizard
        .command(&grant_cmd)
        .await
        .expect("grant command failed");

    let grant_result = wizard
        .expect_any_timeout(Duration::from_secs(5))
        .await
        .expect("no grant response");

    // Check it's not an error
    if grant_result["type"].as_str() == Some("error") {
        panic!(
            "Grant path failed: {:?}",
            grant_result["message"].as_str().unwrap_or("unknown error")
        );
    }

    // Phase 3: Builder creates a room
    builder
        .command(r#"create room /d/builder-zone/goblin-cave "Goblin Cave" "A dank cave filled with the stench of goblins.""#)
        .await
        .expect("create room failed");

    // Skip echo, get actual response
    let create_result = builder.expect("output").await.expect("no create response");

    let create_type = create_result["type"].as_str().unwrap_or("");
    if create_type == "error" {
        panic!(
            "Create room failed: {:?}",
            create_result["message"].as_str().unwrap_or("unknown error")
        );
    }

    let create_text = create_result["text"].as_str().unwrap_or("");
    assert!(
        create_text.contains("Created") && create_text.contains("Goblin Cave"),
        "Create should confirm creation: {}",
        create_text
    );

    // Phase 4: Builder creates a monster in the room
    builder
        .command(r#"create npc /d/builder-zone/goblin "Goblin" hp=10 max_hp=10 parent=/d/builder-zone/goblin-cave"#)
        .await
        .expect("create npc failed");

    let npc_result = builder
        .expect("output")
        .await
        .expect("no npc create response");

    let npc_text = npc_result["text"].as_str().unwrap_or("");
    assert!(
        npc_text.contains("Created"),
        "NPC create should succeed: {}",
        npc_text
    );

    // Phase 5: Builder creates treasure in the room
    builder
        .command(r#"create item /d/builder-zone/gold-coin "Gold Coin" value=50 parent=/d/builder-zone/goblin-cave"#)
        .await
        .expect("create item failed");

    let item_result = builder
        .expect("output")
        .await
        .expect("no item create response");

    let item_text = item_result["text"].as_str().unwrap_or("");
    assert!(
        item_text.contains("Created"),
        "Item create should succeed: {}",
        item_text
    );

    // Phase 6: Wizard links rooms - add "up" exit from underground-pool to goblin-cave
    // (use "up" since north/south/east/west are already used in pool room)
    wizard
        .command(r#"eval local room = game.get_object("/rooms/underground-pool"); if room then local exits = room.exits or {}; exits.up = "/d/builder-zone/goblin-cave"; return game.update_object("/rooms/underground-pool", {exits = exits}) else return "room not found" end"#)
        .await
        .expect("link room failed");

    let link_result = wizard.expect("output").await.expect("no link response");
    let link_text = link_result["text"].as_str().unwrap_or("");
    assert!(
        !link_text.to_lowercase().contains("error") && !link_text.contains("nil"),
        "Link room should succeed: {}",
        link_text
    );

    // Add reverse exit from goblin-cave back to pool
    wizard
        .command(r#"eval return game.update_object("/d/builder-zone/goblin-cave", {exits = {down = "/rooms/underground-pool"}})"#)
        .await
        .expect("reverse link failed");

    let reverse_result = wizard
        .expect("output")
        .await
        .expect("no reverse link response");
    let reverse_text = reverse_result["text"].as_str().unwrap_or("");
    assert!(
        !reverse_text.to_lowercase().contains("error") && !reverse_text.contains("nil"),
        "Reverse link should succeed: {}",
        reverse_text
    );

    // Phase 7: Builder tries to create in forbidden path (should fail)
    builder
        .command(r#"create item /d/forbidden/thing "Bad Item""#)
        .await
        .expect("forbidden create command failed");

    let forbidden_result = builder
        .expect("error")
        .await
        .expect("no forbidden create response");

    let forbidden_msg = forbidden_result["message"]
        .as_str()
        .unwrap_or("")
        .to_lowercase();
    assert!(
        forbidden_msg.contains("permission") || forbidden_msg.contains("access"),
        "Error should mention permission: {}",
        forbidden_msg
    );

    // Phase 8: Player adventure
    // Path: entrance -> north -> passage -> east -> pool -> up -> goblin-cave

    // Player starts at entrance with Rusty Short Sword
    // Try to take the sword
    player.command("take sword").await.expect("take failed");
    let take_result = player.expect("output").await.expect("no take response");
    let take_text = take_result["text"].as_str().unwrap_or("");

    // The take command should work (might say "picked up" or similar)
    // If it says "Player not found", that's a known limitation
    let sword_taken = take_text.to_lowercase().contains("pick")
        || take_text.to_lowercase().contains("take")
        || take_text.to_lowercase().contains("got");
    if !sword_taken {
        eprintln!("Note: Take sword result: {}", take_text);
    }

    // Go north to narrow passage
    player.command("north").await.expect("north failed");
    let passage_room = player.expect("room").await.expect("no room msg");
    let passage_name = passage_room["name"].as_str().unwrap_or("");
    assert!(
        passage_name.to_lowercase().contains("passage"),
        "Should be in passage: {}",
        passage_name
    );

    // Go east to underground pool
    player.command("east").await.expect("east failed");
    let pool_room = player.expect("room").await.expect("no pool room");
    let pool_name = pool_room["name"].as_str().unwrap_or("");
    assert!(
        pool_name.to_lowercase().contains("pool"),
        "Should be in pool: {}",
        pool_name
    );

    // Go up to goblin cave (the new room created by builder)
    player.command("up").await.expect("up failed");
    let goblin_room = player
        .expect("room")
        .await
        .expect("no goblin room - exit may not be linked");

    let room_name = goblin_room["name"].as_str().unwrap_or("");
    assert!(
        room_name.to_lowercase().contains("goblin"),
        "Should be in goblin cave: {}",
        room_name
    );

    // Check room contents show goblin and gold
    let contents = goblin_room["contents"]
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        })
        .unwrap_or_default();

    assert!(
        contents.to_lowercase().contains("goblin"),
        "Room should contain Goblin: {}",
        contents
    );
    assert!(
        contents.to_lowercase().contains("gold") || contents.to_lowercase().contains("coin"),
        "Room should contain Gold Coin: {}",
        contents
    );

    // Phase 9: Combat - attack the goblin until dead
    let mut goblin_dead = false;

    for attack_num in 0..20 {
        // 10 HP goblin, should take a few hits with 1d6 damage
        player
            .command("attack goblin")
            .await
            .expect("attack failed");
        let attack_result = player.expect("output").await.expect("no attack response");
        let attack_text = attack_result["text"].as_str().unwrap_or("").to_lowercase();

        if attack_num == 0 {
            // First attack should connect (hit or miss)
            assert!(
                attack_text.contains("hit")
                    || attack_text.contains("miss")
                    || attack_text.contains("damage")
                    || attack_text.contains("strike")
                    || attack_text.contains("fumble"),
                "First attack should show combat result: {}",
                attack_text
            );
        }

        if attack_text.contains("slain") || attack_text.contains("killed") {
            goblin_dead = true;
            break;
        }

        // Drain any extra messages
        player.drain().await;
    }

    // If not dead from combat messages, check if it's gone from room
    if !goblin_dead {
        player
            .command("attack goblin")
            .await
            .expect("attack failed");
        let final_attack = player
            .expect_any_timeout(Duration::from_secs(5))
            .await
            .expect("no response");

        // Skip echo
        let final_attack = if final_attack["type"].as_str() == Some("echo") {
            player
                .expect_any_timeout(Duration::from_secs(5))
                .await
                .expect("no response")
        } else {
            final_attack
        };

        if final_attack["type"].as_str() == Some("error") {
            let err = final_attack["message"]
                .as_str()
                .unwrap_or("")
                .to_lowercase();
            if err.contains("don't see") {
                goblin_dead = true;
            }
        }
    }

    assert!(goblin_dead, "Goblin should be defeated after combat");

    // Verify final room state: goblin gone, gold coin still there
    player.command("look").await.expect("look failed");
    let final_room = player.expect("room").await.expect("no room");

    let final_contents = final_room["contents"]
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
        !final_contents.contains("goblin"),
        "Dead goblin should not be in room: {}",
        final_contents
    );
    assert!(
        final_contents.contains("gold") || final_contents.contains("coin"),
        "Gold coin should still be in room: {}",
        final_contents
    );

    // Cleanup - close connections
    wizard.close().await.ok();
    builder.close().await.ok();
    player.close().await.ok();
}

/// Test: Builder can create objects only in granted paths
#[tokio::test]
async fn test_builder_path_restriction() {
    let server = TestServer::start().await.expect("Failed to start server");

    // Connect wizard and builder
    let mut wizard = server
        .connect_as(Role::Wizard {
            username: "restriction_wiz".to_string(),
        })
        .await
        .expect("Failed to connect as wizard");

    let mut builder = server
        .connect_as(Role::Builder {
            username: "restricted_builder".to_string(),
            regions: vec![],
        })
        .await
        .expect("Failed to connect as builder");

    let builder_account_id = builder
        .account_id()
        .expect("builder has account")
        .to_string();

    wizard.drain().await;
    builder.drain().await;

    // Grant access to /d/allowed only
    let grant_cmd = format!(
        r#"eval return game.grant_path("{}", "/d/allowed", false)"#,
        builder_account_id
    );
    wizard.command(&grant_cmd).await.expect("grant failed");
    wizard.drain().await;

    // Builder can create in /d/allowed
    builder
        .command(r#"create room /d/allowed/test-room "Test Room""#)
        .await
        .expect("create in allowed failed");

    let allowed_result = builder
        .expect("output")
        .await
        .expect("no allowed create response");

    assert!(
        allowed_result["text"]
            .as_str()
            .unwrap_or("")
            .contains("Created"),
        "Should be able to create in /d/allowed: {:?}",
        allowed_result
    );

    // Builder cannot create in /d/forbidden
    builder
        .command(r#"create room /d/forbidden/bad-room "Bad Room""#)
        .await
        .expect("create in forbidden failed");

    let forbidden_result = builder
        .expect("error")
        .await
        .expect("no forbidden create response");

    assert!(
        forbidden_result["message"].as_str().is_some(),
        "Should NOT be able to create in /d/forbidden"
    );

    // Builder cannot create in /d/allowed-other (different path)
    builder
        .command(r#"create room /d/allowed-other/room "Other Room""#)
        .await
        .expect("create in allowed-other failed");

    let other_result = builder
        .expect("error")
        .await
        .expect("no other create response");

    assert!(
        other_result["message"].as_str().is_some(),
        "Should NOT be able to create in /d/allowed-other (not a subpath of /d/allowed)"
    );

    wizard.close().await.ok();
    builder.close().await.ok();
}

/// Test: Player cannot use create command
#[tokio::test]
async fn test_player_cannot_create() {
    let server = TestServer::start().await.expect("Failed to start server");

    let mut player = server
        .connect_as(Role::Player {
            username: "sneaky_player".to_string(),
        })
        .await
        .expect("Failed to connect as player");

    player.drain().await;

    // Try to create something
    player
        .command(r#"create room /rooms/hacked "Hacked Room""#)
        .await
        .expect("create command failed");

    let result = player.expect("error").await.expect("no response");

    let error_msg = result["message"].as_str().unwrap_or("").to_lowercase();
    assert!(
        error_msg.contains("permission") || error_msg.contains("builder"),
        "Error should mention permission/builder: {}",
        error_msg
    );

    player.close().await.ok();
}

/// Test: Wizard can create anywhere without grants
#[tokio::test]
async fn test_wizard_can_create_anywhere() {
    let server = TestServer::start().await.expect("Failed to start server");

    let mut wizard = server
        .connect_as(Role::Wizard {
            username: "powerful_wiz".to_string(),
        })
        .await
        .expect("Failed to connect as wizard");

    wizard.drain().await;

    // Create in various paths without any grants
    wizard
        .command(r#"create room /d/anywhere/test "Test Room""#)
        .await
        .expect("create failed");

    let result = wizard.expect("output").await.expect("no response");

    assert!(
        result["text"].as_str().unwrap_or("").contains("Created"),
        "Wizard should be able to create anywhere: {:?}",
        result
    );

    wizard.close().await.ok();
}
