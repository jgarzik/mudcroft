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
