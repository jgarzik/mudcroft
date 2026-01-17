//! Integration Test Harness
//!
//! Professional-grade test infrastructure for HemiMUD:
//! - `TestServer` - Spawns a real server on random port with in-memory DB
//! - `TestClient` - Role-based authenticated WebSocket client
//! - `TestWorld` - Pre-configured universe with regions and rooms
//!
//! # Example
//!
//! ```rust,ignore
//! use harness::{TestServer, Role};
//!
//! #[tokio::test]
//! async fn test_player_movement() {
//!     let server = TestServer::start().await.unwrap();
//!
//!     let mut player = server.connect_as(Role::Player {
//!         username: "hero".into()
//!     }).await.unwrap();
//!
//!     player.command("look").await.unwrap();
//!     let output = player.expect("output").await.unwrap();
//!     assert!(output["text"].as_str().unwrap().contains("Spawn Room"));
//! }
//! ```

#![allow(dead_code)]

mod client;
mod server;
mod world;

// Primary exports
pub use client::Role;
#[allow(unused_imports)]
pub use client::TestClient;
#[allow(unused_imports)]
pub use server::{MuddTest, RawWsClient, TestServer};
#[allow(unused_imports)]
pub use world::TestWorld;

// Backward compatibility alias - WsClient now points to RawWsClient
#[allow(unused)]
pub type WsClient = RawWsClient;
