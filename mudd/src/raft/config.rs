//! Raft tuning parameters

use std::sync::Arc;

use openraft::Config;

/// Snapshot threshold - create snapshot after this many log entries
pub const SNAPSHOT_THRESHOLD: u64 = 1000;

/// Heartbeat interval in milliseconds (with more than one member)
pub const HEARTBEAT_INTERVAL: u64 = 100;

/// Minimum election timeout in milliseconds
pub const ELECTION_TIMEOUT_MIN: u64 = 400;

/// Maximum election timeout in milliseconds
pub const ELECTION_TIMEOUT_MAX: u64 = 800;

/// The OpenRaft configuration.
///
/// A node starts with its tick, heartbeat and election timers off (a single
/// member has no peers; as in the sister project roodb); they are switched
/// on at runtime ([`super::transport::enable_cluster_timers`]) once the
/// cluster has more than one member.
pub fn create_openraft_config() -> Arc<Config> {
    Arc::new(Config {
        cluster_name: "mudcroft".to_string(),
        heartbeat_interval: HEARTBEAT_INTERVAL,
        election_timeout_min: ELECTION_TIMEOUT_MIN,
        election_timeout_max: ELECTION_TIMEOUT_MAX,
        enable_tick: false,
        enable_heartbeat: false,
        enable_elect: false,
        snapshot_policy: openraft::SnapshotPolicy::LogsSinceLast(SNAPSHOT_THRESHOLD),
        ..Default::default()
    })
}
