//! Robustness scenario tests
//!
//! Input handling and state consistency bugs found during the documentation
//! audit, each pinned by a test.

use crate::harness::{Role, TestClient, TestServer};

async fn connect(server: &TestServer, role: Role) -> TestClient {
    let mut c = server.connect_as(role).await.expect("connect");
    c.drain().await;
    c
}

async fn reply_text(c: &mut TestClient, cmd: &str) -> String {
    c.command(cmd).await.unwrap();
    let _ = c.expect("echo").await;
    let r = c.expect_any().await.unwrap();
    r["text"]
        .as_str()
        .or(r["message"].as_str())
        .unwrap_or_default()
        .to_string()
}

async fn parent_of(server: &TestServer, id: &str) -> Option<String> {
    sqlx::query_scalar("SELECT parent_id FROM objects WHERE id = ?")
        .bind(id)
        .fetch_one(server.pool())
        .await
        .expect("object row")
}

/// goto moves the player object too, not just the session's room
#[tokio::test]
async fn test_goto_moves_player_object() {
    let server = TestServer::start().await.expect("server");
    let chamber = server.world().chamber_id.clone();
    let mut wiz = connect(
        &server,
        Role::Wizard {
            username: "goto_wiz".into(),
        },
    )
    .await;
    let pid = wiz.player_id().unwrap().to_string();

    wiz.command(&format!("goto {}", chamber)).await.unwrap();
    wiz.expect("room").await.expect("arrived");
    assert_eq!(
        parent_of(&server, &pid).await.as_deref(),
        Some(chamber.as_str())
    );
}

/// Living things (NPCs, players) cannot be picked up
#[tokio::test]
async fn test_cannot_take_living_things() {
    let server = TestServer::start().await.expect("server");
    let mut pl = connect(
        &server,
        Role::Player {
            username: "bat_grabber".into(),
        },
    )
    .await;
    pl.command("north").await.unwrap();
    pl.expect("room").await.unwrap();
    pl.drain().await;

    let text = reply_text(&mut pl, "take bat").await;
    assert_eq!(text, "You can't take that", "{}", text);
    let bat = server.world().bat_id.clone();
    let passage = server.world().passage_id.clone();
    assert_eq!(
        parent_of(&server, &bat).await.as_deref(),
        Some(passage.as_str())
    );
}

/// Item names are matched literally: SQL LIKE wildcards in a name match
/// nothing special
#[tokio::test]
async fn test_item_names_match_literally() {
    let server = TestServer::start().await.expect("server");
    let mut pl = connect(
        &server,
        Role::Player {
            username: "wildcard_taker".into(),
        },
    )
    .await;
    // (a bare "_" would legitimately match a name containing an underscore)
    for pattern in ["%", "s_ord", "r%y"] {
        let text = reply_text(&mut pl, &format!("take {}", pattern)).await;
        assert_eq!(text, "You don't see that here", "pattern {:?}", pattern);
    }
}

/// Registration rejects unusable usernames and passwords
#[tokio::test]
async fn test_register_validates_credentials() {
    let server = TestServer::start_with_world(false).await.expect("server");
    for (user, pass) in [
        ("", ""),
        ("ab", "secret1"),
        ("has space", "secret1"),
        ("fine_user", "12345"),
    ] {
        let resp = server
            .post(
                "/auth/register",
                &serde_json::json!({"username": user, "password": pass}),
            )
            .await
            .unwrap();
        assert_eq!(
            resp.status(),
            400,
            "({:?}, {:?}) must be rejected",
            user,
            pass
        );
    }
    let resp = server
        .post(
            "/auth/register",
            &serde_json::json!({"username": "fine_user", "password": "123456"}),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), 201);
}

/// run_script accepts a universe id in any case, like /ws and create
#[tokio::test]
async fn test_run_script_universe_id_case_insensitive() {
    let server = TestServer::start().await.expect("server");
    let login: serde_json::Value = server
        .post(
            "/auth/login",
            &serde_json::json!({"username": "test_admin", "password": "testpass123"}),
        )
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let token = login["token"].as_str().unwrap();
    let upper = server.universe_id().to_uppercase();
    let resp = server
        .post_auth(
            &format!("/universe/{}/run_script", upper),
            &serde_json::json!({"script": "no-such-script.lua"}),
            token,
        )
        .await
        .unwrap();
    let body = resp.text().await.unwrap();
    assert!(
        body.contains("Script not found"),
        "universe should be found: {}",
        body
    );
}

/// A database whose only administrator is an owner still starts
#[tokio::test]
async fn test_server_starts_with_owner_but_no_admin() {
    let mut server = TestServer::start_with_world(false).await.expect("server");
    sqlx::query("UPDATE accounts SET access_level = 'owner' WHERE access_level = 'admin'")
        .execute(server.pool())
        .await
        .unwrap();
    server
        .restart()
        .await
        .expect("restart with an owner account");
}

/// get_living_in includes subclasses of npc and player
#[tokio::test]
async fn test_get_living_in_includes_subclasses() {
    let server = TestServer::start().await.expect("server");
    let entrance = server.world().entrance_id.clone();
    let mut wiz = connect(
        &server,
        Role::Wizard {
            username: "living_wiz".into(),
        },
    )
    .await;
    let text = reply_text(
        &mut wiz,
        &format!(
            r#"eval game.define_class("hound", {{parent = "npc"}})
               game.create_object("/npcs/hound", "hound", "{room}", {{name = "Hound"}})
               local names = {{}}
               for _, o in ipairs(game.get_living_in("{room}")) do names[#names + 1] = o.name end
               return table.concat(names, ",")"#,
            room = entrance
        ),
    )
    .await;
    assert!(text.contains("Hound"), "living list: {}", text);
}

/// A verb handler that returns "" (or nil) sends no reply line
#[tokio::test]
async fn test_verb_empty_result_is_silent() {
    let server = TestServer::start().await.expect("server");
    let entrance = server.world().entrance_id.clone();
    let mut wiz = connect(
        &server,
        Role::Wizard {
            username: "silent_wiz".into(),
        },
    )
    .await;
    reply_text(
        &mut wiz,
        &format!(
            r#"eval local h = game.store_code([[return {{ hum = function(a) game.send(a.actor_id, "hmm"); return "" end }}]])
               game.create_object("/items/drone", "item", "{}", {{name = "drone", code_hash = h}})
               return game.add_action("hum", "/items/drone", "hum")"#,
            entrance
        ),
    )
    .await;

    wiz.command("hum").await.unwrap();
    let _ = wiz.expect("echo").await;
    let first = wiz.expect_any().await.unwrap();
    assert_eq!(first["text"], "hmm");
    // The very next message must be the echo of our next command
    wiz.command("look").await.unwrap();
    let next = wiz.expect_any().await.unwrap();
    assert_eq!(next["type"], "echo", "no blank output in between: {}", next);
}

/// Unauthenticated run_script calls get 401 before any input validation
#[tokio::test]
async fn test_run_script_requires_auth_first() {
    let server = TestServer::start_with_world(false).await.expect("server");
    let resp = server
        .post(
            "/universe/BAD!!/run_script",
            &serde_json::json!({"script": "x.lua"}),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), 401);
}
