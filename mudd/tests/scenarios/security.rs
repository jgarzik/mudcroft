//! Security scenario tests
//!
//! Privileged Lua functions must enforce the documented access levels:
//! - game.set_access_level: admin+, cannot grant above own level, admin/owner
//!   grants need owner, cannot modify peers or superiors
//! - game.admin_grant_credits / game.update_universe: admin+
//! - game.set_actor: cannot impersonate an account at or above the caller's level

use crate::harness::{Role, TestClient, TestServer};

/// Run `eval <code>` and return the output text (panics on error replies)
async fn eval(client: &mut TestClient, code: &str) -> String {
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

/// The account's stored level in the test universe: its member level, or
/// its global level when it is not a member
async fn db_access_level(server: &TestServer, account_id: &str) -> String {
    sqlx::query_scalar::<_, String>(
        "SELECT COALESCE(
            (SELECT level FROM universe_members WHERE universe_id = ? AND account_id = ?),
            (SELECT access_level FROM accounts WHERE id = ?))",
    )
    .bind(server.universe_id())
    .bind(account_id)
    .bind(account_id)
    .fetch_one(server.pool())
    .await
    .expect("account row")
}

async fn connect(server: &TestServer, role: Role) -> TestClient {
    server.connect_as(role).await.expect("connect")
}

fn wizard(name: &str) -> Role {
    Role::Wizard {
        username: name.to_string(),
    }
}

fn admin(name: &str) -> Role {
    Role::Admin {
        username: name.to_string(),
    }
}

fn player(name: &str) -> Role {
    Role::Player {
        username: name.to_string(),
    }
}

#[tokio::test]
async fn test_wizard_cannot_set_access_level() {
    let server = TestServer::start().await.expect("server");
    let mut wiz = connect(&server, wizard("sec_wiz")).await;
    let wiz_id = wiz.account_id().to_string();

    let out = eval(
        &mut wiz,
        &format!(r#"return game.set_access_level("{}", "owner")"#, wiz_id),
    )
    .await;

    assert_eq!(out, "false", "wizard self-promotion must be refused");
    assert_eq!(db_access_level(&server, &wiz_id).await, "wizard");
}

#[tokio::test]
async fn test_admin_set_access_level_rules() {
    let server = TestServer::start().await.expect("server");
    let mut adm = connect(&server, admin("sec_admin")).await;
    let peer = connect(&server, admin("sec_admin_peer")).await;
    let pl = connect(&server, player("sec_player")).await;
    let peer_id = peer.account_id().to_string();
    let pl_id = pl.account_id().to_string();

    // Allowed: promote a player to builder
    let out = eval(
        &mut adm,
        &format!(r#"return game.set_access_level("{}", "builder")"#, pl_id),
    )
    .await;
    assert_eq!(out, "true");
    assert_eq!(db_access_level(&server, &pl_id).await, "builder");

    // Refused: granting admin requires owner
    let out = eval(
        &mut adm,
        &format!(r#"return game.set_access_level("{}", "admin")"#, pl_id),
    )
    .await;
    assert_eq!(out, "false", "admin must not grant admin");
    assert_eq!(db_access_level(&server, &pl_id).await, "builder");

    // Refused: demoting a peer admin
    let out = eval(
        &mut adm,
        &format!(r#"return game.set_access_level("{}", "player")"#, peer_id),
    )
    .await;
    assert_eq!(out, "false", "admin must not modify a peer admin");
    assert_eq!(db_access_level(&server, &peer_id).await, "admin");
}

#[tokio::test]
async fn test_grant_credits_requires_admin() {
    let server = TestServer::start().await.expect("server");
    let mut wiz = connect(&server, wizard("cred_wiz")).await;
    let mut adm = connect(&server, admin("cred_admin")).await;
    let pl = connect(&server, player("cred_player")).await;
    let pl_id = pl.account_id().to_string();

    let out = eval(
        &mut wiz,
        &format!(r#"return game.admin_grant_credits("{}", 50)"#, pl_id),
    )
    .await;
    assert_eq!(out, "false", "wizard must not grant credits");

    let out = eval(
        &mut adm,
        &format!(r#"return game.admin_grant_credits("{}", 50)"#, pl_id),
    )
    .await;
    assert_eq!(out, "true", "admin may grant credits");
}

#[tokio::test]
async fn test_update_universe_requires_admin() {
    let server = TestServer::start().await.expect("server");
    let mut wiz = connect(&server, wizard("uni_wiz")).await;
    let mut adm = connect(&server, admin("uni_admin")).await;

    let out = eval(&mut wiz, r#"return game.update_universe({motd = "wiz"})"#).await;
    assert_eq!(out, "false", "wizard must not change universe config");

    let out = eval(&mut adm, r#"return game.update_universe({motd = "adm"})"#).await;
    assert_eq!(out, "true", "admin may change universe config");
}

#[tokio::test]
async fn test_set_actor_cannot_impersonate_superior() {
    let server = TestServer::start().await.expect("server");
    let mut wiz = connect(&server, wizard("actor_wiz")).await;
    let adm = connect(&server, admin("actor_admin")).await;
    let pl = connect(&server, player("actor_player")).await;
    let adm_id = adm.account_id().to_string();
    let pl_id = pl.account_id().to_string();

    // Impersonating a superior is refused and leaves the actor unchanged
    let out = eval(
        &mut wiz,
        &format!(
            r#"local ok = game.set_actor("{}"); return tostring(ok) .. ":" .. tostring(game.get_actor())"#,
            adm_id
        ),
    )
    .await;
    assert_eq!(out, "false:nil", "wizard must not act as an admin");

    // ...so it cannot be used to reach admin-only functions
    let out = eval(
        &mut wiz,
        &format!(
            r#"game.set_actor("{}"); return game.set_access_level("{}", "builder")"#,
            adm_id, pl_id
        ),
    )
    .await;
    assert_eq!(out, "false");
    assert_eq!(db_access_level(&server, &pl_id).await, "player");

    // Forging the old Lua global has no effect
    let out = eval(
        &mut wiz,
        &format!(
            r#"_current_actor_id = "{}"; return game.set_access_level("{}", "builder")"#,
            adm_id, pl_id
        ),
    )
    .await;
    assert_eq!(out, "false", "a Lua global must not override the actor");
    assert_eq!(db_access_level(&server, &pl_id).await, "player");

    // Acting as a lower-level account is still allowed
    let out = eval(
        &mut wiz,
        &format!(
            r#"local ok = game.set_actor("{}"); return tostring(ok) .. ":" .. tostring(game.get_actor())"#,
            pl_id
        ),
    )
    .await;
    assert_eq!(out, format!("true:{}", pl_id));
}

/// Player command arguments must never be spliced into Lua source.
/// `take x\"); return 'INJECTED' --` used to break out of the string literal.
#[tokio::test]
async fn test_take_argument_cannot_inject_lua() {
    let server = TestServer::start().await.expect("server");
    let mut pl = connect(&server, player("inject_player")).await;
    pl.drain().await;

    for cmd in [
        r#"take x\"); return 'INJECTED' --"#,
        r#"drop x\"); return 'INJECTED' --"#,
    ] {
        pl.command(cmd).await.expect("send");
        let _ = pl.expect("echo").await;
        let reply = pl.expect_any().await.expect("reply");
        let text = reply["text"]
            .as_str()
            .or(reply["message"].as_str())
            .unwrap_or_default();
        assert_ne!(text, "INJECTED", "Lua injection via `{}`", cmd);
    }
}

/// game.store_code is wizard+ (PermAction::StoreCode): acting as a player
/// must not be able to plant code
#[tokio::test]
async fn test_store_code_requires_wizard() {
    let server = TestServer::start().await.expect("server");
    let mut wiz = connect(&server, wizard("code_wiz")).await;
    let pl = connect(&server, player("code_player")).await;
    let pl_id = pl.account_id().to_string();

    let out = eval(
        &mut wiz,
        r#"local h = game.store_code("return {}"); return type(h)"#,
    )
    .await;
    assert_eq!(out, "string", "wizard may store code");

    let out = eval(
        &mut wiz,
        &format!(
            r#"game.set_actor("{}"); local ok, err = pcall(game.store_code, "return {{}}"); return tostring(ok)"#,
            pl_id
        ),
    )
    .await;
    assert_eq!(out, "false", "a player identity must not store code");
}

/// Objects created via Lua are owned by the creating user
#[tokio::test]
async fn test_created_object_owned_by_creator() {
    let server = TestServer::start().await.expect("server");
    let mut wiz = connect(&server, wizard("owner_wiz")).await;
    let wiz_id = wiz.account_id().to_string();

    eval(
        &mut wiz,
        r#"return game.create_object("/items/owned-thing", "item", nil, {name = "thing"}).id"#,
    )
    .await;

    let owner: Option<String> =
        sqlx::query_scalar("SELECT owner_id FROM objects WHERE id = '/items/owned-thing'")
            .fetch_one(server.pool())
            .await
            .expect("object row");
    assert_eq!(owner.as_deref(), Some(wiz_id.as_str()));
}

/// Lua object writes are permission-checked for the acting user:
/// a player identity may not create, modify or delete objects it does not
/// own and has no path grant for, but may move non-fixed items.
#[tokio::test]
async fn test_lua_object_writes_are_permission_checked() {
    let server = TestServer::start().await.expect("server");
    let mut wiz = connect(&server, wizard("perm_wiz")).await;
    let pl = connect(&server, player("perm_player")).await;
    let pl_id = pl.account_id().to_string();

    // Wizard sets up objects (wizards bypass path checks)
    eval(
        &mut wiz,
        r#"game.create_object("/d/perm/box", "item", nil, {name = "box"})
           game.create_object("/d/perm/statue", "item", nil, {name = "statue", fixed = true})
           return true"#,
    )
    .await;

    let as_player = |code: &str| {
        format!(
            r#"game.set_actor("{}"); local ok, r = pcall(function() {} end); if not ok then return "denied" end; if type(r) == "table" and r.error then return "denied" end; return tostring(r)"#,
            pl_id, code
        )
    };

    let out = eval(
        &mut wiz,
        &as_player(r#"return game.create_object("/d/perm/new", "item", nil, {})"#),
    )
    .await;
    assert_eq!(out, "denied", "player must not create objects");

    let out = eval(
        &mut wiz,
        &as_player(r#"return game.update_object("/d/perm/box", {name = "mine"})"#),
    )
    .await;
    assert_eq!(out, "denied", "player must not modify others' objects");

    let out = eval(
        &mut wiz,
        &as_player(r#"return game.delete_object("/d/perm/box")"#),
    )
    .await;
    assert_eq!(out, "denied", "player must not delete others' objects");

    let out = eval(
        &mut wiz,
        &as_player(r#"return game.move_object("/d/perm/statue", nil)"#),
    )
    .await;
    assert_eq!(out, "denied", "player must not move fixed objects");

    let out = eval(
        &mut wiz,
        &as_player(r#"return game.clone_object("/d/perm/box", "/d/perm/box2", nil)"#),
    )
    .await;
    assert_eq!(out, "denied", "player must not clone into unowned paths");

    let out = eval(
        &mut wiz,
        &as_player(r#"return game.move_object("/d/perm/box", nil)"#),
    )
    .await;
    assert_eq!(out, "true", "player may move non-fixed items");

    // The box is untouched by the denied writes
    let name: String = sqlx::query_scalar(
        "SELECT json_extract(properties, '$.name') FROM objects WHERE id = '/d/perm/box'",
    )
    .fetch_one(server.pool())
    .await
    .expect("box still exists");
    assert_eq!(name, "box");
}

/// goto/setportal must not reach rooms in another universe
#[tokio::test]
async fn test_goto_and_setportal_stay_in_universe() {
    let server = TestServer::start().await.expect("server");
    let mut wiz = connect(&server, wizard("xuni_wiz")).await;
    let owner = server.world().wizard_account_id.clone();

    // A second universe with one room
    sqlx::query("INSERT INTO universes (id, name, owner_id) VALUES ('other-world', 'Other', ?)")
        .bind(&owner)
        .execute(server.pool())
        .await
        .expect("create universe");
    let mut room = mudd::objects::Object::new("/rooms/elsewhere", "other-world", "room").unwrap();
    room.set_property("name", serde_json::json!("Elsewhere"));
    server
        .world()
        .store()
        .create(&room)
        .await
        .expect("create room");

    for cmd in ["goto /rooms/elsewhere", "setportal /rooms/elsewhere"] {
        wiz.drain().await;
        wiz.command(cmd).await.unwrap();
        let _ = wiz.expect("echo").await;
        let reply = wiz.expect_any().await.unwrap();
        assert_eq!(
            reply["type"], "error",
            "`{}` must be refused: {}",
            cmd, reply
        );
    }
}

/// Object code runs as the object's owner, so attaching code requires being
/// the owner or outranking them
#[tokio::test]
async fn test_attaching_code_requires_owner_or_superior() {
    let server = TestServer::start().await.expect("server");
    let mut wiz = connect(&server, wizard("attach_wiz")).await;
    let mut adm = connect(&server, admin("attach_admin")).await;

    eval(
        &mut adm,
        r#"game.create_object("/items/admin-orb", "item", nil, {name = "orb"}); return true"#,
    )
    .await;
    eval(
        &mut wiz,
        r#"game.create_object("/items/wiz-orb", "item", nil, {name = "orb"}); return true"#,
    )
    .await;

    // Wizard cannot attach code to an admin's object (it would run as admin)
    let out = eval(
        &mut wiz,
        r#"local h = game.store_code("return {}")
           local ok = pcall(game.update_object, "/items/admin-orb", {code_hash = h})
           return tostring(ok)"#,
    )
    .await;
    assert_eq!(
        out, "false",
        "wizard must not attach code to an admin's object"
    );

    // Plain property edits are still fine for wizards
    let out = eval(
        &mut wiz,
        r#"return game.update_object("/items/admin-orb", {shiny = true})"#,
    )
    .await;
    assert_eq!(out, "true");

    // Owners and superiors may attach code
    let out = eval(
        &mut wiz,
        r#"local h = game.store_code("return {}"); return game.update_object("/items/wiz-orb", {code_hash = h})"#,
    )
    .await;
    assert_eq!(out, "true", "owner may attach code");
    let out = eval(
        &mut adm,
        r#"local h = game.store_code("return {}"); return game.update_object("/items/wiz-orb", {code_hash = h})"#,
    )
    .await;
    assert_eq!(out, "true", "a superior may attach code");
}

/// game.use_object runs the object's code as its owner: an admin using a
/// builder's object does not lend the builder admin rights (an admin could
/// promote the builder to wizard; the builder itself cannot)
#[tokio::test]
async fn test_use_object_runs_as_owner() {
    let server = TestServer::start().await.expect("server");
    let mut wiz = connect(&server, admin("use_admin")).await;
    let bld = server
        .connect_as(Role::Builder {
            username: "use_builder".to_string(),
            regions: vec![],
        })
        .await
        .unwrap();
    let bld_id = bld.account_id().to_string();

    // The builder owns a trinket whose code tries to promote the builder
    let out = eval(
        &mut wiz,
        &format!(
            r#"game.grant_path("{b}", "/d/workshop", false)
               local h = game.store_code([[return {{
                 on_use = function(a) return game.set_access_level("{b}", "wizard") end,
               }}]])
               game.set_actor("{b}")
               local t = game.create_object("/d/workshop/trinket", "item", nil, {{name = "trinket", code_hash = h}})
               game.set_actor(nil)
               return t.owner_id"#,
            b = bld_id
        ),
    )
    .await;
    assert_eq!(out, bld_id, "builder owns the trinket");

    let out = eval(
        &mut wiz,
        r#"return tostring(game.use_object("/d/workshop/trinket", "someone", "use"))"#,
    )
    .await;
    assert_eq!(out, "false", "the code ran as the builder, not the admin");
    assert_eq!(db_access_level(&server, &bld_id).await, "builder");
}

/// Only wizards+ may define classes (in their universe), and
/// built-in base classes cannot be redefined
#[tokio::test]
async fn test_define_class_restrictions() {
    let server = TestServer::start().await.expect("server");
    let mut wiz = connect(&server, wizard("class_wiz")).await;
    let pl = connect(&server, player("class_player")).await;
    let pl_id = pl.account_id().to_string();

    let out = eval(
        &mut wiz,
        r#"return tostring(game.define_class("lantern", {parent = "item", properties = {lit = {type = "boolean", default = false}}}))"#,
    )
    .await;
    assert_eq!(out, "true", "wizard may define a new class");

    let out = eval(
        &mut wiz,
        r#"return tostring(pcall(game.define_class, "weapon", {parent = "item"}))"#,
    )
    .await;
    assert_eq!(out, "false", "built-in classes cannot be redefined");

    let out = eval(
        &mut wiz,
        &format!(
            r#"game.set_actor("{}"); return tostring(pcall(game.define_class, "trap", {{parent = "item"}}))"#,
            pl_id
        ),
    )
    .await;
    assert_eq!(out, "false", "players may not define classes");
}

/// Insert a second universe ("other-world") with one non-fixed item
async fn other_universe(server: &TestServer) {
    let owner = server.world().wizard_account_id.clone();
    sqlx::query("INSERT INTO universes (id, name, owner_id) VALUES ('other-world', 'Other', ?)")
        .bind(&owner)
        .execute(server.pool())
        .await
        .expect("create universe");
    let mut gem = mudd::objects::Object::new("/d/other/gem", "other-world", "item").unwrap();
    gem.set_property("name", serde_json::json!("gem"));
    server
        .world()
        .store()
        .create(&gem)
        .await
        .expect("create gem");
}

/// Object ids are per universe: another universe's objects are invisible to
/// scripts, so they can be neither read nor changed
#[tokio::test]
async fn test_lua_cannot_see_other_universes() {
    let server = TestServer::start().await.expect("server");
    other_universe(&server).await;
    let mut wiz = connect(&server, wizard("xuni_writer")).await;

    let out = eval(
        &mut wiz,
        r#"local seen = game.get_object("/d/other/gem")
           local updated = game.update_object("/d/other/gem", {name = "mine"})
           local deleted = game.delete_object("/d/other/gem")
           local moved = pcall(game.move_object, "/d/other/gem", nil)
           local cloned = game.clone_object("/d/other/gem", "/d/here/gem", nil)
           return table.concat({tostring(seen), tostring(updated), tostring(deleted),
                                tostring(moved), tostring(cloned)}, ",")"#,
    )
    .await;
    assert_eq!(out, "nil,false,false,false,nil");

    let name: String = sqlx::query_scalar(
        "SELECT json_extract(properties, '$.name') FROM objects
         WHERE universe_id = 'other-world' AND id = '/d/other/gem'",
    )
    .fetch_one(server.pool())
    .await
    .expect("gem untouched");
    assert_eq!(name, "gem");
}

/// The same path can exist in two universes, independently
#[tokio::test]
async fn test_same_path_in_two_universes() {
    let server = TestServer::start().await.expect("server");
    other_universe(&server).await;
    let mut wiz = connect(&server, wizard("twin_paths")).await;

    let out = eval(
        &mut wiz,
        r#"local g = game.create_object("/d/other/gem", "item", nil, {name = "local gem"})
           game.update_object("/d/other/gem", {shiny = true})
           return g.universe_id .. ":" .. game.get_object("/d/other/gem").name"#,
    )
    .await;
    assert_eq!(out, format!("{}:local gem", server.universe_id()));

    let rows: Vec<(String, String)> = sqlx::query_as(
        "SELECT universe_id, json_extract(properties, '$.name') FROM objects
         WHERE id = '/d/other/gem' ORDER BY universe_id",
    )
    .fetch_all(server.pool())
    .await
    .unwrap();
    assert_eq!(rows.len(), 2, "one object per universe: {:?}", rows);
    assert!(rows.contains(&("other-world".to_string(), "gem".to_string())));
}

/// Script messages only reach players in the script's universe
#[tokio::test]
async fn test_script_messages_stay_in_universe() {
    use futures_util::StreamExt;
    let server = TestServer::start().await.expect("server");
    other_universe(&server).await;
    let mut wiz = connect(&server, wizard("xuni_sender")).await;

    // Bob's account exists, but he never connects to the main universe
    let reg: serde_json::Value = server
        .post(
            "/auth/register",
            &serde_json::json!({"username": "xuni_bob", "password": "test123"}),
        )
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let token = reg["token"].as_str().unwrap().to_string();
    let bob_pid = format!("/players/p-{}", reg["account_id"].as_str().unwrap());

    // Bob plays in the other universe only
    let url = format!(
        "ws://{}/ws?universe=other-world&token={}",
        server.addr, token
    );
    let (mut ws, _) = tokio_tungstenite::connect_async(&url).await.expect("ws");
    // Wait for the welcome (it names bob's player id), then the greeting
    let welcome = ws.next().await.unwrap().unwrap();
    assert!(
        welcome.to_text().unwrap().contains(&bob_pid),
        "player id format: {}",
        welcome
    );
    let _ = tokio::time::timeout(std::time::Duration::from_millis(300), async {
        while ws.next().await.is_some() {}
    })
    .await;

    eval(
        &mut wiz,
        &format!(
            r#"game.send("{}", "cross-universe hello"); return true"#,
            bob_pid
        ),
    )
    .await;

    let leaked = tokio::time::timeout(std::time::Duration::from_millis(600), async {
        while let Some(Ok(msg)) = ws.next().await {
            if msg
                .to_text()
                .unwrap_or_default()
                .contains("cross-universe hello")
            {
                return true;
            }
        }
        false
    })
    .await
    .unwrap_or(false);
    assert!(!leaked, "message crossed universes");
}

/// use_object cannot fan out without bound, and cannot reach private
/// (underscore) handlers or engine hooks of another owner's object
#[tokio::test]
async fn test_use_object_limits() {
    let server = TestServer::start().await.expect("server");
    let mut wiz = connect(&server, wizard("fanout_wiz")).await;

    // An object whose on_use uses itself repeatedly: without a shared
    // budget this is exponential
    let out = eval(
        &mut wiz,
        r#"local h = game.store_code([[return {
             on_use = function(a)
               -- pcall swallows nested errors, so only a shared budget stops this
               for i = 1, 10 do pcall(game.use_object, a.object_id, a.actor_id, "use") end
               return "done"
             end,
             _secret = function(a) return "leaked" end,
             on_death = function(a) return "fake death" end,
           }]])
           game.create_object("/items/hydra", "item", nil, {name = "hydra", code_hash = h})
           return tostring(game.use_object("/items/hydra", "me", "use"))"#,
    )
    .await;
    // Returning at all (instead of ~10^8 sandbox runs) shows the cap works
    assert_eq!(out, "done");

    for verb in ["_secret", "on_death"] {
        let out = eval(
            &mut wiz,
            &format!(
                r#"return tostring(game.use_object("/items/hydra", "me", "{}"))"#,
                verb
            ),
        )
        .await;
        assert_eq!(out, "nil", "{} must not be callable via use_object", verb);
    }
}

/// Class defaults never attach code to new objects (that would bypass the
/// code-attach rules), and empty default names are not copied
#[tokio::test]
async fn test_class_defaults_do_not_attach_code() {
    let server = TestServer::start().await.expect("server");
    let mut wiz = connect(&server, wizard("classdef_wiz")).await;
    let out = eval(
        &mut wiz,
        r#"local h = game.store_code("return {}")
           game.define_class("trapped_chest", {parent = "item",
             properties = {code_hash = {type = "string", default = h}, lock = {type = "string", default = "iron"}}})
           local c = game.create_object("/items/chest1", "trapped_chest", nil, {})
           return tostring(c.code_hash) .. ":" .. tostring(c.metadata.lock) .. ":" .. tostring(c.name)"#,
    )
    .await;
    assert_eq!(out, "nil:iron:nil");
}

/// A custom class may be redefined only by whoever defined it (or an admin)
#[tokio::test]
async fn test_define_class_protects_others_classes() {
    let server = TestServer::start().await.expect("server");
    let mut wiz_a = connect(&server, wizard("class_owner_a")).await;
    let mut wiz_b = connect(&server, wizard("class_owner_b")).await;
    let mut adm = connect(&server, admin("class_admin")).await;

    let def = r#"return tostring(pcall(game.define_class, "torch", {parent = "item"}))"#;
    assert_eq!(eval(&mut wiz_a, def).await, "true", "first definition");
    assert_eq!(eval(&mut wiz_a, def).await, "true", "definer may redefine");
    assert_eq!(
        eval(&mut wiz_b, def).await,
        "false",
        "another wizard may not"
    );
    assert_eq!(eval(&mut adm, def).await, "true", "an admin may");
}

/// Actions run the object's code as its owner, so registering one needs the
/// owner's (or a superior's) authority, and private handlers or engine hooks
/// are never dispatchable as verbs
#[tokio::test]
async fn test_add_action_respects_owner_and_private_handlers() {
    let server = TestServer::start().await.expect("server");
    let entrance = server.world().entrance_id.clone();
    let mut adm = connect(&server, admin("action_admin")).await;
    let mut wiz = connect(&server, wizard("action_wizard")).await;

    eval(
        &mut adm,
        &format!(
            r#"local h = game.store_code([[return {{
                 polish = function(a) return "shiny" end,
                 _wipe = function(a) return "wiped" end,
                 on_death = function(a) return "dead" end,
               }}]])
               game.create_object("/items/admin-idol", "item", "{}", {{name = "idol", code_hash = h}})
               return true"#,
            entrance
        ),
    )
    .await;

    // A wizard (who can modify the idol) must not wire verbs onto an admin's code
    let out = eval(
        &mut wiz,
        r#"return tostring(game.add_action("polish", "/items/admin-idol", "polish"))"#,
    )
    .await;
    assert_eq!(
        out, "false",
        "wizard must not add actions to an admin's object"
    );

    // Even the owner cannot expose private handlers or engine hooks
    for method in ["_wipe", "on_death"] {
        let out = eval(
            &mut adm,
            &format!(
                r#"return tostring(game.add_action("x{m}", "/items/admin-idol", "{m}"))"#,
                m = method
            ),
        )
        .await;
        assert_eq!(out, "false", "{} must not become a verb", method);
    }
    let out = eval(
        &mut adm,
        r#"return tostring(game.add_action("polish", "/items/admin-idol", "polish"))"#,
    )
    .await;
    assert_eq!(out, "true", "owner may add a normal action");
}

/// Each universe has its own player object for an account, so play (and the
/// inventory) in a second universe is independent of the first
#[tokio::test]
async fn test_inventory_is_per_universe() {
    use futures_util::{SinkExt, StreamExt};
    use tokio_tungstenite::tungstenite::Message;
    let server = TestServer::start().await.expect("server");

    // Universe "other-world" with a portal room holding a gem
    // Same config (core library) as the main universe
    sqlx::query(
        "INSERT INTO universes (id, name, owner_id, config)
         SELECT 'other-world', 'Other', owner_id, config FROM universes WHERE id = ?",
    )
    .bind(server.universe_id())
    .execute(server.pool())
    .await
    .unwrap();
    let store = server.world().store();
    let mut hall = mudd::objects::Object::new("/d/other/hall", "other-world", "room").unwrap();
    hall.set_property("name", serde_json::json!("Hall"));
    store.create(&hall).await.unwrap();
    let mut gem = mudd::objects::Object::new("/d/other/gem", "other-world", "item").unwrap();
    gem.set_property("name", serde_json::json!("gem"));
    gem.parent_id = Some("/d/other/hall".to_string());
    store.create(&gem).await.unwrap();
    store
        .set_portal("other-world", "/d/other/hall")
        .await
        .unwrap();

    let reg: serde_json::Value = server
        .post(
            "/auth/register",
            &serde_json::json!({"username": "traveler", "password": "test123"}),
        )
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let token = reg["token"].as_str().unwrap().to_string();

    // First visit the main universe (this creates the player object there)
    let url = |u: &str| format!("ws://{}/ws?universe={}&token={}", server.addr, u, token);
    let (mut first, _) = tokio_tungstenite::connect_async(url(server.universe_id()))
        .await
        .unwrap();
    first.next().await; // welcome
    first.close(None).await.ok();
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;

    // Then play in the other universe
    let (mut ws, _) = tokio_tungstenite::connect_async(url("other-world"))
        .await
        .unwrap();
    let _ = tokio::time::timeout(std::time::Duration::from_millis(300), async {
        while ws.next().await.is_some() {}
    })
    .await;
    ws.send(Message::Text(
        serde_json::json!({"type": "command", "text": "take gem"})
            .to_string()
            .into(),
    ))
    .await
    .unwrap();
    let reply = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while let Some(Ok(msg)) = ws.next().await {
            let v: serde_json::Value =
                serde_json::from_str(msg.to_text().unwrap_or("{}")).unwrap_or_default();
            if v["type"] == "output" || v["type"] == "error" {
                return v;
            }
        }
        serde_json::Value::Null
    })
    .await
    .expect("reply");
    assert_eq!(reply["text"], "You take gem.", "{}", reply);

    // Back in the first universe the pack does not hold the gem
    ws.close(None).await.ok();
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    let (mut back, _) = tokio_tungstenite::connect_async(url(server.universe_id()))
        .await
        .unwrap();
    let _ = tokio::time::timeout(std::time::Duration::from_millis(300), async {
        while back.next().await.is_some() {}
    })
    .await;
    back.send(Message::Text(
        serde_json::json!({"type": "command", "text": "drop gem"})
            .to_string()
            .into(),
    ))
    .await
    .unwrap();
    let reply = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while let Some(Ok(msg)) = back.next().await {
            let v: serde_json::Value =
                serde_json::from_str(msg.to_text().unwrap_or("{}")).unwrap_or_default();
            if v["type"] == "output" || v["type"] == "error" {
                return v;
            }
        }
        serde_json::Value::Null
    })
    .await
    .expect("reply");
    assert_eq!(reply["text"], "You don't have that", "{}", reply);

    // Two player objects, one per universe; the gem is in the other one's pack
    let owners: Vec<(String, Option<String>)> = sqlx::query_as(
        "SELECT universe_id, (SELECT parent_id FROM objects g
                               WHERE g.universe_id = p.universe_id AND g.id = '/d/other/gem')
         FROM objects p WHERE p.id = ? ORDER BY universe_id",
    )
    .bind(mudd::player::PlayerManager::player_path(
        reg["account_id"].as_str().unwrap(),
    ))
    .fetch_all(server.pool())
    .await
    .unwrap();
    assert_eq!(
        owners.len(),
        2,
        "a player object per universe: {:?}",
        owners
    );
}

/// Scripts cannot create player objects: the server makes them on connect
#[tokio::test]
async fn test_scripts_cannot_create_player_objects() {
    let server = TestServer::start().await.expect("server");
    let mut wiz = connect(&server, wizard("fake_player_wiz")).await;
    for code in [
        r#"local r = game.create_object("/d/here/fake", "player", nil, {}); return r.error and "refused" or "created""#,
        r#"local r = game.create_object("/players/p-nobody", "item", nil, {}); return r.error and "refused" or "created""#,
    ] {
        assert_eq!(eval(&mut wiz, code).await, "refused", "{}", code);
    }
}

/// Universe confinement also covers room broadcasts and the parent of new
/// objects
#[tokio::test]
async fn test_broadcast_and_create_parent_stay_in_universe() {
    use futures_util::StreamExt;
    let server = TestServer::start().await.expect("server");
    sqlx::query(
        "INSERT INTO universes (id, name, owner_id, config)
         SELECT 'other-world', 'Other', owner_id, config FROM universes WHERE id = ?",
    )
    .bind(server.universe_id())
    .execute(server.pool())
    .await
    .unwrap();
    let store = server.world().store();
    let mut plaza = mudd::objects::Object::new("/d/other/plaza", "other-world", "room").unwrap();
    plaza.set_property("name", serde_json::json!("Plaza"));
    store.create(&plaza).await.unwrap();
    store
        .set_portal("other-world", "/d/other/plaza")
        .await
        .unwrap();

    let reg: serde_json::Value = server
        .post(
            "/auth/register",
            &serde_json::json!({"username": "plaza_bob", "password": "test123"}),
        )
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let url = format!(
        "ws://{}/ws?universe=other-world&token={}",
        server.addr,
        reg["token"].as_str().unwrap()
    );
    let (mut ws, _) = tokio_tungstenite::connect_async(&url).await.unwrap();
    let _ = tokio::time::timeout(std::time::Duration::from_millis(300), async {
        while ws.next().await.is_some() {}
    })
    .await;

    let mut wiz = connect(&server, wizard("plaza_wiz")).await;
    eval(
        &mut wiz,
        r#"game.broadcast("/d/other/plaza", "cross-room spam"); return true"#,
    )
    .await;
    let leaked = tokio::time::timeout(std::time::Duration::from_millis(600), async {
        while let Some(Ok(msg)) = ws.next().await {
            if msg
                .to_text()
                .unwrap_or_default()
                .contains("cross-room spam")
            {
                return true;
            }
        }
        false
    })
    .await
    .unwrap_or(false);
    assert!(!leaked, "broadcast reached another universe's room");

    let out = eval(
        &mut wiz,
        r#"local r = game.create_object("/d/here/junk", "item", "/d/other/plaza", {name = "junk"})
           local c = game.clone_object("/d/here/junk2-src", "/d/here/junk2", "/d/other/plaza")
           return (r.error and "refused" or "created")"#,
    )
    .await;
    assert_eq!(
        out, "refused",
        "create with a foreign parent must be refused"
    );
}

/// Access levels are per universe: a wizard of the test universe is a plain
/// player elsewhere, and a universe's owner is owner inside it only
#[tokio::test]
async fn test_access_levels_are_per_universe() {
    let server = TestServer::start().await.expect("server");
    let realm_owner = connect(&server, player("realm_owner")).await;
    let owner_id = realm_owner.account_id().to_string();
    server
        .create_universe("owned-realm", &owner_id)
        .await
        .expect("second universe");

    // The test universe's wizard is a player in owned-realm
    let wiz = connect(&server, wizard("home_wiz")).await;
    let wiz_id = wiz.account_id().to_string();
    let mut wiz_away = server
        .connect_in(player("home_wiz"), "owned-realm")
        .await
        .expect("connect");
    wiz_away.drain().await;
    wiz_away.command("eval return 1").await.unwrap();
    let _ = wiz_away.expect("echo").await;
    let err = wiz_away.expect("error").await.expect("eval refused");
    assert!(err["message"].as_str().unwrap().contains("wizard+"));

    // The realm's owner is owner there and may promote the wizard there
    let mut owner_home = server
        .connect_in(player("realm_owner"), "owned-realm")
        .await
        .expect("connect");
    owner_home.drain().await;
    let level = eval(
        &mut owner_home,
        &format!(r#"return game.get_access_level("{}")"#, owner_id),
    )
    .await;
    assert_eq!(level, "owner");
    let out = eval(
        &mut owner_home,
        &format!(r#"return game.set_access_level("{}", "builder")"#, wiz_id),
    )
    .await;
    assert_eq!(out, "true");

    // ...which leaves the wizard's level in the test universe alone
    assert_eq!(db_access_level(&server, &wiz_id).await, "wizard");

    // In the test universe the realm owner is a plain player
    let mut owner_away = connect(&server, player("realm_owner")).await;
    owner_away.drain().await;
    owner_away.command("eval return 1").await.unwrap();
    let _ = owner_away.expect("echo").await;
    assert!(owner_away.expect("error").await.is_ok());
}

/// A universe's owner (an ordinary account) is owner inside it, but must not
/// gain a global admin's identity: no set_actor to them, no run_script as them
#[tokio::test]
async fn test_universe_owner_cannot_act_as_global_admin() {
    let server = TestServer::start().await.expect("server");
    let owner = connect(&server, player("realm_lord")).await;
    let owner_id = owner.account_id().to_string();
    server
        .create_universe("lord-realm", &owner_id)
        .await
        .expect("universe");
    let adm = connect(&server, admin("realm_visitor_admin")).await;
    let admin_id = adm.account_id().to_string();

    let mut lord = server
        .connect_in(player("realm_lord"), "lord-realm")
        .await
        .expect("connect");
    let out = eval(
        &mut lord,
        &format!(r#"return game.set_actor("{}")"#, admin_id),
    )
    .await;
    assert_eq!(out, "false", "owner must not impersonate a global admin");

    let resp = server
        .post_auth(
            "/universe/lord-realm/run_script",
            &serde_json::json!({"script": "no-such-script.lua", "account_id": admin_id}),
            lord.auth_token(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), 403, "{}", resp.text().await.unwrap());

    // Acting as an ordinary player in their own universe still works
    let pl = connect(&server, player("realm_serf")).await;
    let out = eval(
        &mut lord,
        &format!(r#"return game.set_actor("{}")"#, pl.account_id()),
    )
    .await;
    assert_eq!(out, "true");
}

/// A level change applies to an open session at once, without reconnecting
#[tokio::test]
async fn test_level_change_applies_to_open_session() {
    let server = TestServer::start().await.expect("server");
    let mut pl = connect(&server, player("live_promo")).await;
    let pl_id = pl.account_id().to_string();
    let mut adm = connect(&server, admin("live_promoter")).await;

    pl.drain().await;
    pl.command("eval return 1").await.unwrap();
    let _ = pl.expect("echo").await;
    assert!(pl.expect("error").await.is_ok(), "a player cannot eval");

    let out = eval(
        &mut adm,
        &format!(r#"return game.set_access_level("{}", "wizard")"#, pl_id),
    )
    .await;
    assert_eq!(out, "true");
    assert_eq!(
        eval(&mut pl, "return 6 * 7").await,
        "42",
        "promoted session evals"
    );

    let out = eval(
        &mut adm,
        &format!(r#"return game.set_access_level("{}", "player")"#, pl_id),
    )
    .await;
    assert_eq!(out, "true");
    pl.drain().await;
    pl.command("eval return 1").await.unwrap();
    let _ = pl.expect("echo").await;
    assert!(
        pl.expect("error").await.is_ok(),
        "demoted session cannot eval"
    );
}

/// POST /universe/{id}/portal sets the spawn room: universe admin+ only
/// (a universe's owner counts), and only to a room in that universe
#[tokio::test]
async fn test_portal_endpoint() {
    let server = TestServer::start().await.expect("server");
    let uid = server.universe_id().to_string();
    let mut spawn = mudd::objects::Object::new("/rooms/new-spawn", &uid, "room").unwrap();
    spawn.set_property("name", serde_json::json!("New Spawn"));
    server.world().store().create(&spawn).await.unwrap();
    let path = format!("/universe/{}/portal", uid);
    let body = serde_json::json!({"room_id": "/rooms/new-spawn"});
    let portal = || async {
        sqlx::query_scalar::<_, String>(
            "SELECT value FROM universe_settings WHERE universe_id = ? AND key = 'portal_room_id'",
        )
        .bind(server.universe_id())
        .fetch_one(server.pool())
        .await
        .unwrap()
    };
    let before = portal().await;

    let resp = reqwest::Client::new()
        .post(format!("{}{}", server.base_url(), path))
        .json(&body)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401);

    let pl = connect(&server, player("portal_peasant")).await;
    let resp = server
        .post_auth(&path, &body, pl.auth_token())
        .await
        .unwrap();
    assert_eq!(resp.status(), 403);
    assert_eq!(portal().await, before);

    let adm = connect(&server, admin("portal_admin")).await;
    for (room, status) in [
        ("/rooms/nowhere", 404),
        (server.world().bat_id.as_str(), 400),
    ] {
        let resp = server
            .post_auth(
                &path,
                &serde_json::json!({"room_id": room}),
                adm.auth_token(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), status, "{}", room);
    }
    let resp = server
        .post_auth(&path, &body, adm.auth_token())
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "{}", resp.text().await.unwrap());
    assert_eq!(portal().await, "/rooms/new-spawn");

    // A universe's owner is admin+ inside it, and only inside it
    let lord_id = pl.account_id().to_string();
    server
        .create_universe("peasant-realm", &lord_id)
        .await
        .unwrap();
    let resp = server
        .post_auth(
            "/universe/peasant-realm/portal",
            &serde_json::json!({"room_id": "/rooms/hall"}),
            pl.auth_token(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "{}", resp.text().await.unwrap());
    // The test universe's new spawn room does not exist in peasant-realm
    let resp = server
        .post_auth("/universe/peasant-realm/portal", &body, pl.auth_token())
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);
}

/// Login and registration are rate-limited before any password hashing:
/// per username (the configured rate) and per client IP (twice it)
#[tokio::test]
async fn test_auth_rate_limits() {
    let server = TestServer::start_with_args(false, &["--auth-limit", "3"])
        .await
        .expect("server");
    let login = |name: &str| {
        let body = serde_json::json!({"username": name, "password": "wrong-password"});
        let server = &server;
        async move { server.post("/auth/login", &body).await.unwrap() }
    };
    for _ in 0..3 {
        assert_eq!(login("target").await.status(), 401);
    }
    let limited = login("target").await;
    assert_eq!(limited.status(), 429, "per-username limit");
    assert!(limited.headers().contains_key("retry-after"));
    // Other names from the same client still get through, up to the IP limit
    assert_eq!(login("someone_else").await.status(), 401);
    assert_eq!(login("third_person").await.status(), 401);
    assert_eq!(login("fourth_person").await.status(), 429, "per-IP limit");
}

/// Only failed logins count: the owner logging in again and again is never
/// limited (by the per-username limits)
#[tokio::test]
async fn test_successful_logins_do_not_count() {
    let server = TestServer::start_with_args(false, &["--auth-limit", "3"])
        .await
        .expect("server");
    let creds = serde_json::json!({"username": "regular", "password": "testpass123"});
    assert_eq!(
        server
            .post("/auth/register", &creds)
            .await
            .unwrap()
            .status(),
        201
    );
    // 1 registration + 5 logins stays within the client's limit (6)
    for _ in 0..5 {
        assert_eq!(
            server.post("/auth/login", &creds).await.unwrap().status(),
            200
        );
    }
}

/// A successful login hands out a device token; presenting it renews it
/// (a new one each time), and logout with it revokes it
#[tokio::test]
async fn test_device_tokens_issued_and_renewed() {
    let server = TestServer::start_with_world(false).await.expect("server");
    let creds = serde_json::json!({"username": "devicey", "password": "testpass123"});
    let reg: serde_json::Value = server
        .post("/auth/register", &creds)
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let first = reg["device_token"]
        .as_str()
        .expect("issued at registration")
        .to_string();
    let login = serde_json::json!({"username": "devicey", "password": "testpass123", "device_token": first});
    let renewed: serde_json::Value = server
        .post("/auth/login", &login)
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let second = renewed["device_token"]
        .as_str()
        .expect("renewed")
        .to_string();
    assert_ne!(first, second);
    let out = server
        .post(
            "/auth/logout",
            &serde_json::json!({"token": renewed["token"], "device_token": second}),
        )
        .await
        .unwrap();
    assert_eq!(out.status(), 200);
}
