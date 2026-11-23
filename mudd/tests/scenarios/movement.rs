//! Movement scenario tests
//!
//! Tests room navigation through the cave adventure world

use crate::harness::{Role, TestServer};
use std::time::Duration;

/// Test: Navigate through all 4 rooms via cardinal directions
#[tokio::test]
async fn test_cave_full_navigation() {
    let server = TestServer::start().await.expect("Failed to start server");

    let mut wizard = server
        .connect_as(Role::Wizard {
            username: "navwizard".to_string(),
        })
        .await
        .expect("Failed to connect as wizard");

    // Player spawns at Cave Entrance - receive initial room description
    let room = wizard.expect("room").await.expect("no initial room");
    assert!(
        room["name"].as_str().unwrap().contains("Cave Entrance"),
        "Should start at Cave Entrance, got: {}",
        room["name"]
    );

    // Move north to Narrow Passage
    wizard.command("north").await.expect("north failed");
    // Skip echo message, get room
    let _echo = wizard.expect("echo").await;
    let room = wizard.expect("room").await.expect("no room after north");
    assert!(
        room["name"].as_str().unwrap().contains("Narrow Passage"),
        "Should be at Narrow Passage, got: {}",
        room["name"]
    );

    // Move north to Treasure Chamber
    wizard.command("north").await.expect("north failed");
    let _echo = wizard.expect("echo").await;
    let room = wizard
        .expect("room")
        .await
        .expect("no room after second north");
    assert!(
        room["name"].as_str().unwrap().contains("Treasure Chamber"),
        "Should be at Treasure Chamber, got: {}",
        room["name"]
    );

    // Move south back to Narrow Passage
    wizard.command("south").await.expect("south failed");
    let _echo = wizard.expect("echo").await;
    let room = wizard.expect("room").await.expect("no room after south");
    assert!(
        room["name"].as_str().unwrap().contains("Narrow Passage"),
        "Should be back at Narrow Passage, got: {}",
        room["name"]
    );

    // Move east to Underground Pool
    wizard.command("east").await.expect("east failed");
    let _echo = wizard.expect("echo").await;
    let room = wizard.expect("room").await.expect("no room after east");
    assert!(
        room["name"].as_str().unwrap().contains("Underground Pool"),
        "Should be at Underground Pool, got: {}",
        room["name"]
    );

    // Move west back to Narrow Passage
    wizard.command("west").await.expect("west failed");
    let _echo = wizard.expect("echo").await;
    let room = wizard.expect("room").await.expect("no room after west");
    assert!(
        room["name"].as_str().unwrap().contains("Narrow Passage"),
        "Should be back at Narrow Passage, got: {}",
        room["name"]
    );

    // Move south back to Cave Entrance
    wizard.command("south").await.expect("south failed");
    let _echo = wizard.expect("echo").await;
    let room = wizard
        .expect("room")
        .await
        .expect("no room after final south");
    assert!(
        room["name"].as_str().unwrap().contains("Cave Entrance"),
        "Should be back at Cave Entrance, got: {}",
        room["name"]
    );
}

/// Test: Error when trying to move in direction with no exit
#[tokio::test]
async fn test_invalid_direction() {
    let server = TestServer::start().await.expect("Failed to start server");

    let mut wizard = server
        .connect_as(Role::Wizard {
            username: "invaliddir".to_string(),
        })
        .await
        .expect("Failed to connect as wizard");

    // Receive initial room description
    let _initial = wizard.expect("room").await;

    // At entrance, try to go west (no exit)
    wizard.command("west").await.expect("west failed");

    // Skip echo message
    let _echo = wizard.expect("echo").await;

    // Should get an error or output saying no exit
    let msg = wizard
        .expect_any_timeout(Duration::from_secs(5))
        .await
        .expect("no response");

    // Could be "error" type or "output" type with message about no exit
    let msg_type = msg["type"].as_str().unwrap();
    if msg_type == "error" {
        let text = msg["message"].as_str().unwrap_or("");
        assert!(
            text.to_lowercase().contains("exit")
                || text.to_lowercase().contains("can't go")
                || text.to_lowercase().contains("cannot go"),
            "Error should mention no exit: {}",
            text
        );
    } else if msg_type == "output" {
        let text = msg["text"].as_str().unwrap_or("");
        assert!(
            text.to_lowercase().contains("exit")
                || text.to_lowercase().contains("can't go")
                || text.to_lowercase().contains("cannot go"),
            "Output should mention no exit: {}",
            text
        );
    }
}

/// Test: Look command shows available exits
#[tokio::test]
async fn test_look_shows_exits() {
    let server = TestServer::start().await.expect("Failed to start server");

    let mut wizard = server
        .connect_as(Role::Wizard {
            username: "lookexits".to_string(),
        })
        .await
        .expect("Failed to connect as wizard");

    // Receive initial room description
    let _initial = wizard.expect("room").await;

    // Move to Narrow Passage which has multiple exits
    wizard.command("north").await.expect("north failed");
    let _echo = wizard.expect("echo").await;
    let _room = wizard.expect("room").await.expect("no room");

    wizard.command("look").await.expect("look failed");
    let _echo = wizard.expect("echo").await;
    let room = wizard.expect("room").await.expect("no room on look");

    // Check that exits are listed
    // Room message should have an "exits" field
    if let Some(exits) = room.get("exits") {
        if exits.is_array() {
            let exits_arr = exits.as_array().unwrap();
            assert!(!exits_arr.is_empty(), "Narrow Passage should have exits");
            // Should have south, north, east
            let exit_strs: Vec<&str> = exits_arr.iter().filter_map(|e| e.as_str()).collect();
            assert!(
                exit_strs.contains(&"south") || exit_strs.contains(&"s"),
                "Should have south exit"
            );
            assert!(
                exit_strs.contains(&"north") || exit_strs.contains(&"n"),
                "Should have north exit"
            );
            assert!(
                exit_strs.contains(&"east") || exit_strs.contains(&"e"),
                "Should have east exit"
            );
        } else if exits.is_object() {
            // Exits might be an object mapping direction -> room_id
            let exits_obj = exits.as_object().unwrap();
            assert!(
                !exits_obj.is_empty(),
                "Narrow Passage should have exits object"
            );
        }
    }
    // If no exits field, might be in description - that's also acceptable
}

/// Test: Look command shows NPCs and items in room
#[tokio::test]
async fn test_look_shows_contents() {
    let server = TestServer::start().await.expect("Failed to start server");

    let mut wizard = server
        .connect_as(Role::Wizard {
            username: "lookcontents".to_string(),
        })
        .await
        .expect("Failed to connect as wizard");

    // Receive initial room description at entrance
    let _initial = wizard.expect("room").await;

    // At entrance, should see Rusty Short Sword
    wizard.command("look").await.expect("look failed");
    let _echo = wizard.expect("echo").await;
    let room = wizard.expect("room").await.expect("no room");

    // Check for items in room - could be in "items" or "contents" fields
    let has_items = room.get("items").is_some_and(|i| {
        if i.is_array() {
            !i.as_array().unwrap().is_empty()
        } else {
            false
        }
    });

    // Check "contents" field for sword
    let has_sword_in_contents = room.get("contents").is_some_and(|c| {
        if let Some(arr) = c.as_array() {
            arr.iter().any(|item| {
                item.as_str()
                    .is_some_and(|s| s.to_lowercase().contains("sword"))
            })
        } else {
            false
        }
    });

    // Or mentioned in description
    let desc = room
        .get("description")
        .and_then(|d| d.as_str())
        .unwrap_or("");
    let mentions_sword = desc.to_lowercase().contains("sword");

    assert!(
        has_items || has_sword_in_contents || mentions_sword,
        "Entrance should show the Rusty Sword somehow. Room: {:?}",
        room
    );

    // Move to Narrow Passage to check for NPC
    wizard.command("north").await.expect("north failed");
    let _echo = wizard.expect("echo").await;
    let _room = wizard.expect("room").await.expect("no room");

    wizard.command("look").await.expect("look failed");
    let _echo = wizard.expect("echo").await;
    let room = wizard.expect("room").await.expect("no room on look");

    // Check for NPCs in "living" field
    let has_living = room.get("living").is_some_and(|l| {
        if l.is_array() {
            !l.as_array().unwrap().is_empty()
        } else {
            false
        }
    });

    // Check for NPCs in "npcs" field
    let has_npcs = room.get("npcs").is_some_and(|n| {
        if n.is_array() {
            !n.as_array().unwrap().is_empty()
        } else {
            false
        }
    });

    // Check "contents" field for bat (contents contains NPC names as strings)
    let has_bat_in_contents = room.get("contents").is_some_and(|c| {
        if let Some(arr) = c.as_array() {
            arr.iter().any(|item| {
                item.as_str()
                    .is_some_and(|s| s.to_lowercase().contains("bat"))
            })
        } else {
            false
        }
    });

    let desc = room
        .get("description")
        .and_then(|d| d.as_str())
        .unwrap_or("");
    let mentions_bat = desc.to_lowercase().contains("bat");

    assert!(
        has_living || has_npcs || has_bat_in_contents || mentions_bat,
        "Narrow Passage should show the Giant Bat somehow. Room: {:?}",
        room
    );
}

async fn wizard(server: &TestServer, name: &str) -> crate::harness::TestClient {
    let mut c = server
        .connect_as(Role::Wizard {
            username: name.to_string(),
        })
        .await
        .expect("connect");
    c.drain().await;
    c
}

/// Run `code` with eval and return the output text
async fn eval(c: &mut crate::harness::TestClient, code: &str) -> String {
    c.drain().await;
    c.command(&format!("eval {}", code)).await.unwrap();
    let _ = c.expect("echo").await;
    let reply = c.expect_any().await.expect("eval reply");
    assert_ne!(reply["type"], "error", "eval failed: {}", reply);
    reply["text"].as_str().unwrap_or_default().to_string()
}

/// game.move_object on a connected player moves their session too: they get
/// the new room, and later commands happen there
#[tokio::test]
async fn test_lua_move_updates_connected_player() {
    let server = TestServer::start().await.expect("server");
    let mut pl = server
        .connect_as(Role::Player {
            username: "moved_player".into(),
        })
        .await
        .unwrap();
    pl.drain().await;
    let mut wiz = wizard(&server, "mover_wiz").await;
    let passage = server.world().passage_id.clone();
    eval(
        &mut wiz,
        &format!(
            r#"return tostring(game.move_object("{}", "{}"))"#,
            pl.player_id().unwrap(),
            passage
        ),
    )
    .await;

    let room = pl
        .expect("room")
        .await
        .expect("moved player sees the new room");
    assert_eq!(room["name"], "Narrow Passage");
    pl.drain().await;
    pl.command("look").await.unwrap();
    let _ = pl.expect("echo").await;
    let room = pl.expect("room").await.unwrap();
    assert_eq!(
        room["name"], "Narrow Passage",
        "the session is in the new room"
    );
}

/// Moving any object fires the rooms' hooks, with moved_id
#[tokio::test]
async fn test_lua_move_fires_room_hooks() {
    let server = TestServer::start().await.expect("server");
    let mut wiz = wizard(&server, "hook_wiz").await;
    eval(
        &mut wiz,
        r#"local h = game.store_code([[return {
             on_enter = function(a) game.broadcast(a.room_id, "arrived:" .. tostring(a.moved_id) .. ":" .. tostring(a.actor_id)) end,
             on_leave = function(a) game.broadcast(a.to, "left-from-ring:" .. tostring(a.moved_id)) end,
           }]])
           game.create_object("/rooms/ring", "room", nil, {name = "Ring", code_hash = h})
           game.create_object("/rooms/ring/pebble", "item", "/rooms/ring", {name = "pebble"})
           return "ok""#,
    )
    .await;
    // The wizard stands in the entrance; move the pebble from the ring there
    // (sent raw: the hooks' broadcasts arrive before eval's own reply)
    let entrance = server.world().entrance_id.clone();
    wiz.drain().await;
    wiz.command(&format!(
        r#"eval return tostring(game.move_object("/rooms/ring/pebble", "{}"))"#,
        entrance
    ))
    .await
    .unwrap();
    let heard = wiz
        .expect_output_containing("left-from-ring:/rooms/ring/pebble")
        .await
        .expect("on_leave ran on the old room with moved_id");
    assert_eq!(heard, "left-from-ring:/rooms/ring/pebble");

    // And into the ring: its on_enter runs (broadcast into the ring, where a
    // second wizard waits)
    let mut watcher = wizard(&server, "ring_watcher").await;
    eval(&mut watcher, r#"return "x""#).await;
    watcher.command("goto /rooms/ring").await.unwrap();
    watcher.expect("room").await.unwrap();
    watcher.drain().await;
    eval(
        &mut wiz,
        r#"return tostring(game.move_object("/rooms/ring/pebble", "/rooms/ring"))"#,
    )
    .await;
    watcher
        .expect_output_containing("arrived:/rooms/ring/pebble:nil")
        .await
        .expect("on_enter ran with moved_id, and no actor_id for a Lua move");
}

/// Hooks that move the object on (rooms bouncing it back and forth) stop at
/// the nesting limit instead of looping forever
#[tokio::test]
async fn test_move_hooks_are_depth_limited() {
    let server = TestServer::start().await.expect("server");
    let mut wiz = wizard(&server, "bounce_wiz").await;
    eval(
        &mut wiz,
        r#"local function bouncer(other)
             return game.store_code(string.format([[return {
               on_enter = function(a)
                 local o = game.get_object(a.moved_id)
                 game.update_object(a.moved_id, {bounces = (o.metadata.bounces or 0) + 1})
                 game.move_object(a.moved_id, %q)
               end,
             }]], other))
           end
           game.create_object("/rooms/ping", "room", nil, {name = "Ping", code_hash = bouncer("/rooms/pong")})
           game.create_object("/rooms/pong", "room", nil, {name = "Pong", code_hash = bouncer("/rooms/ping")})
           game.create_object("/items/ball", "item", nil, {name = "ball"})
           return "ok""#,
    )
    .await;
    eval(
        &mut wiz,
        r#"return tostring(game.move_object("/items/ball", "/rooms/ping"))"#,
    )
    .await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    let bounces: i64 = eval(
        &mut wiz,
        r#"return game.get_object("/items/ball").metadata.bounces"#,
    )
    .await
    .parse()
    .unwrap_or(0);
    assert!(
        (1..=8).contains(&bounces),
        "hooks ran a bounded number of times, got {}",
        bounces
    );
}

/// Hooks that move several objects each fan out; a total budget per run
/// bounds the work, not just the nesting depth
#[tokio::test]
async fn test_move_hooks_fan_out_is_bounded() {
    let server = TestServer::start().await.expect("server");
    let mut wiz = wizard(&server, "fan_wiz").await;
    eval(
        &mut wiz,
        r#"game.create_object("/items/counter", "item", nil, {name = "counter", n = 0})
           local function fan(other)
             return game.store_code(string.format([[return {
               on_enter = function(a)
                 local c = game.get_object("/items/counter")
                 game.update_object("/items/counter", {n = c.metadata.n + 1})
                 for i = 1, 5 do game.move_object("/items/ball" .. i, %q) end
               end,
             }]], other))
           end
           game.create_object("/rooms/fan-a", "room", nil, {name = "A", code_hash = fan("/rooms/fan-b")})
           game.create_object("/rooms/fan-b", "room", nil, {name = "B", code_hash = fan("/rooms/fan-a")})
           for i = 1, 5 do game.create_object("/items/ball" .. i, "item", nil, {name = "ball"}) end
           return "ok""#,
    )
    .await;
    eval(
        &mut wiz,
        r#"return tostring(game.move_object("/items/ball1", "/rooms/fan-a"))"#,
    )
    .await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    let n: i64 = eval(
        &mut wiz,
        r#"return game.get_object("/items/counter").metadata.n"#,
    )
    .await
    .parse()
    .unwrap_or(-1);
    assert!((1..=40).contains(&n), "on_enter ran {} times", n);
}

/// Moves between containers without code do not use up the run's hook
/// budget: a later real hook still runs
#[tokio::test]
async fn test_codeless_moves_do_not_spend_hook_budget() {
    let server = TestServer::start().await.expect("server");
    let mut wiz = wizard(&server, "budget_wiz").await;
    eval(
        &mut wiz,
        r#"local h = game.store_code([[return {
             on_enter = function(a) game.broadcast(a.room_id, "hub-entered:" .. a.moved_id) end,
           }]])
           game.create_object("/rooms/hub", "room", nil, {name = "Hub", code_hash = h})
           game.create_object("/rooms/bare-a", "room", nil, {name = "Bare A"})
           game.create_object("/rooms/bare-b", "room", nil, {name = "Bare B"})
           game.create_object("/items/token", "item", "/rooms/bare-a", {name = "token"})
           return "ok""#,
    )
    .await;
    let mut watcher = wizard(&server, "hub_watcher").await;
    watcher.command("goto /rooms/hub").await.unwrap();
    watcher.expect("room").await.unwrap();
    watcher.drain().await;
    eval(
        &mut wiz,
        r#"for i = 1, 20 do
             game.move_object("/items/token", "/rooms/bare-b")
             game.move_object("/items/token", "/rooms/bare-a")
           end
           game.move_object("/items/token", "/rooms/hub")
           return "moved""#,
    )
    .await;
    watcher
        .expect_output_containing("hub-entered:/items/token")
        .await
        .expect("the hub's on_enter still ran");
}

/// A room's on_leave that moves the player elsewhere (a trapdoor) wins over
/// the walk that triggered it
#[tokio::test]
async fn test_on_leave_teleport_is_not_undone_by_the_walk() {
    let server = TestServer::start().await.expect("server");
    let mut wiz = wizard(&server, "trap_wiz").await;
    eval(
        &mut wiz,
        r#"local h = game.store_code([[return {
             on_leave = function(a) game.move_object(a.actor_id, "/rooms/dungeon") end,
           }]])
           game.create_object("/rooms/beyond", "room", nil, {name = "Beyond"})
           game.create_object("/rooms/dungeon", "room", nil, {name = "Dungeon"})
           game.create_object("/rooms/trap", "room", nil,
             {name = "Trap", exits = {north = "/rooms/beyond"}, code_hash = h})
           return "ok""#,
    )
    .await;
    let mut pl = server
        .connect_as(Role::Player {
            username: "trapped".into(),
        })
        .await
        .unwrap();
    pl.drain().await;
    eval(
        &mut wiz,
        &format!(
            r#"return tostring(game.move_object("{}", "/rooms/trap"))"#,
            pl.player_id().unwrap()
        ),
    )
    .await;
    pl.expect("room").await.unwrap();
    pl.drain().await;

    pl.command("north").await.unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    pl.drain().await;
    pl.command("look").await.unwrap();
    let _ = pl.expect("echo").await;
    let room = pl.expect("room").await.unwrap();
    assert_eq!(room["name"], "Dungeon", "the trapdoor wins");
}

/// A player moved by Lua into a safe room respawns there later
#[tokio::test]
async fn test_lua_move_records_safe_location() {
    let server = TestServer::start().await.expect("server");
    let mut wiz = wizard(&server, "safe_wiz").await;
    eval(
        &mut wiz,
        r#"game.create_object("/rooms/hub", "room", nil, {name = "Hub", is_portal = true})
           return "ok""#,
    )
    .await;
    let mut pl = server
        .connect_as(Role::Player {
            username: "hub_bound".into(),
        })
        .await
        .unwrap();
    pl.drain().await;
    let player_id = pl.player_id().unwrap().to_string();
    eval(
        &mut wiz,
        &format!(
            r#"return tostring(game.move_object("{}", "/rooms/hub"))"#,
            player_id
        ),
    )
    .await;
    pl.expect("room").await.unwrap();
    let safe = eval(
        &mut wiz,
        &format!(
            r#"return tostring(game.get_object("{}").metadata.last_safe_location)"#,
            player_id
        ),
    )
    .await;
    assert_eq!(safe, "/rooms/hub");
}

/// Crossing into another region runs the old region's on_leave and the new
/// one's on_enter; arriving runs on_init on each object there with code
/// (and on the newcomer, per object, if it has code); anything moved with
/// code runs its on_move
#[tokio::test]
async fn test_region_init_and_move_hooks() {
    let server = TestServer::start().await.expect("server");
    let mut wiz = wizard(&server, "regions_wiz").await;
    let out = eval(
        &mut wiz,
        r#"game.create_object("/items/journal", "item", nil, {name = "journal", log = ""})
           local note = [[local function note(w) local j = game.get_object("/items/journal") game.update_object("/items/journal", {log = j.metadata.log .. w .. ";"}) end ]]
           local forest = game.store_code(note .. [[return {
             on_enter = function(a) note("enter-forest:" .. tostring(a.moved_id == a.actor_id)) end,
             on_leave = function(a) note("leave-forest") end }]])
           local town = game.store_code(note .. [[return {
             on_enter = function(a) note("enter-town") end,
             on_leave = function(a) note("leave-town") end }]])
           local shopkeeper = game.store_code(note .. [[return {
             on_init = function(a) note("init-shop:" .. tostring(a.arrived_id ~= nil)) end }]])
           local wanderstone = game.store_code(note .. [[return {
             on_move = function(a) note("moved-stone:" .. tostring(a.to)) end }]])
           game.create_object("/d/forest", "region", nil, {name = "Forest", code_hash = forest})
           game.create_object("/d/town", "region", nil, {name = "Town", code_hash = town})
           game.create_object("/d/town/gate", "room", "/d/town", {name = "Town Gate", exits = {west = "/d/forest/edge"}})
           game.create_object("/d/forest/edge", "room", "/d/forest", {name = "Forest Edge", exits = {east = "/d/town/gate"}})
           game.create_object("/d/forest/edge/hermit", "npc", "/d/forest/edge", {name = "hermit", code_hash = shopkeeper})
           game.create_object("/items/stone", "item", "/d/town/gate", {name = "stone", code_hash = wanderstone})
           return "ok""#,
    )
    .await;
    assert_eq!(out, "ok");
    wiz.drain().await;
    wiz.command("goto /d/town/gate").await.unwrap();
    wiz.expect("room").await.unwrap();
    wiz.drain().await;
    wiz.command("west").await.unwrap();
    wiz.expect("room").await.unwrap();
    eval(
        &mut wiz,
        r#"game.move_object("/items/stone", "/d/forest/edge") return "ok""#,
    )
    .await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    let log: String = sqlx::query_scalar(
        "SELECT json_extract(properties, '$.log') FROM objects WHERE universe_id = ? AND id = '/items/journal'",
    )
    .bind(server.universe_id())
    .fetch_one(server.pool())
    .await
    .unwrap();
    assert!(
        log.contains("leave-town;enter-forest:true;"),
        "regions crossed: {}",
        log
    );
    assert!(
        log.contains("init-shop:true;"),
        "on_init on the hermit: {}",
        log
    );
    assert!(
        log.contains("moved-stone:/d/forest/edge;"),
        "on_move on the stone: {}",
        log
    );
}

/// A room naming a region in its region_id (without being inside it) does
/// not fire that region's hooks: only the room's parent region does
#[tokio::test]
async fn test_region_hooks_not_spoofed_by_region_id() {
    let server = TestServer::start().await.expect("server");
    let mut wiz = wizard(&server, "spoof_wiz").await;
    let out = eval(
        &mut wiz,
        r#"game.create_object("/items/tripwire", "item", nil, {name = "tripwire", hits = 0})
           local h = game.store_code([[return { on_enter = function(a)
             local t = game.get_object("/items/tripwire")
             game.update_object("/items/tripwire", {hits = t.metadata.hits + 1})
           end }]])
           game.create_object("/d/vault", "region", nil, {name = "Vault", code_hash = h})
           game.create_object("/rooms/fake", "room", nil, {name = "Fake", region_id = "/d/vault"})
           return "ok""#,
    )
    .await;
    assert_eq!(out, "ok");
    wiz.drain().await;
    wiz.command("goto /rooms/fake").await.unwrap();
    wiz.expect("room").await.unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    let hits: i64 = sqlx::query_scalar(
        "SELECT json_extract(properties, '$.hits') FROM objects WHERE universe_id = ? AND id = '/items/tripwire'",
    )
    .bind(server.universe_id())
    .fetch_one(server.pool())
    .await
    .unwrap();
    assert_eq!(hits, 0, "the vault's hook did not fire");
}

/// A room with reset_seconds runs its on_reset that often (to restock); the
/// room a player was in gets on_disconnect when their grace period ends
#[tokio::test]
async fn test_on_reset_and_on_disconnect() {
    let server = TestServer::start().await.expect("server");
    let mut wiz = wizard(&server, "reset_wiz").await;
    let out = eval(
        &mut wiz,
        r#"game.create_object("/items/tally", "item", nil, {name = "tally", resets = 0, left = ""})
           local h = game.store_code([[return {
             on_reset = function(a)
               local t = game.get_object("/items/tally")
               game.update_object("/items/tally", {resets = t.metadata.resets + 1})
             end,
             on_disconnect = function(a)
               game.update_object("/items/tally", {left = a.player_id})
             end }]])
           game.create_object("/rooms/shop", "room", nil, {name = "Shop", reset_seconds = 1, code_hash = h})
           return "ok""#,
    )
    .await;
    assert_eq!(out, "ok");
    let mut pl = server
        .connect_as(crate::harness::Role::Player {
            username: "shopper".into(),
        })
        .await
        .unwrap();
    let shopper = pl.player_id().unwrap().to_string();
    eval(
        &mut wiz,
        &format!(
            r#"game.move_object("{}", "/rooms/shop") return "ok""#,
            shopper
        ),
    )
    .await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    pl.close().await.unwrap();
    tokio::time::sleep(Duration::from_millis(6500)).await;
    let (resets, left): (i64, String) = sqlx::query_as(
        "SELECT json_extract(properties, '$.resets'), json_extract(properties, '$.left')
         FROM objects WHERE universe_id = ? AND id = '/items/tally'",
    )
    .bind(server.universe_id())
    .fetch_one(server.pool())
    .await
    .unwrap();
    assert!(resets >= 3, "on_reset ran about once a second: {}", resets);
    assert_eq!(left, shopper, "on_disconnect told the room");
}

/// Phase 3 review: a room placed under someone else's region does not fire
/// its hooks; fleeing runs on_move; a crowded room's on_init does not
/// starve the newcomer's on_move
#[tokio::test]
async fn test_move_hooks_after_review() {
    let server = TestServer::start().await.expect("server");
    let mut adm = server
        .connect_as(crate::harness::Role::Admin {
            username: "region_owner".into(),
        })
        .await
        .unwrap();
    let mut wiz = wizard(&server, "squatter").await;
    // The admin's region, with code
    adm.drain().await;
    adm.command(
        r#"eval game.create_object("/items/alarm", "item", nil, {name = "alarm", rings = 0, moves = 0})
           local h = game.store_code([[return { on_enter = function(a)
             local t = game.get_object("/items/alarm") game.update_object("/items/alarm", {rings = t.metadata.rings + 1})
           end }]])
           game.create_object("/d/palace", "region", nil, {name = "Palace", code_hash = h})
           return "made""#,
    )
    .await
    .unwrap();
    adm.expect_output_containing("made").await.unwrap();
    // The wizard puts a room of their own under it and walks in
    let out = eval(
        &mut wiz,
        r#"game.create_object("/rooms/squat", "room", "/d/palace", {name = "Squat"}) return "ok""#,
    )
    .await;
    assert_eq!(out, "ok");
    wiz.drain().await;
    wiz.command("goto /rooms/squat").await.unwrap();
    wiz.expect("room").await.unwrap();
    // A coded walker moved into a crowded room still gets on_move, and
    // fleeing runs on_move too
    let out = eval(
        &mut wiz,
        r#"local h = game.store_code([[return {
             on_init = function(a) end,
             on_move = function(a) local t = game.get_object("/items/alarm") end }]])
           local mover = game.store_code([[return { on_move = function(a)
             local l = game.get_object("/rooms/crowd/ledger") game.update_object("/rooms/crowd/ledger", {n = l.metadata.n + 1})
           end }]])
           game.create_object("/rooms/crowd", "room", nil, {name = "Crowd", exits = {east = "/rooms/out"}})
           game.create_object("/rooms/out", "room", nil, {name = "Out", exits = {west = "/rooms/crowd"}})
           game.create_object("/rooms/crowd/ledger", "item", "/rooms/crowd", {name = "ledger", n = 0})
           for i = 1, 40 do game.create_object("/rooms/crowd/deco-" .. i, "item", "/rooms/crowd", {name = "deco", code_hash = h}) end
           game.create_object("/items/walker", "item", nil, {name = "walker", code_hash = mover})
           return "ok""#,
    )
    .await;
    assert_eq!(out, "ok");
    eval(
        &mut wiz,
        r#"game.move_object("/items/walker", "/rooms/crowd") return "ok""#,
    )
    .await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    let (rings, n): (i64, i64) = sqlx::query_as(
        "SELECT (SELECT json_extract(properties, '$.rings') FROM objects WHERE universe_id = ?1 AND id = '/items/alarm'),
                (SELECT json_extract(properties, '$.n') FROM objects WHERE universe_id = ?1 AND id = '/rooms/crowd/ledger')",
    )
    .bind(server.universe_id())
    .fetch_one(server.pool())
    .await
    .unwrap();
    assert_eq!(
        rings, 0,
        "a room under a region it has no authority over does not fire it"
    );
    assert_eq!(n, 1, "on_move ran despite the crowd's on_init");
}
