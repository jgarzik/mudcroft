//! Action registry for contextual verbs
//!
//! Objects can register actions (verbs) that players can use when in context.
//! For example, a lever might add a "pull" action when a player enters the room.
//! Object ids are scoped per universe, so every key includes the universe.
//!
//! The `actions` table is the truth (written through Raft); the maps here
//! are a cache of it for dispatch, rebuilt when the node becomes primary.

use std::collections::BTreeMap;
use std::sync::Arc;

use serde_json::json;
use sqlx::SqlitePool;
use tokio::sync::RwLock;

use crate::raft::RaftWriter;

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

type Statement = (String, Vec<serde_json::Value>);

/// Registry for contextual actions
#[derive(Default)]
pub struct ActionRegistry {
    /// Room-scoped actions: (universe, room) -> verb -> list of actions
    room_actions: RwLock<BTreeMap<Key, BTreeMap<String, Vec<Action>>>>,
    /// Object-scoped actions: (universe, object) -> verb -> action
    object_actions: RwLock<BTreeMap<Key, BTreeMap<String, Action>>>,
    /// The table (none in unit tests: memory only)
    pool: Option<SqlitePool>,
    raft_writer: Option<Arc<RaftWriter>>,
}

impl std::fmt::Debug for ActionRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ActionRegistry { .. }")
    }
}

impl ActionRegistry {
    /// A memory-only registry (tests)
    pub fn new() -> Self {
        Self::default()
    }

    /// A registry stored in the `actions` table, written through Raft
    pub fn with_db(pool: SqlitePool, raft_writer: Arc<RaftWriter>) -> Self {
        Self {
            pool: Some(pool),
            raft_writer: Some(raft_writer),
            ..Self::default()
        }
    }

    async fn persist(&self, statement: Statement) -> anyhow::Result<()> {
        match self.raft_writer {
            Some(ref raft) => raft.execute_batch(vec![statement]).await,
            None => Ok(()),
        }
    }

    /// Rebuild the cache from the table (on becoming primary)
    pub async fn load_from_db(&self) -> anyhow::Result<()> {
        let Some(ref pool) = self.pool else {
            return Ok(());
        };
        let rows: Vec<(String, String, String, String, String, String)> = sqlx::query_as(
            "SELECT universe_id, scope, scope_id, verb, object_id, method FROM actions",
        )
        .fetch_all(&mut *crate::db::run::reader(pool).await?)
        .await?;
        let mut rooms: BTreeMap<Key, BTreeMap<String, Vec<Action>>> = BTreeMap::new();
        let mut objects: BTreeMap<Key, BTreeMap<String, Action>> = BTreeMap::new();
        for (universe, scope, scope_id, verb, object_id, method) in rows {
            let action = Action {
                verb: verb.clone(),
                object_id,
                method,
            };
            if scope == "room" {
                rooms
                    .entry((universe, scope_id))
                    .or_default()
                    .entry(verb)
                    .or_default()
                    .push(action);
            } else {
                objects
                    .entry((universe, scope_id))
                    .or_default()
                    .insert(verb, action);
            }
        }
        *self.room_actions.write().await = rooms;
        *self.object_actions.write().await = objects;
        Ok(())
    }

    fn upsert(universe_id: &str, scope: &str, scope_id: &str, action: &Action) -> Statement {
        (
            "INSERT OR REPLACE INTO actions (universe_id, scope, scope_id, verb, object_id, method)
             VALUES (?, ?, ?, ?, ?, ?)"
                .to_string(),
            vec![
                json!(universe_id),
                json!(scope),
                json!(scope_id),
                json!(action.verb),
                json!(action.object_id),
                json!(action.method),
            ],
        )
    }

    /// Wrap in Arc for sharing
    pub fn shared() -> Arc<Self> {
        Arc::new(Self::new())
    }

    /// Add an action for a room context
    pub async fn add_room_action(
        &self,
        universe_id: &str,
        room_id: &str,
        action: Action,
    ) -> anyhow::Result<()> {
        self.persist(Self::upsert(universe_id, "room", room_id, &action))
            .await?;
        let mut actions = self.room_actions.write().await;
        let room_entry = actions.entry(key(universe_id, room_id)).or_default();
        let verb_entry = room_entry.entry(action.verb.clone()).or_default();
        verb_entry.retain(|a| a.object_id != action.object_id);
        verb_entry.push(action);
        Ok(())
    }

    /// Remove an action from a room context
    pub async fn remove_room_action(
        &self,
        universe_id: &str,
        room_id: &str,
        verb: &str,
        object_id: &str,
    ) -> anyhow::Result<()> {
        self.persist((
            "DELETE FROM actions WHERE universe_id = ? AND scope = 'room' AND scope_id = ?
             AND verb = ? AND object_id = ?"
                .to_string(),
            vec![
                json!(universe_id),
                json!(room_id),
                json!(verb),
                json!(object_id),
            ],
        ))
        .await?;
        let mut actions = self.room_actions.write().await;
        if let Some(room_entry) = actions.get_mut(&key(universe_id, room_id)) {
            if let Some(verb_entry) = room_entry.get_mut(verb) {
                verb_entry.retain(|a| a.object_id != object_id);
            }
        }
        Ok(())
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

    /// Add (or update) an action for an object (inventory item)
    pub async fn add_object_action(
        &self,
        universe_id: &str,
        object_id: &str,
        action: Action,
    ) -> anyhow::Result<()> {
        self.persist(Self::upsert(universe_id, "object", object_id, &action))
            .await?;
        let mut actions = self.object_actions.write().await;
        let obj_entry = actions.entry(key(universe_id, object_id)).or_default();
        obj_entry.insert(action.verb.clone(), action);
        Ok(())
    }

    /// Remove an action from an object
    pub async fn remove_object_action(
        &self,
        universe_id: &str,
        object_id: &str,
        verb: &str,
    ) -> anyhow::Result<()> {
        self.persist((
            "DELETE FROM actions WHERE universe_id = ? AND scope = 'object' AND scope_id = ?
             AND verb = ?"
                .to_string(),
            vec![json!(universe_id), json!(object_id), json!(verb)],
        ))
        .await?;
        let mut actions = self.object_actions.write().await;
        if let Some(obj_entry) = actions.get_mut(&key(universe_id, object_id)) {
            obj_entry.remove(verb);
        }
        Ok(())
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

    /// Drop the cache (a node that stops being primary; rebuilt from the
    /// table when it becomes primary again)
    pub async fn clear_all(&self) {
        self.room_actions.write().await.clear();
        self.object_actions.write().await.clear();
    }

    /// Drop a room's actions from the cache (the room was deleted; its rows
    /// cascaded)
    pub async fn clear_room(&self, universe_id: &str, room_id: &str) {
        self.room_actions
            .write()
            .await
            .remove(&key(universe_id, room_id));
    }

    /// Drop an object's actions from the cache (it was deleted; its rows
    /// cascaded)
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
            .await
            .unwrap();

        let actions = registry.get_room_actions("u", "room_1", "pull").await;
        assert_eq!(actions.len(), 1);
        assert_eq!(actions[0].object_id, "lever_1");

        registry
            .remove_room_action("u", "room_1", "pull", "lever_1")
            .await
            .unwrap();
        assert!(
            registry
                .get_room_actions("u", "room_1", "pull")
                .await
                .is_empty()
        );
    }

    #[tokio::test]
    async fn test_object_actions() {
        let registry = ActionRegistry::new();
        registry
            .add_object_action("u", "potion_1", action("drink", "potion_1", "on_drink"))
            .await
            .unwrap();

        let found = registry.get_object_action("u", "potion_1", "drink").await;
        assert_eq!(found.unwrap().method, "on_drink");
    }

    #[tokio::test]
    async fn test_clear_by_object() {
        let registry = ActionRegistry::new();
        registry
            .add_room_action("u", "room_1", action("pull", "lever_1", "on_pull"))
            .await
            .unwrap();
        registry
            .add_room_action("u", "room_1", action("push", "lever_1", "on_push"))
            .await
            .unwrap();

        registry.clear_by_object("u", "lever_1").await;
        assert!(
            registry
                .get_room_actions("u", "room_1", "pull")
                .await
                .is_empty()
        );
    }

    /// The same object id in two universes has independent actions
    #[tokio::test]
    async fn test_actions_are_per_universe() {
        let registry = ActionRegistry::new();
        registry
            .add_object_action("a", "/items/x", action("rub", "/items/x", "rub"))
            .await
            .unwrap();
        registry
            .add_room_action("a", "/rooms/r", action("pull", "/items/x", "pull"))
            .await
            .unwrap();

        assert!(
            registry
                .get_object_action("b", "/items/x", "rub")
                .await
                .is_none()
        );
        assert!(
            registry
                .get_room_actions("b", "/rooms/r", "pull")
                .await
                .is_empty()
        );

        // Clearing the object in universe b leaves universe a's actions
        registry.clear_by_object("b", "/items/x").await;
        assert!(
            registry
                .get_object_action("a", "/items/x", "rub")
                .await
                .is_some()
        );
        assert_eq!(
            registry
                .get_room_actions("a", "/rooms/r", "pull")
                .await
                .len(),
            1
        );
    }
}
