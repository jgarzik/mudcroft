//! The primary lifecycle (docs/cluster-design.md, Node roles and lifecycle)
//!
//! Only the Raft leader, the primary, runs the timer loop and the effect
//! scheduler, and its caches are what game logic reads. On becoming
//! primary a node first confirms leadership with a quorum and catches up
//! on everything committed before its term (`ensure_linearizable`), then
//! rebuilds its caches from the tables and starts the schedulers. On losing
//! leadership it stops them. Timers and effects carry absolute deadlines,
//! so anything that fell due during a failover runs at once, and only once.

use std::time::Duration;

use openraft::async_runtime::WatchReceiver;
use openraft::ReadPolicy;
use tokio::sync::watch;
use tracing::{info, warn};

use super::{effect_scheduler, scripting, AppState};

/// Run the lifecycle forever. `active` is true while this node is the
/// active primary (caches rebuilt, schedulers running); game traffic is
/// served locally only then.
pub async fn run(state: AppState, active: watch::Sender<bool>) {
    let raft = state.raft_writer.raft().clone();
    let me = state.raft_writer.node_id();
    let mut metrics = raft.metrics();
    loop {
        // Wait to be the leader
        while !leading(&metrics.borrow_watched(), me) {
            if metrics.changed().await.is_err() {
                return;
            }
        }
        // Confirm with a quorum and apply everything committed so far
        if let Err(e) = raft.ensure_linearizable(ReadPolicy::ReadIndex).await {
            warn!("primary: not confirmed as leader yet: {}", e);
            tokio::time::sleep(Duration::from_millis(100)).await;
            continue;
        }
        if let Err(e) = rebuild_caches(&state).await {
            // Without its caches the node cannot serve as primary; try again
            warn!("primary: rebuilding caches failed: {}", e);
            tokio::time::sleep(Duration::from_millis(500)).await;
            continue;
        }
        let timer_loop = tokio::spawn(scripting::run_timer_loop(state.clone()));
        let recovery = tokio::spawn(handle_orphaned_sessions(state.clone()));
        let effect_loop = tokio::spawn(effect_scheduler::run_effect_scheduler(state.clone()));
        // The term this activation belongs to: a later term means another
        // node may have led meanwhile (watch notifications can merge a lost
        // and regained leadership), so the caches must be rebuilt
        let term = metrics.borrow_watched().current_term;
        info!(
            "primary: node {} is the primary (term {}, schedulers started)",
            me, term
        );
        active.send_replace(true);

        // Serve until another node leads, the term changes, or the core stops
        let mut stopped = false;
        loop {
            {
                let m = metrics.borrow_watched();
                if !leading(&m, me) || m.current_term != term {
                    break;
                }
            }
            if metrics.changed().await.is_err() {
                stopped = true;
                break;
            }
        }
        active.send_replace(false);
        timer_loop.abort();
        effect_loop.abort();
        recovery.abort();
        // The next primary has the verbs (they are in the table)
        state.actions.clear_all().await;
        // Sessions move to the new primary (directly connected players
        // reconnect; gateways relaying sessions follow)
        state.connections.close_all().await;
        info!("primary: node {} stepped down (schedulers stopped)", me);
        if stopped {
            return;
        }
    }
}

/// Players recorded as online but not connected here, once the reconnect
/// grace period has passed, lost their server (a crash, or a primary that
/// stepped down): handle their disconnects as the old server would have
async fn handle_orphaned_sessions(state: AppState) {
    tokio::time::sleep(super::websocket::RECONNECT_GRACE_PERIOD).await;
    let online = match state.player_manager.online_players().await {
        Ok(online) => online,
        Err(e) => {
            warn!("primary: reading online players failed: {}", e);
            return;
        }
    };
    for (universe, player, since) in online {
        // Not if they have a session or a disconnect pending here (that
        // grace period stays in charge); a reconnect meanwhile cancels it
        let Some(gen) = state
            .connections
            .begin_orphaned_disconnect(&universe, &player)
            .await
        else {
            continue;
        };
        if !state
            .connections
            .claim_expired_disconnect(&universe, &player, gen)
            .await
        {
            continue;
        }
        info!(
            "primary: {} lost its server; handling the disconnect",
            player
        );
        if let Err(e) = state
            .player_manager
            .finish_disconnect(&universe, &player, &since)
            .await
        {
            warn!("primary: disconnect of {} failed: {}", player, e);
        }
    }
}

/// Whether this node leads: its server state, not just current_leader (a
/// leader that steps down, e.g. removed from the cluster, can still name
/// itself there)
fn leading(m: &openraft::RaftMetrics<crate::raft::TypeConfig>, me: crate::raft::NodeId) -> bool {
    m.state == openraft::ServerState::Leader && m.current_leader == Some(me)
}

/// Rebuild every cache game logic reads from its table
async fn rebuild_caches(state: &AppState) -> anyhow::Result<()> {
    state.timers.load_from_db().await?;
    state.permissions.reset().await?;
    state.classes.write().await.load_from_db().await?;
    state.effects.load_from_db(state.db.pool()).await?;
    state.actions.load_from_db().await?;
    Ok(())
}
