//! Raft configuration
//!
//! Node configuration, peer management, and tuning parameters.

use std::collections::BTreeMap;
use std::sync::Arc;

use openraft::Config;

use super::types::NodeId;

/// Snapshot threshold - create snapshot after this many log entries
pub const SNAPSHOT_THRESHOLD: u64 = 1000;

/// Heartbeat interval in milliseconds
pub const HEARTBEAT_INTERVAL: u64 = 500;

/// Minimum election timeout in milliseconds
pub const ELECTION_TIMEOUT_MIN: u64 = 1500;

/// Maximum election timeout in milliseconds
pub const ELECTION_TIMEOUT_MAX: u64 = 3000;

/// Raft node configuration
#[derive(Debug, Clone)]
pub struct RaftNodeConfig {
    /// This node's ID
    pub node_id: NodeId,
    /// Peer addresses: node_id -> (host, raft_port)
    pub peers: BTreeMap<NodeId, (String, u16)>,
    /// Raft port for this node
    pub raft_port: u16,
    /// Path to the database file (for snapshots)
    pub db_path: Option<String>,
}

impl Default for RaftNodeConfig {
    fn default() -> Self {
        Self {
            node_id: 1,
            peers: BTreeMap::new(),
            raft_port: 9000,
            db_path: None,
        }
    }
}

impl RaftNodeConfig {
    /// Create a single-node configuration (no replication)
    pub fn single(node_id: NodeId, port: u16) -> Self {
        let mut peers = BTreeMap::new();
        peers.insert(node_id, ("127.0.0.1".to_string(), port));
        Self {
            node_id,
            peers,
            raft_port: port,
            db_path: None,
        }
    }

    /// Create a multi-node configuration
    pub fn cluster(node_id: NodeId, port: u16, peers: BTreeMap<NodeId, (String, u16)>) -> Self {
        Self {
            node_id,
            peers,
            raft_port: port,
            db_path: None,
        }
    }

    /// Set the database path
    pub fn with_db_path(mut self, path: impl Into<String>) -> Self {
        self.db_path = Some(path.into());
        self
    }

    /// Get this node's address
    pub fn node_addr(&self) -> Option<String> {
        self.peers
            .get(&self.node_id)
            .map(|(host, port)| format!("{}:{}", host, port))
    }

    /// Get peer address by node ID
    pub fn peer_addr(&self, node_id: NodeId) -> Option<String> {
        self.peers
            .get(&node_id)
            .map(|(host, port)| format!("{}:{}", host, port))
    }

    /// Parse peers from string format: "1=host1:9000,2=host2:9001"
    pub fn parse_peers(s: &str) -> BTreeMap<NodeId, (String, u16)> {
        let mut peers = BTreeMap::new();
        for part in s.split(',') {
            let part = part.trim();
            if part.is_empty() {
                continue;
            }
            if let Some((id_str, addr)) = part.split_once('=') {
                if let Ok(id) = id_str.trim().parse::<NodeId>() {
                    if let Some((host, port_str)) = addr.rsplit_once(':') {
                        if let Ok(port) = port_str.parse::<u16>() {
                            peers.insert(id, (host.to_string(), port));
                        }
                    }
                }
            }
        }
        peers
    }

    /// Check if this is a single-node cluster
    pub fn is_single_node(&self) -> bool {
        self.peers.len() <= 1
    }

    /// Get all peer node IDs (excluding self)
    pub fn other_nodes(&self) -> Vec<NodeId> {
        self.peers
            .keys()
            .filter(|&&id| id != self.node_id)
            .copied()
            .collect()
    }
}

/// Create the OpenRaft configuration with our tuning parameters
pub fn create_openraft_config() -> Arc<Config> {
    Arc::new(Config {
        heartbeat_interval: HEARTBEAT_INTERVAL,
        election_timeout_min: ELECTION_TIMEOUT_MIN,
        election_timeout_max: ELECTION_TIMEOUT_MAX,
        snapshot_policy: openraft::SnapshotPolicy::LogsSinceLast(SNAPSHOT_THRESHOLD),
        ..Default::default()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_single_node_config() {
        let config = RaftNodeConfig::single(1, 9000);
        assert_eq!(config.node_id, 1);
        assert_eq!(config.raft_port, 9000);
        assert!(config.peers.contains_key(&1));
        assert!(config.is_single_node());
    }

    #[test]
    fn test_parse_peers() {
        let peers = RaftNodeConfig::parse_peers("1=127.0.0.1:9000,2=127.0.0.1:9001");
        assert_eq!(peers.len(), 2);
        assert_eq!(peers.get(&1), Some(&("127.0.0.1".to_string(), 9000)));
        assert_eq!(peers.get(&2), Some(&("127.0.0.1".to_string(), 9001)));
    }

    #[test]
    fn test_parse_peers_with_spaces() {
        let peers = RaftNodeConfig::parse_peers(" 1 = 10.0.0.1:9000 , 2 = 10.0.0.2:9001 ");
        assert_eq!(peers.len(), 2);
    }

    #[test]
    fn test_parse_peers_empty() {
        let peers = RaftNodeConfig::parse_peers("");
        assert!(peers.is_empty());
    }

    #[test]
    fn test_node_addr() {
        let config = RaftNodeConfig::single(1, 9000);
        assert_eq!(config.node_addr(), Some("127.0.0.1:9000".to_string()));
    }

    #[test]
    fn test_other_nodes() {
        let peers =
            RaftNodeConfig::parse_peers("1=127.0.0.1:9000,2=127.0.0.1:9001,3=127.0.0.1:9002");
        let config = RaftNodeConfig::cluster(1, 9000, peers);
        let others = config.other_nodes();
        assert_eq!(others.len(), 2);
        assert!(others.contains(&2));
        assert!(others.contains(&3));
        assert!(!others.contains(&1));
    }

    #[test]
    fn test_with_db_path() {
        let config = RaftNodeConfig::single(1, 9000).with_db_path("/data/game.db");
        assert_eq!(config.db_path, Some("/data/game.db".to_string()));
    }
}
