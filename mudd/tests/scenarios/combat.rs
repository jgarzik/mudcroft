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

/// Send an attack command and return the text of the blow it brings: the
/// first blow of a fight, or, when already fighting (retyping attack adds
/// no blow), the next round's
async fn attack(c: &mut TestClient, cmd: &str) -> String {
    try_attack(c, cmd)
        .await
        .unwrap_or_else(|e| panic!("attack failed: {}", e))
}

/// Like `attack`, but an error reply (e.g. the target died meanwhile) is
/// returned instead of failing the test
async fn try_attack(c: &mut TestClient, cmd: &str) -> Result<String, String> {
    let ours = |t: &str| {
        t.starts_with("You hit")
            || t.starts_with("You miss")
            || t.starts_with("CRITICAL")
            || t.starts_with("You fumble")
    };
    // Skip unrelated messages (an NPC's blows, effect ticks) until the
    // attack's own reply; within a round of the last blow, wait and retry
    let mut text = String::new();
    for _ in 0..5 {
        c.command(cmd).await.expect("send attack");
        let _ = c.expect("echo").await;
        text = loop {
            let reply = c.expect_any().await.expect("attack reply");
            if reply["type"] == "error" {
                return Err(reply.to_string());
            }
            let text = reply["text"].as_str().unwrap_or_default().to_string();
            if reply["type"] == "output"
                && (ours(&text)
                    || text.contains("already fighting")
                    || text.contains("turn to fight")
                    || text.contains("not ready"))
            {
                break text;
            }
        };
        if !text.contains("not ready") {
            break;
        }
        tokio::time::sleep(Duration::from_millis(1100)).await;
    }
    if ours(&text) {
        return Ok(text);
    }
    for _ in 0..20 {
        let m = c
            .expect_any_timeout(Duration::from_secs(3))
            .await
            .expect("the next round's blow");
        let t = m["text"].as_str().unwrap_or_default();
        // The next blow, or the fight's end (the target died meanwhile)
        if m["type"] == "output"
            && (ours(t) || t.contains(" dies.") || t.contains("You stop fighting"))
        {
            return Ok(t.to_string());
        }
    }
    Err(format!("no round blow after {}", cmd))
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
        if text.contains("slain") || text.contains(" dies.") {
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

    // One attack starts the fight; rounds finish it
    let first = attack(&mut alice, "attack bat").await;
    let mut slain = first.contains("Giant Bat is slain!") || first.contains("Giant Bat dies");
    let mut all = vec![first];
    for _ in 0..30 {
        if slain {
            break;
        }
        let heard = alice.outputs_within(Duration::from_millis(1000)).await;
        // Slain by a blow, or dead of bleeding
        slain = heard
            .iter()
            .any(|t| t.contains("Giant Bat is slain!") || t.contains("Giant Bat dies"));
        all.extend(heard);
    }
    assert!(slain, "bat should die within 30 rounds: {:?}", all);

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
            // The rat may bleed to death between attacks
            let Ok(text) = try_attack(&mut alice, &format!("attack {}", rat.to_lowercase())).await
            else {
                break;
            };
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
            if text.contains("slain") || text.contains(" dies.") {
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
        if text.contains("slain") || text.contains(" dies.") {
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
            t.starts_with("You hit")
                || t.starts_with("You miss")
                || t.starts_with("CRITICAL")
                || t.starts_with("You fumble")
        })
        .count();
    let theirs = heard
        .iter()
        .filter(|t| {
            t.contains("ogre hits you")
                || t.contains("ogre misses you")
                || t.contains("ogre critically hits you")
                || t.contains("ogre fumbles")
        })
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

/// Attack, then let the rounds go on until the target dies (by a blow or
/// by bleeding)
async fn attack_until_slain(c: &mut TestClient, cmd: &str, name: &str) {
    let dead = |t: &str| {
        t.contains(&format!("{} is slain", name)) || t.contains(&format!("{} dies", name))
    };
    c.drain().await;
    c.command(cmd).await.unwrap();
    let mut heard = Vec::new();
    for _ in 0..30 {
        let texts = c.outputs_within(Duration::from_millis(500)).await;
        if texts.iter().any(|t| dead(t)) {
            return;
        }
        heard.extend(texts);
    }
    panic!("{} never slain: {:?}", name, heard);
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

/// An NPC that never respawns is deleted on death with everything it still
/// holds, fixed things and nested contents included (a death must never
/// fail on what an object contains)
#[tokio::test]
async fn test_npc_deleted_with_fixed_contents() {
    let server = TestServer::start().await.expect("server");
    let entrance = server.world().entrance_id.clone();
    let mut adm = server
        .connect_as(Role::Admin {
            username: "exterminator".into(),
        })
        .await
        .unwrap();
    let out = eval_as(
        &mut adm,
        &format!(
            r#"game.create_object("/npcs/slime", "npc", "{e}", {{name = "slime", hp = 1, max_hp = 1, armor_class = 1, respawn_seconds = 0}})
               game.create_object("/items/core", "item", "/npcs/slime", {{name = "core", fixed = true}})
               game.create_object("/items/pouch", "container", "/npcs/slime", {{name = "pouch"}})
               game.create_object("/items/pebble", "item", "/items/pouch", {{name = "pebble"}})
               return "ready""#,
            e = entrance
        ),
    )
    .await;
    assert_eq!(out["text"], "ready", "{}", out);
    attack_until_slain(&mut adm, "attack slime", "slime").await;
    let left: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM objects WHERE universe_id = ? AND id IN ('/npcs/slime', '/items/core')",
    )
    .bind(server.universe_id())
    .fetch_one(server.pool())
    .await
    .unwrap();
    assert_eq!(left, 0, "the slime and its fixed core are gone");
    // Its loose belongings went to a corpse, nested contents and all
    let pebble_in: String = sqlx::query_scalar(
        "SELECT parent_id FROM objects WHERE universe_id = ? AND id = '/items/pebble'",
    )
    .bind(server.universe_id())
    .fetch_one(server.pool())
    .await
    .unwrap();
    assert_eq!(pebble_in, "/items/pouch");
}

/// Only containers and corpses can be emptied, and never a locked one
#[tokio::test]
async fn test_take_from_respects_containers() {
    let server = TestServer::start().await.expect("server");
    let entrance = server.world().entrance_id.clone();
    let mut adm = server
        .connect_as(Role::Admin {
            username: "looter".into(),
        })
        .await
        .unwrap();
    eval_as(
        &mut adm,
        &format!(
            r#"game.create_object("/items/chest", "container", "{e}", {{name = "chest", fixed = true, locked = true}})
               game.create_object("/items/gold", "item", "/items/chest", {{name = "gold"}})
               game.create_object("/items/statue", "item", "{e}", {{name = "statue", fixed = true}})
               game.create_object("/items/gem", "item", "/items/statue", {{name = "gem"}})
               return "ok""#,
            e = entrance
        ),
    )
    .await;
    adm.drain().await;
    adm.command("get gold from chest").await.unwrap();
    adm.expect_output_containing("locked")
        .await
        .expect("locked");
    adm.drain().await;
    adm.command("get gem from statue").await.unwrap();
    adm.expect_output_containing("can't take things out")
        .await
        .expect("not a container");
    eval_as(
        &mut adm,
        r#"game.update_object("/items/chest", {locked = false}) return "ok""#,
    )
    .await;
    adm.drain().await;
    adm.command("get gold from chest").await.unwrap();
    adm.expect_output_containing("You take gold")
        .await
        .expect("unlocked");
}

/// An admin standing in the entrance, with 1 s combat rounds
async fn arena_admin(server: &TestServer, name: &str) -> TestClient {
    let mut adm = server
        .connect_as(Role::Admin {
            username: name.into(),
        })
        .await
        .unwrap();
    eval_as(
        &mut adm,
        r#"game.update_universe({combat_round_ms = 1000}) return "ok""#,
    )
    .await;
    adm
}

/// An aggressive NPC attacks a player who walks in, unprovoked
#[tokio::test]
async fn test_npc_aggro_on_arrival() {
    let server = TestServer::start().await.expect("server");
    let passage = server.world().passage_id.clone();
    let mut adm = arena_admin(&server, "wanderer").await;
    eval_as(
        &mut adm,
        &format!(
            r#"game.create_object("/npcs/wolf", "npc", "{}", {{name = "wolf", hp = 500, max_hp = 500, attack_bonus = 30, aggro = true}}) return "ok""#,
            passage
        ),
    )
    .await;
    adm.drain().await;
    adm.command("north").await.unwrap();
    let heard = adm.outputs_within(Duration::from_millis(1500)).await;
    assert!(
        heard.iter().any(|t| t.contains("wolf hits you")
            || t.contains("wolf misses you")
            || t.contains("wolf critically")),
        "the wolf attacked: {:?}",
        heard
    );
}

/// An NPC below its flee_threshold runs instead of fighting
#[tokio::test]
async fn test_npc_flees_when_hurt() {
    let server = TestServer::start().await.expect("server");
    let entrance = server.world().entrance_id.clone();
    let mut adm = arena_admin(&server, "chaser").await;
    eval_as(
        &mut adm,
        &format!(
            r#"game.create_object("/npcs/hare", "npc", "{}", {{name = "hare", hp = 45, max_hp = 100, armor_class = 1, flee_threshold = 0.5}}) return "ok""#,
            entrance
        ),
    )
    .await;
    adm.drain().await;
    adm.command("attack hare").await.unwrap();
    let heard = adm.outputs_within(Duration::from_millis(2500)).await;
    assert!(
        heard.iter().any(|t| t.contains("hare flees")),
        "{:?}",
        heard
    );
    let room: String = sqlx::query_scalar(
        "SELECT parent_id FROM objects WHERE universe_id = ? AND id = '/npcs/hare'",
    )
    .bind(server.universe_id())
    .fetch_one(server.pool())
    .await
    .unwrap();
    assert_ne!(room, entrance, "it left");
}

/// NPCs of the same faction join a fight against one of their own
#[tokio::test]
async fn test_npc_faction_assists() {
    let server = TestServer::start().await.expect("server");
    let entrance = server.world().entrance_id.clone();
    let mut adm = arena_admin(&server, "provoker").await;
    eval_as(
        &mut adm,
        &format!(
            r#"for _, n in ipairs({{"orc", "brute"}}) do
                 game.create_object("/npcs/" .. n, "npc", "{}", {{name = n, hp = 500, max_hp = 500, attack_bonus = 30, faction = "horde"}})
               end
               return "ok""#,
            entrance
        ),
    )
    .await;
    adm.drain().await;
    adm.command("attack orc").await.unwrap();
    let heard = adm.outputs_within(Duration::from_millis(2500)).await;
    assert!(
        heard.iter().any(|t| t.contains("brute hits you")
            || t.contains("brute misses you")
            || t.contains("brute critically")),
        "the brute joined in: {:?}",
        heard
    );
}

/// An NPC with wander_seconds roams its region by itself
#[tokio::test]
async fn test_npc_wanders() {
    let server = TestServer::start().await.expect("server");
    let entrance = server.world().entrance_id.clone();
    let mut adm = arena_admin(&server, "watcher").await;
    eval_as(
        &mut adm,
        &format!(
            r#"game.create_object("/npcs/rat", "npc", "{}", {{name = "rat", wander_seconds = 1}}) return "ok""#,
            entrance
        ),
    )
    .await;
    let mut moved = false;
    for _ in 0..40 {
        tokio::time::sleep(Duration::from_millis(100)).await;
        let room: String = sqlx::query_scalar(
            "SELECT parent_id FROM objects WHERE universe_id = ? AND id = '/npcs/rat'",
        )
        .bind(server.universe_id())
        .fetch_one(server.pool())
        .await
        .unwrap();
        if room != entrance {
            assert_eq!(
                room,
                server.world().passage_id,
                "within the region, by an exit"
            );
            moved = true;
            break;
        }
    }
    assert!(moved, "the rat wandered");
}

/// Deleting a dead NPC never deletes a player it holds: the player is put
/// in the room instead
#[tokio::test]
async fn test_npc_deletion_spares_players_inside() {
    let server = TestServer::start().await.expect("server");
    let entrance = server.world().entrance_id.clone();
    let mut adm = server
        .connect_as(Role::Admin {
            username: "rescuer".into(),
        })
        .await
        .unwrap();
    let mut pl = server
        .connect_as(Role::Player {
            username: "swallowed".into(),
        })
        .await
        .unwrap();
    let victim = pl.player_id().unwrap().to_string();
    let out = eval_as(
        &mut adm,
        &format!(
            r#"game.create_object("/npcs/whale", "npc", "{e}", {{name = "whale", hp = 1, max_hp = 1, armor_class = 1, respawn_seconds = 0}})
               game.move_object("{v}", "/npcs/whale")
               return "ready""#,
            e = entrance,
            v = victim
        ),
    )
    .await;
    assert_eq!(out["text"], "ready", "{}", out);
    tokio::time::sleep(Duration::from_millis(200)).await;
    pl.drain().await;
    attack_until_slain(&mut adm, "attack whale", "whale").await;
    let room: Option<String> =
        sqlx::query_scalar("SELECT parent_id FROM objects WHERE universe_id = ? AND id = ?")
            .bind(server.universe_id())
            .bind(&victim)
            .fetch_one(server.pool())
            .await
            .unwrap();
    assert_eq!(
        room.as_deref(),
        Some(entrance.as_str()),
        "the player survives, in the room"
    );
    // ... and is shown where they are now
    let mut shown = false;
    for _ in 0..20 {
        match pl.expect_any_timeout(Duration::from_millis(250)).await {
            Ok(m) if m["type"] == "room" && m["name"] == "Cave Entrance" => {
                shown = true;
                break;
            }
            Ok(_) => {}
            Err(_) => {}
        }
    }
    assert!(shown, "the freed player's session follows the move");
}

/// Wielding a weapon replaces the bare 1d6 with its damage dice; worn armor
/// adds to armor class; gear that leaves its wearer is no longer equipped
#[tokio::test]
async fn test_equipment() {
    let server = TestServer::start().await.expect("server");
    let entrance = server.world().entrance_id.clone();
    let mut adm = server
        .connect_as(Role::Admin {
            username: "knight".into(),
        })
        .await
        .unwrap();
    let me = adm.player_id().unwrap().to_string();
    eval_as(
        &mut adm,
        &format!(
            r#"game.create_object("/items/maul", "item", "{me}", {{name = "maul", slot = "weapon", damage = "1d1+9", damage_type = "bludgeoning", attack_bonus = 40}})
               game.create_object("/items/helm", "item", "{me}", {{name = "helm", slot = "head", armor_class = 5}})
               game.create_object("/items/plate", "item", "{me}", {{name = "plate", slot = "body", armor_class = 8}})
               game.create_object("/npcs/dummy", "npc", "{e}", {{name = "dummy", hp = 1000, max_hp = 1000}})
               return "ok""#,
            me = me,
            e = entrance
        ),
    )
    .await;
    async fn say(c: &mut TestClient, cmd: &str) -> String {
        c.drain().await;
        c.command(cmd).await.unwrap();
        let _ = c.expect("echo").await;
        let reply = c.expect_any().await.unwrap();
        reply["text"]
            .as_str()
            .or(reply["message"].as_str())
            .unwrap_or_default()
            .to_string()
    }
    assert!(say(&mut adm, "wield maul").await.contains("You wield maul"));
    assert!(say(&mut adm, "wear helm").await.contains("You wear helm"));
    assert!(say(&mut adm, "wear plate").await.contains("You wear plate"));
    assert!(say(&mut adm, "wear maul").await.contains("can't wear"));
    let eq = say(&mut adm, "equipment").await;
    assert!(
        eq.contains("maul")
            && eq.contains("helm")
            && eq.contains("plate")
            && eq.contains("Armor class: 23"),
        "{}",
        eq
    );
    // The maul's fixed 10 damage (20 on a critical), never a d6 (a natural
    // 1 always misses: take blows until one lands)
    adm.drain().await;
    let mut hit = String::new();
    for _ in 0..10 {
        hit = attack(&mut adm, "attack dummy").await;
        if hit.contains(" damage") {
            break;
        }
    }
    assert!(
        hit.contains("for 10 damage") || hit.contains("for 20 damage"),
        "{}",
        hit
    );
    // Dropped gear is unequipped, and stays so when picked up again
    say(&mut adm, "remove helm").await;
    say(&mut adm, "drop plate").await;
    say(&mut adm, "get plate").await;
    let eq = say(&mut adm, "equipment").await;
    assert!(
        !eq.contains("plate") && !eq.contains("helm") && eq.contains("maul"),
        "{}",
        eq
    );
    assert!(eq.contains("Armor class: 10"), "{}", eq);
}

/// Retyping attack during a fight does not add blows: rounds set the pace
#[tokio::test]
async fn test_attack_spam_adds_no_blows() {
    let server = TestServer::start().await.expect("server");
    let entrance = server.world().entrance_id.clone();
    let mut adm = server
        .connect_as(Role::Admin {
            username: "spammer".into(),
        })
        .await
        .unwrap();
    eval_as(
        &mut adm,
        &format!(
            r#"game.update_universe({{combat_round_ms = 5000}})
               game.create_object("/npcs/golem", "npc", "{}", {{name = "golem", hp = 1000, max_hp = 1000}})
               return "ok""#,
            entrance
        ),
    )
    .await;
    adm.drain().await;
    for _ in 0..5 {
        adm.command("attack golem").await.unwrap();
    }
    let heard = adm.outputs_within(Duration::from_millis(1000)).await;
    let blows = heard
        .iter()
        .filter(|t| {
            t.starts_with("You hit")
                || t.starts_with("You miss")
                || t.starts_with("CRITICAL")
                || t.starts_with("You fumble")
        })
        .count();
    assert_eq!(blows, 1, "{:?}", heard);
    assert!(
        heard.iter().any(|t| t.contains("already fighting")),
        "{:?}",
        heard
    );
}

/// An NPC deleted for good still runs its on_death (before it goes)
#[tokio::test]
async fn test_on_death_runs_for_npc_deleted_for_good() {
    let server = TestServer::start().await.expect("server");
    let entrance = server.world().entrance_id.clone();
    let mut adm = server
        .connect_as(Role::Admin {
            username: "bosskiller".into(),
        })
        .await
        .unwrap();
    let out = eval_as(
        &mut adm,
        &format!(
            r#"local h = game.store_code([[return {{ on_death = function(a)
                 game.create_object("/items/trophy", "item", a.room_id, {{name = "trophy"}})
               end }}]])
               game.create_object("/npcs/boss", "npc", "{}", {{name = "boss", hp = 1, max_hp = 1, armor_class = 1, respawn_seconds = 0, code_hash = h}})
               return "ready""#,
            entrance
        ),
    )
    .await;
    assert_eq!(out["text"], "ready", "{}", out);
    attack_until_slain(&mut adm, "attack boss", "boss").await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    let trophy: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM objects WHERE universe_id = ? AND id = '/items/trophy'",
    )
    .bind(server.universe_id())
    .fetch_one(server.pool())
    .await
    .unwrap();
    assert_eq!(trophy, 1, "on_death dropped its trophy");
}

/// An NPC whose home room is gone respawns where it died instead
#[tokio::test]
async fn test_respawn_without_home_room() {
    let server = TestServer::start().await.expect("server");
    let entrance = server.world().entrance_id.clone();
    let mut adm = server
        .connect_as(Role::Admin {
            username: "homeless".into(),
        })
        .await
        .unwrap();
    eval_as(
        &mut adm,
        &format!(
            r#"game.create_object("/npcs/imp", "npc", "{}", {{name = "imp", hp = 1, max_hp = 1, armor_class = 1, respawn_seconds = 1, home_room = "/rooms/gone"}})
               return "ok""#,
            entrance
        ),
    )
    .await;
    attack_until_slain(&mut adm, "attack imp", "imp").await;
    tokio::time::sleep(Duration::from_millis(2000)).await;
    let room: Option<String> = sqlx::query_scalar(
        "SELECT parent_id FROM objects WHERE universe_id = ? AND id = '/npcs/imp'",
    )
    .bind(server.universe_id())
    .fetch_one(server.pool())
    .await
    .unwrap();
    assert_eq!(room.as_deref(), Some(entrance.as_str()));
}

/// One item per slot counts, however equipped_by got set: clones of a worn
/// item come unequipped
#[tokio::test]
async fn test_equipment_one_per_slot() {
    let server = TestServer::start().await.expect("server");
    let mut adm = server
        .connect_as(Role::Admin {
            username: "stacker".into(),
        })
        .await
        .unwrap();
    let me = adm.player_id().unwrap().to_string();
    eval_as(
        &mut adm,
        &format!(
            r#"game.create_object("/items/buckler", "item", "{me}", {{name = "buckler", slot = "shield", armor_class = 5}})
               game.create_object("/items/target", "item", "{me}", {{name = "target", slot = "shield", armor_class = 7, equipped_by = "{me}"}})
               return "ok""#,
            me = me
        ),
    )
    .await;
    adm.drain().await;
    adm.command("wear buckler").await.unwrap();
    adm.expect_output_containing("You wear buckler")
        .await
        .unwrap();
    eval_as(
        &mut adm,
        &format!(
            r#"for i = 1, 3 do game.clone_object("/items/buckler", "/items/buckler-" .. i, "{me}") end return "ok""#,
            me = me
        ),
    )
    .await;
    adm.drain().await;
    adm.command("equipment").await.unwrap();
    let eq = adm.expect_output_containing("Armor class").await.unwrap();
    // Base 10 plus one shield (5 or 7, whichever counts), never several
    assert!(
        eq.contains("Armor class: 15") || eq.contains("Armor class: 17"),
        "{}",
        eq
    );
}

/// A room's own verb named like a built-in (remove, wear, get X from Y) is
/// still reachable: custom verbs come first for those
#[tokio::test]
async fn test_custom_verbs_beat_soft_builtins() {
    let server = TestServer::start().await.expect("server");
    let entrance = server.world().entrance_id.clone();
    let mut adm = server
        .connect_as(Role::Admin {
            username: "curator".into(),
        })
        .await
        .unwrap();
    eval_as(
        &mut adm,
        &format!(
            r#"local h = game.store_code([[return {{ take_down = function(a) return "You take down the painting." end }}]])
               game.create_object("/items/painting", "item", "{e}", {{name = "painting", fixed = true, code_hash = h}})
               game.add_action("remove", "/items/painting", "take_down")
               return "ok""#,
            e = entrance
        ),
    )
    .await;
    adm.drain().await;
    adm.command("remove painting").await.unwrap();
    adm.expect_output_containing("You take down the painting")
        .await
        .expect("the room's verb ran");
}

/// Wielding replaces every weapon marked as the player's, not only the one
/// that counted
#[tokio::test]
async fn test_wield_clears_shadowed_weapon() {
    let server = TestServer::start().await.expect("server");
    let mut adm = server
        .connect_as(Role::Admin {
            username: "juggler".into(),
        })
        .await
        .unwrap();
    let me = adm.player_id().unwrap().to_string();
    eval_as(
        &mut adm,
        &format!(
            r#"game.create_object("/items/a-axe", "item", "{me}", {{name = "axe", slot = "weapon", equipped_by = "{me}"}})
               game.create_object("/items/b-bow", "item", "{me}", {{name = "bow", slot = "weapon", equipped_by = "{me}"}})
               game.create_object("/items/c-club", "item", "{me}", {{name = "club", slot = "weapon"}})
               return "ok""#,
            me = me
        ),
    )
    .await;
    adm.drain().await;
    adm.command("wield club").await.unwrap();
    adm.expect_output_containing("You wield club")
        .await
        .unwrap();
    adm.drain().await;
    adm.command("equipment").await.unwrap();
    let eq = adm.expect_output_containing("Armor class").await.unwrap();
    assert!(eq.contains("weapon: club"), "{}", eq);
}

/// A killing blow runs on_damage, then on_death, even for an NPC deleted
/// for good (it is deleted after both)
#[tokio::test]
async fn test_hooks_order_on_kill_of_npc_deleted_for_good() {
    let server = TestServer::start().await.expect("server");
    let entrance = server.world().entrance_id.clone();
    let mut adm = server
        .connect_as(Role::Admin {
            username: "chronicler".into(),
        })
        .await
        .unwrap();
    let out = eval_as(
        &mut adm,
        &format!(
            r#"game.create_object("/items/ledger", "item", "{e}", {{name = "ledger", log = ""}})
               local h = game.store_code([[
                 local function note(w) local l = game.get_object("/items/ledger") game.update_object("/items/ledger", {{log = l.metadata.log .. w}}) end
                 return {{ on_damage = function(a) note("D") end, on_death = function(a) note("X") end }}]])
               game.create_object("/npcs/moth", "npc", "{e}", {{name = "moth", hp = 1, max_hp = 1, armor_class = 1, respawn_seconds = 0, code_hash = h}})
               return "ready""#,
            e = entrance
        ),
    )
    .await;
    assert_eq!(out["text"], "ready", "{}", out);
    attack_until_slain(&mut adm, "attack moth", "moth").await;
    tokio::time::sleep(Duration::from_millis(1500)).await;
    let (log, moth): (String, i64) = sqlx::query_as(
        "SELECT (SELECT json_extract(properties, '$.log') FROM objects WHERE universe_id = ?1 AND id = '/items/ledger'),
                (SELECT COUNT(*) FROM objects WHERE universe_id = ?1 AND id = '/npcs/moth')",
    )
    .bind(server.universe_id())
    .fetch_one(server.pool())
    .await
    .unwrap();
    assert!(log.ends_with("DX"), "on_damage then on_death: {:?}", log);
    assert_eq!(moth, 0, "deleted afterwards");
}

/// A shadowing verb that fails falls back to the built-in
#[tokio::test]
async fn test_failing_shadow_verb_falls_back() {
    let server = TestServer::start().await.expect("server");
    let entrance = server.world().entrance_id.clone();
    let mut adm = server
        .connect_as(Role::Admin {
            username: "unbuggy".into(),
        })
        .await
        .unwrap();
    let me = adm.player_id().unwrap().to_string();
    eval_as(
        &mut adm,
        &format!(
            r#"local h = game.store_code([[return {{ broken = function(a) error("oops") end }}]])
               game.create_object("/items/totem", "item", "{e}", {{name = "totem", fixed = true, code_hash = h}})
               game.add_action("remove", "/items/totem", "broken")
               game.create_object("/items/cap", "item", "{me}", {{name = "cap", slot = "head", equipped_by = "{me}"}})
               return "ok""#,
            e = entrance,
            me = me
        ),
    )
    .await;
    adm.drain().await;
    adm.command("remove cap").await.unwrap();
    adm.expect_output_containing("You stop using cap")
        .await
        .expect("the built-in ran");
}

/// `flee` leaves through a random exit and ends the fight; a universe can
/// make fleeing fail (flee_fail_percent) or switch combat off entirely
#[tokio::test]
async fn test_flee_and_combat_settings() {
    let server = TestServer::start().await.expect("server");
    let entrance = server.world().entrance_id.clone();
    let mut adm = server
        .connect_as(Role::Admin {
            username: "runner".into(),
        })
        .await
        .unwrap();
    eval_as(
        &mut adm,
        &format!(
            r#"game.create_object("/npcs/bear", "npc", "{}", {{name = "bear", hp = 1000, max_hp = 1000, aggro = true}}) return "ok""#,
            entrance
        ),
    )
    .await;
    adm.drain().await;
    adm.command("attack bear").await.unwrap();
    adm.drain().await;
    adm.command("flee").await.unwrap();
    adm.expect_output_containing("You flee north")
        .await
        .expect("fled through the only exit");
    let fighting: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM combat WHERE entity_id LIKE '/players/%'")
            .fetch_one(server.pool())
            .await
            .unwrap();
    assert_eq!(fighting, 0, "the fleer's fight ended");

    // Fleeing always fails here
    eval_as(
        &mut adm,
        r#"game.update_universe({flee_fail_percent = 100}) return "ok""#,
    )
    .await;
    adm.drain().await;
    adm.command("flee").await.unwrap();
    adm.expect_output_containing("fail to get away")
        .await
        .expect("flee failed");

    // A social universe: no attacking, no aggro
    eval_as(
        &mut adm,
        r#"game.update_universe({combat = "off"}) return "ok""#,
    )
    .await;
    adm.drain().await;
    adm.command("south").await.unwrap();
    let heard = adm.outputs_within(Duration::from_millis(1500)).await;
    assert!(
        !heard
            .iter()
            .any(|t| t.contains("bear hits") || t.contains("bear misses")),
        "{:?}",
        heard
    );
    adm.drain().await;
    adm.command("attack bear").await.unwrap();
    let _ = adm.expect("echo").await;
    let reply = adm.expect_any().await.unwrap();
    assert!(
        reply["message"]
            .as_str()
            .unwrap_or_default()
            .contains("no fighting"),
        "{}",
        reply
    );
}

/// An NPC's ai_combat_tick and ai_idle_tick replace the built-in behaviour
/// when they return something; object code can make its own NPC attack but
/// never a player
#[tokio::test]
async fn test_ai_overrides_and_attack_authority() {
    let server = TestServer::start().await.expect("server");
    let entrance = server.world().entrance_id.clone();
    let passage = server.world().passage_id.clone();
    let mut adm = server
        .connect_as(Role::Admin {
            username: "puppeteer".into(),
        })
        .await
        .unwrap();
    let me = adm.player_id().unwrap().to_string();
    let out = eval_as(
        &mut adm,
        &format!(
            r#"local pacifist = game.store_code([[return {{
                 ai_combat_tick = function(a) game.broadcast(a.room_id, "The monk meditates.") return "handled" end,
               }}]])
               game.create_object("/npcs/monk", "npc", "{e}", {{name = "monk", hp = 1000, max_hp = 1000, code_hash = pacifist}})
               local greeter = game.store_code([[return {{
                 ai_idle_tick = function(a) game.broadcast(a.room_id, "The hound wags its tail.") return "handled" end,
               }}]])
               game.create_object("/npcs/hound", "npc", "{p}", {{name = "hound", hp = 50, aggro = true, code_hash = greeter}})
               local puppet = game.store_code([[return {{
                 make_fight = function(a) local r, why = game.attack(a.object_id, a.target_id) return r and "fought" or why end,
                 make_player_fight = function(a) local r, why = game.attack(a.target_id, "/npcs/monk") return r and "fought" or why end,
               }}]])
               game.create_object("/npcs/puppet", "npc", "{e}", {{name = "puppet", hp = 1000, max_hp = 1000, code_hash = puppet}})
               return "ready""#,
            e = entrance,
            p = passage
        ),
    )
    .await;
    assert_eq!(out["text"], "ready", "{}", out);

    // The monk's turn is its own ai_combat_tick: it never strikes back
    adm.drain().await;
    adm.command("attack monk").await.unwrap();
    let heard = adm.outputs_within(Duration::from_millis(2500)).await;
    assert!(heard.iter().any(|t| t.contains("meditates")), "{:?}", heard);
    assert!(
        !heard
            .iter()
            .any(|t| t.contains("monk hits you") || t.contains("monk misses you")),
        "{:?}",
        heard
    );
    eval_as(
        &mut adm,
        &format!(r#"game.stop_combat("{}") return "ok""#, me),
    )
    .await;

    // Object code makes its own NPC attack, but never a player
    let out = eval_as(
        &mut adm,
        &format!(
            r#"return game.use_object("/npcs/puppet", "/npcs/puppet", "make_fight", "/npcs/monk") .. "|" ..
                      game.use_object("/npcs/puppet", "/npcs/puppet", "make_player_fight", "{}")"#,
            me
        ),
    )
    .await;
    let text = out["text"].as_str().unwrap_or_default().to_string();
    assert!(text.starts_with("fought|"), "{}", text);
    assert!(text.contains("only for themselves"), "{}", text);
    eval_as(&mut adm, r#"game.stop_combat("/npcs/monk") return "ok""#).await;

    // The aggressive hound's ai_idle_tick greets instead of attacking
    adm.drain().await;
    adm.command("north").await.unwrap();
    let heard = adm.outputs_within(Duration::from_millis(1500)).await;
    assert!(
        heard.iter().any(|t| t.contains("wags its tail")),
        "{:?}",
        heard
    );
    assert!(
        !heard
            .iter()
            .any(|t| t.contains("hound hits") || t.contains("hound misses")),
        "{:?}",
        heard
    );
}

/// The tutorial's Part B crypt (docs/langtutor.md), taken from the doc
/// itself, builds and plays on the engine: the skeleton guard attacks on
/// sight, a potion heals. Keeps the tutorial in step with the engine.
#[tokio::test]
async fn test_tutorial_crypt_plays() {
    const TUTORIAL: &str = include_str!("../../docs/langtutor.md");
    let start = TUTORIAL.find("### `crypt.lua`").expect("crypt section");
    let code_start = TUTORIAL[start..].find("```lua\n").expect("code block") + start + 7;
    let code_end = TUTORIAL[code_start..].find("\n```").expect("block end") + code_start;
    let crypt = &TUTORIAL[code_start..code_end];

    let server = TestServer::start().await.expect("server");
    let mut wiz = server
        .connect_as(Role::Wizard {
            username: "cryptkeeper".into(),
        })
        .await
        .unwrap();
    let out = eval_as(
        &mut wiz,
        &format!("{}\nreturn build_crypt() .. ' / ' .. crypt_verbs()", crypt),
    )
    .await;
    let text = out["text"].as_str().unwrap_or_default().to_string();
    assert!(
        text.contains("is built") && text.contains("4 verbs"),
        "{}",
        out
    );

    wiz.drain().await;
    wiz.command("goto /d/crypt/entrance").await.unwrap();
    wiz.expect("room").await.expect("at the entrance");
    wiz.drain().await;
    wiz.command("s").await.unwrap();
    let heard = wiz.outputs_within(Duration::from_millis(2500)).await;
    assert!(
        heard.iter().any(|t| t.contains("skeleton guard hits you")
            || t.contains("skeleton guard misses you")
            || t.contains("skeleton guard critically")),
        "the guard attacks on sight: {:?}",
        heard
    );
    wiz.drain().await;
    wiz.command("take potion").await.unwrap();
    wiz.expect_output_containing("You take healing potion")
        .await
        .expect("took the potion");
    wiz.drain().await;
    wiz.command("drink potion").await.unwrap();
    wiz.expect_output_containing("You feel better")
        .await
        .expect("drank it");
}

/// Blows are paced even across stop_combat: a script cannot land blow
/// after blow by stopping and restarting a fight
#[tokio::test]
async fn test_blow_pacing_survives_stop() {
    let server = TestServer::start().await.expect("server");
    let entrance = server.world().entrance_id.clone();
    let mut adm = server
        .connect_as(Role::Admin {
            username: "pacer".into(),
        })
        .await
        .unwrap();
    let out = eval_as(
        &mut adm,
        &format!(
            r#"game.update_universe({{combat_round_ms = 5000}})
               game.create_object("/npcs/brawler", "npc", "{e}", {{name = "brawler", hp = 1000, max_hp = 1000}})
               game.create_object("/npcs/bag", "npc", "{e}", {{name = "bag", hp = 1000, max_hp = 1000}})
               local first = game.attack("/npcs/brawler", "/npcs/bag")
               game.stop_combat("/npcs/brawler")
               local second, why = game.attack("/npcs/brawler", "/npcs/bag")
               return tostring(first ~= nil) .. "|" .. tostring(second ~= nil) .. "|" .. tostring(why)"#,
            e = entrance
        ),
    )
    .await;
    let text = out["text"].as_str().unwrap_or_default();
    assert!(text.starts_with("true|false|"), "{}", out);
    assert!(text.contains("not ready"), "{}", out);
}

/// AI handlers that move people around cannot chain without end: an NPC
/// in each of two rooms that bounces arrivals to the other room stops
/// after a bounded number of bounces, and the server stays responsive
#[tokio::test]
async fn test_ai_hooks_are_bounded() {
    let server = TestServer::start().await.expect("server");
    let mut adm = server
        .connect_as(Role::Admin {
            username: "pinball".into(),
        })
        .await
        .unwrap();
    let out = eval_as(
        &mut adm,
        r#"local h = game.store_code([[return { ai_idle_tick = function(a)
             if a.reason ~= "arrival" then return nil end
             local me = game.get_object(a.object_id)
             local c = game.get_object("/items/counter")
             game.update_object("/items/counter", {n = c.metadata.n + 1})
             game.move_object(a.arrived_id, me.metadata.other)
             return "bounced"
           end }]])
           game.create_object("/rooms/ping", "room", nil, {name = "Ping", exits = {east = "/rooms/pong"}})
           game.create_object("/rooms/pong", "room", nil, {name = "Pong", exits = {west = "/rooms/ping"}})
           game.create_object("/items/counter", "item", nil, {name = "counter", n = 0})
           game.create_object("/rooms/ping/bouncer", "npc", "/rooms/ping", {name = "bouncer", other = "/rooms/pong", code_hash = h})
           game.create_object("/rooms/pong/bouncer", "npc", "/rooms/pong", {name = "bouncer", other = "/rooms/ping", code_hash = h})
           return "ready""#,
    )
    .await;
    assert_eq!(out["text"], "ready", "{}", out);
    adm.drain().await;
    adm.command("goto /rooms/ping").await.unwrap();
    tokio::time::sleep(Duration::from_millis(1500)).await;
    adm.drain().await;
    adm.command("look").await.unwrap();
    adm.expect("room").await.expect("still responsive");
    let n: i64 = sqlx::query_scalar(
        "SELECT json_extract(properties, '$.n') FROM objects WHERE universe_id = ? AND id = '/items/counter'",
    )
    .bind(server.universe_id())
    .fetch_one(server.pool())
    .await
    .unwrap();
    assert!((1..=40).contains(&n), "bounded bounces: {}", n);
}

/// Phase 1 review: AI handlers are engine hooks (no script reaches them);
/// game.damage needs control of its source to credit it; a dead NPC waiting
/// to respawn cannot be damaged or healed; aggro only for creatures (so the
/// player can always fight back); a player fleeing into an aggressive NPC
/// fights back at once
#[tokio::test]
async fn test_engine_binding_rules() {
    let server = TestServer::start().await.expect("server");
    let entrance = server.world().entrance_id.clone();
    let mut adm = server
        .connect_as(Role::Admin {
            username: "auditor".into(),
        })
        .await
        .unwrap();
    let pl = server
        .connect_as(Role::Player {
            username: "bystander".into(),
        })
        .await
        .unwrap();
    let other = pl.player_id().unwrap().to_string();
    let out = eval_as(
        &mut adm,
        &format!(
            r#"local h = game.store_code([[return {{ ai_combat_tick = function(a) return "ran" end }}]])
               game.create_object("/npcs/scripted", "npc", "{e}", {{name = "scripted", hp = 50, code_hash = h}})
               local via_use = game.use_object("/npcs/scripted", "/npcs/scripted", "ai_combat_tick")
               game.create_object("/npcs/prize", "npc", "{e}", {{name = "prize", hp = 5, max_hp = 5, xp_value = 999}})
               local forged, why = game.damage("/npcs/prize", 100, "physical", "{o}")
               game.create_object("/npcs/ghost", "npc", "{e}", {{name = "ghost", hp = 5, max_hp = 5, respawn_seconds = 60}})
               game.damage("/npcs/ghost", 100)
               local again, why2 = game.damage("/npcs/ghost", 100)
               return tostring(via_use) .. "|" .. tostring(forged) .. "|" .. tostring(why) .. "|" .. tostring(again) .. "|" .. tostring(why2)"#,
            e = entrance,
            o = other
        ),
    )
    .await;
    let text = out["text"].as_str().unwrap_or_default().to_string();
    let parts: Vec<&str> = text.split('|').collect();
    assert_eq!(parts.len(), 5, "{}", out);
    assert_eq!(
        parts[0], "nil",
        "ai_combat_tick is not reachable via use_object: {}",
        text
    );
    assert_eq!(
        parts[1], "nil",
        "a source the caller does not control is refused: {}",
        text
    );
    assert_eq!(
        parts[3], "nil",
        "a dead NPC waiting to respawn takes no damage: {}",
        text
    );
    assert!(parts[4].contains("not in the world"), "{}", text);
    let xp: Option<i64> = sqlx::query_scalar(
        "SELECT json_extract(properties, '$.xp') FROM objects WHERE universe_id = ? AND id = ?",
    )
    .bind(server.universe_id())
    .bind(&other)
    .fetch_one(server.pool())
    .await
    .unwrap();
    assert_eq!(xp, None, "no XP forged for the bystander");

    // Aggro only for creatures: a non-npc class with aggro does not attack
    let out = eval_as(
        &mut adm,
        r#"game.define_class("statue_class", {parent = "item"})
           game.create_object("/rooms/gallery", "room", nil, {name = "Gallery", exits = {east = "/rooms/hallway"}})
           game.create_object("/rooms/hallway", "room", nil, {name = "Hallway", exits = {west = "/rooms/gallery"}})
           game.create_object("/rooms/hallway/statue", "statue_class", "/rooms/hallway", {name = "statue", hp = 50, aggro = true})
           game.create_object("/rooms/hallway/troll", "npc", "/rooms/hallway", {name = "troll", hp = 500, max_hp = 500, aggro = true})
           game.create_object("/rooms/gallery/orc", "npc", "/rooms/gallery", {name = "orc", hp = 500, max_hp = 500})
           return "ok""#,
    )
    .await;
    assert_eq!(out["text"], "ok", "{}", out);
    adm.drain().await;
    adm.command("goto /rooms/gallery").await.unwrap();
    adm.expect("room").await.unwrap();
    adm.drain().await;
    adm.command("attack orc").await.unwrap();
    tokio::time::sleep(Duration::from_millis(1200)).await;
    adm.drain().await;
    adm.command("flee").await.unwrap();
    adm.expect_output_containing("You flee east").await.unwrap();
    // Fleeing into the troll's room: the troll attacks, and the player
    // fights back at once (a row targeting the troll)
    tokio::time::sleep(Duration::from_millis(300)).await;
    let me = adm.player_id().unwrap().to_string();
    let target: Option<String> =
        sqlx::query_scalar("SELECT target_id FROM combat WHERE universe_id = ? AND entity_id = ?")
            .bind(server.universe_id())
            .bind(&me)
            .fetch_optional(server.pool())
            .await
            .unwrap();
    assert_eq!(
        target.as_deref(),
        Some("/rooms/hallway/troll"),
        "fights back at once"
    );
    let statue_fights: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM combat WHERE universe_id = ? AND entity_id = '/rooms/hallway/statue'",
    )
    .bind(server.universe_id())
    .fetch_one(server.pool())
    .await
    .unwrap();
    assert_eq!(statue_fights, 0, "an aggro non-creature does not attack");
}

/// Phase 2 review: fleeing from a dead end keeps the fight; an NPC kept out
/// of the world on purpose (not respawning) can be healed and damaged;
/// living-derived classes fight
#[tokio::test]
async fn test_engine_edges_after_review() {
    let server = TestServer::start().await.expect("server");
    let mut adm = server
        .connect_as(Role::Admin {
            username: "edgecase".into(),
        })
        .await
        .unwrap();
    let me = adm.player_id().unwrap().to_string();
    let out = eval_as(
        &mut adm,
        r#"game.create_object("/rooms/deadend", "room", nil, {name = "Dead End"})
           game.create_object("/rooms/deadend/orc", "npc", "/rooms/deadend", {name = "orc", hp = 500, max_hp = 500})
           game.create_object("/npcs/limbo-boss", "npc", nil, {name = "boss", hp = 50, max_hp = 100})
           local healed, why = game.heal("/npcs/limbo-boss", 10)
           game.define_class("golem", {parent = "living"})
           game.create_object("/rooms/deadend/golem", "golem", "/rooms/deadend", {name = "golem", hp = 50})
           local ok, why2 = game.can_attack("/rooms/deadend/golem", "/rooms/deadend/orc")
           return tostring(healed) .. "|" .. tostring(why) .. "|" .. tostring(ok) .. "|" .. tostring(why2)"#,
    )
    .await;
    let text = out["text"].as_str().unwrap_or_default().to_string();
    assert!(text.starts_with("60|nil|true|"), "{}", out);

    adm.drain().await;
    adm.command("goto /rooms/deadend").await.unwrap();
    adm.expect("room").await.unwrap();
    adm.drain().await;
    adm.command("attack orc").await.unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    adm.drain().await;
    adm.command("flee").await.unwrap();
    adm.expect_output_containing("nowhere to run")
        .await
        .unwrap();
    let target: Option<String> =
        sqlx::query_scalar("SELECT target_id FROM combat WHERE universe_id = ? AND entity_id = ?")
            .bind(server.universe_id())
            .bind(&me)
            .fetch_optional(server.pool())
            .await
            .unwrap();
    assert_eq!(
        target.as_deref(),
        Some("/rooms/deadend/orc"),
        "still fighting"
    );
}

/// An NPC deleted for good whose on_destroy puts things inside it is still
/// deleted (with them)
#[tokio::test]
async fn test_on_destroy_cannot_block_deletion() {
    let server = TestServer::start().await.expect("server");
    let entrance = server.world().entrance_id.clone();
    let mut adm = server
        .connect_as(Role::Admin {
            username: "stubborn".into(),
        })
        .await
        .unwrap();
    let out = eval_as(
        &mut adm,
        &format!(
            r#"local h = game.store_code([[return {{ on_destroy = function(a)
                 game.create_object(a.object_id .. "/stowaway", "item", a.object_id, {{name = "stowaway"}})
               end }}]])
               game.create_object("/npcs/limpet", "npc", "{}", {{name = "limpet", hp = 1, max_hp = 1, armor_class = 1, respawn_seconds = 0, code_hash = h}})
               return "ready""#,
            entrance
        ),
    )
    .await;
    assert_eq!(out["text"], "ready", "{}", out);
    attack_until_slain(&mut adm, "attack limpet", "limpet").await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    let left: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM objects WHERE universe_id = ? AND id LIKE '/npcs/limpet%'",
    )
    .bind(server.universe_id())
    .fetch_one(server.pool())
    .await
    .unwrap();
    assert_eq!(left, 0, "deleted with what its hook added");
}
