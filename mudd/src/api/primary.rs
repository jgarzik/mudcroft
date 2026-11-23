//! The primary lifecycle (docs/cluster-design.md, Node roles and lifecycle)
//!
//! Only the Raft leader, the primary, runs the timer loop and the effect
//! scheduler, and its caches are what game logic reads. On becoming
//! primary a node first confirms leadership with a quorum and catches up
//! on everything committed before its term (`ensure_linearizable`), then
//! rebuilds its caches from the tables and starts the schedulers, each
//! restarted if it panics (`supervise`). On losing leadership it stops
//! them. Timers and effects carry absolute deadlines, so anything that fell
//! due during a failover runs at once, and only once.

use std::time::Duration;

use openraft::ReadPolicy;
use openraft::async_runtime::WatchReceiver;
use tokio::sync::watch;
use tracing::{info, warn};

use super::{AppState, combat, effect_scheduler, scripting};

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
        let s = state.clone();
        let timer_loop = tokio::spawn(supervise("timer loop", move || {
            scripting::run_timer_loop(s.clone())
        }));
        let s = state.clone();
        let recovery = tokio::spawn(supervise("orphaned-session recovery", move || {
            handle_orphaned_sessions(s.clone())
        }));
        let s = state.clone();
        let effect_loop = tokio::spawn(supervise("effect scheduler", move || {
            effect_scheduler::run_effect_scheduler(s.clone())
        }));
        let s = state.clone();
        let combat_loop = tokio::spawn(supervise("combat scheduler", move || {
            combat::run_combat_scheduler(s.clone())
        }));
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
        combat_loop.abort();
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
        let Some(disconnect_id) = state
            .connections
            .begin_orphaned_disconnect(&universe, &player)
            .await
        else {
            continue;
        };
        if !state
            .connections
            .claim_expired_disconnect(&universe, &player, disconnect_id)
            .await
        {
            continue;
        }
        info!(
            "primary: {} lost its server; handling the disconnect",
            player
        );
        let room = state
            .object_store
            .get(&universe, &player)
            .await
            .ok()
            .flatten()
            .and_then(|p| p.parent_id);
        if let Err(e) = state
            .player_manager
            .finish_disconnect(&universe, &player, &since)
            .await
        {
            warn!("primary: disconnect of {} failed: {}", player, e);
        }
        super::websocket::disconnect_hook(&state, &universe, &player, room).await;
    }
}

/// Whether this node leads: its server state, not just current_leader (a
/// leader that steps down, e.g. removed from the cluster, can still name
/// itself there)
fn leading(m: &openraft::RaftMetrics<crate::raft::TypeConfig>, me: crate::raft::NodeId) -> bool {
    m.state == openraft::ServerState::Leader && m.current_leader == Some(me)
}

/// Rebuild every cache game logic reads from its table
pub(crate) async fn rebuild_caches(state: &AppState) -> anyhow::Result<()> {
    state.timers.load_from_db().await?;
    state.permissions.reset().await?;
    state.classes.write().await.load_from_db().await?;
    state.effects.load_from_db(state.db.pool()).await?;
    state.actions.load_from_db().await?;
    Ok(())
}

/// First wait before restarting a task that panicked; doubles per panic
const RESTART_MIN: Duration = Duration::from_secs(1);
/// Longest wait between restarts
const RESTART_MAX: Duration = Duration::from_secs(60);
/// A task that ran this long before panicking restarts after RESTART_MIN
const RESTART_STABLE: Duration = Duration::from_secs(300);

/// Aborts the task when dropped, so aborting a supervisor stops its task
struct AbortOnDrop(tokio::task::JoinHandle<()>);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// Run one of the primary's tasks, restarting it (with backoff) if it
/// panics, so a bug cannot silently stop timers, effects or combat until
/// the next failover. Returns when the task finishes. Aborting the
/// supervisor (stepping down) aborts the task too.
async fn supervise<F, Fut>(name: &'static str, mut start: F)
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = ()> + Send + 'static,
{
    let mut backoff = RESTART_MIN;
    loop {
        let started = tokio::time::Instant::now();
        let mut task = AbortOnDrop(tokio::spawn(start()));
        match (&mut task.0).await {
            Err(e) if e.is_panic() => {
                if started.elapsed() >= RESTART_STABLE {
                    backoff = RESTART_MIN;
                }
                tracing::error!(
                    "primary: {} panicked ({}); restarting in {:?}",
                    name,
                    e,
                    backoff
                );
                tokio::time::sleep(backoff).await;
                backoff = (backoff * 2).min(RESTART_MAX);
            }
            // Finished (or cancelled by the runtime shutting down)
            _ => return,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

    /// A task that panics is restarted (after a backoff), and one that
    /// finishes is not
    #[tokio::test(start_paused = true)]
    async fn test_supervise_restarts_after_panic() {
        let runs = Arc::new(AtomicU32::new(0));
        let r = runs.clone();
        supervise("test", move || {
            let r = r.clone();
            async move {
                if r.fetch_add(1, Ordering::SeqCst) < 2 {
                    panic!("scheduler bug");
                }
            }
        })
        .await;
        assert_eq!(runs.load(Ordering::SeqCst), 3);
    }

    /// Aborting the supervisor (stepping down) stops the task it runs
    #[tokio::test(start_paused = true)]
    async fn test_supervise_abort_stops_the_task() {
        struct Flag(Arc<AtomicBool>);
        impl Drop for Flag {
            fn drop(&mut self) {
                self.0.store(true, Ordering::SeqCst);
            }
        }
        let dropped = Arc::new(AtomicBool::new(false));
        let d = dropped.clone();
        let handle = tokio::spawn(supervise("test", move || {
            let flag = Flag(d.clone());
            async move {
                let _flag = flag;
                std::future::pending::<()>().await;
            }
        }));
        tokio::time::sleep(Duration::from_millis(10)).await;
        assert!(!dropped.load(Ordering::SeqCst));
        handle.abort();
        let _ = handle.await;
        tokio::time::sleep(Duration::from_millis(10)).await;
        assert!(dropped.load(Ordering::SeqCst), "task still running");
    }
}
