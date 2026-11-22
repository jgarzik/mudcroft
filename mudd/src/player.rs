//! Player management for persistent player objects
//!
//! Each account has one player object per universe it visits, at
//! `/players/p-<account>` inside that universe (object ids are scoped per
//! universe). Handles the player object lifecycle:
//! - Create the player object on first connect to a universe
//! - Track last safe location for respawn
//! - Handle disconnect (drop inventory if not in safe zone)
//! - Handle death (drop inventory, respawn)

use std::sync::Arc;

use anyhow::Result;
use tracing::{info, warn};

use crate::objects::{Object, ObjectStore};

/// Manages persistent player objects
pub struct PlayerManager {
    object_store: Arc<ObjectStore>,
}

impl PlayerManager {
    /// Create a new PlayerManager
    pub fn new(object_store: Arc<ObjectStore>) -> Self {
        Self { object_store }
    }

    /// Get player object path for an account (the same in every universe).
    /// Uses 'p-' prefix since UUIDs may start with digits but path segments must start with letters.
    pub fn player_path(account_id: &str) -> String {
        format!("/players/p-{}", account_id)
    }

    /// Get or create the account's player object in a universe.
    pub async fn get_or_create_player(
        &self,
        account_id: &str,
        universe_id: &str,
        username: &str,
    ) -> Result<Object> {
        let player_id = Self::player_path(account_id);

        if let Some(player) = self.object_store.get(universe_id, &player_id).await? {
            info!(
                "Found existing player object: {} in {}",
                player_id, universe_id
            );
            return Ok(player);
        }

        info!("Creating player object: {} in {}", player_id, universe_id);
        let mut player = Object::new_with_owner(&player_id, universe_id, "player", account_id)?;
        player.set_property("name", serde_json::json!(username));
        player.set_property("hp", serde_json::json!(100));
        player.set_property("max_hp", serde_json::json!(100));

        self.object_store.create(&player).await?;

        Ok(player)
    }

    /// Get the spawn location for a player on connect.
    /// Priority: last_safe_location > current safe room > universe portal
    pub async fn get_spawn_location(
        &self,
        player: &Object,
        universe_id: &str,
    ) -> Result<Option<String>> {
        // Check last safe location (verify room still exists)
        if let Some(last_safe) = player.get_string("last_safe_location") {
            if self
                .object_store
                .get(universe_id, last_safe)
                .await?
                .is_some()
            {
                return Ok(Some(last_safe.to_string()));
            }
        }

        // Check if player has a current location (parent_id) that's a valid room
        if let Some(ref parent_id) = player.parent_id {
            if let Some(room) = self.object_store.get(universe_id, parent_id).await? {
                if room.class == "room" && self.is_safe_zone(universe_id, parent_id, player).await?
                {
                    return Ok(Some(parent_id.clone()));
                }
            }
        }

        // Fallback to universe portal
        self.object_store.get_portal(universe_id).await
    }

    /// Check if a room is a safe zone.
    /// Safe zones: rooms with is_portal=true, universe portal room, OR player's workroom
    pub async fn is_safe_zone(
        &self,
        universe_id: &str,
        room_id: &str,
        player: &Object,
    ) -> Result<bool> {
        // Check if room is player's workroom
        if let Some(workroom_id) = player.get_string("workroom_id") {
            if workroom_id == room_id {
                return Ok(true);
            }
        }

        if let Some(room) = self.object_store.get(universe_id, room_id).await? {
            if room.get_bool("is_portal").unwrap_or(false) {
                return Ok(true);
            }
        }

        // Check if this room is the universe portal
        Ok(self.object_store.get_portal(universe_id).await?.as_deref() == Some(room_id))
    }

    /// Update the player's last safe location.
    /// Called when player moves to a safe zone.
    pub async fn update_safe_location(
        &self,
        universe_id: &str,
        player_id: &str,
        room_id: &str,
    ) -> Result<()> {
        // In place: rewriting the whole row could undo HP changed meanwhile
        self.object_store
            .set_properties(
                universe_id,
                player_id,
                &[("last_safe_location", serde_json::json!(room_id))],
            )
            .await
    }

    /// Handle player disconnect.
    /// If not in a safe zone, drops all inventory to the current room.
    pub async fn handle_disconnect(&self, universe_id: &str, player_id: &str) -> Result<()> {
        let player = match self.object_store.get(universe_id, player_id).await? {
            Some(p) => p,
            None => {
                warn!("Player object not found during disconnect: {}", player_id);
                return Ok(());
            }
        };

        let current_room_id = match &player.parent_id {
            Some(room_id) => room_id.clone(),
            None => {
                warn!("Player has no location during disconnect: {}", player_id);
                return Ok(());
            }
        };

        // Check if in safe zone
        if self
            .is_safe_zone(universe_id, &current_room_id, &player)
            .await?
        {
            info!(
                "Player {} disconnected in safe zone {}, inventory persists",
                player_id, current_room_id
            );
            return Ok(());
        }

        // Not in safe zone - drop all inventory
        info!(
            "Player {} disconnected outside safe zone, dropping inventory",
            player_id
        );
        self.drop_inventory(universe_id, player_id, &current_room_id)
            .await?;

        // Move player to last safe location or leave in place
        if let Some(last_safe) = player.get_string("last_safe_location") {
            if self
                .object_store
                .get(universe_id, last_safe)
                .await?
                .is_some()
            {
                self.object_store
                    .move_object(universe_id, player_id, Some(last_safe))
                    .await?;
                info!("Moved disconnected player {} to safe location", player_id);
            }
        }

        Ok(())
    }

    /// Handle player death.
    /// Drops all inventory to current room and respawns player.
    /// Returns the respawn room ID.
    pub async fn handle_death(&self, player_id: &str, universe_id: &str) -> Result<String> {
        let player = match self.object_store.get(universe_id, player_id).await? {
            Some(p) => p,
            None => {
                return Err(anyhow::anyhow!(
                    "Player object not found during death: {}",
                    player_id
                ));
            }
        };

        let current_room_id = player.parent_id.clone().unwrap_or_default();

        // Drop all inventory to current room
        if !current_room_id.is_empty() {
            self.drop_inventory(universe_id, player_id, &current_room_id)
                .await?;
        }

        // Determine respawn location
        let respawn_room_id = self.get_respawn_location(&player, universe_id).await?;

        // Restore HP and move to respawn, in place and atomically
        let max_hp = player.get_i64("max_hp").unwrap_or(100);
        self.object_store
            .execute_batch(vec![
                ObjectStore::set_properties_statement(
                    universe_id,
                    player_id,
                    &[("hp", serde_json::json!(max_hp))],
                )?,
                ObjectStore::set_parent_statement(universe_id, player_id, Some(&respawn_room_id)),
            ])
            .await?;

        info!(
            "Player {} died and respawned at {}",
            player_id, respawn_room_id
        );

        Ok(respawn_room_id)
    }

    /// Get respawn location for death.
    /// Priority: workroom > universe portal
    pub async fn get_respawn_location(&self, player: &Object, universe_id: &str) -> Result<String> {
        // Check workroom
        if let Some(workroom_id) = player.get_string("workroom_id") {
            if self
                .object_store
                .get(universe_id, workroom_id)
                .await?
                .is_some()
            {
                return Ok(workroom_id.to_string());
            }
        }

        // Fallback to universe portal
        if let Some(portal_id) = self.object_store.get_portal(universe_id).await? {
            return Ok(portal_id);
        }

        Err(anyhow::anyhow!(
            "No respawn location found for universe {}",
            universe_id
        ))
    }

    /// Drop all inventory items from a player to a room.
    pub async fn drop_inventory(
        &self,
        universe_id: &str,
        player_id: &str,
        room_id: &str,
    ) -> Result<()> {
        let items = self
            .object_store
            .get_contents(universe_id, player_id)
            .await?;

        for item in items {
            // Skip items that shouldn't be dropped (though they shouldn't be in inventory anyway)
            if item.get_bool("fixed").unwrap_or(false) {
                continue;
            }

            self.object_store
                .move_object(universe_id, &item.id, Some(room_id))
                .await?;
            info!("Dropped {} from {} to {}", item.id, player_id, room_id);
        }

        Ok(())
    }

    /// Move player to a new room and update safe location if needed.
    /// Returns true if the new room is a safe zone.
    pub async fn move_player(
        &self,
        universe_id: &str,
        player_id: &str,
        new_room_id: &str,
    ) -> Result<bool> {
        // Move player object
        self.object_store
            .move_object(universe_id, player_id, Some(new_room_id))
            .await?;

        // Check if new room is safe zone
        let player = match self.object_store.get(universe_id, player_id).await? {
            Some(p) => p,
            None => return Ok(false),
        };

        let is_safe = self.is_safe_zone(universe_id, new_room_id, &player).await?;

        if is_safe {
            self.update_safe_location(universe_id, player_id, new_room_id)
                .await?;
        }

        Ok(is_safe)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_player_path() {
        assert_eq!(PlayerManager::player_path("abc-123"), "/players/p-abc-123");
        // Verify UUIDs work (start with digits)
        assert_eq!(
            PlayerManager::player_path("78555ec2-d61d-44db-9ee1-b02ba9757182"),
            "/players/p-78555ec2-d61d-44db-9ee1-b02ba9757182"
        );
    }
}
