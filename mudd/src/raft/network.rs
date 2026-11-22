//! Raft network layer for inter-node communication
//!
//! Implements RPC transport for Raft messages between cluster nodes
//! using HTTP/JSON for simplicity.

use std::collections::BTreeMap;
use std::future::Future;
use std::io;
use std::io::Cursor;
use std::sync::Arc;
use std::time::Duration;

use openraft::alias::{SnapshotMetaOf, SnapshotOf, VoteOf};
use openraft::errors::{RPCError, ReplicationClosed, StreamingError, Unreachable};
use openraft::network::v2::RaftNetworkV2;
use openraft::network::{RPCOption, RaftNetworkFactory};
use openraft::raft::{
    AppendEntriesRequest, AppendEntriesResponse, AppendResult, SnapshotResponse, VoteRequest,
    VoteResponse,
};
use openraft::{BasicNode, OptionalSend};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use tracing::{debug, error, warn};

use super::types::{NodeId, TypeConfig};

/// A whole snapshot sent in one request (`/raft/snapshot`)
#[derive(Debug, Serialize, Deserialize)]
pub struct FullSnapshotRequest {
    pub vote: VoteOf<TypeConfig>,
    pub meta: SnapshotMetaOf<TypeConfig>,
    /// Base64 in JSON (a byte array would be a list of decimal numbers)
    #[serde(with = "base64_bytes")]
    pub data: Vec<u8>,
}

mod base64_bytes {
    use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(bytes: &[u8], s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&BASE64.encode(bytes))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<u8>, D::Error> {
        let text = String::deserialize(d)?;
        BASE64.decode(text).map_err(serde::de::Error::custom)
    }
}

/// Network configuration for Raft cluster
#[derive(Debug, Clone)]
pub struct NetworkConfig {
    /// Node addresses: node_id -> "host:port"
    pub nodes: BTreeMap<NodeId, String>,
    /// Request timeout
    pub timeout: Duration,
}

impl Default for NetworkConfig {
    fn default() -> Self {
        Self {
            nodes: BTreeMap::new(),
            timeout: Duration::from_secs(5),
        }
    }
}

impl NetworkConfig {
    /// Create a new network configuration
    pub fn new(nodes: BTreeMap<NodeId, String>) -> Self {
        Self {
            nodes,
            timeout: Duration::from_secs(5),
        }
    }

    /// Create from RaftNodeConfig
    pub fn from_raft_config(config: &super::config::RaftNodeConfig) -> Self {
        let nodes: BTreeMap<NodeId, String> = config
            .peers
            .iter()
            .map(|(&id, (host, port))| (id, format!("{}:{}", host, port)))
            .collect();

        Self {
            nodes,
            timeout: Duration::from_secs(5),
        }
    }

    /// Get address for a node
    pub fn get_addr(&self, node_id: NodeId) -> Option<&String> {
        self.nodes.get(&node_id)
    }
}

/// Factory for creating network connections to peers
#[derive(Debug, Clone)]
pub struct RaftNetworkFactoryImpl {
    config: Arc<NetworkConfig>,
    client: Client,
}

impl RaftNetworkFactoryImpl {
    /// Create a new network factory
    pub fn new(config: NetworkConfig) -> Self {
        let client = Client::builder()
            .timeout(config.timeout)
            .build()
            .expect("failed to create HTTP client");

        Self {
            config: Arc::new(config),
            client,
        }
    }
}

impl RaftNetworkFactory<TypeConfig> for RaftNetworkFactoryImpl {
    type Network = RaftNetworkImpl;

    async fn new_client(&mut self, target: NodeId, _node: &BasicNode) -> Self::Network {
        RaftNetworkImpl {
            target,
            config: Arc::clone(&self.config),
            client: self.client.clone(),
        }
    }
}

/// Network connection to a single peer
#[derive(Debug, Clone)]
pub struct RaftNetworkImpl {
    target: NodeId,
    config: Arc<NetworkConfig>,
    client: Client,
}

impl RaftNetworkImpl {
    /// Send an RPC request to the target node
    async fn send_rpc<T: Serialize, R: for<'de> Deserialize<'de>>(
        &mut self,
        endpoint: &str,
        request: &T,
    ) -> Result<R, Unreachable<TypeConfig>> {
        let addr = self.config.get_addr(self.target).ok_or_else(|| {
            Unreachable::new(&io::Error::new(
                io::ErrorKind::NotFound,
                format!("node {} not found in config", self.target),
            ))
        })?;

        let url = format!("http://{}/raft/{}", addr, endpoint);
        debug!("Sending RPC to {}: {}", self.target, url);

        let response = self
            .client
            .post(&url)
            .json(request)
            .send()
            .await
            .map_err(|e| {
                warn!("RPC to {} failed: {}", self.target, e);
                Unreachable::new(&io::Error::new(
                    io::ErrorKind::ConnectionRefused,
                    e.to_string(),
                ))
            })?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            error!("RPC error from {}: {} - {}", self.target, status, body);
            return Err(Unreachable::from_string(format!(
                "HTTP {}: {}",
                status, body
            )));
        }

        response.json().await.map_err(|e| {
            error!("Failed to parse RPC response: {}", e);
            Unreachable::new(&io::Error::new(io::ErrorKind::InvalidData, e.to_string()))
        })
    }
}

impl RaftNetworkV2<TypeConfig> for RaftNetworkImpl {
    type SnapshotData = Cursor<Vec<u8>>;

    async fn append_entries(
        &mut self,
        rpc: AppendEntriesRequest<TypeConfig>,
        _option: RPCOption,
    ) -> Result<AppendResult<TypeConfig>, RPCError<TypeConfig>> {
        let prev_log_id = rpc.prev_log_id;
        let last_log_id = rpc.last_log_id();
        let response: AppendEntriesResponse<TypeConfig> = self
            .send_rpc("append_entries", &rpc)
            .await
            .map_err(RPCError::Unreachable)?;
        Ok(response.into_append_result(prev_log_id, last_log_id))
    }

    async fn vote(
        &mut self,
        rpc: VoteRequest<TypeConfig>,
        _option: RPCOption,
    ) -> Result<VoteResponse<TypeConfig>, RPCError<TypeConfig>> {
        self.send_rpc("vote", &rpc)
            .await
            .map_err(RPCError::Unreachable)
    }

    async fn full_snapshot(
        &mut self,
        vote: VoteOf<TypeConfig>,
        snapshot: SnapshotOf<TypeConfig, Self::SnapshotData>,
        _cancel: impl Future<Output = ReplicationClosed> + OptionalSend + 'static,
        _option: RPCOption,
    ) -> Result<SnapshotResponse<TypeConfig>, StreamingError<TypeConfig>> {
        let request = FullSnapshotRequest {
            vote,
            meta: snapshot.meta,
            data: snapshot.snapshot.into_inner(),
        };
        self.send_rpc("snapshot", &request)
            .await
            .map_err(StreamingError::Unreachable)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_network_config() {
        let mut nodes = BTreeMap::new();
        nodes.insert(1, "127.0.0.1:9001".to_string());
        nodes.insert(2, "127.0.0.1:9002".to_string());

        let config = NetworkConfig::new(nodes);
        assert_eq!(config.get_addr(1), Some(&"127.0.0.1:9001".to_string()));
        assert_eq!(config.get_addr(2), Some(&"127.0.0.1:9002".to_string()));
        assert_eq!(config.get_addr(3), None);
    }

    #[test]
    fn test_network_factory_creation() {
        let config = NetworkConfig::default();
        let _factory = RaftNetworkFactoryImpl::new(config);
    }

    /// Snapshot bytes travel as a base64 string and round-trip exactly
    #[test]
    fn test_full_snapshot_request_base64() {
        let request = FullSnapshotRequest {
            vote: openraft::impls::Vote::new(1, 1),
            meta: SnapshotMetaOf::<TypeConfig> {
                last_log_id: None,
                last_membership: Default::default(),
            },
            data: vec![0, 1, 2, 250, 255],
        };
        let json = serde_json::to_value(&request).unwrap();
        assert_eq!(json["data"], "AAEC+v8=");
        let back: FullSnapshotRequest = serde_json::from_value(json).unwrap();
        assert_eq!(back.data, vec![0, 1, 2, 250, 255]);
    }
}
