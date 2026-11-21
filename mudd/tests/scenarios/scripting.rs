//! Scripting scenario tests
//!
//! Object code attached via code_hash, hooks, custom verbs and timers,
//! all running through the server's script runner.

use std::time::Duration;

use crate::harness::{Role, TestClient, TestServer};

/// Run `eval <code>` and return the output text (panics on error replies)
pub async fn eval(client: &mut TestClient, code: &str) -> String {
    client.drain().await;
    client
        .command(&format!("eval {}", code))
        .await
        .expect("send eval");
    let _ = client.expect("echo").await;
    let reply = client.expect_any().await.expect("eval reply");
    assert_ne!(
        reply["type"], "error",
        "eval returned error: {}",
        reply["message"]
    );
    reply["text"].as_str().unwrap_or_default().to_string()
}

pub async fn wizard(server: &TestServer, name: &str) -> TestClient {
    let mut c = server
        .connect_as(Role::Wizard {
            username: name.to_string(),
        })
        .await
        .expect("connect wizard");
    c.drain().await;
    c
}

/// A `code_hash` property attaches stored code to the object
#[tokio::test]
async fn test_code_hash_attaches_object_code() {
    let server = TestServer::start().await.expect("server");
    let mut wiz = wizard(&server, "code_attach_wiz").await;

    let out = eval(
        &mut wiz,
        r#"local h = game.store_code("return { on_use = function(a) return 'used by ' .. a.actor_id end }")
           local o = game.create_object("/items/wand", "item", nil, {name = "wand", code_hash = h})
           local got = game.get_object("/items/wand")
           assert(got.code_hash == h, "code_hash on object")
           assert(got.metadata.code_hash == nil, "code_hash is not a plain property")
           return game.use_object("/items/wand", "me", "use")"#,
    )
    .await;
    assert_eq!(out, "used by me");

    // update_object can attach code to an existing object
    let out = eval(
        &mut wiz,
        r#"local h = game.store_code("return { on_use = function(a) return 'rock' end }")
           game.create_object("/items/rock", "item", nil, {name = "rock"})
           game.update_object("/items/rock", {code_hash = h})
           return game.use_object("/items/rock", "me", "use")"#,
    )
    .await;
    assert_eq!(out, "rock");

    // Unknown code is rejected
    let out = eval(
        &mut wiz,
        r#"local r = game.create_object("/items/bad", "item", nil, {code_hash = "deadbeef"})
           return r.error ~= nil and "rejected" or "accepted""#,
    )
    .await;
    assert_eq!(out, "rejected");
}

pub async fn player(server: &TestServer, name: &str) -> TestClient {
    let mut c = server
        .connect_as(Role::Player {
            username: name.to_string(),
        })
        .await
        .expect("connect player");
    c.drain().await;
    c
}

/// Direction of the exit from `from` that leads to `to`
async fn direction(server: &TestServer, from: &str, to: &str) -> String {
    let room = server.world().store().get(from).await.unwrap().unwrap();
    room.properties["exits"]
        .as_object()
        .unwrap()
        .iter()
        .find(|(_, dest)| dest.as_str() == Some(to))
        .map(|(dir, _)| dir.clone())
        .expect("exit exists")
}

/// Next message of `msg_type`, failing on timeout
async fn next_of(client: &mut TestClient, msg_type: &str) -> serde_json::Value {
    client.expect(msg_type).await.expect(msg_type)
}

/// Room on_enter / on_leave hooks run when a player walks in and out;
/// on_enter output follows the new room's description.
#[tokio::test]
async fn test_room_enter_and_leave_hooks() {
    let server = TestServer::start().await.expect("server");
    let entrance = server.world().entrance_id.clone();
    let passage = server.world().passage_id.clone();
    let mut wiz = wizard(&server, "hook_wiz").await;
    eval(
        &mut wiz,
        &format!(
            r#"local h = game.store_code([[return {{
                 on_enter = function(a) game.send(a.actor_id, "A chill greets you. (from " .. tostring(a.from) .. ")") end,
                 on_leave = function(a) game.send(a.actor_id, "The chill lets go.") end,
               }}]])
               return game.update_object("{}", {{code_hash = h}})"#,
            passage
        ),
    )
    .await;

    let mut alice = player(&server, "hook_alice").await;
    let there = direction(&server, &entrance, &passage).await;
    let back = direction(&server, &passage, &entrance).await;

    alice.command(&there).await.unwrap();
    let _ = next_of(&mut alice, "echo").await;
    let room = alice.expect_any().await.unwrap();
    assert_eq!(room["type"], "room", "room description comes first");
    let text = alice
        .expect_output_containing("A chill greets you.")
        .await
        .expect("on_enter ran");
    assert!(text.contains(&entrance), "on_enter gets from: {}", text);

    alice.command(&back).await.unwrap();
    alice
        .expect_output_containing("The chill lets go.")
        .await
        .expect("on_leave ran");
}

/// NPC on_damage / on_death hooks run during combat
#[tokio::test]
async fn test_npc_damage_and_death_hooks() {
    let server = TestServer::start().await.expect("server");
    let entrance = server.world().entrance_id.clone();
    let mut wiz = wizard(&server, "combat_hook_wiz").await;
    eval(
        &mut wiz,
        &format!(
            r#"local h = game.store_code([[return {{
                 on_damage = function(a) game.send(a.attacker_id, "The goblin yelps (" .. a.damage .. ").") end,
                 on_death = function(a) game.broadcast(a.room_id, "The goblin's amulet clatters to the floor.") end,
               }}]])
               local g = game.create_object("/npcs/goblin", "npc", "{}", {{name = "Goblin", hp = 2, max_hp = 2, code_hash = h}})
               return g.id"#,
            entrance
        ),
    )
    .await;

    let mut alice = player(&server, "goblin_slayer").await;
    let mut outputs = Vec::new();
    for _ in 0..60 {
        alice.command("attack goblin").await.unwrap();
        outputs.extend(alice.outputs_within(Duration::from_millis(150)).await);
        if outputs.iter().any(|t| t.contains("amulet clatters")) {
            break;
        }
    }
    assert!(
        outputs.iter().any(|t| t.contains("The goblin yelps")),
        "on_damage ran: {:?}",
        outputs
    );
    assert!(
        outputs.iter().any(|t| t.contains("amulet clatters")),
        "on_death ran: {:?}",
        outputs
    );
}
