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
