//! Scripting scenario tests
//!
//! Object code attached via code_hash, hooks, custom verbs and timers,
//! all running through the server's script runner.

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
