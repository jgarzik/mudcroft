//! Messaging system for Lua scripts
//!
//! Collects messages during one Lua execution; the script runner
//! (`api::scripting`) delivers them to connected players afterwards.
//! Each execution gets its own queue, so concurrent scripts never deliver
//! each other's messages.

use std::sync::Arc;

use parking_lot::Mutex;

/// A message to be delivered after Lua execution
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GameMessage {
    /// Send to a specific player
    Send { target_id: String, message: String },
    /// Broadcast to all players in a room, optionally excluding one player
    Broadcast {
        room_id: String,
        message: String,
        exclude_id: Option<String>,
    },
    /// Broadcast to all players in a region
    BroadcastRegion { region_id: String, message: String },
    /// An object was moved by game.move_object: update a connected player's
    /// session and run the containers' on_leave/on_enter hooks
    Moved {
        object_id: String,
        from: Option<String>,
        to: Option<String>,
    },
}

/// Queue for messages generated during Lua execution
#[derive(Debug, Default)]
pub struct MessageQueue {
    messages: Mutex<Vec<GameMessage>>,
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
    pub fn send(&self, target_id: &str, message: &str) {
        self.messages.lock().push(GameMessage::Send {
            target_id: target_id.to_string(),
            message: message.to_string(),
        });
    }

    /// Queue a broadcast to a room, optionally excluding one player
    pub fn broadcast(&self, room_id: &str, message: &str, exclude_id: Option<&str>) {
        self.messages.lock().push(GameMessage::Broadcast {
            room_id: room_id.to_string(),
            message: message.to_string(),
            exclude_id: exclude_id.map(str::to_string),
        });
    }

    /// Queue a broadcast to a region
    pub fn broadcast_region(&self, region_id: &str, message: &str) {
        self.messages.lock().push(GameMessage::BroadcastRegion {
            region_id: region_id.to_string(),
            message: message.to_string(),
        });
    }

    /// Queue a move notification (see GameMessage::Moved)
    pub fn moved(&self, object_id: &str, from: Option<&str>, to: Option<&str>) {
        self.messages.lock().push(GameMessage::Moved {
            object_id: object_id.to_string(),
            from: from.map(str::to_string),
            to: to.map(str::to_string),
        });
    }

    /// Drain all messages from the queue
    pub fn drain(&self) -> Vec<GameMessage> {
        std::mem::take(&mut *self.messages.lock())
    }

    /// Get count of pending messages
    pub fn len(&self) -> usize {
        self.messages.lock().len()
    }

    /// Check if queue is empty
    pub fn is_empty(&self) -> bool {
        self.messages.lock().is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_message_queue() {
        let queue = MessageQueue::new();

        queue.send("player_1", "Hello!");
        queue.broadcast("room_1", "Someone arrived.", Some("player_2"));
        queue.broadcast_region("/d/forest", "Thunder rumbles.");

        assert_eq!(queue.len(), 3);

        let messages = queue.drain();
        assert_eq!(
            messages,
            vec![
                GameMessage::Send {
                    target_id: "player_1".into(),
                    message: "Hello!".into(),
                },
                GameMessage::Broadcast {
                    room_id: "room_1".into(),
                    message: "Someone arrived.".into(),
                    exclude_id: Some("player_2".into()),
                },
                GameMessage::BroadcastRegion {
                    region_id: "/d/forest".into(),
                    message: "Thunder rumbles.".into(),
                },
            ]
        );

        assert!(queue.is_empty());
    }
}
