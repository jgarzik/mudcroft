//! Credit scenario tests: who pays, shared budgets, and refusals

use crate::harness::{Role, TestClient, TestServer};

async fn connect(server: &TestServer, role: Role) -> TestClient {
    let mut c = server.connect_as(role).await.expect("connect");
    c.drain().await;
    c
}

async fn eval(c: &mut TestClient, code: &str) -> serde_json::Value {
    c.drain().await;
    c.command(&format!("eval {}", code)).await.unwrap();
    let _ = c.expect("echo").await;
    c.expect_any().await.expect("eval reply")
}

async fn balance(server: &TestServer, kind: &str, id: &str) -> i64 {
    sqlx::query_scalar(
        "SELECT COALESCE((SELECT balance FROM credits
          WHERE universe_id = ? AND holder_kind = ? AND holder_id = ?), 0)",
    )
    .bind(server.universe_id())
    .bind(kind)
    .bind(id)
    .fetch_one(server.pool())
    .await
    .unwrap()
}

/// Run a verb as `pl` and return the reply text
async fn verb(pl: &mut TestClient, command: &str) -> String {
    pl.drain().await;
    pl.command(command).await.unwrap();
    let _ = pl.expect("echo").await;
    let reply = pl.expect_any().await.expect("verb reply");
    reply["text"]
        .as_str()
        .or(reply["message"].as_str())
        .unwrap_or_default()
        .to_string()
}

/// A shared budget on an object: players donate to it with a verb, and a
/// verb spends from it; a verb's default payer is the player who typed it
#[tokio::test]
async fn test_object_budget_and_default_payer() {
    let server = TestServer::start().await.expect("server");
    let entrance = server.world().entrance_id.clone();
    let mut adm = connect(
        &server,
        Role::Admin {
            username: "bank_admin".into(),
        },
    )
    .await;
    let pl = connect(
        &server,
        Role::Player {
            username: "patron".into(),
        },
    )
    .await;
    let pl_id = pl.account_id().to_string();
    let mut pl = pl;

    let out = eval(
        &mut adm,
        &format!(
            r#"game.admin_grant_credits("{p}", 20)
               local h = game.store_code([[return {{
                 donate = function(a) return tostring(game.transfer_credits("actor", "object", 5)) end,
                 repair = function(a) return tostring(game.deduct_credits(3, "repairs", "object")) end,
                 buy = function(a) return tostring(game.deduct_credits(2, "lamp oil")) end,
                 balances = function(a) return game.get_credits() .. "/" .. game.object_credits() end,
               }}]])
               game.create_object("/items/chest", "item", "{room}", {{name = "chest", fixed = true, code_hash = h}})
               for _, v in ipairs({{"donate", "repair", "buy", "balances"}}) do
                 game.add_action(v, "/items/chest", v)
               end
               return "ok""#,
            p = pl_id,
            room = entrance
        ),
    )
    .await;
    assert_eq!(out["text"], "ok", "{}", out);

    assert_eq!(verb(&mut pl, "donate chest").await, "true");
    assert_eq!(verb(&mut pl, "donate chest").await, "true");
    assert_eq!(balance(&server, "object", "/items/chest").await, 10);
    assert_eq!(verb(&mut pl, "repair chest").await, "true");
    assert_eq!(balance(&server, "object", "/items/chest").await, 7);
    // The default payer is the player who typed the verb, not the owner
    assert_eq!(verb(&mut pl, "buy chest").await, "true");
    assert_eq!(balance(&server, "account", &pl_id).await, 20 - 10 - 2);
    assert_eq!(verb(&mut pl, "balances chest").await, "8/7");
}

/// Passive hooks (walking into a room) cannot bill the player, and no code
/// can spend from someone else's account
#[tokio::test]
async fn test_hooks_and_strangers_cannot_spend_players_credits() {
    let server = TestServer::start().await.expect("server");
    let mut adm = connect(
        &server,
        Role::Admin {
            username: "toll_admin".into(),
        },
    )
    .await;
    let victim = connect(
        &server,
        Role::Player {
            username: "traveller".into(),
        },
    )
    .await;
    let victim_id = victim.account_id().to_string();
    let mut victim = victim;
    eval(
        &mut adm,
        &format!(
            r#"game.admin_grant_credits("{v}", 50)
               local h = game.store_code([[return {{
                 on_enter = function(a)
                   local ok = pcall(game.deduct_credits, 10, "toll", "actor")
                   game.broadcast(a.room_id, "toll:" .. tostring(ok))
                 end,
               }}]])
               game.create_object("/rooms/tollgate", "room", nil, {{name = "Tollgate", code_hash = h}})
               return "ok""#,
            v = victim_id
        ),
    )
    .await;
    eval(
        &mut adm,
        &format!(
            r#"return tostring(game.move_object("{}", "/rooms/tollgate"))"#,
            victim.player_id().unwrap()
        ),
    )
    .await;
    victim
        .expect_output_containing("toll:false")
        .await
        .expect("the hook was refused");
    assert_eq!(balance(&server, "account", &victim_id).await, 50);

    // Spending from an arbitrary account is not a thing: payers are only
    // "actor" or "object"
    let out = eval(
        &mut adm,
        &format!(
            r#"local ok = pcall(game.transfer_credits, "{v}", "/rooms/tollgate", 5)
               return tostring(ok)"#,
            v = victim_id
        ),
    )
    .await;
    assert_eq!(out["text"], "false", "{}", out);
    assert_eq!(balance(&server, "account", &victim_id).await, 50);
}

/// Object code that a verb reaches through game.use_object does not inherit
/// the player's wallet: only the verb's own handler may bill them
#[tokio::test]
async fn test_nested_use_object_cannot_bill_the_player() {
    let server = TestServer::start().await.expect("server");
    let entrance = server.world().entrance_id.clone();
    let mut adm = connect(
        &server,
        Role::Admin {
            username: "nest_admin".into(),
        },
    )
    .await;
    let pl = connect(
        &server,
        Role::Player {
            username: "nest_patron".into(),
        },
    )
    .await;
    let pl_id = pl.account_id().to_string();
    let mut pl = pl;
    let out = eval(
        &mut adm,
        &format!(
            r#"game.admin_grant_credits("{p}", 30)
               local thief = game.store_code([[return {{
                 on_use = function(a) return tostring(pcall(game.deduct_credits, 30, "skim")) end,
               }}]])
               game.create_object("/items/skimmer", "item", "{room}", {{name = "skimmer", fixed = true, code_hash = thief}})
               local front = game.store_code([[return {{
                 press = function(a) return game.use_object("/items/skimmer", a.actor_id, "use") end,
               }}]])
               game.create_object("/items/button", "item", "{room}", {{name = "button", fixed = true, code_hash = front}})
               game.add_action("press", "/items/button", "press")
               return "ok""#,
            p = pl_id,
            room = entrance
        ),
    )
    .await;
    assert_eq!(out["text"], "ok", "{}", out);
    assert_eq!(verb(&mut pl, "press button").await, "false");
    assert_eq!(balance(&server, "account", &pl_id).await, 30);
}

/// Venice calls cost what the universe config says, charged to the payer
/// before the call and refunded when it fails (here: no API key in tests)
#[tokio::test]
async fn test_venice_calls_charge_the_payer() {
    let server = TestServer::start().await.expect("server");
    let entrance = server.world().entrance_id.clone();
    let mut adm = connect(
        &server,
        Role::Admin {
            username: "oracle_admin".into(),
        },
    )
    .await;
    let pl = connect(
        &server,
        Role::Player {
            username: "seeker".into(),
        },
    )
    .await;
    let pl_id = pl.account_id().to_string();
    let mut pl = pl;
    let out = eval(
        &mut adm,
        &format!(
            r#"game.update_universe({{venice_chat_cost = 5, venice_image_cost = 8}})
               game.admin_grant_credits("{p}", 7)
               local h = game.store_code([[
                 local function show(r) return type(r) == "table" and r.error or tostring(r) end
                 local msgs = {{{{role = "user", content = "hi"}}}}
                 return {{
                   ask = function(a) return show(game.llm_chat(msgs)) end,
                   paint = function(a) return show(game.llm_image("a cave")) end,
                   ask_house = function(a) return show(game.llm_chat(msgs, nil, {{payer = "object"}})) end,
                 }}]])
               game.create_object("/items/oracle", "item", "{room}", {{name = "oracle", fixed = true, code_hash = h}})
               for _, v in ipairs({{"ask", "paint", "ask_house"}}) do
                 game.add_action(v, "/items/oracle", v)
               end
               return "ok""#,
            p = pl_id,
            room = entrance
        ),
    )
    .await;
    assert_eq!(out["text"], "ok", "{}", out);

    // Affordable: charged, then refunded because the call failed
    let reply = verb(&mut pl, "ask oracle").await;
    assert!(reply.contains("not configured"), "{}", reply);
    assert_eq!(balance(&server, "account", &pl_id).await, 7);
    // Too dear: refused before calling
    let reply = verb(&mut pl, "paint oracle").await;
    assert!(reply.contains("not enough credits"), "{}", reply);
    assert_eq!(balance(&server, "account", &pl_id).await, 7);
    // The object pays when asked to, and it has nothing
    let reply = verb(&mut pl, "ask_house oracle").await;
    assert!(reply.contains("not enough credits"), "{}", reply);
}

/// A verb bills the player only when they clearly picked the object: the
/// only one offering the verb, or named exactly / unambiguously. Another
/// builder's look-alike object cannot capture the verb and the player's
/// wallet.
#[tokio::test]
async fn test_ambiguous_verb_target_gets_no_wallet() {
    let server = TestServer::start().await.expect("server");
    let entrance = server.world().entrance_id.clone();
    let mut adm = connect(
        &server,
        Role::Admin {
            username: "verb_admin".into(),
        },
    )
    .await;
    let pl = connect(
        &server,
        Role::Player {
            username: "careful".into(),
        },
    )
    .await;
    let pl_id = pl.account_id().to_string();
    let mut pl = pl;
    let out = eval(
        &mut adm,
        &format!(
            r#"game.admin_grant_credits("{p}", 50)
               local h = game.store_code([[return {{
                 donate = function(a)
                   local ok, r = pcall(game.transfer_credits, "actor", "object", 5)
                   return ok and tostring(r) or "no wallet"
                 end,
               }}]])
               -- The planted one is listed first and its name is in almost anything
               game.create_object("/items/c", "item", "{room}", {{name = "c", fixed = true, code_hash = h}})
               game.create_object("/items/chest", "item", "{room}", {{name = "chest", fixed = true, code_hash = h}})
               game.add_action("donate", "/items/c", "donate")
               game.add_action("donate", "/items/chest", "donate")
               return "ok""#,
            p = pl_id,
            room = entrance
        ),
    )
    .await;
    assert_eq!(out["text"], "ok", "{}", out);

    assert_eq!(verb(&mut pl, "donate chest").await, "true");
    assert_eq!(verb(&mut pl, "donate ches").await, "no wallet");
    assert_eq!(verb(&mut pl, "donate").await, "no wallet");
    assert_eq!(balance(&server, "object", "/items/chest").await, 5);
    assert_eq!(balance(&server, "object", "/items/c").await, 0);
    assert_eq!(balance(&server, "account", &pl_id).await, 45);
}
