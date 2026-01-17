//! Action registry for contextual verbs
//!
//! Objects can register actions (verbs) that players can use when in context.
//! For example, a lever might add a "pull" action when a player enters the room.

use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

/// An action registered by an object
#[derive(Debug, Clone)]
pub struct Action {
    /// The verb that triggers this action
    pub verb: String,
    /// The object that registered this action
    pub object_id: String,
    /// The method to call on the object
    pub method: String,
}

/// Registry for contextual actions
/// Actions are scoped per-room (room_id -> verb -> action)
#[derive(Debug, Default)]
pub struct ActionRegistry {
    /// Room-scoped actions: room_id -> verb -> list of actions
    room_actions: RwLock<HashMap<String, HashMap<String, Vec<Action>>>>,
    /// Object-scoped actions: object_id -> verb -> action (for inventory items)
    object_actions: RwLock<HashMap<String, HashMap<String, Action>>>,
}

impl ActionRegistry {
    /// Create a new action registry
    pub fn new() -> Self {
        Self::default()
    }

    /// Wrap in Arc for sharing
    pub fn shared() -> Arc<Self> {
        Arc::new(Self::new())
    }

    /// Add an action for a room context
    pub async fn add_room_action(&self, room_id: &str, action: Action) {
        let mut actions = self.room_actions.write().await;
        let room_entry = actions.entry(room_id.to_string()).or_default();
        let verb_entry = room_entry.entry(action.verb.clone()).or_default();

        // Don't add duplicate
        if !verb_entry.iter().any(|a| a.object_id == action.object_id) {
            verb_entry.push(action);
        }
    }

    /// Remove an action from a room context
    pub async fn remove_room_action(&self, room_id: &str, verb: &str, object_id: &str) {
        let mut actions = self.room_actions.write().await;
        if let Some(room_entry) = actions.get_mut(room_id) {
            if let Some(verb_entry) = room_entry.get_mut(verb) {
                verb_entry.retain(|a| a.object_id != object_id);
            }
        }
    }

    /// Get actions for a verb in a room
    pub async fn get_room_actions(&self, room_id: &str, verb: &str) -> Vec<Action> {
        let actions = self.room_actions.read().await;
        actions
            .get(room_id)
            .and_then(|r| r.get(verb))
            .cloned()
            .unwrap_or_default()
    }

    /// Add an action for an object (inventory item)
    pub async fn add_object_action(&self, object_id: &str, action: Action) {
        let mut actions = self.object_actions.write().await;
        let obj_entry = actions.entry(object_id.to_string()).or_default();
        obj_entry.insert(action.verb.clone(), action);
    }

    /// Remove an action from an object
    pub async fn remove_object_action(&self, object_id: &str, verb: &str) {
        let mut actions = self.object_actions.write().await;
        if let Some(obj_entry) = actions.get_mut(object_id) {
            obj_entry.remove(verb);
        }
    }

    /// Get action for a verb on an object
    pub async fn get_object_action(&self, object_id: &str, verb: &str) -> Option<Action> {
        let actions = self.object_actions.read().await;
        actions.get(object_id).and_then(|o| o.get(verb)).cloned()
    }

    /// Clear all actions for a room (e.g., when room is destroyed)
    pub async fn clear_room(&self, room_id: &str) {
        let mut actions = self.room_actions.write().await;
        actions.remove(room_id);
    }

    /// Clear all actions by an object (e.g., when object is destroyed)
    pub async fn clear_by_object(&self, object_id: &str) {
        // Remove from room actions
        let mut room_actions = self.room_actions.write().await;
        for room_entry in room_actions.values_mut() {
            for verb_entry in room_entry.values_mut() {
                verb_entry.retain(|a| a.object_id != object_id);
            }
        }
        drop(room_actions);

        // Remove object actions
        let mut obj_actions = self.object_actions.write().await;
        obj_actions.remove(object_id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_room_actions() {
        let registry = ActionRegistry::new();

        let action = Action {
            verb: "pull".to_string(),
            object_id: "lever_1".to_string(),
            method: "on_pull".to_string(),
        };

        registry.add_room_action("room_1", action.clone()).await;

        let actions = registry.get_room_actions("room_1", "pull").await;
        assert_eq!(actions.len(), 1);
        assert_eq!(actions[0].object_id, "lever_1");

        registry.remove_room_action("room_1", "pull", "lever_1").await;
        let actions = registry.get_room_actions("room_1", "pull").await;
        assert!(actions.is_empty());
    }

    #[tokio::test]
    async fn test_object_actions() {
        let registry = ActionRegistry::new();

        let action = Action {
            verb: "drink".to_string(),
            object_id: "potion_1".to_string(),
            method: "on_drink".to_string(),
        };

        registry.add_object_action("potion_1", action).await;

        let found = registry.get_object_action("potion_1", "drink").await;
        assert!(found.is_some());
        assert_eq!(found.unwrap().method, "on_drink");
    }

    #[tokio::test]
    async fn test_clear_by_object() {
        let registry = ActionRegistry::new();

        let action1 = Action {
            verb: "pull".to_string(),
            object_id: "lever_1".to_string(),
            method: "on_pull".to_string(),
        };

        let action2 = Action {
            verb: "push".to_string(),
            object_id: "lever_1".to_string(),
            method: "on_push".to_string(),
        };

        registry.add_room_action("room_1", action1).await;
        registry.add_room_action("room_1", action2).await;

        registry.clear_by_object("lever_1").await;

        let actions = registry.get_room_actions("room_1", "pull").await;
        assert!(actions.is_empty());
    }
}
