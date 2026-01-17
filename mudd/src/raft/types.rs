//! Raft type definitions
//!
//! Core types for Raft replication: Request, Response, and type configuration.

use std::fmt::Display;
use std::io::Cursor;

use openraft::{BasicNode, Entry, RaftTypeConfig};
use serde::{Deserialize, Serialize};

/// Node ID type for Raft cluster
pub type NodeId = u64;

/// Type configuration for our Raft instance
#[derive(
    Debug, Clone, Copy, Default, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize,
)]
pub struct TypeConfig;

impl Display for TypeConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "TypeConfig")
    }
}

impl RaftTypeConfig for TypeConfig {
    type D = Request;
    type R = Response;
    type Node = BasicNode;
    type NodeId = NodeId;
    type Entry = Entry<TypeConfig>;
    type SnapshotData = Cursor<Vec<u8>>;
    type AsyncRuntime = openraft::TokioRuntime;
    type Responder = openraft::impls::OneshotResponder<TypeConfig>;
}

/// Application request - SQL command to replicate
///
/// All non-deterministic values (UUIDs, timestamps) must be
/// pre-computed by the leader before replication.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Request {
    /// SQL statement to execute
    pub sql: String,
    /// Bound parameters as JSON values
    pub params: Vec<serde_json::Value>,
}

impl Request {
    /// Create a new request
    pub fn new(sql: impl Into<String>, params: Vec<serde_json::Value>) -> Self {
        Self {
            sql: sql.into(),
            params,
        }
    }

    /// Create a simple request with no parameters
    pub fn simple(sql: impl Into<String>) -> Self {
        Self {
            sql: sql.into(),
            params: vec![],
        }
    }
}

/// Application response from state machine
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Response {
    /// Whether the operation succeeded
    pub success: bool,
    /// Number of rows affected
    pub rows_affected: u64,
    /// Error message if any
    pub error: Option<String>,
}

impl Response {
    /// Create a successful response
    pub fn ok(rows_affected: u64) -> Self {
        Self {
            success: true,
            rows_affected,
            error: None,
        }
    }

    /// Create an error response
    pub fn error(msg: impl Into<String>) -> Self {
        Self {
            success: false,
            rows_affected: 0,
            error: Some(msg.into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_request_creation() {
        let req = Request::new(
            "INSERT INTO test VALUES (?)",
            vec![serde_json::json!("hello")],
        );
        assert_eq!(req.sql, "INSERT INTO test VALUES (?)");
        assert_eq!(req.params.len(), 1);
    }

    #[test]
    fn test_request_simple() {
        let req = Request::simple("DELETE FROM test");
        assert_eq!(req.sql, "DELETE FROM test");
        assert!(req.params.is_empty());
    }

    #[test]
    fn test_response_ok() {
        let resp = Response::ok(5);
        assert!(resp.success);
        assert_eq!(resp.rows_affected, 5);
        assert!(resp.error.is_none());
    }

    #[test]
    fn test_response_error() {
        let resp = Response::error("constraint violation");
        assert!(!resp.success);
        assert_eq!(resp.rows_affected, 0);
        assert_eq!(resp.error.as_deref(), Some("constraint violation"));
    }
}
