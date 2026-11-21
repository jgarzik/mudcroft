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

async fn db_access_level(server: &TestServer, account_id: &str) -> String {
    sqlx::query_scalar::<_, String>("SELECT access_level FROM accounts WHERE id = ?")
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
    let wiz_id = wiz.account_id().unwrap().to_string();

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
    let peer_id = peer.account_id().unwrap().to_string();
    let pl_id = pl.account_id().unwrap().to_string();

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
    let pl_id = pl.account_id().unwrap().to_string();

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
    let adm_id = adm.account_id().unwrap().to_string();
    let pl_id = pl.account_id().unwrap().to_string();

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
    let pl_id = pl.account_id().unwrap().to_string();

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
    let wiz_id = wiz.account_id().unwrap().to_string();

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
    let pl_id = pl.account_id().unwrap().to_string();

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
    let bld_id = bld.account_id().unwrap().to_string();

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

/// Classes are shared by every universe: only wizards+ may define them, and
/// built-in base classes cannot be redefined
#[tokio::test]
async fn test_define_class_restrictions() {
    let server = TestServer::start().await.expect("server");
    let mut wiz = connect(&server, wizard("class_wiz")).await;
    let pl = connect(&server, player("class_player")).await;
    let pl_id = pl.account_id().unwrap().to_string();

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

/// Scripts cannot modify, move, delete or clone objects of another universe
#[tokio::test]
async fn test_lua_writes_stay_in_universe() {
    let server = TestServer::start().await.expect("server");
    other_universe(&server).await;
    let mut wiz = connect(&server, wizard("xuni_writer")).await;

    for code in [
        r#"return tostring(pcall(game.update_object, "/d/other/gem", {name = "mine"}))"#,
        r#"return tostring(pcall(game.move_object, "/d/other/gem", nil))"#,
        r#"return tostring(pcall(game.delete_object, "/d/other/gem"))"#,
    ] {
        assert_eq!(eval(&mut wiz, code).await, "false", "{}", code);
    }
    let out = eval(
        &mut wiz,
        r#"local c = game.clone_object("/d/other/gem", "/d/here/gem", nil)
           return c == nil and "none" or (c.error and "error" or c.universe_id)"#,
    )
    .await;
    assert_ne!(
        out, "other-world",
        "clone must not land in the other universe"
    );
    assert_ne!(
        out,
        server.universe_id(),
        "nor copy another universe's object"
    );
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
