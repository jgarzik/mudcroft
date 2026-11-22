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

async fn eval_as(c: &mut TestClient, code: &str) -> serde_json::Value {
    c.drain().await;
    c.command(&format!("eval {}", code)).await.unwrap();
    let _ = c.expect("echo").await;
    c.expect_any().await.expect("eval reply")
}

/// game.apply_effect / get_effects / remove_effect, stored in the table
/// (surviving a restart), and checked like other object writes
#[tokio::test]
async fn test_lua_effects_api() {
    let mut server = TestServer::start().await.expect("server");
    let bat = server.world().bat_id.clone();
    let mut wiz = server
        .connect_as(Role::Wizard {
            username: "effect_wiz".into(),
        })
        .await
        .unwrap();
    let out = eval_as(
        &mut wiz,
        &format!(
            r#"game.apply_effect("{b}", "poisoned", 5, 2)
               game.apply_effect("{b}", "stunned", 3, 0)
               local e = game.get_effects("{b}")
               table.sort(e, function(x, y) return x.type < y.type end)
               return e[1].type .. ":" .. e[1].remaining .. ":" .. e[1].magnitude .. ":" .. e[1].damage_type .. "," .. e[2].type"#,
            b = bat
        ),
    )
    .await;
    assert_eq!(out["text"], "poisoned:5:2:poison,stunned", "{}", out);

    server.restart().await.expect("restart");
    let mut wiz = server
        .connect_as(Role::Wizard {
            username: "effect_wiz".into(),
        })
        .await
        .unwrap();
    let out = eval_as(
        &mut wiz,
        &format!(
            r#"local removed = game.remove_effect("{b}", "stunned")
               return tostring(removed) .. ":" .. #game.get_effects("{b}")"#,
            b = bat
        ),
    )
    .await;
    assert_eq!(out["text"], "true:1", "survived the restart: {}", out);

    // A builder (no grant covering the bat) may not apply effects to it
    let bld = server
        .connect_as(Role::Builder {
            username: "effect_builder".into(),
            regions: vec![],
        })
        .await
        .unwrap();
    let out = eval_as(
        &mut wiz,
        &format!(
            r#"game.set_actor("{who}")
               local ok = pcall(game.apply_effect, "{b}", "stunned", 3, 0)
               game.set_actor(nil)
               return tostring(ok)"#,
            who = bld.account_id(),
            b = bat
        ),
    )
    .await;
    assert_eq!(out["text"], "false", "{}", out);
}

async fn bat_row(server: &TestServer) -> (Option<i64>, Option<String>) {
    sqlx::query_as(
        "SELECT json_extract(properties, '$.hp'), parent_id FROM objects
         WHERE universe_id = ? AND id = ?",
    )
    .bind(server.universe_id())
    .bind(&server.world().bat_id)
    .fetch_one(server.pool())
    .await
    .unwrap()
}

/// Effects tick once a second: poison deals its damage each tick, through
/// the object's HP, and expires when its ticks run out
#[tokio::test]
async fn test_effects_tick_down() {
    let server = TestServer::start().await.expect("server");
    let mut wiz = server
        .connect_as(Role::Wizard {
            username: "tick_wiz".into(),
        })
        .await
        .unwrap();
    let bat = server.world().bat_id.clone();
    eval_as(
        &mut wiz,
        &format!(r#"return game.apply_effect("{}", "poisoned", 3, 2)"#, bat),
    )
    .await;
    tokio::time::sleep(Duration::from_millis(4500)).await;
    let (hp, parent) = bat_row(&server).await;
    assert_eq!(hp, Some(15 - 3 * 2), "three ticks of 2 poison");
    assert!(parent.is_some(), "still alive");
    let left: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM active_effects WHERE entity_id = ?")
        .bind(&bat)
        .fetch_one(server.pool())
        .await
        .unwrap();
    assert_eq!(left, 0, "expired effects are gone");
}

/// Damage over time can kill: the NPC leaves the room and onlookers hear it
#[tokio::test]
async fn test_effect_damage_kills() {
    let server = TestServer::start().await.expect("server");
    let mut watcher = player_in_passage(&server, "burn_watcher").await;
    let mut wiz = server
        .connect_as(Role::Wizard {
            username: "burn_wiz".into(),
        })
        .await
        .unwrap();
    let bat = server.world().bat_id.clone();
    eval_as(
        &mut wiz,
        &format!(r#"return game.apply_effect("{}", "burning", 2, 50)"#, bat),
    )
    .await;
    watcher
        .expect_output_containing("Giant Bat dies")
        .await
        .expect("the room hears the death");
    // Out of the world, at full HP, until its respawn
    let (hp, parent) = bat_row(&server).await;
    assert_eq!((hp, parent), (Some(15), None));
    let respawns: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM deadlines WHERE object_id = ? AND kind = 'respawn'",
    )
    .bind(&bat)
    .fetch_one(server.pool())
    .await
    .unwrap();
    assert_eq!(respawns, 1);
}

/// A player under an effect is told what it does each tick
#[tokio::test]
async fn test_player_hears_their_effects() {
    let server = TestServer::start().await.expect("server");
    let mut pl = player_in_passage(&server, "poisoned_player").await;
    let mut wiz = server
        .connect_as(Role::Wizard {
            username: "poisoner_wiz".into(),
        })
        .await
        .unwrap();
    eval_as(
        &mut wiz,
        &format!(
            r#"return game.apply_effect("{}", "poisoned", 2, 3)"#,
            pl.player_id().unwrap()
        ),
    )
    .await;
    pl.expect_output_containing("3 poison damage")
        .await
        .expect("damage message");
    pl.expect_output_containing("no longer poisoned")
        .await
        .expect("expiry message");
}

/// Deadlines, not a sweep: a plain effect writes once when applied and once
/// when it ends, nothing while it lasts
#[tokio::test]
async fn test_plain_effect_costs_no_writes_while_active() {
    let server = TestServer::start().await.expect("server");
    let mut wiz = server
        .connect_as(Role::Wizard {
            username: "stun_wiz".into(),
        })
        .await
        .unwrap();
    let bat = server.world().bat_id.clone();
    async fn effect_writes(server: &TestServer) -> i64 {
        sqlx::query_scalar("SELECT COUNT(*) FROM raft_log WHERE payload LIKE '%active_effects%'")
            .fetch_one(server.pool())
            .await
            .unwrap()
    }
    let before = effect_writes(&server).await;
    eval_as(
        &mut wiz,
        &format!(r#"return game.apply_effect("{}", "stunned", 2.5, 0)"#, bat),
    )
    .await;
    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert_eq!(effect_writes(&server).await - before, 1, "only the apply");
    tokio::time::sleep(Duration::from_millis(2000)).await;
    assert_eq!(effect_writes(&server).await - before, 2, "and the end");
}

/// A stunned attacker cannot act
#[tokio::test]
async fn test_stunned_player_cannot_attack() {
    let server = TestServer::start().await.expect("server");
    let mut pl = player_in_passage(&server, "dazed_player").await;
    let mut wiz = server
        .connect_as(Role::Wizard {
            username: "stunner_wiz".into(),
        })
        .await
        .unwrap();
    eval_as(
        &mut wiz,
        &format!(
            r#"return game.apply_effect("{}", "stunned", 30, 0)"#,
            pl.player_id().unwrap()
        ),
    )
    .await;
    pl.drain().await;
    pl.command("attack bat").await.unwrap();
    let _ = pl.expect("echo").await;
    let reply = pl.expect("error").await.expect("refused");
    assert!(
        reply["message"].as_str().unwrap().contains("stunned"),
        "{}",
        reply
    );
}

/// Durations and periods must be real numbers: NaN would defeat the clamps
#[tokio::test]
async fn test_effect_times_must_be_finite() {
    let server = TestServer::start().await.expect("server");
    let mut wiz = server
        .connect_as(Role::Wizard {
            username: "nan_wiz".into(),
        })
        .await
        .unwrap();
    let bat = server.world().bat_id.clone();
    let out = eval_as(
        &mut wiz,
        &format!(
            r#"local a = pcall(game.apply_effect, "{b}", "poisoned", 0/0, 1)
               local b = pcall(game.apply_effect, "{b}", "poisoned", 5, 1, nil, 0/0)
               return tostring(a) .. ":" .. tostring(b) .. ":" .. #game.get_effects("{b}")"#,
            b = bat
        ),
    )
    .await;
    assert_eq!(out["text"], "false:false:0", "{}", out);
}

/// Effects belong to one object: deleting it and creating a new one with
/// the same id does not hand the old effects to the new object
#[tokio::test]
async fn test_effects_do_not_survive_object_recreation() {
    let server = TestServer::start().await.expect("server");
    let mut wiz = server
        .connect_as(Role::Wizard {
            username: "recreate_wiz".into(),
        })
        .await
        .unwrap();
    let out = eval_as(
        &mut wiz,
        r#"game.create_object("/npcs/ghost", "npc", nil, {name = "ghost", hp = 20})
           game.apply_effect("/npcs/ghost", "poisoned", 3, 5)
           game.delete_object("/npcs/ghost")
           game.create_object("/npcs/ghost", "npc", nil, {name = "ghost", hp = 20})
           return #game.get_effects("/npcs/ghost")"#,
    )
    .await;
    assert_eq!(out["text"], "0", "{}", out);
    tokio::time::sleep(Duration::from_millis(1600)).await;
    let hp: i64 = sqlx::query_scalar(
        "SELECT json_extract(properties, '$.hp') FROM objects WHERE universe_id = ? AND id = '/npcs/ghost'",
    )
    .bind(server.universe_id())
    .fetch_one(server.pool())
    .await
    .unwrap();
    assert_eq!(hp, 20, "the new object takes no poison");
}

/// A target that is already dead (as a second attacker racing the killing
/// blow would find it) cannot be killed again: no second death or on_death
#[tokio::test]
async fn test_dead_target_is_not_killed_twice() {
    let server = TestServer::start().await.expect("server");
    let mut pl = player_in_passage(&server, "late_attacker").await;
    sqlx::query(
        "UPDATE objects SET properties = json_set(properties, '$.hp', 0)
         WHERE universe_id = ? AND id = ?",
    )
    .bind(server.universe_id())
    .bind(&server.world().bat_id)
    .execute(server.pool())
    .await
    .unwrap();
    for _ in 0..10 {
        pl.drain().await;
        pl.command("attack bat").await.unwrap();
        let _ = pl.expect("echo").await;
        let reply = pl.expect_any().await.unwrap();
        let text = reply["text"]
            .as_str()
            .or(reply["message"].as_str())
            .unwrap_or_default();
        assert!(!text.contains("slain"), "killed twice: {}", text);
    }
}

/// update_object writes only the properties it was given, so it cannot put
/// back an HP that combat or an effect changed meanwhile
#[tokio::test]
async fn test_update_object_writes_only_given_properties() {
    let server = TestServer::start().await.expect("server");
    let mut wiz = server
        .connect_as(Role::Wizard {
            username: "partial_wiz".into(),
        })
        .await
        .unwrap();
    let bat = server.world().bat_id.clone();
    eval_as(
        &mut wiz,
        &format!(
            r#"return game.update_object("{}", {{name = "Old Bat"}})"#,
            bat
        ),
    )
    .await;
    let payload: String = sqlx::query_scalar(
        "SELECT payload FROM raft_log WHERE payload LIKE '%Old Bat%' ORDER BY log_index DESC LIMIT 1",
    )
    .fetch_one(server.pool())
    .await
    .unwrap();
    assert!(payload.contains("json_set"), "{}", payload);
    assert!(!payload.contains("hp"), "hp was rewritten: {}", payload);
}

/// An NPC killed by an effect fires on_death like a kill by attack, with
/// the effect's source as killer
#[tokio::test]
async fn test_effect_death_fires_on_death() {
    let server = TestServer::start().await.expect("server");
    let mut watcher = player_in_passage(&server, "death_watcher").await;
    let passage = server.world().passage_id.clone();
    let mut wiz = server
        .connect_as(Role::Wizard {
            username: "doom_wiz".into(),
        })
        .await
        .unwrap();
    eval_as(
        &mut wiz,
        &format!(
            r#"local h = game.store_code([[return {{
                 on_death = function(a) game.broadcast("{room}", "rat on_death fired") end,
               }}]])
               game.create_object("/npcs/rat", "npc", "{room}", {{name = "rat", hp = 3, code_hash = h}})
               return game.apply_effect("/npcs/rat", "burning", 2, 50)"#,
            room = passage
        ),
    )
    .await;
    watcher
        .expect_output_containing("rat on_death fired")
        .await
        .expect("on_death ran for the effect kill");
}

/// A player killed by an effect respawns at once (HP restored, moved to the
/// respawn room) and is shown the new room
#[tokio::test]
async fn test_player_killed_by_effect_respawns() {
    let server = TestServer::start().await.expect("server");
    let mut pl = player_in_passage(&server, "doomed_player").await;
    let player_id = pl.player_id().unwrap().to_string();
    let mut wiz = server
        .connect_as(Role::Wizard {
            username: "reaper_wiz".into(),
        })
        .await
        .unwrap();
    eval_as(
        &mut wiz,
        &format!(
            r#"return game.apply_effect("{}", "burning", 2, 500)"#,
            player_id
        ),
    )
    .await;
    pl.expect_output_containing("You have died")
        .await
        .expect("death message");
    let room = pl.expect("room").await.expect("shown the respawn room");
    assert_eq!(room["name"], "Cave Entrance");
    let (hp, parent): (i64, String) = sqlx::query_as(
        "SELECT json_extract(properties, '$.hp'), parent_id FROM objects WHERE universe_id = ? AND id = ?",
    )
    .bind(server.universe_id())
    .bind(&player_id)
    .fetch_one(server.pool())
    .await
    .unwrap();
    assert_eq!(
        (hp, parent.as_str()),
        (100, server.world().entrance_id.as_str())
    );
}

/// With no respawn room (no workroom, no portal yet) a player killed by an
/// effect still dies once and revives where they fell, instead of the
/// lethal pulse being retried forever
#[tokio::test]
async fn test_effect_death_without_respawn_room() {
    let server = TestServer::start().await.expect("server");
    let mut pl = player_in_passage(&server, "portalless").await;
    let player_id = pl.player_id().unwrap().to_string();
    sqlx::query("DELETE FROM universe_settings WHERE universe_id = ? AND key = 'portal_room_id'")
        .bind(server.universe_id())
        .execute(server.pool())
        .await
        .unwrap();
    sqlx::query(
        "UPDATE objects SET properties = json_remove(properties, '$.workroom_id')
         WHERE universe_id = ? AND id = ?",
    )
    .bind(server.universe_id())
    .bind(&player_id)
    .execute(server.pool())
    .await
    .unwrap();
    let mut wiz = server
        .connect_as(Role::Wizard {
            username: "nowhere_wiz".into(),
        })
        .await
        .unwrap();
    eval_as(
        &mut wiz,
        &format!(
            r#"return game.apply_effect("{}", "burning", 2, 500)"#,
            player_id
        ),
    )
    .await;
    pl.expect_output_containing("You have died")
        .await
        .expect("died");
    let (hp, parent, effects): (i64, String, i64) = sqlx::query_as(
        "SELECT json_extract(properties, '$.hp'), parent_id,
                (SELECT COUNT(*) FROM active_effects e WHERE e.universe_id = o.universe_id AND e.entity_id = o.id)
         FROM objects o WHERE universe_id = ? AND id = ?",
    )
    .bind(server.universe_id())
    .bind(&player_id)
    .fetch_one(server.pool())
    .await
    .unwrap();
    assert_eq!(
        (hp, parent.as_str(), effects),
        (100, server.world().passage_id.as_str(), 0)
    );
}

/// A player killed by an effect leaves what they carry in a corpse (the
/// default policy) in the same atomic batch that revives them
#[tokio::test]
async fn test_effect_death_drops_inventory_atomically() {
    let server = TestServer::start().await.expect("server");
    let mut pl = player_in_passage(&server, "gem_carrier").await;
    let player_id = pl.player_id().unwrap().to_string();
    let mut wiz = server
        .connect_as(Role::Wizard {
            username: "gem_wiz".into(),
        })
        .await
        .unwrap();
    eval_as(
        &mut wiz,
        &format!(
            r#"game.create_object("/items/gem", "item", "{p}", {{name = "gem"}})
               return game.apply_effect("{p}", "burning", 2, 500)"#,
            p = player_id
        ),
    )
    .await;
    pl.expect_output_containing("You have died")
        .await
        .expect("died");
    let (corpse, corpse_room): (String, String) = sqlx::query_as(
        "SELECT c.id, c.parent_id FROM objects g JOIN objects c
           ON c.universe_id = g.universe_id AND c.id = g.parent_id
         WHERE g.universe_id = ? AND g.id = '/items/gem'",
    )
    .bind(server.universe_id())
    .fetch_one(server.pool())
    .await
    .unwrap();
    assert!(corpse.starts_with("/corpses/"), "{}", corpse);
    assert_eq!(corpse_room, server.world().passage_id);
    // One Raft entry both revives the player and moves the gem
    let entries: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM raft_log WHERE payload LIKE '%/items/gem%'
           AND payload LIKE '%SET parent_id%' AND payload LIKE '%json_set%'",
    )
    .fetch_one(server.pool())
    .await
    .unwrap();
    assert_eq!(entries, 1);
}

/// A portal pointing at a deleted room counts as no respawn room: the
/// player revives in place, and other effects keep ticking
#[tokio::test]
async fn test_effect_death_with_dangling_portal() {
    let server = TestServer::start().await.expect("server");
    let mut pl = player_in_passage(&server, "dangling").await;
    let player_id = pl.player_id().unwrap().to_string();
    sqlx::query("UPDATE universe_settings SET value = '/rooms/demolished' WHERE universe_id = ? AND key = 'portal_room_id'")
        .bind(server.universe_id())
        .execute(server.pool())
        .await
        .unwrap();
    sqlx::query(
        "UPDATE objects SET properties = json_remove(properties, '$.workroom_id')
         WHERE universe_id = ? AND id = ?",
    )
    .bind(server.universe_id())
    .bind(&player_id)
    .execute(server.pool())
    .await
    .unwrap();
    let mut wiz = server
        .connect_as(Role::Wizard {
            username: "dangle_wiz".into(),
        })
        .await
        .unwrap();
    eval_as(
        &mut wiz,
        &format!(
            r#"return game.apply_effect("{}", "burning", 2, 500)"#,
            player_id
        ),
    )
    .await;
    pl.expect_output_containing("You have died")
        .await
        .expect("died despite the dangling portal");
    let parent: String =
        sqlx::query_scalar("SELECT parent_id FROM objects WHERE universe_id = ? AND id = ?")
            .bind(server.universe_id())
            .bind(&player_id)
            .fetch_one(server.pool())
            .await
            .unwrap();
    assert_eq!(parent, server.world().passage_id);
}

/// One attack starts a fight that goes on by itself: a round per fighter
/// every `combat_round_ms` (a universe setting), the target fighting back;
/// walking away ends it
#[tokio::test]
async fn test_combat_rounds_and_retaliation() {
    let server = TestServer::start().await.expect("server");
    let entrance = server.world().entrance_id.clone();
    let mut adm = server
        .connect_as(Role::Admin {
            username: "brawler".into(),
        })
        .await
        .unwrap();
    let out = eval_as(
        &mut adm,
        &format!(
            r#"game.update_universe({{combat_round_ms = 1000}})
               game.create_object("/npcs/ogre", "npc", "{}", {{name = "ogre", hp = 1000, max_hp = 1000, armor_class = 1, attack_bonus = 30}})
               return "ready""#,
            entrance
        ),
    )
    .await;
    assert_eq!(out["text"], "ready", "{}", out);
    let first = attack(&mut adm, "attack ogre").await;
    assert!(first.contains("ogre"), "{}", first);

    // Without another command: our rounds and the ogre's
    let heard = adm.outputs_within(Duration::from_millis(3500)).await;
    let ours = heard
        .iter()
        .filter(|t| {
            t.contains("You hit the ogre")
                || t.contains("You miss the ogre")
                || t.contains("You hit ogre")
                || t.contains("You miss ogre")
        })
        .count();
    let theirs = heard
        .iter()
        .filter(|t| t.contains("ogre hits you") || t.contains("ogre misses you"))
        .count();
    assert!(ours >= 2, "our rounds went on: {:?}", heard);
    assert!(theirs >= 2, "the ogre fought back: {:?}", heard);
    let fighting: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM combat")
        .fetch_one(server.pool())
        .await
        .unwrap();
    assert_eq!(fighting, 2, "both fighting");

    // Walking away ends the fight on both sides
    adm.drain().await;
    adm.command("north").await.unwrap();
    adm.expect("room").await.expect("moved");
    tokio::time::sleep(Duration::from_millis(2500)).await;
    let fighting: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM combat")
        .fetch_one(server.pool())
        .await
        .unwrap();
    assert_eq!(fighting, 0, "the fight ended");
}

/// Attack until the target dies (a blow can miss)
async fn attack_until_slain(c: &mut TestClient, cmd: &str, name: &str) {
    for _ in 0..40 {
        c.drain().await;
        c.command(cmd).await.unwrap();
        let texts = c.outputs_within(Duration::from_millis(300)).await;
        if texts
            .iter()
            .any(|t| t.contains(&format!("{} is slain", name)))
        {
            return;
        }
        if texts.iter().any(|t| t.contains("You don't see")) {
            panic!("target gone without being slain: {:?}", texts);
        }
    }
    panic!("{} never slain", name);
}

/// An NPC's death by the default policy: its belongings in a corpse (which
/// decays after corpse_decay_s, spilling what is left), its killer gains
/// xp_value, and it respawns at home after respawn_seconds. Things can be
/// taken out of a corpse.
#[tokio::test]
async fn test_npc_death_corpse_xp_respawn() {
    let server = TestServer::start().await.expect("server");
    let entrance = server.world().entrance_id.clone();
    let mut adm = server
        .connect_as(Role::Admin {
            username: "slayer".into(),
        })
        .await
        .unwrap();
    let me = adm.player_id().unwrap().to_string();
    let out = eval_as(
        &mut adm,
        &format!(
            r#"game.update_universe({{corpse_decay_s = 2}})
               game.create_object("/npcs/goblin", "npc", "{e}", {{name = "goblin", hp = 1, max_hp = 1, armor_class = 1, xp_value = 7, respawn_seconds = 1}})
               game.create_object("/items/coin", "item", "/npcs/goblin", {{name = "coin"}})
               game.create_object("/items/ring", "item", "/npcs/goblin", {{name = "ring"}})
               return "ready""#,
            e = entrance
        ),
    )
    .await;
    assert_eq!(out["text"], "ready", "{}", out);
    attack_until_slain(&mut adm, "attack goblin", "goblin").await;

    let xp: i64 = sqlx::query_scalar(
        "SELECT json_extract(properties, '$.xp') FROM objects WHERE universe_id = ? AND id = ?",
    )
    .bind(server.universe_id())
    .bind(&me)
    .fetch_one(server.pool())
    .await
    .unwrap();
    assert_eq!(xp, 7);
    let corpse: String = sqlx::query_scalar(
        "SELECT parent_id FROM objects WHERE universe_id = ? AND id = '/items/coin'",
    )
    .bind(server.universe_id())
    .fetch_one(server.pool())
    .await
    .unwrap();
    assert!(corpse.starts_with("/corpses/"), "{}", corpse);

    // Take the coin out of the corpse
    adm.drain().await;
    adm.command("get coin from corpse").await.unwrap();
    adm.expect_output_containing("You take coin")
        .await
        .expect("took it out");

    // The goblin comes back home; the corpse crumbles, the ring spills
    let heard = adm.outputs_within(Duration::from_millis(3500)).await;
    assert!(
        heard.iter().any(|t| t.contains("goblin appears")),
        "{:?}",
        heard
    );
    assert!(heard.iter().any(|t| t.contains("crumbles")), "{:?}", heard);
    let (goblin_room, ring_room): (String, String) = sqlx::query_as(
        "SELECT (SELECT parent_id FROM objects WHERE universe_id = ?1 AND id = '/npcs/goblin'),
                (SELECT parent_id FROM objects WHERE universe_id = ?1 AND id = '/items/ring')",
    )
    .bind(server.universe_id())
    .fetch_one(server.pool())
    .await
    .unwrap();
    assert_eq!(goblin_room, entrance);
    assert_eq!(ring_room, entrance);
}

/// A universe can let players keep what they carry when they die
#[tokio::test]
async fn test_player_death_keep_policy() {
    let server = TestServer::start().await.expect("server");
    let mut pl = player_in_passage(&server, "lucky").await;
    let player_id = pl.player_id().unwrap().to_string();
    let mut adm = server
        .connect_as(Role::Admin {
            username: "keeper".into(),
        })
        .await
        .unwrap();
    eval_as(
        &mut adm,
        &format!(
            r#"game.update_universe({{player_death = "keep"}})
               game.create_object("/items/amulet", "item", "{p}", {{name = "amulet"}})
               return game.apply_effect("{p}", "burning", 2, 500)"#,
            p = player_id
        ),
    )
    .await;
    pl.expect_output_containing("You have died")
        .await
        .expect("died");
    let parent: String = sqlx::query_scalar(
        "SELECT parent_id FROM objects WHERE universe_id = ? AND id = '/items/amulet'",
    )
    .bind(server.universe_id())
    .fetch_one(server.pool())
    .await
    .unwrap();
    assert_eq!(parent, player_id, "kept");
}

/// Player against player follows the universe's pvp_policy: arena (the
/// default: only in rooms with is_arena), flagged (both typed `pvp on`),
/// open, or disabled
#[tokio::test]
async fn test_pvp_policy() {
    let server = TestServer::start().await.expect("server");
    let entrance = server.world().entrance_id.clone();
    let mut adm = server
        .connect_as(Role::Admin {
            username: "referee".into(),
        })
        .await
        .unwrap();
    let mut duelist = server
        .connect_as(Role::Player {
            username: "duelist".into(),
        })
        .await
        .unwrap();
    let mut rival = server
        .connect_as(Role::Player {
            username: "rival".into(),
        })
        .await
        .unwrap();
    // Both stand in the entrance; replies to an attack
    async fn try_attack(c: &mut TestClient) -> String {
        c.drain().await;
        c.command("attack rival").await.unwrap();
        let _ = c.expect("echo").await;
        let reply = c.expect_any().await.unwrap();
        reply["text"]
            .as_str()
            .or(reply["message"].as_str())
            .unwrap_or_default()
            .to_string()
    }
    let refused = |t: &str| t.contains("can't attack");

    // Default (arena): not here
    assert!(refused(&try_attack(&mut duelist).await));
    // ... but in an arena
    eval_as(
        &mut adm,
        &format!(
            r#"game.update_object("{}", {{is_arena = true}}) return "ok""#,
            entrance
        ),
    )
    .await;
    let t = try_attack(&mut duelist).await;
    assert!(!refused(&t) && t.contains("rival"), "{}", t);
    rival.drain().await;

    // Disabled: not even in the arena
    eval_as(
        &mut adm,
        r#"game.update_universe({pvp_policy = "disabled"}) return "ok""#,
    )
    .await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(refused(&try_attack(&mut duelist).await));

    // Flagged: only when both are flagged
    eval_as(
        &mut adm,
        r#"game.update_universe({pvp_policy = "flagged"}) return "ok""#,
    )
    .await;
    assert!(refused(&try_attack(&mut duelist).await));
    duelist.drain().await;
    duelist.command("pvp on").await.unwrap();
    duelist
        .expect_output_containing("PvP on")
        .await
        .expect("flagged");
    assert!(
        refused(&try_attack(&mut duelist).await),
        "rival is not flagged"
    );
    rival.drain().await;
    rival.command("pvp on").await.unwrap();
    rival
        .expect_output_containing("PvP on")
        .await
        .expect("flagged");
    let t = try_attack(&mut duelist).await;
    assert!(!refused(&t) && t.contains("rival"), "{}", t);
}
