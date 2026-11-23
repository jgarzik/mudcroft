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

/// A call_out is consumed in the same change as its handler's writes: a
/// handler that succeeds runs once and its writes stand; one that fails
/// keeps none of its writes and is dropped, not retried (as in LPMud)
#[tokio::test]
async fn test_call_out_consumed_with_its_handler() {
    let mut server = TestServer::start().await.expect("server");
    let entrance = server.world().entrance_id.clone();
    let mut wiz = wizard(&server, "consume_wiz").await;
    eval(
        &mut wiz,
        &format!(
            r#"local code = game.store_code([[return {{
                 arm = function(a)
                   game.call_out(0.2, "count")
                   game.call_out(0.2, "fail")
                 end,
                 count = function(a)
                   local o = game.get_object(a.object_id)
                   game.update_object(a.object_id, {{count = (o.metadata.count or 0) + 1}})
                 end,
                 fail = function(a)
                   game.update_object(a.object_id, {{failed_write = true}})
                   error("handler bug")
                 end,
               }}]])
               game.create_object("/items/counter", "item", "{room}", {{name = "counter", code_hash = code}})
               game.use_object("/items/counter", "consume_wiz", "arm")
               return true"#,
            room = entrance
        ),
    )
    .await;
    // Wait until both have fired, then a little longer: a retried or
    // duplicated one would run again
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while tokio::time::Instant::now() < deadline
        && eval(
            &mut wiz,
            r#"return tostring(game.get_object("/items/counter").metadata.count)"#,
        )
        .await
            == "nil"
    {
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    tokio::time::sleep(Duration::from_millis(800)).await;
    let count = eval(
        &mut wiz,
        r#"return game.get_object("/items/counter").metadata.count"#,
    )
    .await;
    assert_eq!(count, "1", "the succeeding call_out ran once");
    let failed = eval(
        &mut wiz,
        r#"return tostring(game.get_object("/items/counter").metadata.failed_write)"#,
    )
    .await;
    assert_eq!(failed, "nil", "a failing handler keeps none of its writes");

    drop(wiz);
    let pool = sqlx::SqlitePool::connect(&format!("sqlite:{}", server.db_path.display()))
        .await
        .unwrap();
    let pending = || async {
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM timers")
            .fetch_one(&pool)
            .await
            .unwrap()
    };
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while pending().await != 0 && tokio::time::Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert_eq!(pending().await, 0, "both call_outs consumed");
    server.stop();
}

/// A server killed partway through a batch of due call_outs loses none of
/// them and runs none twice: each is consumed only with its handler's
/// writes, so after the restart the rest fire, exactly once each
#[tokio::test]
async fn test_call_outs_survive_a_crash_mid_batch() {
    const N: usize = 24;
    let mut server = TestServer::start().await.expect("server");
    let entrance = server.world().entrance_id.clone();
    let mut wiz = wizard(&server, "batch_wiz").await;
    let notify = wiz.player_id().expect("player id").to_string();
    eval(
        &mut wiz,
        &format!(
            r#"local code = game.store_code([[return {{
                 arm = function(a)
                   for i = 1, {n} do game.call_out(0.5, "hit", tostring(i)) end
                 end,
                 hit = function(a)
                   local o = game.get_object(a.object_id)
                   local k = "k" .. a.args
                   game.update_object(a.object_id, {{[k] = (o.metadata[k] or 0) + 1}})
                   for _ = 1, 200000 do end
                   game.send(o.metadata.notify, "hit " .. a.args)
                 end,
               }}]])
               game.create_object("/items/batch", "item", "{room}",
                 {{name = "batch", notify = "{notify}", code_hash = code}})
               game.use_object("/items/batch", "batch_wiz", "arm")
               return true"#,
            n = N,
            room = entrance,
            notify = notify
        ),
    )
    .await;
    // A callback is running (its message goes out just before its change
    // commits) and the rest of the batch has not run yet: kill the server
    // now. Whether or not that callback committed, each call_out must end
    // up applied exactly once.
    wiz.expect_output_containing("hit ")
        .await
        .expect("first call_out fired");
    server.stop();
    drop(wiz);

    server.start_again().await.expect("restart");
    let pool = sqlx::SqlitePool::connect(&format!("sqlite:{}", server.db_path.display()))
        .await
        .unwrap();
    let props = || async {
        let json: String =
            sqlx::query_scalar("SELECT properties FROM objects WHERE id = '/items/batch'")
                .fetch_one(&pool)
                .await
                .unwrap();
        serde_json::from_str::<serde_json::Value>(&json).unwrap()
    };
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    loop {
        let p = props().await;
        let done = (1..=N).filter(|i| p.get(format!("k{i}")).is_some()).count();
        if done == N || tokio::time::Instant::now() > deadline {
            break;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    let p = props().await;
    for i in 1..=N {
        assert_eq!(
            p.get(format!("k{i}")).and_then(|v| v.as_i64()),
            Some(1),
            "call_out {i} ran exactly once: {p}"
        );
    }
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

/// A Lua run is one change: if it errors part way, nothing it did is kept:
/// no object, no verb, no timer (tables and caches alike)
#[tokio::test]
async fn test_failed_run_keeps_nothing() {
    let server = TestServer::start().await.expect("server");
    let entrance = server.world().entrance_id.clone();
    let mut wiz = server
        .connect_as(Role::Wizard {
            username: "atomic_wiz".into(),
        })
        .await
        .unwrap();
    // A bell exists (with code); a failing run adds a verb to it and a new
    // object, then errors
    wiz.drain().await;
    wiz.command(&format!(
        r#"eval local h = game.store_code([[return {{ ring = function(a) return "Ding!" end }}]])
           return game.create_object("/items/bell", "item", "{}", {{name = "bell", fixed = true, code_hash = h}}) and "made" or "not made""#,
        entrance
    ))
    .await
    .unwrap();
    wiz.expect_output_containing("made")
        .await
        .expect("bell made");
    wiz.drain().await;
    wiz.command(&format!(
        r#"eval assert(game.add_action("ring", "/items/bell", "ring"), "add_action failed")
           assert(game.create_object("/items/clapper", "item", "{}", {{name = "clapper"}}), "create failed")
           error("changed my mind")"#,
        entrance
    ))
    .await
    .unwrap();
    let err = wiz.expect("error").await.expect("the run failed");
    assert!(
        err["message"]
            .as_str()
            .unwrap_or_default()
            .contains("changed my mind"),
        "{err}"
    );
    let objects: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM objects WHERE id = '/items/clapper'")
            .fetch_one(server.pool())
            .await
            .unwrap();
    assert_eq!(objects, 0, "the object was not kept");
    let actions: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM actions WHERE object_id = '/items/bell'")
            .fetch_one(server.pool())
            .await
            .unwrap();
    assert_eq!(actions, 0, "nor its verb");
    wiz.drain().await;
    wiz.command("ring bell").await.unwrap();
    wiz.expect_output_containing("Unknown command")
        .await
        .expect("the verb is not in the cache either");

    // A run that succeeds keeps everything, as one change, and can read its
    // own writes along the way
    wiz.drain().await;
    wiz.command(&format!(
        r#"eval game.create_object("/items/gong", "item", "{}", {{name = "gong"}})
           local g = game.get_object("/items/gong")
           return g and ("read back " .. g.name) or "not visible""#,
        entrance
    ))
    .await
    .unwrap();
    wiz.expect_output_containing("read back gong")
        .await
        .expect("a run sees its own writes");
}

/// `clients` wizards each send writing commands (read a counter, write it
/// back) as fast as replies come, for `secs`: (commands done, errors seen)
async fn concurrent_writes(server: &TestServer, clients: usize, secs: u64) -> (u64, Vec<String>) {
    static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let entrance = server.world().entrance_id.clone();
    let mut handles = Vec::new();
    for _ in 0..clients {
        let c = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let mut wiz = server
            .connect_as(Role::Wizard {
                username: format!("writer{}", c),
            })
            .await
            .unwrap();
        wiz.drain().await;
        wiz.command(&format!(
            r#"eval game.create_object("/items/ctr{c}", "item", "{e}", {{name = "ctr", n = 0}}) return "made""#,
            c = c,
            e = entrance
        ))
        .await
        .unwrap();
        wiz.expect_output_containing("made").await.unwrap();
        handles.push(tokio::spawn(async move {
            let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(secs);
            let mut done = 0u64;
            let mut errors = Vec::new();
            while tokio::time::Instant::now() < deadline {
                wiz.command(&format!(
                    r#"eval local o = game.get_object("/items/ctr{c}") game.update_object("/items/ctr{c}", {{n = (o.n or (o.properties or {{}}).n or 0) + 1}}) return "k""#,
                    c = c
                ))
                .await
                .unwrap();
                loop {
                    let m = wiz.expect_any().await.unwrap();
                    if m["type"] == "output" && m["text"] == "k" {
                        done += 1;
                        break;
                    }
                    if m["type"] == "error" {
                        errors.push(m.to_string());
                        break;
                    }
                }
            }
            (done, errors)
        }));
    }
    let mut total = 0;
    let mut errors = Vec::new();
    for h in handles {
        let (done, errs) = h.await.unwrap();
        total += done;
        errors.extend(errs);
    }
    (total, errors)
}

/// Many players writing at once: every command succeeds (no "database is
/// locked" from SQLite's shared cache, no pool exhaustion while runs wait
/// for the write gate)
#[tokio::test]
async fn test_concurrent_writing_commands() {
    let server = TestServer::start().await.expect("server");
    let (done, errors) = concurrent_writes(&server, 32, 2).await;
    assert!(
        errors.is_empty(),
        "{} errors, e.g. {:?}",
        errors.len(),
        errors.first()
    );
    assert!(done > 32, "progress: {}", done);
}

/// Throughput of writing commands from concurrent clients (manual:
/// `cargo test --release --test integration_test bench_writes -- --ignored --nocapture`)
#[tokio::test]
#[ignore]
async fn bench_writes() {
    let server = TestServer::start().await.expect("server");
    for clients in [1, 8, 32] {
        let (done, errors) = concurrent_writes(&server, clients, 3).await;
        println!(
            "BENCH {} clients: {}/s writing commands, {} errors",
            clients,
            done / 3,
            errors.len()
        );
    }
}

/// A command is one change: walking into a room moves the player and runs
/// the room's on_enter hook (which writes), all as one Raft entry
#[tokio::test]
async fn test_command_is_one_raft_entry() {
    let server = TestServer::start().await.expect("server");
    let passage = "/rooms/hall".to_string();
    let mut wiz = server
        .connect_as(Role::Wizard {
            username: "batch_wiz".into(),
        })
        .await
        .unwrap();
    wiz.drain().await;
    wiz.command(&format!(
        r#"eval local h = game.store_code([[return {{ on_enter = function(a)
               local r = game.get_object("{p}")
               game.update_object("{p}", {{visits = (r.metadata.visits or 0) + 1}})
             end }}]])
           assert(game.create_object("/rooms/porch", "room", nil, {{name = "Porch", exits = {{east = "{p}"}}}}))
           assert(game.create_object("{p}", "room", nil, {{name = "Hall", code_hash = h}}))
           return "rooms built""#,
        p = passage
    ))
    .await
    .unwrap();
    wiz.expect_output_containing("rooms built")
        .await
        .expect("built");
    wiz.drain().await;
    wiz.command("goto /rooms/porch").await.unwrap();
    wiz.expect("room").await.expect("at the porch");
    let entries = || async {
        sqlx::query_scalar::<_, i64>("SELECT MAX(log_index) FROM raft_log")
            .fetch_one(server.pool())
            .await
            .unwrap()
    };
    let before = entries().await;
    wiz.drain().await;
    wiz.command("east").await.unwrap();
    wiz.expect("room").await.expect("moved");
    let mut visits = None;
    for _ in 0..50 {
        visits = sqlx::query_scalar::<_, Option<i64>>(
            "SELECT json_extract(properties, '$.visits') FROM objects WHERE id = ?",
        )
        .bind(&passage)
        .fetch_one(server.pool())
        .await
        .unwrap();
        if visits.is_some() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    assert_eq!(visits, Some(1), "the hook ran");
    assert_eq!(entries().await - before, 1, "move and hook in one entry");
}

/// A command sent with an id is answered with `done` once saved, and the
/// same id sent again (a gateway retrying after a failover) does not run it
/// a second time
#[tokio::test]
async fn test_command_id_runs_once() {
    let server = TestServer::start().await.expect("server");
    let entrance = server.world().entrance_id.clone();
    let mut wiz = server
        .connect_as(Role::Wizard {
            username: "once_wiz".into(),
        })
        .await
        .unwrap();
    wiz.drain().await;
    let cmd = serde_json::json!({
        "type": "command",
        "id": "cmd-1",
        "text": format!(
            r#"eval local o = game.get_object("/items/tally") if o then game.update_object("/items/tally", {{n = o.metadata.n + 1}}) else game.create_object("/items/tally", "item", "{}", {{name = "tally", n = 1}}) end return "counted""#,
            entrance
        ),
    });
    wiz.send_json(&cmd).await.unwrap();
    wiz.expect_output_containing("counted").await.expect("ran");
    let done = wiz.expect("done").await.expect("done");
    assert_eq!(done["id"], "cmd-1");

    // The retry: not run again
    wiz.send_json(&cmd).await.unwrap();
    let done = wiz.expect("done").await.expect("done again");
    assert_eq!(done["id"], "cmd-1");
    let n: i64 = sqlx::query_scalar(
        "SELECT json_extract(properties, '$.n') FROM objects WHERE id = '/items/tally'",
    )
    .fetch_one(server.pool())
    .await
    .unwrap();
    assert_eq!(n, 1, "the command ran once");

    // A new id runs
    let mut next = cmd.clone();
    next["id"] = "cmd-2".into();
    wiz.send_json(&next).await.unwrap();
    wiz.expect("done").await.expect("done");
    let n: i64 = sqlx::query_scalar(
        "SELECT json_extract(properties, '$.n') FROM objects WHERE id = '/items/tally'",
    )
    .fetch_one(server.pool())
    .await
    .unwrap();
    assert_eq!(n, 2);
}

/// Command ids are per player: another player's command with the same id
/// still runs (nobody can suppress someone else's commands)
#[tokio::test]
async fn test_command_ids_are_per_player() {
    let server = TestServer::start().await.expect("server");
    let mut a = server
        .connect_as(Role::Wizard {
            username: "ida".into(),
        })
        .await
        .unwrap();
    let mut b = server
        .connect_as(Role::Wizard {
            username: "idb".into(),
        })
        .await
        .unwrap();
    let cmd = |who: &str| {
        serde_json::json!({
            "type": "command",
            "id": "same-id",
            "text": format!(r#"eval game.create_object("/items/mark-{}", "item", nil, {{name = "mark"}}) return "marked""#, who),
        })
    };
    a.drain().await;
    a.send_json(&cmd("a")).await.unwrap();
    a.expect("done").await.expect("a done");
    b.drain().await;
    b.send_json(&cmd("b")).await.unwrap();
    b.expect_output_containing("marked")
        .await
        .expect("b's command ran despite a's id");
}

/// An object can never end up inside itself (directly or through what it
/// contains): such a move is refused
#[tokio::test]
async fn test_no_containment_cycles() {
    let server = TestServer::start().await.expect("server");
    let entrance = server.world().entrance_id.clone();
    let mut wiz = server
        .connect_as(Role::Wizard {
            username: "cycle_wiz".into(),
        })
        .await
        .unwrap();
    wiz.drain().await;
    wiz.command(&format!(
        r#"eval game.create_object("/items/outer", "container", "{e}", {{name = "outer"}})
           game.create_object("/items/inner", "container", "/items/outer", {{name = "inner"}})
           local ok1 = pcall(game.move_object, "/items/outer", "/items/inner")
           local ok2 = pcall(game.move_object, "/items/outer", "/items/outer")
           local ok3 = pcall(game.update_object, "/items/outer", {{parent_id = "/items/inner"}})
           return tostring(ok1) .. tostring(ok2) .. tostring(ok3)"#,
        e = entrance
    ))
    .await
    .unwrap();
    wiz.expect_output_containing("falsefalse")
        .await
        .expect("moves into itself refused");
    let parent: String = sqlx::query_scalar(
        "SELECT parent_id FROM objects WHERE universe_id = ? AND id = '/items/outer'",
    )
    .bind(server.universe_id())
    .fetch_one(server.pool())
    .await
    .unwrap();
    assert_eq!(parent, entrance);
}

/// Lifecycle hooks: an object's on_create runs when it is created (or
/// cloned) with code, its on_destroy before it is deleted; a creating chain
/// (on_create creating more of itself) is bounded
#[tokio::test]
async fn test_lifecycle_create_destroy() {
    let server = TestServer::start().await.expect("server");
    let entrance = server.world().entrance_id.clone();
    let mut wiz = server
        .connect_as(Role::Wizard {
            username: "lifecycle_wiz".into(),
        })
        .await
        .unwrap();
    wiz.drain().await;
    wiz.command(&format!(
        r#"eval game.create_object("/items/ledger", "item", "{e}", {{name = "ledger", log = ""}})
           local h = game.store_code([[
             local function note(w) local l = game.get_object("/items/ledger") game.update_object("/items/ledger", {{log = l.metadata.log .. w}}) end
             return {{
               on_create = function(a) note("C") end,
               on_destroy = function(a) note("D") end,
             }}]])
           game.create_object("/items/egg", "item", "{e}", {{name = "egg", code_hash = h}})
           game.clone_object("/items/egg", "/items/egg-two", "{e}")
           game.delete_object("/items/egg")
           local breeder = game.store_code([[return {{ on_create = function(a)
             local n = (tonumber(string.match(a.object_id, "(%d+)$")) or 0) + 1
             game.create_object("/items/rabbit-" .. n, "item", nil, {{name = "rabbit", code_hash = game.get_object(a.object_id).code_hash}})
           end }}]])
           game.create_object("/items/rabbit-0", "item", nil, {{name = "rabbit", code_hash = breeder}})
           return game.get_object("/items/ledger").metadata.log"#,
        e = entrance
    ))
    .await
    .unwrap();
    wiz.expect_output_containing("CCD")
        .await
        .expect("created, cloned, destroyed");
    let rabbits: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM objects WHERE universe_id = ? AND id LIKE '/items/rabbit-%'",
    )
    .bind(server.universe_id())
    .fetch_one(server.pool())
    .await
    .unwrap();
    assert!((2..=40).contains(&rabbits), "a bounded chain: {}", rabbits);
}

/// A clone keeps code only for a cloner who may act as the original's
/// owner (else the copy's code would run as the cloner); on_destroy cannot
/// block a deletion by adding contents
#[tokio::test]
async fn test_lifecycle_hooks_cannot_be_abused() {
    let server = TestServer::start().await.expect("server");
    let entrance = server.world().entrance_id.clone();
    let mut adm = server
        .connect_as(Role::Admin {
            username: "lifecycle_adm".into(),
        })
        .await
        .unwrap();
    let mut wiz = server
        .connect_as(Role::Wizard {
            username: "lifecycle_cloner".into(),
        })
        .await
        .unwrap();
    adm.drain().await;
    adm.command(&format!(
        r#"eval local h = game.store_code([[return {{
             on_destroy = function(a) game.create_object(a.object_id .. "/stowaway", "item", a.object_id, {{name = "stowaway"}}) end,
           }}]])
           game.create_object("/items/idol", "item", "{e}", {{name = "idol", code_hash = h}})
           game.create_object("/items/crate", "item", "{e}", {{name = "crate", code_hash = h}})
           return "made""#,
        e = entrance
    ))
    .await
    .unwrap();
    adm.expect_output_containing("made").await.unwrap();

    // The wizard's copy of the admin's idol carries no code
    wiz.drain().await;
    wiz.command(&format!(
        r#"eval local c = game.clone_object("/items/idol", "/items/idol-copy", "{e}") return "code:" .. tostring(c.code_hash)"#,
        e = entrance
    ))
    .await
    .unwrap();
    wiz.expect_output_containing("code:nil")
        .await
        .expect("code dropped");

    // on_destroy putting a stowaway inside does not stop the deletion
    adm.drain().await;
    adm.command(r#"eval return tostring(game.delete_object("/items/crate"))"#)
        .await
        .unwrap();
    adm.expect_output_containing("true").await.expect("deleted");
    let crate_left: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM objects WHERE universe_id = ? AND id = '/items/crate'",
    )
    .bind(server.universe_id())
    .fetch_one(server.pool())
    .await
    .unwrap();
    assert_eq!(crate_left, 0);
}
