//! Confirmed payments (game.request_payment)
//!
//! Object code never takes a player's credits by itself. It asks: a row in
//! `payment_requests` (with an absolute deadline) and a `confirm` prompt to
//! the player. When the player accepts in time, the request is claimed
//! (deleted through Raft: only one answer counts), the credits move from
//! the player to the requesting object, and the object's handler runs (as
//! its owner) with `{payment_id, amount, reason, data, actor_id}`. Transfer
//! and handler are one change (db::run): if the handler fails, or does not
//! exist, neither is kept and the player is not charged; the claim stands
//! either way, so a request is answered once.

use serde_json::json;
use tracing::warn;

use super::AppState;
use super::websocket::ServerMessage;
use crate::credits::Holder;

/// How long a player has to answer
pub const PAYMENT_DEADLINE_MS: u64 = 60_000;

/// Most unanswered requests a player may have at once
pub const MAX_PENDING_PER_PLAYER: i64 = 5;

/// Answer request `id` (or, with None, the player's latest unexpired one).
/// None if there is no such request for this player.
pub async fn answer(
    state: &AppState,
    universe_id: &str,
    account_id: &str,
    player_id: &str,
    id: Option<&str>,
    accept: bool,
) -> Option<ServerMessage> {
    let now = crate::combat::now_ms() as i64;
    type Row = (String, String, String, i64, String, String);
    let row: Option<Row> = sqlx::query_as(
        "SELECT id, object_id, handler, amount, reason, data FROM payment_requests
         WHERE universe_id = ? AND account_id = ? AND expires_at > ? AND (? IS NULL OR id = ?)
         ORDER BY expires_at DESC LIMIT 1",
    )
    .bind(universe_id)
    .bind(account_id)
    .bind(now)
    .bind(id)
    .bind(id)
    .fetch_optional(&mut *crate::db::run::reader(state.db.pool()).await.ok()?)
    .await
    .ok()
    .flatten();
    let (id, object_id, handler, amount, reason, data) = row?;

    // One change: claim, transfer and handler. The claim stands whatever
    // happens (one answer per request); a failing (or missing) handler
    // undoes the transfer and its own writes
    let outcome = super::scripting::in_run(state, async {
        let claimed = state
            .raft_writer
            .execute_kept(
                "DELETE FROM payment_requests WHERE id = ? AND expires_at > ?",
                vec![json!(id), json!(now)],
            )
            .await;
        match claimed {
            Ok(r) if r.success && r.rows_affected == 1 => {}
            Ok(_) => return Ok(None),
            Err(e) => {
                warn!("payment {}: claiming failed: {}", id, e);
                return Ok(Some(ServerMessage::Error {
                    message: "The payment could not be processed; try again.".to_string(),
                }));
            }
        }
        if !accept {
            return Ok(Some(ServerMessage::Output {
                text: format!("You decline to pay {} credits.", amount),
            }));
        }

        let payer = Holder::Account(account_id.to_string());
        let payee = Holder::Object(object_id.clone());
        match state
            .credits
            .transfer(universe_id, &payer, &payee, amount)
            .await
        {
            Ok(true) => {}
            Ok(false) => {
                return Ok(Some(ServerMessage::Output {
                    text: format!("You don't have {} credits.", amount),
                }));
            }
            Err(e) => {
                warn!("payment {}: transfer failed: {}", id, e);
                return Err("the transfer failed".to_string());
            }
        }

        let room_id = state.connections.get_room_id(universe_id, player_id).await;
        let args = json!({
            "payment_id": id,
            "amount": amount,
            "reason": reason,
            "data": data,
            "actor_id": player_id,
            // For a refund (game.transfer_credits("object", actor_account, n))
            "actor_account": account_id,
        });
        let result = super::scripting::invoke_handler(
            state,
            universe_id,
            &object_id,
            room_id,
            &handler,
            args,
        )
        .await;
        match result {
            Ok(Some(text)) if text != "nil" && !text.is_empty() => {
                Ok(Some(ServerMessage::Output { text }))
            }
            Ok(Some(_)) => Ok(Some(ServerMessage::Output {
                text: format!("You pay {} credits.", amount),
            })),
            Err(e) => {
                warn!("payment {}: handler {} failed: {}", id, handler, e);
                Err(e)
            }
            Ok(None) => {
                warn!("payment {}: no handler {}", id, handler);
                Err(format!("no handler {}", handler))
            }
        }
    })
    .await;
    match outcome {
        Ok(message) => message,
        // Nothing but the claim was kept
        Err(_) => Some(ServerMessage::Output {
            text: format!(
                "Something went wrong; you were not charged the {} credits.",
                amount
            ),
        }),
    }
}
