//! RaftWriter - Central write coordinator for all database writes
//!
//! All SQLite writes must go through this coordinator to ensure
//! consensus across the cluster.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Result, bail};
use openraft::async_runtime::WatchReceiver;
use openraft::{BasicNode, ChangeMembers};
use sqlx::SqlitePool;
use tokio::net::TcpListener;
use tracing::{debug, info, warn};

use super::storage::{read_node_id, write_node_id};
use super::transport::{self, ClusterNetwork, ClusterSecurity, ClusterService};
use super::types::{NodeId, Request, Response};
use super::{GameRaft, create_raft_node};

/// How long a new node keeps trying to join
const JOIN_LIMIT: Duration = Duration::from_secs(120);

/// What a node needs to be part of a cluster
pub struct ClusterSetup {
    /// TLS configuration and shared secret
    pub security: Arc<ClusterSecurity>,
    /// The bound cluster port
    pub listener: TcpListener,
    /// The address peers reach this node at (`host:port`)
    pub advertise: String,
    /// Members to join through (empty: this node is or was node 1)
    pub join: Vec<String>,
}

/// Central coordinator for all database writes via Raft consensus
pub struct RaftWriter {
    raft: GameRaft,
    node_id: NodeId,
    db_path: String,
    /// The cluster port's service and security (None for a single node)
    cluster: Option<(Arc<ClusterService>, Arc<ClusterSecurity>)>,
}

impl RaftWriter {
    /// A node without a cluster: a one-member Raft cluster (node 1 unless
    /// the database records another id)
    pub async fn single_node(pool: SqlitePool, db_path: &str) -> Result<Self> {
        Self::start(pool, db_path, None).await
    }

    /// Start this node's Raft. Without `cluster`, a one-member cluster.
    /// With it: serve the cluster port, and either resume (the database
    /// records the node's id), join through `cluster.join` (a new, empty
    /// node), or become node 1 of a new cluster.
    pub async fn start(
        pool: SqlitePool,
        db_path: &str,
        cluster: Option<ClusterSetup>,
    ) -> Result<Self> {
        let stored = read_node_id(&pool).await?;
        let has_state = super::storage::has_raft_state(&pool).await?;
        let security = cluster.as_ref().map(|c| c.security.clone());
        let join_flag = cluster.as_ref().is_some_and(|c| !c.join.is_empty());
        // Join when new, or when an id came from a join that never got as
        // far as any Raft state (a crash right after the leader answered)
        let joining = join_flag && !has_state && stored != Some(1);
        let node_id = match &cluster {
            Some(c) if joining => {
                transport::join_cluster(&c.join, &c.advertise, c.security.clone(), JOIN_LIMIT)
                    .await?
            }
            _ => stored.unwrap_or(1),
        };
        if !joining && !has_state && node_id != 1 {
            bail!(
                "this node (id {}) joined a cluster but has no Raft state yet; start it with --join",
                node_id
            );
        }
        if stored != Some(node_id) {
            write_node_id(&pool, node_id).await?;
        }
        info!(
            "Starting Raft node {} ({})",
            node_id,
            if cluster.is_some() {
                "cluster"
            } else {
                "single node"
            }
        );

        let raft = create_raft_node(
            node_id,
            pool.clone(),
            ClusterNetwork::new(security.clone()),
            Some(db_path.to_string()),
        )
        .await?;

        // A node removed from the cluster (`mudd_admin cluster remove`) must
        // not come back with its old id: to return, it starts empty and
        // joins again
        if stored.is_some() {
            // The metrics show the membership loaded from storage only once
            // the core has started
            let _ = raft
                .wait(Some(Duration::from_secs(5)))
                .metrics(
                    |m| m.membership_config.membership().nodes().count() > 0,
                    "membership loaded",
                )
                .await;
            let membership = raft.metrics().borrow_watched().membership_config.clone();
            let membership = membership.membership();
            // Its own log may not say so: the leader stops replicating to a
            // node once it is removed. So also ask the members it knows.
            let removed_here =
                membership.nodes().count() > 0 && membership.get_node(&node_id).is_none();
            let removed_there = match (&security, removed_here) {
                (Some(security), false) => {
                    let peers: Vec<String> = membership
                        .nodes()
                        .filter(|(id, n)| **id != node_id && !n.addr.is_empty())
                        .map(|(_, n)| n.addr.clone())
                        .collect();
                    transport::removed_by_peers(node_id, &peers, security.clone()).await
                }
                _ => false,
            };
            if removed_here || removed_there {
                raft.shutdown().await.ok();
                bail!(
                    "node {} was removed from the cluster; to rejoin, start from an empty \
                     database (mudd_init --empty) with --join",
                    node_id
                );
            }
        }

        let advertise = cluster
            .as_ref()
            .map(|c| c.advertise.clone())
            .unwrap_or_default();
        // A new cluster (or single node) is initialized the first time only:
        // on a database this node has served before, the Raft state (vote,
        // log) is already persisted and initialize() is refused. A joining
        // node is brought in by the leader instead.
        // Only node 1 ever starts a cluster
        if !joining && node_id == 1 && !raft.is_initialized().await? {
            let mut members = BTreeMap::new();
            members.insert(node_id, BasicNode::new(advertise.clone()));
            raft.initialize(members).await?;
        }

        let mut serving = None;
        if let Some(cluster) = cluster {
            let service = ClusterService::new(raft.clone(), pool.clone());
            serving = Some((service.clone(), cluster.security.clone()));
            tokio::spawn(transport::serve(
                cluster.listener,
                cluster.security,
                service,
            ));
            // Timers on whenever the cluster has (or gains) other members;
            // and the leader keeps its own advertised address current
            tokio::spawn(watch_membership(raft.clone(), node_id, advertise));
        }

        Ok(Self {
            raft,
            node_id,
            db_path: db_path.to_string(),
            cluster: serving,
        })
    }

    /// The cluster port's service and security (None for a single node)
    pub fn cluster(&self) -> Option<&(Arc<ClusterService>, Arc<ClusterSecurity>)> {
        self.cluster.as_ref()
    }

    /// Execute a single SQL write via Raft consensus
    ///
    /// The SQL and params are replicated to all nodes before being applied.
    /// Non-deterministic values (UUIDs, timestamps) must be pre-computed.
    pub async fn execute(&self, sql: &str, params: Vec<serde_json::Value>) -> Result<Response> {
        // Inside a run (one logical change): part of its batch
        if let Some(run) = crate::db::run::current() {
            let rows = run.execute(&[(sql.to_string(), params)]).await?;
            return Ok(Response::ok(rows));
        }
        let request = Request::new(sql, params);

        debug!("Submitting write to Raft: {}", sql);

        let response = self.raft.client_write(request).await.map_err(not_primary)?;

        if !response.data.success {
            return Err(ApplyFailed(format!(
                "Raft write failed: {}",
                response.data.error.as_deref().unwrap_or("unknown error")
            ))
            .into());
        }

        Ok(response.data)
    }

    /// Like [`Self::execute`], but inside a run the write stands even if the
    /// run fails (see db::run)
    pub async fn execute_kept(
        &self,
        sql: &str,
        params: Vec<serde_json::Value>,
    ) -> Result<Response> {
        if let Some(run) = crate::db::run::current() {
            let rows = run.execute_kept(&[(sql.to_string(), params)]).await?;
            return Ok(Response::ok(rows));
        }
        self.execute(sql, params).await
    }

    /// Execute a batch of SQL statements atomically via Raft consensus
    ///
    /// The statements are one log entry, applied in one transaction: if any
    /// fails, none are applied and an error is returned.
    pub async fn execute_batch(
        &self,
        statements: Vec<(String, Vec<serde_json::Value>)>,
    ) -> Result<()> {
        // Inside a run (one logical change): part of its batch
        if let Some(run) = crate::db::run::current() {
            run.execute(&statements).await?;
            return Ok(());
        }
        self.execute_batch_direct(statements).await
    }

    /// Propose a batch now, even inside a run (for writes that must not
    /// wait for, or depend on, the run: e.g. billing an external call)
    pub async fn execute_batch_direct(
        &self,
        statements: Vec<(String, Vec<serde_json::Value>)>,
    ) -> Result<()> {
        let response = self
            .raft
            .client_write(Request::batch(statements))
            .await
            .map_err(not_primary)?;
        if !response.data.success {
            return Err(ApplyFailed(format!(
                "Batch failed: {}",
                response.data.error.as_deref().unwrap_or("unknown error")
            ))
            .into());
        }
        Ok(())
    }

    /// Wait for this node to have a leader (either self or another node)
    pub async fn wait_for_leader(&self, timeout: Duration) -> Result<()> {
        let start = std::time::Instant::now();

        loop {
            if start.elapsed() > timeout {
                bail!("Timeout waiting for Raft leader");
            }

            let metrics = self.raft.metrics().borrow_watched().clone();

            if let Some(leader) = metrics.current_leader {
                info!("Raft leader elected: node {}", leader);
                return Ok(());
            }

            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    /// Resolves when the Raft core stops: a fatal error (e.g. the state
    /// machine could not apply a committed entry) or a shutdown. Returns
    /// the reason. After this every write fails, so the server must stop.
    pub async fn stopped(&self) -> String {
        let mut metrics = self.raft.metrics();
        loop {
            if let Err(fatal) = &metrics.borrow_watched().running_state {
                return fatal.to_string();
            }
            if metrics.changed().await.is_err() {
                return "Raft core exited".to_string();
            }
        }
    }

    /// Get the Raft instance (for advanced operations)
    pub fn raft(&self) -> &GameRaft {
        &self.raft
    }

    /// Get this node's ID
    pub fn node_id(&self) -> NodeId {
        self.node_id
    }

    /// Get the database path
    pub fn db_path(&self) -> &str {
        &self.db_path
    }

    /// Check if this node is currently the leader
    pub fn is_leader(&self) -> bool {
        // The server state, not just current_leader: a leader that steps
        // down (e.g. removed from the cluster) can still name itself there
        let metrics = self.raft.metrics().borrow_watched().clone();
        metrics.state == openraft::ServerState::Leader
            && metrics.current_leader == Some(self.node_id)
    }

    /// Get the current leader's node ID (if known)
    pub fn current_leader(&self) -> Option<NodeId> {
        self.raft.metrics().borrow_watched().current_leader
    }
}

/// A write that reached the log but was refused when applied (an SQL error,
/// the same on every node): retrying it fails the same way, unlike a write
/// refused because this node is not (or no longer) the primary
#[derive(Debug)]
pub struct ApplyFailed(pub String);

impl std::fmt::Display for ApplyFailed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ApplyFailed {}

/// Whether `e` (or what caused it) is an [`ApplyFailed`]
pub fn is_apply_failure(e: &anyhow::Error) -> bool {
    e.chain().any(|c| c.is::<ApplyFailed>())
}

impl crate::db::run::Committer for RaftWriter {
    fn commit<'a>(
        &'a self,
        statements: Vec<crate::db::run::Statement>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<()>> + Send + 'a>> {
        Box::pin(self.execute_batch_direct(statements))
    }
}

/// A write refused because this node is not the leader says so plainly
/// (only the primary writes; see docs/cluster-design.md)
fn not_primary(
    e: openraft::error::RaftError<
        super::TypeConfig,
        openraft::error::ClientWriteError<super::TypeConfig>,
    >,
) -> anyhow::Error {
    use openraft::error::ClientWriteError;
    match e.api_error() {
        Some(ClientWriteError::ForwardToLeader(f)) => anyhow::anyhow!(
            "not the primary (the primary is node {})",
            f.leader_id
                .map(|id| id.to_string())
                .unwrap_or_else(|| "unknown".to_string())
        ),
        _ => anyhow::Error::new(e),
    }
}

/// Switch the Raft timers on once the membership has more than one node,
/// and, as leader, record this node's advertised address if it changed
/// (e.g. a single node that has just been given cluster flags)
async fn watch_membership(raft: GameRaft, node_id: NodeId, advertise: String) {
    let mut metrics = raft.metrics();
    let mut timers_on = false;
    let mut address_checked = false;
    loop {
        let (leader, membership) = {
            let m = metrics.borrow_watched();
            (m.current_leader, m.membership_config.clone())
        };
        if !timers_on && membership.membership().nodes().count() > 1 {
            transport::enable_cluster_timers(&raft);
            timers_on = true;
        }
        let mut retry = false;
        if !address_checked && leader == Some(node_id) {
            let current = membership
                .membership()
                .get_node(&node_id)
                .map(|n| n.addr.clone());
            if current.as_deref() == Some(advertise.as_str()) {
                address_checked = true;
            } else {
                let mut nodes = BTreeMap::new();
                nodes.insert(node_id, BasicNode::new(advertise.clone()));
                match raft
                    .change_membership(ChangeMembers::SetNodes(nodes), false)
                    .await
                {
                    Ok(_) => {
                        info!("cluster: node {} now advertised at {}", node_id, advertise);
                        address_checked = true;
                    }
                    // E.g. another membership change in progress: try again
                    Err(e) => {
                        warn!(
                            "cluster: recording this node's address failed (retrying): {}",
                            e
                        );
                        retry = true;
                    }
                }
            }
        }
        if retry {
            tokio::select! {
                changed = metrics.changed() => if changed.is_err() { return },
                _ = tokio::time::sleep(Duration::from_secs(1)) => {}
            }
        } else if metrics.changed().await.is_err() {
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A private database path for the Raft config (never a shared fixed
    /// path: parallel tests would collide)
    fn test_db_path() -> (tempfile::TempDir, String) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.db").to_string_lossy().to_string();
        (dir, path)
    }
    use sqlx::sqlite::SqlitePoolOptions;

    async fn test_pool_with_raft_tables() -> SqlitePool {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();

        // Create raft tables
        sqlx::query(
            "CREATE TABLE raft_log (
                log_index INTEGER PRIMARY KEY,
                term INTEGER NOT NULL,
                leader_node INTEGER NOT NULL DEFAULT 0,
                entry_type TEXT NOT NULL,
                payload TEXT,
                created_at INTEGER NOT NULL DEFAULT (unixepoch())
            )",
        )
        .execute(&pool)
        .await
        .unwrap();

        sqlx::query(
            "CREATE TABLE raft_vote (
                id INTEGER PRIMARY KEY CHECK (id = 1),
                term INTEGER NOT NULL,
                node_id INTEGER,
                committed INTEGER NOT NULL DEFAULT 0
            )",
        )
        .execute(&pool)
        .await
        .unwrap();

        sqlx::query(
            "CREATE TABLE raft_meta (
                key TEXT PRIMARY KEY,
                value TEXT NOT NULL
            )",
        )
        .execute(&pool)
        .await
        .unwrap();

        // Create a test table for write operations
        sqlx::query("CREATE TABLE test_data (id TEXT PRIMARY KEY, value TEXT)")
            .execute(&pool)
            .await
            .unwrap();

        pool
    }

    #[tokio::test]
    async fn test_single_node_writer_creation() {
        let pool = test_pool_with_raft_tables().await;
        let (_dir, db_path) = test_db_path();
        let writer = RaftWriter::single_node(pool, &db_path).await;
        assert!(writer.is_ok());
    }

    #[tokio::test]
    async fn test_single_node_write() {
        let pool = test_pool_with_raft_tables().await;
        let (_dir, db_path) = test_db_path();
        let writer = RaftWriter::single_node(pool, &db_path).await.unwrap();

        // Wait for leader election
        writer
            .wait_for_leader(Duration::from_secs(5))
            .await
            .unwrap();

        // Execute a write
        let response = writer
            .execute(
                "INSERT INTO test_data (id, value) VALUES (?, ?)",
                vec![
                    serde_json::json!("test-id"),
                    serde_json::json!("test-value"),
                ],
            )
            .await
            .unwrap();

        assert!(response.success);
        assert_eq!(response.rows_affected, 1);
    }

    #[tokio::test]
    async fn test_is_leader() {
        let pool = test_pool_with_raft_tables().await;
        let (_dir, db_path) = test_db_path();
        let writer = RaftWriter::single_node(pool, &db_path).await.unwrap();

        writer
            .wait_for_leader(Duration::from_secs(5))
            .await
            .unwrap();

        // Single-node should always be leader
        assert!(writer.is_leader());
        assert_eq!(writer.current_leader(), Some(1));
    }

    /// Concurrent writes (which openraft batches into shared appends and
    /// applies) all commit, each with its own response
    #[tokio::test]
    async fn test_concurrent_writes() {
        let pool = test_pool_with_raft_tables().await;
        let (_dir, db_path) = test_db_path();
        let writer = std::sync::Arc::new(
            RaftWriter::single_node(pool.clone(), &db_path)
                .await
                .unwrap(),
        );
        writer
            .wait_for_leader(Duration::from_secs(5))
            .await
            .unwrap();

        let tasks: Vec<_> = (0..50)
            .map(|i| {
                let writer = writer.clone();
                tokio::spawn(async move {
                    writer
                        .execute(
                            "INSERT INTO test_data (id, value) VALUES (?, ?)",
                            vec![serde_json::json!(format!("id-{}", i)), serde_json::json!(i)],
                        )
                        .await
                })
            })
            .collect();
        for task in tasks {
            let response = task.await.unwrap().unwrap();
            assert_eq!(response.rows_affected, 1);
        }
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM test_data")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(count, 50);
    }

    /// stopped() resolves once the Raft core is gone
    #[tokio::test]
    async fn test_stopped_resolves_when_core_stops() {
        let pool = test_pool_with_raft_tables().await;
        let (_dir, db_path) = test_db_path();
        let writer = RaftWriter::single_node(pool, &db_path).await.unwrap();
        writer
            .wait_for_leader(Duration::from_secs(5))
            .await
            .unwrap();
        writer.raft().shutdown().await.unwrap();
        let reason = tokio::time::timeout(Duration::from_secs(5), writer.stopped())
            .await
            .expect("stopped() should resolve after shutdown");
        assert!(!reason.is_empty());
    }
}
