//! Raft network layer for inter-node communication
//!
//! Implements RPC transport for Raft messages between cluster nodes
//! using HTTP/JSON for simplicity.

use std::collections::BTreeMap;
use std::io;
use std::sync::Arc;
use std::time::Duration;

use openraft::error::{InstallSnapshotError, RPCError, RaftError, Unreachable};
use openraft::network::{RPCOption, RaftNetwork, RaftNetworkFactory};
use openraft::raft::{
    AppendEntriesRequest, AppendEntriesResponse, InstallSnapshotRequest, InstallSnapshotResponse,
    VoteRequest, VoteResponse,
};
use openraft::BasicNode;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use tracing::{debug, error, warn};

use super::types::{NodeId, TypeConfig};

/// Raft RPC request wrapper
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum RaftRpcRequest {
    Vote(VoteRequest<NodeId>),
    AppendEntries(AppendEntriesRequest<TypeConfig>),
    InstallSnapshot(InstallSnapshotRequest<TypeConfig>),
}

/// Raft RPC response wrapper
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum RaftRpcResponse {
    Vote(VoteResponse<NodeId>),
    AppendEntries(AppendEntriesResponse<NodeId>),
    InstallSnapshot(InstallSnapshotResponse<NodeId>),
    Error { message: String },
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
    ) -> Result<R, Unreachable> {
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
            return Err(Unreachable::new(&io::Error::other(format!(
                "HTTP {}: {}",
                status, body
            ))));
        }

        response.json().await.map_err(|e| {
            error!("Failed to parse RPC response: {}", e);
            Unreachable::new(&io::Error::new(io::ErrorKind::InvalidData, e.to_string()))
        })
    }
}

impl RaftNetwork<TypeConfig> for RaftNetworkImpl {
    async fn append_entries(
        &mut self,
        req: AppendEntriesRequest<TypeConfig>,
        _option: RPCOption,
    ) -> Result<AppendEntriesResponse<NodeId>, RPCError<NodeId, BasicNode, RaftError<NodeId>>> {
        self.send_rpc("append_entries", &req)
            .await
            .map_err(RPCError::Unreachable)
    }

    async fn install_snapshot(
        &mut self,
        req: InstallSnapshotRequest<TypeConfig>,
        _option: RPCOption,
    ) -> Result<
        InstallSnapshotResponse<NodeId>,
        RPCError<NodeId, BasicNode, RaftError<NodeId, InstallSnapshotError>>,
    > {
        self.send_rpc("install_snapshot", &req)
            .await
            .map_err(RPCError::Unreachable)
    }

    async fn vote(
        &mut self,
        req: VoteRequest<NodeId>,
        _option: RPCOption,
    ) -> Result<VoteResponse<NodeId>, RPCError<NodeId, BasicNode, RaftError<NodeId>>> {
        self.send_rpc("vote", &req)
            .await
            .map_err(RPCError::Unreachable)
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
}
