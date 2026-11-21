//! Messaging scenario tests
//!
//! Lua game.send / game.broadcast / game.broadcast_region messages reach
//! connected players, and actions are reported once to the actor and once
//! to onlookers.

use std::time::Duration;

use crate::harness::{Role, TestClient, TestServer};

const QUIET: Duration = Duration::from_millis(400);

async fn connect(server: &TestServer, role: Role) -> TestClient {
    let mut c = server.connect_as(role).await.expect("connect");
    c.drain().await;
    c
}

fn player(name: &str) -> Role {
    Role::Player {
        username: name.to_string(),
    }
}

fn wizard(name: &str) -> Role {
    Role::Wizard {
        username: name.to_string(),
    }
}

#[tokio::test]
async fn test_take_reported_once_to_actor_and_to_onlookers() {
    let server = TestServer::start().await.expect("server");
    let mut alice = connect(&server, player("alice")).await;
    let mut bob = connect(&server, player("bob")).await;

    alice.command("take sword").await.unwrap();
    let alice_out = alice.outputs_within(QUIET).await;
    let takes = alice_out.iter().filter(|t| t.contains("You take")).count();
    assert_eq!(takes, 1, "actor should see the take once: {:?}", alice_out);
    assert!(
        !alice_out.iter().any(|t| t.contains("alice takes")),
        "actor must not get the onlooker message: {:?}",
        alice_out
    );

    let seen = bob.expect_output_containing("alice takes").await;
    assert!(seen.is_ok(), "onlooker should see the take");
}

#[tokio::test]
async fn test_lua_send_reaches_target_player() {
    let server = TestServer::start().await.expect("server");
    let mut wiz = connect(&server, wizard("sender_wiz")).await;
    let mut bob = connect(&server, player("bob_target")).await;
    let bob_pid = bob.player_id().unwrap().to_string();

    wiz.command(&format!(r#"eval game.send("{}", "psst, bob")"#, bob_pid))
        .await
        .unwrap();
    bob.expect_output_containing("psst, bob")
        .await
        .expect("bob receives game.send message");
}

#[tokio::test]
async fn test_lua_broadcast_respects_exclude() {
    let server = TestServer::start().await.expect("server");
    let mut wiz = connect(&server, wizard("bcast_wiz")).await;
    let mut bob = connect(&server, player("bob_bcast")).await;
    let room = server.world().entrance_id.clone();
    let wiz_pid = wiz.player_id().unwrap().to_string();

    wiz.command(&format!(
        r#"eval game.broadcast("{}", "the ground shakes", "{}")"#,
        room, wiz_pid
    ))
    .await
    .unwrap();
    bob.expect_output_containing("the ground shakes")
        .await
        .expect("bob hears the broadcast");
    let wiz_out = wiz.outputs_within(QUIET).await;
    assert!(
        !wiz_out.iter().any(|t| t.contains("the ground shakes")),
        "excluded player must not receive the broadcast: {:?}",
        wiz_out
    );
}

#[tokio::test]
async fn test_lua_broadcast_region_reaches_rooms_in_region() {
    let server = TestServer::start().await.expect("server");
    let mut wiz = connect(&server, wizard("region_wiz")).await;
    let mut bob = connect(&server, player("bob_region")).await;
    let region = server.world().region_id.clone();

    wiz.command(&format!(
        r#"eval game.broadcast_region("{}", "a distant rumble")"#,
        region
    ))
    .await
    .unwrap();
    bob.expect_output_containing("a distant rumble")
        .await
        .expect("player in region hears it");
}

#[tokio::test]
async fn test_attack_reported_once_to_attacker() {
    let server = TestServer::start().await.expect("server");
    let room = server.world().entrance_id.clone();
    server
        .world()
        .create_npc("Rat", &room)
        .await
        .expect("create npc");
    let mut alice = connect(&server, player("alice_fighter")).await;
    let mut bob = connect(&server, player("bob_watcher")).await;

    alice.command("attack rat").await.unwrap();
    let alice_out = alice.outputs_within(QUIET).await;
    let combat_lines = alice_out
        .iter()
        .filter(|t| t.contains("Rat") || t.contains("rat"))
        .count();
    assert_eq!(combat_lines, 1, "attacker sees one report: {:?}", alice_out);

    bob.expect_output_containing("alice_fighter attacks Rat")
        .await
        .expect("onlooker sees the attack");
}
