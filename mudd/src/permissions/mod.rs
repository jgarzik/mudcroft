//! Permission and access control system
//!
//! Implements LPC-style access levels:
//! - Player: Normal player, can only interact with non-fixed objects
//! - Builder: Can create/modify objects in assigned regions
//! - Wizard: Can modify any object, bypass fixed restrictions
//! - Admin: Full universe administration
//! - Owner: Universe owner, can grant admin access

use std::collections::{HashMap, HashSet};
use std::str::FromStr;
use std::sync::Arc;
use tokio::sync::RwLock;

use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;

/// Access levels for MUD users
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, Default,
)]
#[repr(u8)]
pub enum AccessLevel {
    /// Normal player - can interact with non-fixed objects
    #[default]
    Player = 0,
    /// Builder - can create/modify objects in assigned regions
    Builder = 1,
    /// Wizard - full object control, can bypass fixed restrictions
    Wizard = 2,
    /// Admin - universe administration (config, credits, etc.)
    Admin = 3,
    /// Owner - universe owner, can grant admin access
    Owner = 4,
}

impl FromStr for AccessLevel {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "player" => Ok(AccessLevel::Player),
            "builder" => Ok(AccessLevel::Builder),
            "wizard" => Ok(AccessLevel::Wizard),
            "admin" => Ok(AccessLevel::Admin),
            "owner" => Ok(AccessLevel::Owner),
            _ => Ok(AccessLevel::Player), // Default to player for unknown
        }
    }
}

impl AccessLevel {
    /// Check if this level can perform builder actions
    pub fn can_build(&self) -> bool {
        *self >= AccessLevel::Builder
    }

    /// Check if this level can bypass fixed object restrictions
    pub fn can_bypass_fixed(&self) -> bool {
        *self >= AccessLevel::Wizard
    }

    /// Check if this level can administer the universe
    pub fn can_admin(&self) -> bool {
        *self >= AccessLevel::Admin
    }

    /// Check if this level can grant admin access
    pub fn can_grant_admin(&self) -> bool {
        *self >= AccessLevel::Owner
    }
}

/// Actions that can be permission-checked
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Action {
    /// Read an object's properties
    Read,
    /// Modify an object's properties
    Modify,
    /// Move an object
    Move,
    /// Delete an object
    Delete,
    /// Create an object
    Create,
    /// Execute code on an object
    Execute,
    /// Administer universe settings
    AdminConfig,
    /// Grant credits to players
    GrantCredits,
}

/// Result of a permission check
#[derive(Debug, Clone)]
pub enum PermissionResult {
    /// Action is allowed
    Allowed,
    /// Action denied with reason
    Denied(String),
}

impl PermissionResult {
    pub fn is_allowed(&self) -> bool {
        matches!(self, PermissionResult::Allowed)
    }
}

/// User information for permission checks
#[derive(Debug, Clone)]
pub struct UserContext {
    /// User's account ID
    pub account_id: String,
    /// User's access level
    pub access_level: AccessLevel,
    /// Regions the user can build in (for builders)
    pub assigned_regions: HashSet<String>,
}

impl UserContext {
    /// Create a new player context
    pub fn player(account_id: &str) -> Self {
        Self {
            account_id: account_id.to_string(),
            access_level: AccessLevel::Player,
            assigned_regions: HashSet::new(),
        }
    }

    /// Create a new builder context with assigned regions
    pub fn builder(account_id: &str, regions: HashSet<String>) -> Self {
        Self {
            account_id: account_id.to_string(),
            access_level: AccessLevel::Builder,
            assigned_regions: regions,
        }
    }

    /// Create a wizard context
    pub fn wizard(account_id: &str) -> Self {
        Self {
            account_id: account_id.to_string(),
            access_level: AccessLevel::Wizard,
            assigned_regions: HashSet::new(),
        }
    }

    /// Create an admin context
    pub fn admin(account_id: &str) -> Self {
        Self {
            account_id: account_id.to_string(),
            access_level: AccessLevel::Admin,
            assigned_regions: HashSet::new(),
        }
    }

    /// Create an owner context
    pub fn owner(account_id: &str) -> Self {
        Self {
            account_id: account_id.to_string(),
            access_level: AccessLevel::Owner,
            assigned_regions: HashSet::new(),
        }
    }
}

/// Object information for permission checks
#[derive(Debug, Clone)]
pub struct ObjectContext {
    /// Object's ID
    pub object_id: String,
    /// Object's owner account ID
    pub owner_id: Option<String>,
    /// Whether the object is fixed (cannot be moved by players)
    pub is_fixed: bool,
    /// Region the object belongs to
    pub region_id: Option<String>,
}

/// Permission manager for a universe
#[derive(Debug)]
pub struct PermissionManager {
    /// User access levels by account ID (in-memory cache)
    user_levels: RwLock<HashMap<String, AccessLevel>>,
    /// Builder region assignments
    builder_regions: RwLock<HashMap<String, HashSet<String>>>,
    /// Database pool for fallback lookups
    db_pool: Option<SqlitePool>,
}

impl Default for PermissionManager {
    fn default() -> Self {
        Self {
            user_levels: RwLock::new(HashMap::new()),
            builder_regions: RwLock::new(HashMap::new()),
            db_pool: None,
        }
    }
}

impl PermissionManager {
    /// Create a new permission manager
    pub fn new() -> Self {
        Self::default()
    }

    /// Create a new permission manager with database pool for fallback lookups
    pub fn with_db(db_pool: SqlitePool) -> Self {
        Self {
            user_levels: RwLock::new(HashMap::new()),
            builder_regions: RwLock::new(HashMap::new()),
            db_pool: Some(db_pool),
        }
    }

    /// Create a shared instance
    pub fn shared() -> Arc<Self> {
        Arc::new(Self::new())
    }

    /// Create a shared instance with database pool
    pub fn shared_with_db(db_pool: SqlitePool) -> Arc<Self> {
        Arc::new(Self::with_db(db_pool))
    }

    /// Set a user's access level
    pub async fn set_access_level(&self, account_id: &str, level: AccessLevel) {
        // Persist to database if pool is available
        if let Some(ref pool) = self.db_pool {
            let level_str = match level {
                AccessLevel::Player => "player",
                AccessLevel::Builder => "builder",
                AccessLevel::Wizard => "wizard",
                AccessLevel::Admin => "admin",
                AccessLevel::Owner => "owner",
            };
            if let Err(e) = sqlx::query("UPDATE accounts SET access_level = ? WHERE id = ?")
                .bind(level_str)
                .bind(account_id)
                .execute(pool)
                .await
            {
                tracing::warn!("Failed to persist access level for {}: {}", account_id, e);
            }
        }

        self.user_levels
            .write()
            .await
            .insert(account_id.to_string(), level);
    }

    /// Get a user's access level
    /// Checks in-memory cache first, then falls back to database if available
    pub async fn get_access_level(&self, account_id: &str) -> AccessLevel {
        // Check in-memory cache first
        if let Some(level) = self.user_levels.read().await.get(account_id).copied() {
            return level;
        }

        // Fall back to database lookup if pool is available
        if let Some(ref pool) = self.db_pool {
            if let Ok(Some(row)) =
                sqlx::query_as::<_, (String,)>("SELECT access_level FROM accounts WHERE id = ?")
                    .bind(account_id)
                    .fetch_optional(pool)
                    .await
            {
                let level = AccessLevel::from_str(&row.0).unwrap_or(AccessLevel::Player);
                // Cache for future lookups
                self.user_levels
                    .write()
                    .await
                    .insert(account_id.to_string(), level);
                return level;
            }
        }

        AccessLevel::Player
    }

    /// Assign a region to a builder
    pub async fn assign_region(&self, account_id: &str, region_id: &str) {
        // Persist to database if pool is available
        if let Some(ref pool) = self.db_pool {
            if let Err(e) = sqlx::query(
                "INSERT OR REPLACE INTO builder_regions (account_id, region_id) VALUES (?, ?)",
            )
            .bind(account_id)
            .bind(region_id)
            .execute(pool)
            .await
            {
                tracing::warn!(
                    "Failed to persist region assignment for {}: {}",
                    account_id,
                    e
                );
            }
        }

        let mut regions = self.builder_regions.write().await;
        regions
            .entry(account_id.to_string())
            .or_default()
            .insert(region_id.to_string());
    }

    /// Remove a region assignment from a builder
    pub async fn unassign_region(&self, account_id: &str, region_id: &str) {
        // Remove from database if pool is available
        if let Some(ref pool) = self.db_pool {
            if let Err(e) =
                sqlx::query("DELETE FROM builder_regions WHERE account_id = ? AND region_id = ?")
                    .bind(account_id)
                    .bind(region_id)
                    .execute(pool)
                    .await
            {
                tracing::warn!(
                    "Failed to remove region assignment for {}: {}",
                    account_id,
                    e
                );
            }
        }

        let mut regions = self.builder_regions.write().await;
        if let Some(user_regions) = regions.get_mut(account_id) {
            user_regions.remove(region_id);
        }
    }

    /// Load builder regions from database on startup
    pub async fn load_builder_regions(&self) -> anyhow::Result<()> {
        let Some(ref pool) = self.db_pool else {
            return Ok(());
        };

        let rows: Vec<(String, String)> =
            sqlx::query_as("SELECT account_id, region_id FROM builder_regions")
                .fetch_all(pool)
                .await?;

        let mut regions = self.builder_regions.write().await;
        for (account_id, region_id) in rows {
            regions.entry(account_id).or_default().insert(region_id);
        }

        tracing::debug!("Loaded {} builder region assignments", regions.len());
        Ok(())
    }

    /// Get a builder's assigned regions
    pub async fn get_assigned_regions(&self, account_id: &str) -> HashSet<String> {
        self.builder_regions
            .read()
            .await
            .get(account_id)
            .cloned()
            .unwrap_or_default()
    }

    /// Build a user context for permission checks
    pub async fn get_user_context(&self, account_id: &str) -> UserContext {
        let access_level = self.get_access_level(account_id).await;
        let assigned_regions = self.get_assigned_regions(account_id).await;

        UserContext {
            account_id: account_id.to_string(),
            access_level,
            assigned_regions,
        }
    }

    /// Check if an action is permitted
    pub fn check_permission(
        &self,
        user: &UserContext,
        action: Action,
        target: &ObjectContext,
    ) -> PermissionResult {
        // Owner-level users can do anything
        if user.access_level >= AccessLevel::Owner {
            return PermissionResult::Allowed;
        }

        // Admin-level checks
        match action {
            Action::AdminConfig | Action::GrantCredits => {
                if user.access_level >= AccessLevel::Admin {
                    return PermissionResult::Allowed;
                }
                return PermissionResult::Denied("Requires admin access".to_string());
            }
            _ => {}
        }

        // Wizard-level can do all object operations
        if user.access_level >= AccessLevel::Wizard {
            return PermissionResult::Allowed;
        }

        // Builder-level checks
        if user.access_level >= AccessLevel::Builder {
            // Builders can only modify objects in their assigned regions
            if let Some(ref region_id) = target.region_id {
                if user.assigned_regions.contains(region_id) {
                    return PermissionResult::Allowed;
                }
                return PermissionResult::Denied(format!("Not assigned to region {}", region_id));
            } else {
                // Objects without a region require wizard access to modify
                return PermissionResult::Denied("Object has no region assigned".to_string());
            }
        }

        // Player-level checks
        match action {
            Action::Read | Action::Execute => {
                // Players can read and interact with most objects
                PermissionResult::Allowed
            }
            Action::Move => {
                // Players cannot move fixed objects
                if target.is_fixed {
                    PermissionResult::Denied("Object is fixed and cannot be moved".to_string())
                } else {
                    PermissionResult::Allowed
                }
            }
            Action::Modify | Action::Delete | Action::Create => {
                // Players cannot modify/delete/create objects directly
                // (they can trigger Lua handlers that do this with proper checks)
                PermissionResult::Denied("Requires builder access".to_string())
            }
            _ => PermissionResult::Denied("Action not permitted".to_string()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_access_level_ordering() {
        assert!(AccessLevel::Owner > AccessLevel::Admin);
        assert!(AccessLevel::Admin > AccessLevel::Wizard);
        assert!(AccessLevel::Wizard > AccessLevel::Builder);
        assert!(AccessLevel::Builder > AccessLevel::Player);
    }

    #[test]
    fn test_access_level_capabilities() {
        assert!(!AccessLevel::Player.can_build());
        assert!(AccessLevel::Builder.can_build());
        assert!(AccessLevel::Wizard.can_build());

        assert!(!AccessLevel::Player.can_bypass_fixed());
        assert!(!AccessLevel::Builder.can_bypass_fixed());
        assert!(AccessLevel::Wizard.can_bypass_fixed());

        assert!(!AccessLevel::Wizard.can_admin());
        assert!(AccessLevel::Admin.can_admin());
        assert!(AccessLevel::Owner.can_admin());
    }

    #[tokio::test]
    async fn test_permission_manager() {
        let pm = PermissionManager::new();

        // Default is player
        assert_eq!(pm.get_access_level("user1").await, AccessLevel::Player);

        // Set and get
        pm.set_access_level("user1", AccessLevel::Builder).await;
        assert_eq!(pm.get_access_level("user1").await, AccessLevel::Builder);
    }

    #[tokio::test]
    async fn test_region_assignment() {
        let pm = PermissionManager::new();

        pm.assign_region("builder1", "region_a").await;
        pm.assign_region("builder1", "region_b").await;

        let regions = pm.get_assigned_regions("builder1").await;
        assert!(regions.contains("region_a"));
        assert!(regions.contains("region_b"));
        assert_eq!(regions.len(), 2);

        pm.unassign_region("builder1", "region_a").await;
        let regions = pm.get_assigned_regions("builder1").await;
        assert!(!regions.contains("region_a"));
        assert!(regions.contains("region_b"));
    }

    #[test]
    fn test_player_permissions() {
        let pm = PermissionManager::new();
        let user = UserContext::player("player1");

        let fixed_obj = ObjectContext {
            object_id: "sword1".to_string(),
            owner_id: None,
            is_fixed: true,
            region_id: None,
        };

        let movable_obj = ObjectContext {
            object_id: "sword2".to_string(),
            owner_id: None,
            is_fixed: false,
            region_id: None,
        };

        // Player can read
        assert!(pm
            .check_permission(&user, Action::Read, &fixed_obj)
            .is_allowed());

        // Player cannot move fixed objects
        assert!(!pm
            .check_permission(&user, Action::Move, &fixed_obj)
            .is_allowed());

        // Player can move non-fixed objects
        assert!(pm
            .check_permission(&user, Action::Move, &movable_obj)
            .is_allowed());

        // Player cannot modify
        assert!(!pm
            .check_permission(&user, Action::Modify, &movable_obj)
            .is_allowed());
    }

    #[test]
    fn test_builder_permissions() {
        let pm = PermissionManager::new();

        let mut regions = HashSet::new();
        regions.insert("region_a".to_string());
        let user = UserContext::builder("builder1", regions);

        let obj_in_region = ObjectContext {
            object_id: "room1".to_string(),
            owner_id: None,
            is_fixed: false,
            region_id: Some("region_a".to_string()),
        };

        let obj_outside_region = ObjectContext {
            object_id: "room2".to_string(),
            owner_id: None,
            is_fixed: false,
            region_id: Some("region_b".to_string()),
        };

        // Builder can modify in their region
        assert!(pm
            .check_permission(&user, Action::Modify, &obj_in_region)
            .is_allowed());

        // Builder cannot modify outside their region
        assert!(!pm
            .check_permission(&user, Action::Modify, &obj_outside_region)
            .is_allowed());
    }

    #[test]
    fn test_wizard_permissions() {
        let pm = PermissionManager::new();
        let user = UserContext::wizard("wizard1");

        let fixed_obj = ObjectContext {
            object_id: "statue1".to_string(),
            owner_id: None,
            is_fixed: true,
            region_id: None,
        };

        // Wizard can move fixed objects
        assert!(pm
            .check_permission(&user, Action::Move, &fixed_obj)
            .is_allowed());

        // Wizard can modify anything
        assert!(pm
            .check_permission(&user, Action::Modify, &fixed_obj)
            .is_allowed());

        // But wizard cannot grant credits
        assert!(!pm
            .check_permission(&user, Action::GrantCredits, &fixed_obj)
            .is_allowed());
    }

    #[test]
    fn test_admin_permissions() {
        let pm = PermissionManager::new();
        let user = UserContext::admin("admin1");

        let obj = ObjectContext {
            object_id: "config".to_string(),
            owner_id: None,
            is_fixed: false,
            region_id: None,
        };

        // Admin can administer
        assert!(pm
            .check_permission(&user, Action::AdminConfig, &obj)
            .is_allowed());
        assert!(pm
            .check_permission(&user, Action::GrantCredits, &obj)
            .is_allowed());
    }
}
