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
