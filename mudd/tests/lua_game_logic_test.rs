//! Integration tests for Lua game logic
//!
//! Runs the user-story suite in `tests/lua/game_logic_tests.lua` (regions,
//! fixed objects, custom weapon class, spawner chest, combat, permissions,
//! class inheritance) through a wizard's `eval` against a real server.
//!
//! Run from mudd/:
//!
//! ```text
//! cargo test --release --test lua_game_logic_test
//! ```
//!
//! The suite is run one SECTION per eval to stay inside the eval budget.
//! Real player and builder accounts are created here and their ids passed
//! to the suite in an `ACCOUNTS` table, since access levels come from the
//! accounts table and `game.set_actor` needs a real account.

mod harness;

use std::time::Duration;

use harness::{Role, TestClient, TestServer};

/// The Lua suite, embedded at compile time
const SUITE: &str = include_str!("lua/game_logic_tests.lua");

/// Quote a value for embedding in Lua source. Only ids and section names
/// are embedded, so anything beyond a conservative charset is a bug.
fn lua_str(value: &str) -> String {
    assert!(
        value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '/')),
        "unexpected characters in {:?}",
        value
    );
    format!("\"{}\"", value)
}

/// Run the suite with `prelude` and `SECTION = section`; returns the report
async fn run_section(wizard: &mut TestClient, prelude: &str, section: &str) -> String {
    let code = format!("{}local SECTION = {}\n{}", prelude, lua_str(section), SUITE);
    wizard.drain().await;
    wizard
        .command(&format!("eval {}", code))
        .await
        .expect("send eval");
    let _ = wizard.expect("echo").await;

    // Skip unrelated output (e.g. room broadcasts) until the suite's reply
    let deadline = tokio::time::Instant::now() + Duration::from_secs(60);
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        let reply = wizard
            .expect_any_timeout(remaining)
            .await
            .unwrap_or_else(|e| panic!("no reply for section {}: {}", section, e));
        match reply["type"].as_str() {
            Some("error") => panic!("Lua error in section {}: {}", section, reply["message"]),
            Some("output") => {
                let text = reply["text"].as_str().unwrap_or_default();
                if text.starts_with("LUA-SUITE") {
                    return text.to_string();
                }
            }
            _ => {}
        }
    }
}

/// Parse `key=<number>` out of a report line
fn field(line: &str, key: &str) -> usize {
    line.split_whitespace()
        .find_map(|w| w.strip_prefix(&format!("{}=", key)))
        .and_then(|v| v.parse().ok())
        .unwrap_or_else(|| panic!("missing {} in {:?}", key, line))
}

/// Run every Lua game-logic test; fails if any Lua test fails
#[tokio::test]
async fn test_lua_game_logic() {
    let server = TestServer::start().await.expect("Failed to start server");

    let mut wizard = server
        .connect_as(Role::Wizard {
            username: "luatester".to_string(),
        })
        .await
        .expect("Failed to connect as wizard");
    // Kept connected for the whole run: the player's body is its player object
    let player = server
        .connect_as(Role::Player {
            username: "luaplayer".to_string(),
        })
        .await
        .expect("Failed to connect as player");
    let builder = server
        .connect_as(Role::Builder {
            username: "luabuilder".to_string(),
            regions: vec![],
        })
        .await
        .expect("Failed to connect as builder");

    let prelude = format!(
        "local ACCOUNTS = {{ wizard = {}, player = {}, builder = {}, player_body = {} }}\n",
        lua_str(wizard.account_id()),
        lua_str(player.account_id()),
        lua_str(builder.account_id()),
        lua_str(player.player_id().expect("player body")),
    );

    // Section names and the total test count come from the suite itself
    let listing = run_section(&mut wizard, &prelude, "list").await;
    let sections: Vec<String> = listing
        .split_whitespace()
        .nth(1)
        .expect("section list")
        .split(',')
        .map(str::to_string)
        .collect();
    let total = field(&listing, "total");

    let (mut passed, mut failed, mut skipped) = (0, 0, 0);
    let mut failures = Vec::new();
    let mut skips = Vec::new();
    for section in &sections {
        let report = run_section(&mut wizard, &prelude, section).await;
        println!("{}", report);
        for line in report.lines() {
            if line.starts_with("FAIL ") {
                failures.push(line.to_string());
            } else if line.starts_with("SKIP ") {
                skips.push(line.to_string());
            } else if line.starts_with("RESULT ") {
                passed += field(line, "passed");
                failed += field(line, "failed");
                skipped += field(line, "skipped");
            }
        }
    }

    println!(
        "\nLua game logic: {} passed, {} failed, {} skipped (of {})",
        passed, failed, skipped, total
    );
    for skip in &skips {
        println!("  {}", skip);
    }
    assert!(
        failures.is_empty() && failed == 0,
        "{} Lua test(s) failed:\n{}",
        failed,
        failures.join("\n")
    );
    assert_eq!(
        passed + skipped,
        total,
        "every Lua test must run in exactly one section"
    );
}

/// Test that game.set_actor works correctly with permissions
#[tokio::test]
async fn test_lua_set_actor_permissions() {
    let server = TestServer::start().await.expect("Failed to start server");
    // Note: TestWorld::create() now sets up the "default" universe with a portal

    // Connect as wizard
    let mut wizard = server
        .connect_as(Role::Wizard {
            username: "permtester".to_string(),
        })
        .await
        .expect("Failed to connect as wizard");

    // Test that set_actor affects get_actor
    let test_code = r#"
game.set_actor("test_actor_id")
local actor = game.get_actor()
if actor == "test_actor_id" then
    return "PASS: Actor was set correctly"
else
    return "FAIL: Actor was " .. tostring(actor)
end
"#;

    wizard
        .command(&format!("eval {}", test_code))
        .await
        .expect("Failed to send eval command");

    let _ = wizard.expect("echo").await;
    let output = wizard.expect_any().await.expect("Should receive response");

    if output["type"] == "error" {
        panic!("Lua error: {}", output["message"]);
    }

    let text = output["text"].as_str().expect("Should have text");
    assert!(text.contains("PASS"), "Expected PASS, got: {}", text);
}
