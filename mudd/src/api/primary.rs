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

/// Run the lifecycle forever. `activations` counts the times this node
/// became primary (the router waits for the first when it starts as leader).
pub async fn run(state: AppState, activations: watch::Sender<u64>) {
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
        let effect_loop = tokio::spawn(effect_scheduler::run_effect_scheduler(state.clone()));
        info!("primary: node {} is the primary (schedulers started)", me);
        activations.send_modify(|n| *n += 1);

        // Serve until another node leads (or the core stops)
        let mut stopped = false;
        while leading(&metrics.borrow_watched(), me) {
            if metrics.changed().await.is_err() {
                stopped = true;
                break;
            }
        }
        timer_loop.abort();
        effect_loop.abort();
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
    Ok(())
}
