//! Messaging system for Lua scripts
//!
//! Collects messages during Lua execution for later delivery.

use std::sync::Arc;
use tokio::sync::RwLock;

/// A message to be delivered after Lua execution
#[derive(Debug, Clone)]
pub enum GameMessage {
    /// Send to a specific player
    Send {
        target_id: String,
        message: String,
    },
    /// Broadcast to all players in a room
    Broadcast {
        room_id: String,
        message: String,
    },
    /// Broadcast to all players in a region
    BroadcastRegion {
        region_id: String,
        message: String,
    },
}

/// Queue for messages generated during Lua execution
#[derive(Debug, Default)]
pub struct MessageQueue {
    messages: RwLock<Vec<GameMessage>>,
}

impl MessageQueue {
    /// Create a new message queue
    pub fn new() -> Self {
        Self::default()
    }

    /// Wrap in Arc for sharing
    pub fn shared() -> Arc<Self> {
        Arc::new(Self::new())
    }

    /// Queue a message for a specific player
    pub async fn send(&self, target_id: &str, message: &str) {
        let mut messages = self.messages.write().await;
        messages.push(GameMessage::Send {
            target_id: target_id.to_string(),
            message: message.to_string(),
        });
    }

    /// Queue a broadcast to a room
    pub async fn broadcast(&self, room_id: &str, message: &str) {
        let mut messages = self.messages.write().await;
        messages.push(GameMessage::Broadcast {
            room_id: room_id.to_string(),
            message: message.to_string(),
        });
    }

    /// Queue a broadcast to a region
    pub async fn broadcast_region(&self, region_id: &str, message: &str) {
        let mut messages = self.messages.write().await;
        messages.push(GameMessage::BroadcastRegion {
            region_id: region_id.to_string(),
            message: message.to_string(),
        });
    }

    /// Drain all messages from the queue
    pub async fn drain(&self) -> Vec<GameMessage> {
        let mut messages = self.messages.write().await;
        std::mem::take(&mut *messages)
    }

    /// Get count of pending messages
    pub async fn len(&self) -> usize {
        self.messages.read().await.len()
    }

    /// Check if queue is empty
    pub async fn is_empty(&self) -> bool {
        self.messages.read().await.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_message_queue() {
        let queue = MessageQueue::new();

        queue.send("player_1", "Hello!").await;
        queue.broadcast("room_1", "Someone arrived.").await;

        assert_eq!(queue.len().await, 2);

        let messages = queue.drain().await;
        assert_eq!(messages.len(), 2);

        match &messages[0] {
            GameMessage::Send { target_id, message } => {
                assert_eq!(target_id, "player_1");
                assert_eq!(message, "Hello!");
            }
            _ => panic!("Expected Send message"),
        }

        match &messages[1] {
            GameMessage::Broadcast { room_id, message } => {
                assert_eq!(room_id, "room_1");
                assert_eq!(message, "Someone arrived.");
            }
            _ => panic!("Expected Broadcast message"),
        }

        assert!(queue.is_empty().await);
    }
}
