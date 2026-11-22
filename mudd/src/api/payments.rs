//! Confirmed payments (game.request_payment)
//!
//! Object code never takes a player's credits by itself. It asks: a row in
//! `payment_requests` (with an absolute deadline) and a `confirm` prompt to
//! the player. When the player accepts in time, the request is claimed
//! (deleted through Raft: only one answer counts), the credits move from
//! the player to the requesting object, and the object's handler runs (as
//! its owner) with `{payment_id, amount, reason, data, actor_id}`. If the
//! handler does not exist the payment is refunded; once it has run the
//! payment stands, even if it failed part way (it may already have handed
//! something over).

use serde_json::json;
use tracing::warn;

use super::websocket::ServerMessage;
use super::AppState;
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

    // Claim it: only one answer counts, and only in time
    let claimed = state
        .raft_writer
        .execute(
            "DELETE FROM payment_requests WHERE id = ? AND expires_at > ?",
            vec![json!(id), json!(now)],
        )
        .await;
    match claimed {
        Ok(r) if r.success && r.rows_affected == 1 => {}
        Ok(_) => return None,
        Err(e) => {
            warn!("payment {}: claiming failed: {}", id, e);
            return Some(ServerMessage::Error {
                message: "The payment could not be processed; try again.".to_string(),
            });
        }
    }
    if !accept {
        return Some(ServerMessage::Output {
            text: format!("You decline to pay {} credits.", amount),
        });
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
            return Some(ServerMessage::Output {
                text: format!("You don't have {} credits.", amount),
            })
        }
        Err(e) => {
            warn!("payment {}: transfer failed: {}", id, e);
            return Some(ServerMessage::Error {
                message: "The payment could not be processed.".to_string(),
            });
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
    let result =
        super::scripting::invoke_handler(state, universe_id, &object_id, room_id, &handler, args)
            .await;
    match result {
        Ok(Some(text)) if text != "nil" && !text.is_empty() => Some(ServerMessage::Output { text }),
        Ok(Some(_)) => Some(ServerMessage::Output {
            text: format!("You pay {} credits.", amount),
        }),
        // The handler ran but failed: it may already have handed something
        // over, so the payment stands (the object can refund with
        // transfer_credits); see docs/roadmap.md for making both one change
        Err(e) => {
            warn!(
                "payment {}: handler {} failed after payment: {}",
                id, handler, e
            );
            Some(ServerMessage::Output {
                text: format!(
                    "Something went wrong after your payment of {} credits; tell the owner.",
                    amount
                ),
            })
        }
        // No such handler: nothing ran, so the money goes back
        Ok(None) => {
            if let Err(e) = state
                .credits
                .transfer(universe_id, &payee, &payer, amount)
                .await
            {
                warn!("payment {}: refund failed: {}", id, e);
            }
            Some(ServerMessage::Output {
                text: format!(
                    "Something went wrong; your {} credits were returned.",
                    amount
                ),
            })
        }
    }
}
