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
    let room = server
        .world()
        .store()
        .get(server.universe_id(), from)
        .await
        .unwrap()
        .unwrap();
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

/// Verbs registered with game.add_action dispatch to object handlers, for
/// objects in the room and in the player's inventory
#[tokio::test]
async fn test_custom_verbs_dispatch_to_object_handlers() {
    let server = TestServer::start().await.expect("server");
    let entrance = server.world().entrance_id.clone();
    let mut wiz = wizard(&server, "verb_wiz").await;
    eval(
        &mut wiz,
        &format!(
            r#"local lever = game.store_code([[return {{
                 pull = function(a) game.send(a.actor_id, "Clunk! (" .. a.args .. ")"); return "You pull the lever." end,
               }}]])
               local horn = game.store_code([[return {{
                 blow = function(a) game.broadcast(a.room_id, "A horn blares!", a.actor_id); return "You blow the horn." end,
               }}]])
               game.create_object("/items/lever", "item", "{room}", {{name = "lever", fixed = true, code_hash = lever}})
               game.create_object("/items/horn", "item", "{room}", {{name = "horn", code_hash = horn}})
               game.add_action("pull", "/items/lever", "pull")
               game.add_action("blow", "/items/horn", "blow")
               return true"#,
            room = entrance
        ),
    )
    .await;

    let mut alice = player(&server, "verb_alice").await;
    let mut bob = player(&server, "verb_bob").await;

    // Object in the room
    alice.command("pull lever").await.unwrap();
    alice
        .expect_output_containing("Clunk! (lever)")
        .await
        .expect("handler message");
    alice
        .expect_output_containing("You pull the lever.")
        .await
        .expect("handler result is the reply");

    // Object in inventory
    alice.command("take horn").await.unwrap();
    alice.expect_output_containing("You take").await.unwrap();
    alice.command("blow horn").await.unwrap();
    alice
        .expect_output_containing("You blow the horn.")
        .await
        .expect("inventory verb works");
    bob.expect_output_containing("A horn blares!")
        .await
        .expect("onlooker hears the horn");

    // Unregistered verbs are still unknown
    alice.command("frobnicate").await.unwrap();
    alice
        .expect_output_containing("Unknown command: frobnicate")
        .await
        .unwrap();
}

/// call_out and heart_beat callbacks fire on the object's code
#[tokio::test]
async fn test_call_out_and_heart_beat_fire() {
    let server = TestServer::start().await.expect("server");
    let entrance = server.world().entrance_id.clone();
    let mut wiz = wizard(&server, "timer_wiz").await;
    eval(
        &mut wiz,
        &format!(
            r#"local bell = game.store_code([[return {{
                 ring = function(a)
                   game.call_out(0.3, "chime", a.actor_id)
                   return "You pull the bell rope."
                 end,
                 chime = function(a) game.send(a.args, "The bell chimes.") end,
                 wind = function(a) game.set_heart_beat(200); return "You wind the clock." end,
                 halt = function(a) game.remove_heart_beat(); return "The clock stops." end,
                 heart_beat = function(a) game.broadcast(a.room_id, "Tick.") end,
               }}]])
               game.create_object("/items/bell", "item", "{room}", {{name = "bell", fixed = true, code_hash = bell}})
               game.add_action("ring", "/items/bell", "ring")
               game.add_action("wind", "/items/bell", "wind")
               game.add_action("halt", "/items/bell", "halt")
               return true"#,
            room = entrance
        ),
    )
    .await;

    let mut alice = player(&server, "timer_alice").await;

    alice.command("ring bell").await.unwrap();
    alice
        .expect_output_containing("You pull the bell rope.")
        .await
        .unwrap();
    alice
        .expect_output_containing("The bell chimes.")
        .await
        .expect("call_out fired");

    alice.command("wind bell").await.unwrap();
    alice
        .expect_output_containing("You wind the clock.")
        .await
        .unwrap();
    alice
        .expect_output_containing("Tick.")
        .await
        .expect("first beat");
    alice
        .expect_output_containing("Tick.")
        .await
        .expect("second beat");

    alice.command("halt bell").await.unwrap();
    alice
        .expect_output_containing("The clock stops.")
        .await
        .unwrap();
    // Allow an in-flight beat to land, then expect silence
    tokio::time::sleep(Duration::from_millis(300)).await;
    alice.drain().await;
    let after = alice.outputs_within(Duration::from_millis(600)).await;
    assert!(
        !after.iter().any(|t| t.contains("Tick.")),
        "heart_beat stopped: {:?}",
        after
    );
}

/// Object code runs as the object's owner, never as the player who
/// triggers it: a wizard's trap cannot borrow an admin's privileges.
#[tokio::test]
async fn test_object_code_runs_as_owner_not_triggerer() {
    let server = TestServer::start().await.expect("server");
    let entrance = server.world().entrance_id.clone();
    let pl = player(&server, "trap_target").await;
    let pl_id = pl.account_id().to_string();
    let mut wiz = wizard(&server, "trap_wiz").await;
    eval(
        &mut wiz,
        &format!(
            r#"local h = game.store_code([[return {{
                 on_enter = function(a)
                   local r = game.set_access_level("{pl}", "wizard")
                   game.send(a.actor_id, "trap sprung: " .. tostring(r))
                 end,
               }}]])
               game.create_object("/rooms/trap", "room", nil, {{name = "Trap"}})
               return game.update_object("/rooms/trap", {{code_hash = h}})"#,
            pl = pl_id
        ),
    )
    .await;
    let _ = entrance;

    // An admin walks in
    let mut adm = server
        .connect_as(Role::Admin {
            username: "trap_admin".to_string(),
        })
        .await
        .unwrap();
    adm.drain().await;
    adm.command("goto /rooms/trap").await.unwrap_or(());
    // goto is wizard+, admins qualify
    let text = adm
        .expect_output_containing("trap sprung:")
        .await
        .expect("hook ran");
    assert_eq!(text, "trap sprung: false", "hook must not act as the admin");

    let level: String = sqlx::query_scalar("SELECT access_level FROM accounts WHERE id = ?")
        .bind(&pl_id)
        .fetch_one(server.pool())
        .await
        .unwrap();
    assert_eq!(level, "player");
}

/// add_action needs write access to the target object, and room actions
/// only dispatch for objects that are actually nearby
#[tokio::test]
async fn test_add_action_requires_access_and_nearby_object() {
    let server = TestServer::start().await.expect("server");
    let entrance = server.world().entrance_id.clone();
    let pool_room = server.world().pool_id.clone();
    let pl = player(&server, "action_player").await;
    let pl_id = pl.account_id().to_string();
    drop(pl);
    let mut wiz = wizard(&server, "action_wiz").await;

    // A player identity may not register actions on objects it can't modify
    let out = eval(
        &mut wiz,
        &format!(
            r#"game.create_object("/items/statue", "item", "{room}", {{name = "statue"}})
               game.set_actor("{pl}")
               return game.add_action("smash", "/items/statue", "on_death")"#,
            room = entrance,
            pl = pl_id
        ),
    )
    .await;
    assert_eq!(out, "false");

    // A room hook registers a room action pointing at a far-away object
    eval(
        &mut wiz,
        &format!(
            r#"local far = game.store_code([[return {{ zap = function(a) return "ZAP" end }}]])
               game.create_object("/items/faraway", "item", "{far_room}", {{name = "orb", code_hash = far}})
               local room = game.store_code([[return {{
                 on_enter = function(a) game.add_action("zap", "/items/faraway", "zap") end,
               }}]])
               return game.update_object("{room}", {{code_hash = room}})"#,
            far_room = pool_room,
            room = entrance
        ),
    )
    .await;

    let mut alice = player(&server, "action_alice").await; // spawn runs on_enter
    alice.command("zap").await.unwrap();
    alice
        .expect_output_containing("Unknown command: zap")
        .await
        .expect("far-away object's action is not dispatched");
}

/// Players get a generic message when a verb handler fails
#[tokio::test]
async fn test_verb_errors_are_generic_for_players() {
    let server = TestServer::start().await.expect("server");
    let entrance = server.world().entrance_id.clone();
    let mut wiz = wizard(&server, "err_wiz").await;
    eval(
        &mut wiz,
        &format!(
            r#"local h = game.store_code([[return {{ kick = function(a) error("secret internals") end }}]])
               game.create_object("/items/crate", "item", "{room}", {{name = "crate", code_hash = h}})
               return game.add_action("kick", "/items/crate", "kick")"#,
            room = entrance
        ),
    )
    .await;

    let mut alice = player(&server, "err_alice").await;
    alice.command("kick crate").await.unwrap();
    let _ = alice.expect("echo").await;
    let reply = alice.expect("error").await.unwrap();
    assert_eq!(reply["message"], "Something went wrong.");

    wiz.drain().await;
    wiz.command("kick crate").await.unwrap();
    let _ = wiz.expect("echo").await;
    let reply = wiz.expect("error").await.unwrap();
    assert!(
        reply["message"]
            .as_str()
            .unwrap()
            .contains("secret internals"),
        "wizards see details: {}",
        reply
    );
}

/// Timers are bounded per object, and deleting an object cancels its
/// timers, heartbeat and actions
#[tokio::test]
async fn test_timer_limits_and_cleanup_on_delete() {
    let server = TestServer::start().await.expect("server");
    let entrance = server.world().entrance_id.clone();
    let mut wiz = wizard(&server, "limit_wiz").await;
    eval(
        &mut wiz,
        &format!(
            r#"local h = game.store_code([[return {{
                 spam = function(a)
                   local n = 0
                   for i = 1, 40 do
                     if game.call_out(60, "noop") ~= "" then n = n + 1 end
                   end
                   return "scheduled " .. n
                 end,
                 noop = function(a) end,
                 wind = function(a) game.set_heart_beat(150); return "wound" end,
                 heart_beat = function(a) game.broadcast(a.room_id, "Tock.") end,
               }}]])
               game.create_object("/items/clock", "item", "{room}", {{name = "clock", fixed = true, code_hash = h}})
               game.add_action("spam", "/items/clock", "spam")
               game.add_action("wind", "/items/clock", "wind")
               return true"#,
            room = entrance
        ),
    )
    .await;

    let mut alice = player(&server, "limit_alice").await;
    alice.command("spam clock").await.unwrap();
    alice
        .expect_output_containing("scheduled 32")
        .await
        .expect("call_outs capped at 32 per object");

    alice.command("wind clock").await.unwrap();
    alice
        .expect_output_containing("Tock.")
        .await
        .expect("beating");

    eval(&mut wiz, r#"return game.delete_object("/items/clock")"#).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    alice.drain().await;
    let after = alice.outputs_within(Duration::from_millis(500)).await;
    assert!(
        !after.iter().any(|t| t.contains("Tock.")),
        "heartbeat cancelled on delete: {:?}",
        after
    );
    alice.command("wind clock").await.unwrap();
    alice
        .expect_output_containing("Unknown command: wind")
        .await
        .expect("actions cleared on delete");
}

/// Object code is limited to 100 database-backed calls per execution;
/// wizard eval gets a much larger budget
#[tokio::test]
async fn test_db_query_limit_for_object_code() {
    let server = TestServer::start().await.expect("server");
    let entrance = server.world().entrance_id.clone();
    let mut wiz = wizard(&server, "budget_wiz").await;

    // Wizard eval: 150 lookups are fine
    let out = eval(
        &mut wiz,
        &format!(
            r#"for i = 1, 150 do game.get_object("{room}") end; return "ok""#,
            room = entrance
        ),
    )
    .await;
    assert_eq!(out, "ok");

    eval(
        &mut wiz,
        &format!(
            r#"local h = game.store_code([[return {{
                 search = function(a)
                   for i = 1, 150 do game.get_object(a.room_id) end
                   return "searched"
                 end,
               }}]])
               game.create_object("/items/haystack", "item", "{room}", {{name = "haystack", code_hash = h}})
               return game.add_action("search", "/items/haystack", "search")"#,
            room = entrance
        ),
    )
    .await;

    wiz.drain().await;
    wiz.command("search haystack").await.unwrap();
    let _ = wiz.expect("echo").await;
    let reply = wiz.expect("error").await.expect("limit error");
    assert!(
        reply["message"]
            .as_str()
            .unwrap()
            .contains("database query limit exceeded"),
        "got: {}",
        reply
    );
}

/// Timer functions need an object context: outside object code (eval) they
/// do nothing and say so
#[tokio::test]
async fn test_timer_functions_need_an_object() {
    let server = TestServer::start().await.expect("server");
    let mut wiz = server
        .connect_as(Role::Wizard {
            username: "timer_wiz".into(),
        })
        .await
        .unwrap();
    wiz.drain().await;
    wiz.command("eval return tostring(game.set_heart_beat(1000)) .. ':' .. tostring(game.remove_call_out('no-such-timer'))")
        .await
        .unwrap();
    let _ = wiz.expect("echo").await;
    let out = wiz.expect("output").await.expect("eval output");
    assert_eq!(out["text"], "false:false");
}

/// A heartbeat is stored, so it keeps beating after a restart
#[tokio::test]
async fn test_heartbeat_survives_restart() {
    let mut server = TestServer::start().await.expect("server");
    let entrance = server.world().entrance_id.clone();
    let mut wiz = wizard(&server, "beat_wiz").await;
    eval(
        &mut wiz,
        &format!(
            r#"local h = game.store_code([[return {{
                 wind = function(a) return tostring(game.set_heart_beat(300)) end,
                 heart_beat = function(a) game.broadcast(a.room_id, "Tick-tock.") end,
               }}]])
               game.create_object("/items/metronome", "item", "{room}", {{name = "metronome", code_hash = h}})
               return game.use_object("/items/metronome", "x", "wind")"#,
            room = entrance
        ),
    )
    .await;
    server.restart().await.expect("restart");

    let mut alice = player(&server, "beat_listener").await;
    alice
        .expect_output_containing("Tick-tock.")
        .await
        .expect("the heartbeat came back after the restart");
}

/// Custom verbs are stored in the database: they survive a restart (and a
/// failover), and go with their object
#[tokio::test]
async fn test_custom_verbs_survive_restart() {
    let mut server = TestServer::start().await.expect("server");
    let entrance = server.world().entrance_id.clone();
    let mut wiz = server
        .connect_as(Role::Wizard {
            username: "verb_keeper".into(),
        })
        .await
        .unwrap();
    wiz.drain().await;
    wiz.command(&format!(
        r#"eval local h = game.store_code([[return {{ ring = function(a) return "Ding!" end }}]])
           game.create_object("/items/gong", "item", "{}", {{name = "gong", fixed = true, code_hash = h}})
           return tostring(game.add_action("ring", "/items/gong", "ring"))"#,
        entrance
    ))
    .await
    .unwrap();
    wiz.expect_output_containing("true")
        .await
        .expect("verb added");
    drop(wiz);

    server.restart().await.expect("restart");
    let mut pl = server
        .connect_as(Role::Player {
            username: "gong_ringer".into(),
        })
        .await
        .unwrap();
    pl.drain().await;
    pl.command("ring gong").await.unwrap();
    pl.expect_output_containing("Ding!")
        .await
        .expect("the verb survived the restart");

    // Deleting the object takes its verb with it
    let pool = server.pool().clone();
    let rows = || async {
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM actions WHERE object_id = '/items/gong'")
            .fetch_one(&pool)
            .await
            .unwrap()
    };
    assert_eq!(rows().await, 1);
    let mut wiz = server
        .connect_as(Role::Wizard {
            username: "verb_keeper".into(),
        })
        .await
        .unwrap();
    wiz.drain().await;
    wiz.command(r#"eval return tostring(game.delete_object("/items/gong"))"#)
        .await
        .unwrap();
    wiz.expect_output_containing("true").await.expect("deleted");
    assert_eq!(rows().await, 0);
    pl.drain().await;
    pl.command("ring gong").await.unwrap();
    pl.expect_output_containing("Unknown command")
        .await
        .expect("the verb went with its object");
}
