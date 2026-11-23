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

/// The next confirm prompt the player gets: (id, text)
async fn prompt(pl: &mut TestClient) -> (String, String) {
    let msg = pl.expect("confirm").await.expect("a confirm prompt");
    (
        msg["id"].as_str().unwrap().to_string(),
        msg["text"].as_str().unwrap_or_default().to_string(),
    )
}

/// Answer a prompt the way the web client does
async fn answer(pl: &mut TestClient, id: &str, accept: bool) -> String {
    pl.send_json(&serde_json::json!({"type": "confirm", "id": id, "accept": accept}))
        .await
        .unwrap();
    let reply = pl.expect_any().await.expect("answer reply");
    reply["text"]
        .as_str()
        .or(reply["message"].as_str())
        .unwrap_or_default()
        .to_string()
}

async fn setup_chest(server: &TestServer, adm: &mut TestClient, player_account: &str) {
    let out = eval(
        adm,
        &format!(
            r#"game.admin_grant_credits("{p}", 20)
               local h = game.store_code([[return {{
                 donate = function(a)
                   game.request_payment(5, "a donation to the chest", "donated", "thanks")
                   return nil
                 end,
                 donated = function(a) return "Thanks for " .. a.amount .. " (" .. a.data .. ")" end,
                 repair = function(a) return tostring(game.deduct_credits(3, "repairs", "object")) end,
                 buy = function(a) return tostring(pcall(game.deduct_credits, 2, "lamp oil")) end,
                 tip = function(a) return tostring(pcall(game.transfer_credits, "actor", "object", 1)) end,
               }}]])
               game.create_object("/items/chest", "item", "{room}", {{name = "chest", fixed = true, code_hash = h}})
               for _, v in ipairs({{"donate", "repair", "buy", "tip"}}) do
                 game.add_action(v, "/items/chest", v)
               end
               return "ok""#,
            p = player_account,
            room = server.world().entrance_id
        ),
    )
    .await;
    assert_eq!(out["text"], "ok", "{}", out);
}

/// A player pays only after confirming: request_payment prompts them, and
/// on acceptance the credits move to the object and its handler runs;
/// declining (by message or by typing no) changes nothing
#[tokio::test]
async fn test_payment_confirmed_or_declined() {
    let server = TestServer::start().await.expect("server");
    let mut adm = connect(
        &server,
        Role::Admin {
            username: "bank_admin".into(),
        },
    )
    .await;
    let mut pl = connect(
        &server,
        Role::Player {
            username: "patron".into(),
        },
    )
    .await;
    let pl_id = pl.account_id().to_string();
    setup_chest(&server, &mut adm, &pl_id).await;

    pl.drain().await;
    pl.command("donate chest").await.unwrap();
    let (id, text) = prompt(&mut pl).await;
    assert!(text.contains("5") && text.contains("donation"), "{}", text);
    assert_eq!(answer(&mut pl, &id, true).await, "Thanks for 5 (thanks)");
    assert_eq!(balance(&server, "account", &pl_id).await, 15);
    assert_eq!(balance(&server, "object", "/items/chest").await, 5);

    pl.command("donate chest").await.unwrap();
    let (id, _) = prompt(&mut pl).await;
    assert!(answer(&mut pl, &id, false).await.contains("decline"));
    // Typing yes/no answers the latest prompt
    pl.command("donate chest").await.unwrap();
    prompt(&mut pl).await;
    pl.command("yes").await.unwrap();
    pl.expect_output_containing("Thanks for 5")
        .await
        .expect("paid");
    assert_eq!(balance(&server, "account", &pl_id).await, 10);
    assert_eq!(balance(&server, "object", "/items/chest").await, 10);

    // The object spends its own money freely
    assert_eq!(verb(&mut pl, "repair chest").await, "true");
    assert_eq!(balance(&server, "object", "/items/chest").await, 7);

    // An answered request cannot be accepted again
    assert!(answer(&mut pl, &id, true).await.contains("no such"));
    assert_eq!(balance(&server, "account", &pl_id).await, 10);
}

/// Object code can no longer take a player's credits directly; an expired
/// request cannot be accepted
#[tokio::test]
async fn test_no_spending_without_consent() {
    let server = TestServer::start().await.expect("server");
    let mut adm = connect(
        &server,
        Role::Admin {
            username: "consent_admin".into(),
        },
    )
    .await;
    let mut pl = connect(
        &server,
        Role::Player {
            username: "careful_patron".into(),
        },
    )
    .await;
    let pl_id = pl.account_id().to_string();
    setup_chest(&server, &mut adm, &pl_id).await;
    assert_eq!(verb(&mut pl, "buy chest").await, "false");
    assert_eq!(verb(&mut pl, "tip chest").await, "false");
    assert_eq!(balance(&server, "account", &pl_id).await, 20);

    pl.command("donate chest").await.unwrap();
    let (id, _) = prompt(&mut pl).await;
    sqlx::query("UPDATE payment_requests SET expires_at = 0 WHERE id = ?")
        .bind(&id)
        .execute(server.pool())
        .await
        .unwrap();
    assert!(answer(&mut pl, &id, true).await.contains("no such"));
    assert_eq!(balance(&server, "account", &pl_id).await, 20);
}

/// Passive code (walking into a room) cannot even ask a player to pay, and
/// no code can spend from an arbitrary account
#[tokio::test]
async fn test_hooks_and_strangers_cannot_bill_players() {
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
                   local ok = pcall(game.request_payment, 10, "toll", "paid")
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

/// Object code reached through game.use_object cannot ask the player to pay
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
                 on_use = function(a) return tostring(pcall(game.request_payment, 30, "skim", "x")) end,
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

/// Venice calls cost what the universe config says, paid by the object
/// (players pay objects through confirmed payments); refunded on failure
/// (here: no API key in tests)
#[tokio::test]
async fn test_venice_calls_charge_the_object() {
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
                   ask_house = function(a) return show(game.llm_chat(msgs, nil, {{payer = "object"}})) end,
                   paint_house = function(a) return show(game.llm_image("a cave", nil, nil, {{payer = "object"}})) end,
                 }}]])
               game.create_object("/items/oracle", "item", "{room}", {{name = "oracle", fixed = true, code_hash = h}})
               for _, v in ipairs({{"ask", "ask_house", "paint_house"}}) do
                 game.add_action(v, "/items/oracle", v)
               end
               return "ok""#,
            p = pl_id,
            room = entrance
        ),
    )
    .await;
    assert_eq!(out["text"], "ok", "{}", out);

    // A paid call never bills the player directly
    let reply = verb(&mut pl, "ask oracle").await;
    assert!(reply.contains("request_payment"), "{}", reply);
    assert_eq!(balance(&server, "account", &pl_id).await, 7);
    // The object pays: refused while it has nothing
    let reply = verb(&mut pl, "ask_house oracle").await;
    assert!(reply.contains("not enough credits"), "{}", reply);
    eval(
        &mut adm,
        r#"return tostring(game.admin_grant_credits("/items/oracle", 6))"#,
    )
    .await;
    // Affordable: charged, then refunded because the call failed
    let reply = verb(&mut pl, "ask_house oracle").await;
    assert!(reply.contains("not configured"), "{}", reply);
    assert_eq!(balance(&server, "object", "/items/oracle").await, 6);
    // Too dear
    let reply = verb(&mut pl, "paint_house oracle").await;
    assert!(reply.contains("not enough credits"), "{}", reply);
}

/// A verb bills (prompts) the player only when they clearly picked the
/// object: a look-alike object cannot capture the verb and ask for money
#[tokio::test]
async fn test_ambiguous_verb_target_cannot_ask_for_payment() {
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
                   local ok = pcall(game.request_payment, 5, "a donation", "thanks")
                   if ok then return nil end
                   return "no wallet"
                 end,
                 thanks = function(a) return "thanked" end,
               }}]])
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
    assert_eq!(verb(&mut pl, "donate ches").await, "no wallet");
    assert_eq!(verb(&mut pl, "donate").await, "no wallet");
    pl.drain().await;
    pl.command("donate chest").await.unwrap();
    let (id, _) = prompt(&mut pl).await;
    assert_eq!(answer(&mut pl, &id, true).await, "thanked");
    assert_eq!(balance(&server, "object", "/items/chest").await, 5);
    assert_eq!(balance(&server, "object", "/items/c").await, 0);
}

/// A payment and its handler are one change: if the handler fails, nothing
/// is kept (no charge, none of the handler's writes) but the request is
/// used up; a handler that does not exist charges nothing either
#[tokio::test]
async fn test_payment_after_handler_failure() {
    let server = TestServer::start().await.expect("server");
    let mut adm = connect(
        &server,
        Role::Admin {
            username: "fail_admin".into(),
        },
    )
    .await;
    let mut pl = connect(
        &server,
        Role::Player {
            username: "unlucky".into(),
        },
    )
    .await;
    let pl_id = pl.account_id().to_string();
    let out = eval(
        &mut adm,
        &format!(
            r#"game.admin_grant_credits("{p}", 20)
               local h = game.store_code([[return {{
                 flaky = function(a) game.request_payment(4, "a flaky thing", "broken") return nil end,
                 ghost = function(a) game.request_payment(4, "nothing", "nosuch") return nil end,
                 broken = function(a)
                   game.update_object("/items/stall", {{sold = true}})
                   error("half way")
                 end,
               }}]])
               game.create_object("/items/stall", "item", "{room}", {{name = "stall", fixed = true, code_hash = h}})
               game.add_action("flaky", "/items/stall", "flaky")
               game.add_action("ghost", "/items/stall", "ghost")
               return "ok""#,
            p = pl_id,
            room = server.world().entrance_id
        ),
    )
    .await;
    assert_eq!(out["text"], "ok", "{}", out);

    pl.drain().await;
    pl.command("flaky stall").await.unwrap();
    let (id, _) = prompt(&mut pl).await;
    let reply = answer(&mut pl, &id, true).await;
    assert!(reply.contains("not charged"), "{}", reply);
    assert_eq!(balance(&server, "account", &pl_id).await, 20, "no charge");
    let sold: Option<String> = sqlx::query_scalar(
        "SELECT json_extract(properties, '$.sold') FROM objects WHERE id = '/items/stall'",
    )
    .fetch_one(server.pool())
    .await
    .unwrap();
    assert_eq!(sold, None, "the handler's write was not kept");
    let pending: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM payment_requests")
        .fetch_one(server.pool())
        .await
        .unwrap();
    assert_eq!(pending, 0, "the request is used up");

    pl.command("ghost stall").await.unwrap();
    let (id, _) = prompt(&mut pl).await;
    let reply = answer(&mut pl, &id, true).await;
    assert!(reply.contains("not charged"), "{}", reply);
    assert_eq!(balance(&server, "account", &pl_id).await, 20, "no charge");
}
