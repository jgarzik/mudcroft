//! Raft type definitions
//!
//! Core types for Raft replication: Request, Response, and type configuration.

use std::fmt;

use openraft::BasicNode;
use serde::{Deserialize, Serialize};

/// Node ID type for Raft cluster
pub type NodeId = u64;

// Everything not named here takes openraft's default (leader_id_adv
// LeaderId, Tokio runtime). The log store persists a log id as (term,
// leader node, index), which is the adv committed leader id.
openraft::declare_raft_types!(
    pub TypeConfig:
        D = Request,
        R = Response,
        NodeId = NodeId,
        Node = BasicNode,
);

/// One SQL statement with its bound parameters
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Statement {
    /// SQL statement to execute
    pub sql: String,
    /// Bound parameters as JSON values
    pub params: Vec<serde_json::Value>,
}

/// Application request: SQL statements to replicate, applied atomically in
/// one transaction (all or nothing)
///
/// All non-deterministic values (UUIDs, timestamps) must be
/// pre-computed by the leader before replication.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Request {
    /// Statements, applied in order inside one transaction
    pub statements: Vec<Statement>,
}

impl fmt::Display for Request {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Request({} statements)", self.statements.len())
    }
}

impl Request {
    /// A request with one statement
    pub fn new(sql: impl Into<String>, params: Vec<serde_json::Value>) -> Self {
        Self {
            statements: vec![Statement {
                sql: sql.into(),
                params,
            }],
        }
    }

    /// A request with one statement and no parameters
    pub fn simple(sql: impl Into<String>) -> Self {
        Self::new(sql, vec![])
    }

    /// A request applying several statements atomically
    pub fn batch(statements: Vec<(String, Vec<serde_json::Value>)>) -> Self {
        Self {
            statements: statements
                .into_iter()
                .map(|(sql, params)| Statement { sql, params })
                .collect(),
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
        assert_eq!(req.statements.len(), 1);
        assert_eq!(req.statements[0].sql, "INSERT INTO test VALUES (?)");
        assert_eq!(req.statements[0].params.len(), 1);
    }

    #[test]
    fn test_request_simple() {
        let req = Request::simple("DELETE FROM test");
        assert_eq!(req.statements[0].sql, "DELETE FROM test");
        assert!(req.statements[0].params.is_empty());
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
