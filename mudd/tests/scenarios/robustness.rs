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

/// One account may play two universes at once: the sessions are independent
#[tokio::test]
async fn test_one_account_in_two_universes() {
    use futures_util::{SinkExt, StreamExt};
    use tokio_tungstenite::tungstenite::Message;
    let server = TestServer::start().await.expect("server");
    sqlx::query(
        "INSERT INTO universes (id, name, owner_id, config)
         SELECT 'second-world', 'Second', owner_id, config FROM universes WHERE id = ?",
    )
    .bind(server.universe_id())
    .execute(server.pool())
    .await
    .unwrap();
    let mut hall = mudd::objects::Object::new("/rooms/hall", "second-world", "room").unwrap();
    hall.set_property("name", serde_json::json!("Second Hall"));
    server.world().store().create(&hall).await.unwrap();
    server
        .world()
        .store()
        .set_portal("second-world", "/rooms/hall")
        .await
        .unwrap();

    let reg: serde_json::Value = server
        .post(
            "/auth/register",
            &serde_json::json!({"username": "two_worlds", "password": "test123"}),
        )
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let token = reg["token"].as_str().unwrap().to_string();
    let url = |u: &str| format!("ws://{}/ws?universe={}&token={}", server.addr, u, token);

    let (mut a, _) = tokio_tungstenite::connect_async(url(server.universe_id()))
        .await
        .unwrap();
    let (mut b, _) = tokio_tungstenite::connect_async(url("second-world"))
        .await
        .unwrap();

    async fn look(
        ws: &mut tokio_tungstenite::WebSocketStream<
            tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
        >,
    ) -> String {
        let _ = tokio::time::timeout(std::time::Duration::from_millis(200), async {
            while ws.next().await.is_some() {}
        })
        .await;
        ws.send(Message::Text(
            serde_json::json!({"type": "command", "text": "look"})
                .to_string()
                .into(),
        ))
        .await
        .unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            while let Some(Ok(msg)) = ws.next().await {
                let v: serde_json::Value =
                    serde_json::from_str(msg.to_text().unwrap_or("{}")).unwrap_or_default();
                if v["type"] == "room" {
                    return v["name"].as_str().unwrap_or_default().to_string();
                }
            }
            String::new()
        })
        .await
        .unwrap_or_default()
    }

    assert_eq!(look(&mut a).await, "Cave Entrance");
    assert_eq!(look(&mut b).await, "Second Hall");

    // Closing one session leaves the other working
    a.close(None).await.ok();
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    assert_eq!(look(&mut b).await, "Second Hall");
}

/// Classes defined with game.define_class are persisted and survive a restart
#[tokio::test]
async fn test_defined_class_survives_restart() {
    let mut server = TestServer::start().await.expect("server");
    let mut wiz = connect(
        &server,
        Role::Wizard {
            username: "class_keeper".into(),
        },
    )
    .await;
    let text = reply_text(
        &mut wiz,
        r#"eval return tostring(game.define_class("lantern", {parent = "item", properties = {lit = {type = "boolean", default = true}}}))"#,
    )
    .await;
    assert_eq!(text, "true");
    wiz.close().await.ok();

    server.restart().await.expect("restart");

    let mut wiz = connect(
        &server,
        Role::Wizard {
            username: "class_keeper".into(),
        },
    )
    .await;
    let text = reply_text(
        &mut wiz,
        r#"eval local o = game.create_object("/items/lamp", "lantern", nil, {}); return tostring(o.metadata.lit) .. ":" .. tostring(game.is_a("/items/lamp", "item"))"#,
    )
    .await;
    assert_eq!(text, "true:true");
}

/// Account writes (register, login, logout) are replicated through Raft
#[tokio::test]
async fn test_account_writes_go_through_raft() {
    let server = TestServer::start().await.expect("server");
    async fn account_entries(server: &TestServer) -> i64 {
        sqlx::query_scalar(
            "SELECT COUNT(*) FROM raft_log WHERE payload LIKE '%accounts SET token%'
                OR payload LIKE '%INSERT INTO accounts%'",
        )
        .fetch_one(server.pool())
        .await
        .unwrap()
    }
    let before = account_entries(&server).await;
    let creds = serde_json::json!({"username": "raft_acct", "password": "test123"});
    let reg: serde_json::Value = server
        .post("/auth/register", &creds)
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(reg["token"].is_string());
    let login: serde_json::Value = server
        .post("/auth/login", &creds)
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let token = login["token"].as_str().unwrap();
    let resp: serde_json::Value = server
        .post("/auth/logout", &serde_json::json!({ "token": token }))
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(resp["success"], true);
    assert_eq!(
        account_entries(&server).await - before,
        3,
        "register, login and logout each append one Raft entry"
    );
}

/// A second connection for the same player replaces the first, which is
/// told so (and the client then stops reconnecting) instead of silently
/// closed
#[tokio::test]
async fn test_replaced_connection_is_told() {
    let server = TestServer::start().await.expect("server");
    let role = Role::Player {
        username: "two_tabs".into(),
    };
    let mut first = server.connect_as(role.clone()).await.expect("first");
    first.drain().await;
    let mut second = server.connect_as(role).await.expect("second");
    let msg = first.expect("replaced").await.expect("replaced notice");
    assert!(msg["message"].as_str().unwrap().contains("another"));

    // The second connection keeps working
    second.drain().await;
    second.command("look").await.unwrap();
    assert!(second.expect("room").await.is_ok());
}

/// A universe can be created by uploading a zip of universe.json and libs
#[tokio::test]
async fn test_universe_upload_zip() {
    use std::io::Write;
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
    let admin_id = login["account_id"].as_str().unwrap();

    let mut buf = std::io::Cursor::new(Vec::new());
    {
        let mut zip = zip::ZipWriter::new(&mut buf);
        let opts = zip::write::SimpleFileOptions::default();
        zip.start_file("universe.json", opts).unwrap();
        zip.write_all(
            serde_json::json!({"id": "zipped-world", "name": "Zipped", "owner_id": admin_id})
                .to_string()
                .as_bytes(),
        )
        .unwrap();
        zip.start_file("lib/greet.lua", opts).unwrap();
        zip.write_all(b"return { hello = function() return 'hi' end }")
            .unwrap();
        zip.finish().unwrap();
    }

    let resp = server
        .client
        .post(format!("{}/universe/upload", server.base_url()))
        .header("Authorization", format!("Bearer {}", token))
        .body(buf.into_inner())
        .send()
        .await
        .unwrap();
    let status = resp.status();
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(status, 201, "{}", body);
    assert_eq!(body["id"], "zipped-world");
    // The uploaded lib, alongside the core mudlib
    let libs = body["libs_loaded"].as_array().unwrap();
    assert!(libs.contains(&serde_json::json!("greet")), "{}", body);
}
