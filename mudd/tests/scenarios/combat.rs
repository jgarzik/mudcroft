//! Combat scenario tests
//!
//! Attacking NPCs, damage, HP tracking and death

use crate::harness::{Role, TestClient, TestServer};
use std::time::Duration;

/// Connect a player and walk north into the Narrow Passage (Giant Bat, 15 HP)
async fn player_in_passage(server: &TestServer, name: &str) -> TestClient {
    let mut c = server
        .connect_as(Role::Player {
            username: name.to_string(),
        })
        .await
        .expect("connect");
    // The spawn room arrives after the welcome; consume it before moving so
    // it cannot be mistaken for the reply to "north"
    let spawn = c.expect("room").await.expect("spawn room");
    assert_eq!(spawn["name"], "Cave Entrance");
    c.drain().await;
    c.command("north").await.expect("north");
    let room = c.expect("room").await.expect("moved north");
    assert_eq!(room["name"], "Narrow Passage");
    c.drain().await;
    c
}

/// Send an attack command and return the reply text
async fn attack(c: &mut TestClient, cmd: &str) -> String {
    c.command(cmd).await.expect("send attack");
    let _ = c.expect("echo").await;
    let reply = c.expect_any().await.expect("attack reply");
    assert_eq!(reply["type"], "output", "attack failed: {}", reply);
    reply["text"].as_str().unwrap().to_string()
}

/// Attacking an NPC reports a hit, miss or fumble against it
#[tokio::test]
async fn test_attack_npc() {
    let server = TestServer::start().await.expect("Failed to start server");
    let mut alice = player_in_passage(&server, "attacker").await;

    for cmd in ["attack bat", "kill bat"] {
        let text = attack(&mut alice, cmd).await;
        assert!(
            text.contains("Giant Bat")
                && (text.contains("hit")
                    || text.contains("HIT")
                    || text.contains("miss")
                    || text.contains("fumble")),
            "combat output should describe the attack: {}",
            text
        );
    }
}

/// HP drops by the damage dealt
#[tokio::test]
async fn test_combat_hp_update() {
    let server = TestServer::start().await.expect("Failed to start server");
    let mut alice = player_in_passage(&server, "hp_checker").await;

    for _ in 0..30 {
        let text = attack(&mut alice, "attack bat").await;
        let Some(dmg_line) = text.lines().find(|l| l.contains(" damage")) else {
            continue; // miss or fumble
        };
        let damage: i64 = dmg_line
            .split(" for ")
            .nth(1)
            .and_then(|r| r.split_whitespace().next())
            .and_then(|n| n.parse().ok())
            .expect("damage amount");
        if text.contains("slain") {
            return; // one-shot kill: nothing left to compare
        }
        let remaining: i64 = text
            .lines()
            .find(|l| l.contains("HP remaining"))
            .and_then(|l| l.split(" has ").nth(1))
            .and_then(|r| r.split_whitespace().next())
            .and_then(|n| n.parse().ok())
            .expect("remaining HP");
        assert_eq!(remaining, 15 - damage, "first hit: {}", text);
        return;
    }
    panic!("no hit landed in 30 attacks");
}

/// Killing an NPC removes it from the room
#[tokio::test]
async fn test_kill_npc() {
    let server = TestServer::start().await.expect("Failed to start server");
    let mut alice = player_in_passage(&server, "bat_killer").await;

    let mut slain = false;
    for _ in 0..60 {
        let text = attack(&mut alice, "attack bat").await;
        if text.contains("Giant Bat is slain!") {
            slain = true;
            break;
        }
        alice.drain().await;
    }
    assert!(slain, "bat should die within 60 attacks");

    alice.command("look").await.expect("look");
    let room = alice.expect("room").await.expect("room");
    let contents = room["contents"].as_array().expect("contents");
    assert!(
        !contents.iter().any(|c| c.as_str() == Some("Giant Bat")),
        "dead bat still in room: {:?}",
        contents
    );

    // And it can no longer be attacked
    alice.command("attack bat").await.unwrap();
    let _ = alice.expect("echo").await;
    let reply = alice.expect("error").await.expect("error");
    assert!(
        reply["message"].as_str().unwrap().contains("don't see"),
        "attacking a dead NPC: {}",
        reply
    );
}

/// Attacking something absent is an error
#[tokio::test]
async fn test_attack_nothing() {
    let server = TestServer::start().await.expect("Failed to start server");
    let mut alice = server
        .connect_as(Role::Player {
            username: "shadowboxer".to_string(),
        })
        .await
        .expect("connect");
    alice.drain().await;

    alice.command("attack dragon").await.unwrap();
    let _ = alice.expect("echo").await;
    let reply = alice
        .expect_any_timeout(Duration::from_secs(5))
        .await
        .unwrap();
    assert_eq!(reply["type"], "error");
    assert_eq!(reply["message"], "You don't see 'dragon' here.");
}

/// Damage is rolled (1d6, doubled on a critical), not a fixed amount
#[tokio::test]
async fn test_attack_damage_is_rolled() {
    let server = TestServer::start().await.expect("Failed to start server");
    let entrance = server.world().entrance_id.clone();
    let mut alice = server
        .connect_as(Role::Player {
            username: "roller".to_string(),
        })
        .await
        .expect("connect");
    alice.drain().await;

    let mut damages = std::collections::BTreeSet::new();
    for round in 0..8 {
        let rat = format!("Rat{}", round);
        server.world().create_npc(&rat, &entrance).await.unwrap();
        for _ in 0..20 {
            let text = attack(&mut alice, &format!("attack {}", rat.to_lowercase())).await;
            if let Some(line) = text.lines().find(|l| l.contains(" damage")) {
                let d: i64 = line
                    .split(" for ")
                    .nth(1)
                    .and_then(|r| r.split_whitespace().next())
                    .and_then(|n| n.parse().ok())
                    .expect("damage");
                assert!((1..=12).contains(&d), "damage out of range: {}", text);
                damages.insert(d);
            }
            if text.contains("slain") {
                break;
            }
        }
        if damages.len() >= 2 {
            return;
        }
    }
    panic!("every hit did the same damage: {:?}", damages);
}

/// Subclasses of npc can be attacked
#[tokio::test]
async fn test_npc_subclasses_are_attackable() {
    let server = TestServer::start().await.expect("Failed to start server");
    let mut wiz = server
        .connect_as(Role::Wizard {
            username: "beast_maker".to_string(),
        })
        .await
        .unwrap();
    wiz.drain().await;
    let entrance = server.world().entrance_id.clone();
    wiz.command(&format!(
        r#"eval game.define_class("beast", {{parent = "npc"}}); return game.create_object("/npcs/wolf", "beast", "{}", {{name = "Wolf", hp = 5}}).id"#,
        entrance
    ))
    .await
    .unwrap();
    wiz.expect_output_containing("/npcs/wolf").await.unwrap();

    let text = attack(&mut wiz, "attack wolf").await;
    assert!(text.contains("Wolf"), "{}", text);
}

/// HP has one home, the object row: after a hit, the bat's `hp` property
/// (what Lua sees) is the HP the attack reported, written in one Raft entry
#[tokio::test]
async fn test_attack_writes_hp_to_the_object() {
    let server = TestServer::start().await.expect("Failed to start server");
    let mut alice = player_in_passage(&server, "row_fighter").await;
    let bat = server.world().bat_id.clone();
    for _ in 0..30 {
        let text = attack(&mut alice, "attack bat").await;
        if text.contains("slain") {
            return;
        }
        let Some(remaining) = text
            .lines()
            .find(|l| l.contains("HP remaining"))
            .and_then(|l| l.split(" has ").nth(1))
            .and_then(|r| r.split_whitespace().next())
            .and_then(|n| n.parse::<i64>().ok())
        else {
            continue; // miss or fumble
        };
        let stored: serde_json::Value = sqlx::query_scalar(
            "SELECT json_extract(properties, '$.hp') FROM objects WHERE universe_id = ? AND id = ?",
        )
        .bind(server.universe_id())
        .bind(&bat)
        .fetch_one(server.pool())
        .await
        .map(|v: i64| serde_json::json!(v))
        .unwrap();
        assert_eq!(stored, serde_json::json!(remaining), "{}", text);
        return;
    }
    panic!("no hit landed in 30 attacks");
}
