//! Action registry for contextual verbs
//!
//! Objects can register actions (verbs) that players can use when in context.
//! For example, a lever might add a "pull" action when a player enters the room.
//! Object ids are scoped per universe, so every key includes the universe.

use std::collections::BTreeMap;
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

/// (universe_id, room or object id)
type Key = (String, String);

fn key(universe_id: &str, id: &str) -> Key {
    (universe_id.to_string(), id.to_string())
}

/// Registry for contextual actions
#[derive(Debug, Default)]
pub struct ActionRegistry {
    /// Room-scoped actions: (universe, room) -> verb -> list of actions
    room_actions: RwLock<BTreeMap<Key, BTreeMap<String, Vec<Action>>>>,
    /// Object-scoped actions: (universe, object) -> verb -> action
    object_actions: RwLock<BTreeMap<Key, BTreeMap<String, Action>>>,
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
    pub async fn add_room_action(&self, universe_id: &str, room_id: &str, action: Action) {
        let mut actions = self.room_actions.write().await;
        let room_entry = actions.entry(key(universe_id, room_id)).or_default();
        let verb_entry = room_entry.entry(action.verb.clone()).or_default();

        // Don't add duplicate
        if !verb_entry.iter().any(|a| a.object_id == action.object_id) {
            verb_entry.push(action);
        }
    }

    /// Remove an action from a room context
    pub async fn remove_room_action(
        &self,
        universe_id: &str,
        room_id: &str,
        verb: &str,
        object_id: &str,
    ) {
        let mut actions = self.room_actions.write().await;
        if let Some(room_entry) = actions.get_mut(&key(universe_id, room_id)) {
            if let Some(verb_entry) = room_entry.get_mut(verb) {
                verb_entry.retain(|a| a.object_id != object_id);
            }
        }
    }

    /// Get actions for a verb in a room
    pub async fn get_room_actions(
        &self,
        universe_id: &str,
        room_id: &str,
        verb: &str,
    ) -> Vec<Action> {
        let actions = self.room_actions.read().await;
        actions
            .get(&key(universe_id, room_id))
            .and_then(|r| r.get(verb))
            .cloned()
            .unwrap_or_default()
    }

    /// Add an action for an object (inventory item)
    pub async fn add_object_action(&self, universe_id: &str, object_id: &str, action: Action) {
        let mut actions = self.object_actions.write().await;
        let obj_entry = actions.entry(key(universe_id, object_id)).or_default();
        obj_entry.insert(action.verb.clone(), action);
    }

    /// Remove an action from an object
    pub async fn remove_object_action(&self, universe_id: &str, object_id: &str, verb: &str) {
        let mut actions = self.object_actions.write().await;
        if let Some(obj_entry) = actions.get_mut(&key(universe_id, object_id)) {
            obj_entry.remove(verb);
        }
    }

    /// Get action for a verb on an object
    pub async fn get_object_action(
        &self,
        universe_id: &str,
        object_id: &str,
        verb: &str,
    ) -> Option<Action> {
        let actions = self.object_actions.read().await;
        actions
            .get(&key(universe_id, object_id))
            .and_then(|o| o.get(verb))
            .cloned()
    }

    /// Clear all actions for a room (e.g., when room is destroyed)
    pub async fn clear_room(&self, universe_id: &str, room_id: &str) {
        self.room_actions
            .write()
            .await
            .remove(&key(universe_id, room_id));
    }

    /// Clear all actions by an object in a universe (e.g., when it is destroyed)
    pub async fn clear_by_object(&self, universe_id: &str, object_id: &str) {
        // Remove from the universe's room actions
        let mut room_actions = self.room_actions.write().await;
        for ((universe, _), room_entry) in room_actions.iter_mut() {
            if universe != universe_id {
                continue;
            }
            for verb_entry in room_entry.values_mut() {
                verb_entry.retain(|a| a.object_id != object_id);
            }
        }
        drop(room_actions);

        // Remove object actions
        self.object_actions
            .write()
            .await
            .remove(&key(universe_id, object_id));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn action(verb: &str, object_id: &str, method: &str) -> Action {
        Action {
            verb: verb.to_string(),
            object_id: object_id.to_string(),
            method: method.to_string(),
        }
    }

    #[tokio::test]
    async fn test_room_actions() {
        let registry = ActionRegistry::new();
        registry
            .add_room_action("u", "room_1", action("pull", "lever_1", "on_pull"))
            .await;

        let actions = registry.get_room_actions("u", "room_1", "pull").await;
        assert_eq!(actions.len(), 1);
        assert_eq!(actions[0].object_id, "lever_1");

        registry
            .remove_room_action("u", "room_1", "pull", "lever_1")
            .await;
        assert!(registry
            .get_room_actions("u", "room_1", "pull")
            .await
            .is_empty());
    }

    #[tokio::test]
    async fn test_object_actions() {
        let registry = ActionRegistry::new();
        registry
            .add_object_action("u", "potion_1", action("drink", "potion_1", "on_drink"))
            .await;

        let found = registry.get_object_action("u", "potion_1", "drink").await;
        assert_eq!(found.unwrap().method, "on_drink");
    }

    #[tokio::test]
    async fn test_clear_by_object() {
        let registry = ActionRegistry::new();
        registry
            .add_room_action("u", "room_1", action("pull", "lever_1", "on_pull"))
            .await;
        registry
            .add_room_action("u", "room_1", action("push", "lever_1", "on_push"))
            .await;

        registry.clear_by_object("u", "lever_1").await;
        assert!(registry
            .get_room_actions("u", "room_1", "pull")
            .await
            .is_empty());
    }

    /// The same object id in two universes has independent actions
    #[tokio::test]
    async fn test_actions_are_per_universe() {
        let registry = ActionRegistry::new();
        registry
            .add_object_action("a", "/items/x", action("rub", "/items/x", "rub"))
            .await;
        registry
            .add_room_action("a", "/rooms/r", action("pull", "/items/x", "pull"))
            .await;

        assert!(registry
            .get_object_action("b", "/items/x", "rub")
            .await
            .is_none());
        assert!(registry
            .get_room_actions("b", "/rooms/r", "pull")
            .await
            .is_empty());

        // Clearing the object in universe b leaves universe a's actions
        registry.clear_by_object("b", "/items/x").await;
        assert!(registry
            .get_object_action("a", "/items/x", "rub")
            .await
            .is_some());
        assert_eq!(
            registry
                .get_room_actions("a", "/rooms/r", "pull")
                .await
                .len(),
            1
        );
    }
}
