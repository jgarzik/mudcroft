//! Chat scenario tests
//!
//! The say command: confirmation to the speaker, delivery to everyone else
//! in the same room, nothing to other rooms.

use crate::harness::{Role, TestClient, TestServer};
use std::time::Duration;

const QUIET: Duration = Duration::from_millis(400);

async fn connect(server: &TestServer, name: &str) -> TestClient {
    let mut c = server
        .connect_as(Role::Player {
            username: name.to_string(),
        })
        .await
        .expect("connect");
    c.drain().await;
    c
}

/// Speaker gets "You say: ...", others in the room get "<name> says: ..."
#[tokio::test]
async fn test_say_heard_by_others_in_room() {
    let server = TestServer::start().await.expect("Failed to start server");
    let mut alice = connect(&server, "alice_says").await;
    let mut bob = connect(&server, "bob_listens").await;

    alice.command("say hello there").await.expect("say failed");

    let reply = alice
        .expect_output_containing("You say: hello there")
        .await
        .expect("speaker confirmation");
    assert_eq!(reply, "You say: hello there");

    let heard = bob
        .expect_output_containing("alice_says says: hello there")
        .await
        .expect("listener hears it");
    assert_eq!(heard, "alice_says says: hello there");

    // The speaker does not also get the third-person version
    let rest = alice.outputs_within(QUIET).await;
    assert!(
        !rest.iter().any(|t| t.contains("alice_says says")),
        "speaker got the onlooker message: {:?}",
        rest
    );
}

/// Players in other rooms do not hear it
#[tokio::test]
async fn test_say_not_heard_in_other_rooms() {
    let server = TestServer::start().await.expect("Failed to start server");
    let mut alice = connect(&server, "alice_far").await;
    let mut carol = connect(&server, "carol_away").await;

    // Move carol out of the entrance
    let exits = server
        .world()
        .store()
        .get(&server.world().entrance_id)
        .await
        .unwrap()
        .unwrap()
        .properties["exits"]
        .as_object()
        .unwrap()
        .keys()
        .next()
        .cloned()
        .expect("entrance has an exit");
    carol.command(&exits).await.unwrap();
    carol.expect("room").await.expect("carol moved");
    carol.drain().await;

    alice.command("say secret").await.unwrap();
    alice
        .expect_output_containing("You say: secret")
        .await
        .unwrap();

    let carol_out = carol.outputs_within(QUIET).await;
    assert!(
        !carol_out.iter().any(|t| t.contains("secret")),
        "player in another room heard it: {:?}",
        carol_out
    );
}

/// Saying nothing asks what to say
#[tokio::test]
async fn test_say_empty() {
    let server = TestServer::start().await.expect("Failed to start server");
    let mut alice = connect(&server, "alice_quiet").await;

    alice.command("say").await.expect("say failed");
    let _ = alice.expect("echo").await;
    let msg = alice.expect("error").await.expect("error reply");
    assert_eq!(msg["message"], "Say what?");
}
