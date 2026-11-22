//! Permission and access control system
//!
//! Implements LPMud-style path-based access levels:
//! - Player: Normal player, can only interact with non-fixed objects
//! - Builder: Can create/modify objects under granted path prefixes
//! - Wizard: Can modify any object, bypass fixed restrictions (like UNIX root)
//! - Admin: Full universe administration
//! - Owner: Universe owner, can grant admin access
//!
//! Permission check order (first match wins):
//! 0. AdminConfig/GrantCredits need Admin+; StoreCode needs Wizard+
//! 1. Wizard+ bypass: access_level >= Wizard → Allowed
//! 2. Owner check: object.owner_id == user.account_id → Allowed
//! 3. Path grant: any grant that covers object.id on a `/` boundary
//!    (PathGrant::covers_path) → Allowed
//! 4. Player actions: Read/Execute/Move-non-fixed → Allowed
//! 5. Default: Denied

use std::collections::HashMap;
use std::str::FromStr;
use std::sync::Arc;
use tokio::sync::RwLock;

use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;

use crate::raft::RaftWriter;

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
    /// The level's name as stored in the database
    pub fn as_str(&self) -> &'static str {
        match self {
            AccessLevel::Player => "player",
            AccessLevel::Builder => "builder",
            AccessLevel::Wizard => "wizard",
            AccessLevel::Admin => "admin",
            AccessLevel::Owner => "owner",
        }
    }

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

    /// Whether a caller at this level may change an account currently at
    /// `target_current` to `new_level`.
    ///
    /// Owners may set any level. Otherwise the caller must be admin+, the
    /// target must be strictly below the caller (no changing peers or
    /// superiors), and admin/owner may only be granted by an owner.
    pub fn can_set_access_level(
        &self,
        target_current: AccessLevel,
        new_level: AccessLevel,
    ) -> bool {
        if self.can_grant_admin() {
            return true;
        }
        self.can_admin() && target_current < *self && new_level < AccessLevel::Admin
    }

    /// Whether a caller at this level may act as (impersonate) an account at
    /// `target` level. Only wizards+ may switch actors, and only to accounts
    /// strictly below their own level, so impersonation never gains power.
    pub fn can_act_as(&self, target: AccessLevel) -> bool {
        self.can_bypass_fixed() && target < *self
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
    /// Store code (wizard+ only)
    StoreCode,
    /// Administer universe settings
    AdminConfig,
    /// Grant credits to players
    GrantCredits,
}

/// A path-based permission grant allowing access to objects under a path prefix
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PathGrant {
    /// Unique grant ID
    pub id: String,
    /// Universe this grant applies to
    pub universe_id: String,
    /// Account that received the grant
    pub grantee_id: String,
    /// Path prefix (e.g., "/d/forest" grants access to "/d/forest/*")
    pub path_prefix: String,
    /// Whether the grantee can sub-delegate to others
    pub can_delegate: bool,
    /// Account that created this grant
    pub granted_by: String,
    /// When the grant was created
    pub granted_at: String,
}

impl PathGrant {
    /// Create a new path grant
    pub fn new(
        universe_id: &str,
        grantee_id: &str,
        path_prefix: &str,
        can_delegate: bool,
        granted_by: &str,
    ) -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            universe_id: universe_id.to_string(),
            grantee_id: grantee_id.to_string(),
            path_prefix: path_prefix.to_string(),
            can_delegate,
            granted_by: granted_by.to_string(),
            granted_at: chrono::Utc::now().to_rfc3339(),
        }
    }

    /// Check if this grant covers the given object path
    pub fn covers_path(&self, object_path: &str) -> bool {
        // Exact match
        if object_path == self.path_prefix {
            return true;
        }
        // Prefix match: /d/forest covers /d/forest/cave but not /d/forestville
        if object_path.starts_with(&self.path_prefix) {
            let remainder = &object_path[self.path_prefix.len()..];
            remainder.starts_with('/')
        } else {
            false
        }
    }

    /// Check if this grant allows delegating the given subpath
    pub fn can_delegate_path(&self, subpath: &str) -> bool {
        self.can_delegate && self.covers_path(subpath)
    }
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
    /// Universe this context applies to
    pub universe_id: String,
    /// User's access level
    pub access_level: AccessLevel,
    /// Path grants for this user in this universe
    pub path_grants: Vec<PathGrant>,
}

impl UserContext {
    /// Create a new player context
    pub fn player(account_id: &str, universe_id: &str) -> Self {
        Self {
            account_id: account_id.to_string(),
            universe_id: universe_id.to_string(),
            access_level: AccessLevel::Player,
            path_grants: Vec::new(),
        }
    }

    /// Create a new builder context with path grants
    pub fn builder(account_id: &str, universe_id: &str, path_grants: Vec<PathGrant>) -> Self {
        Self {
            account_id: account_id.to_string(),
            universe_id: universe_id.to_string(),
            access_level: AccessLevel::Builder,
            path_grants,
        }
    }

    /// Create a wizard context
    pub fn wizard(account_id: &str, universe_id: &str) -> Self {
        Self {
            account_id: account_id.to_string(),
            universe_id: universe_id.to_string(),
            access_level: AccessLevel::Wizard,
            path_grants: Vec::new(),
        }
    }

    /// Create an admin context
    pub fn admin(account_id: &str, universe_id: &str) -> Self {
        Self {
            account_id: account_id.to_string(),
            universe_id: universe_id.to_string(),
            access_level: AccessLevel::Admin,
            path_grants: Vec::new(),
        }
    }

    /// Create an owner context
    pub fn owner(account_id: &str, universe_id: &str) -> Self {
        Self {
            account_id: account_id.to_string(),
            universe_id: universe_id.to_string(),
            access_level: AccessLevel::Owner,
            path_grants: Vec::new(),
        }
    }

    /// Check if user has a path grant covering the given object path
    pub fn has_path_access(&self, object_path: &str) -> bool {
        self.path_grants.iter().any(|g| g.covers_path(object_path))
    }

    /// Check if user can delegate access to the given path
    pub fn can_delegate_path(&self, path: &str) -> bool {
        // Wizards can delegate any path
        if self.access_level >= AccessLevel::Wizard {
            return true;
        }
        // Builders can only delegate subpaths of their grants with can_delegate=true
        self.path_grants.iter().any(|g| g.can_delegate_path(path))
    }
}

/// Object information for permission checks
#[derive(Debug, Clone)]
pub struct ObjectContext {
    /// Object's path-based ID (e.g., "/d/forest/cave")
    pub object_id: String,
    /// Object's owner account ID (creator)
    pub owner_id: Option<String>,
    /// Whether the object is fixed (cannot be moved by players)
    pub is_fixed: bool,
}

/// Permission manager for all universes.
///
/// An account's effective level in a universe is the highest of:
/// - its global level (`accounts.access_level`), but only admin or owner;
///   lower global levels mean nothing
/// - its member level in that universe (`universe_members`)
/// - owner, if it owns the universe (`universes.owner_id`)
pub struct PermissionManager {
    /// Global account levels (`accounts.access_level`) by account ID
    global_levels: RwLock<HashMap<String, AccessLevel>>,
    /// Member levels by (universe_id, account_id)
    member_levels: RwLock<HashMap<(String, String), AccessLevel>>,
    /// Universe owners by universe ID
    universe_owners: RwLock<HashMap<String, String>>,
    /// Path grants by (universe_id, grantee_id) -> Vec<PathGrant>
    path_grants: RwLock<HashMap<(String, String), Vec<PathGrant>>>,
    /// Database pool for fallback lookups
    db_pool: Option<SqlitePool>,
    /// Raft writer for consensus
    raft_writer: Option<Arc<RaftWriter>>,
}

impl std::fmt::Debug for PermissionManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PermissionManager")
            .field("db_pool", &self.db_pool.is_some())
            .field("raft_writer", &self.raft_writer.is_some())
            .finish()
    }
}

impl Default for PermissionManager {
    fn default() -> Self {
        Self {
            global_levels: RwLock::new(HashMap::new()),
            member_levels: RwLock::new(HashMap::new()),
            universe_owners: RwLock::new(HashMap::new()),
            path_grants: RwLock::new(HashMap::new()),
            db_pool: None,
            raft_writer: None,
        }
    }
}

impl PermissionManager {
    /// Create a new permission manager
    pub fn new() -> Self {
        Self::default()
    }

    /// Create a new permission manager with database pool and optional raft writer
    pub fn with_db(db_pool: SqlitePool, raft_writer: Option<Arc<RaftWriter>>) -> Self {
        Self {
            global_levels: RwLock::new(HashMap::new()),
            member_levels: RwLock::new(HashMap::new()),
            universe_owners: RwLock::new(HashMap::new()),
            path_grants: RwLock::new(HashMap::new()),
            db_pool: Some(db_pool),
            raft_writer,
        }
    }

    /// Create a shared instance
    pub fn shared() -> Arc<Self> {
        Arc::new(Self::new())
    }

    /// Create a shared instance with database pool and raft writer
    pub fn shared_with_db(db_pool: SqlitePool, raft_writer: Option<Arc<RaftWriter>>) -> Arc<Self> {
        Arc::new(Self::with_db(db_pool, raft_writer))
    }

    /// Set an account's member level in a universe. Admin is the highest
    /// member level; owner comes only from owning the universe (or a global
    /// owner account). The cache changes only after the write succeeds.
    pub async fn set_access_level(
        &self,
        universe_id: &str,
        account_id: &str,
        level: AccessLevel,
    ) -> anyhow::Result<()> {
        if level > AccessLevel::Admin {
            anyhow::bail!("owner is not a member level");
        }
        let sql = "INSERT OR REPLACE INTO universe_members (universe_id, account_id, level)
                   VALUES (?, ?, ?)";
        if let Some(ref raft_writer) = self.raft_writer {
            raft_writer
                .execute(
                    sql,
                    vec![
                        serde_json::json!(universe_id),
                        serde_json::json!(account_id),
                        serde_json::json!(level.as_str()),
                    ],
                )
                .await?;
        } else if let Some(ref pool) = self.db_pool {
            // Direct SQL fallback (unit tests without Raft)
            sqlx::query(sql)
                .bind(universe_id)
                .bind(account_id)
                .bind(level.as_str())
                .execute(pool)
                .await?;
        }

        self.member_levels
            .write()
            .await
            .insert((universe_id.to_string(), account_id.to_string()), level);
        Ok(())
    }

    /// An account's global level (`accounts.access_level`)
    pub async fn get_global_level(&self, account_id: &str) -> AccessLevel {
        if let Some(level) = self.global_levels.read().await.get(account_id).copied() {
            return level;
        }
        let Some(ref pool) = self.db_pool else {
            return AccessLevel::Player;
        };
        let Ok(Some((level,))) =
            sqlx::query_as::<_, (String,)>("SELECT access_level FROM accounts WHERE id = ?")
                .bind(account_id)
                .fetch_optional(pool)
                .await
        else {
            return AccessLevel::Player;
        };
        let level = AccessLevel::from_str(&level).unwrap_or(AccessLevel::Player);
        self.global_levels
            .write()
            .await
            .insert(account_id.to_string(), level);
        level
    }

    async fn get_member_level(&self, universe_id: &str, account_id: &str) -> AccessLevel {
        let key = (universe_id.to_string(), account_id.to_string());
        if let Some(level) = self.member_levels.read().await.get(&key).copied() {
            return level;
        }
        let Some(ref pool) = self.db_pool else {
            return AccessLevel::Player;
        };
        let level = match sqlx::query_as::<_, (String,)>(
            "SELECT level FROM universe_members WHERE universe_id = ? AND account_id = ?",
        )
        .bind(universe_id)
        .bind(account_id)
        .fetch_optional(pool)
        .await
        {
            Ok(Some((level,))) => AccessLevel::from_str(&level).unwrap_or(AccessLevel::Player),
            Ok(None) => AccessLevel::Player,
            // Don't cache a failed lookup
            Err(_) => return AccessLevel::Player,
        };
        self.member_levels.write().await.insert(key, level);
        level
    }

    async fn get_universe_owner(&self, universe_id: &str) -> Option<String> {
        if let Some(owner) = self.universe_owners.read().await.get(universe_id) {
            return Some(owner.clone());
        }
        let pool = self.db_pool.as_ref()?;
        // Cache only a found owner: a missing universe may be created later
        let owner =
            match sqlx::query_as::<_, (String,)>("SELECT owner_id FROM universes WHERE id = ?")
                .bind(universe_id)
                .fetch_optional(pool)
                .await
            {
                Ok(Some((owner,))) => owner,
                Ok(None) | Err(_) => return None,
            };
        self.universe_owners
            .write()
            .await
            .insert(universe_id.to_string(), owner.clone());
        Some(owner)
    }

    /// Whether `actor` outranks `target` in a universe: a strictly higher
    /// effective level there, and, when the target is a global admin or
    /// owner, a strictly higher global level too. A universe's owner is
    /// owner inside it but never gains a global admin's identity.
    pub async fn outranks(&self, universe_id: &str, actor: &str, target: &str) -> bool {
        let actor_level = self.get_access_level(universe_id, actor).await;
        let target_level = self.get_access_level(universe_id, target).await;
        if target_level >= actor_level {
            return false;
        }
        let target_global = self.get_global_level(target).await;
        target_global < AccessLevel::Admin || self.get_global_level(actor).await > target_global
    }

    /// Whether `actor` may act as `target` (impersonate it, or run code as
    /// it): wizard+ in the universe and outranking it (see `outranks`)
    pub async fn can_act_as(&self, universe_id: &str, actor: &str, target: &str) -> bool {
        self.get_access_level(universe_id, actor)
            .await
            .can_bypass_fixed()
            && self.outranks(universe_id, actor, target).await
    }

    /// An account's effective access level in a universe (see the struct
    /// docs for the rule)
    pub async fn get_access_level(&self, universe_id: &str, account_id: &str) -> AccessLevel {
        let global = self.get_global_level(account_id).await;
        let mut level = if global >= AccessLevel::Admin {
            global
        } else {
            AccessLevel::Player
        };
        level = level.max(self.get_member_level(universe_id, account_id).await);
        if self.get_universe_owner(universe_id).await.as_deref() == Some(account_id) {
            level = AccessLevel::Owner;
        }
        level
    }

    /// Grant a path to a user
    ///
    /// Returns the created PathGrant on success, or an error if:
    /// - Grantor doesn't have permission to delegate the path
    /// - Database operation fails
    pub async fn grant_path(
        &self,
        grantor: &UserContext,
        grantee_id: &str,
        universe_id: &str,
        path_prefix: &str,
        can_delegate: bool,
    ) -> anyhow::Result<PathGrant> {
        // Check if grantor can delegate this path
        if !grantor.can_delegate_path(path_prefix) {
            anyhow::bail!(
                "User {} cannot delegate path {}",
                grantor.account_id,
                path_prefix
            );
        }

        let grant = PathGrant::new(
            universe_id,
            grantee_id,
            path_prefix,
            can_delegate,
            &grantor.account_id,
        );

        // Persist via Raft if available, otherwise direct SQL
        if let Some(ref raft_writer) = self.raft_writer {
            raft_writer
                .execute(
                    "INSERT OR REPLACE INTO path_grants (id, universe_id, grantee_id, path_prefix, can_delegate, granted_by, granted_at) VALUES (?, ?, ?, ?, ?, ?, ?)",
                    vec![
                        serde_json::json!(&grant.id),
                        serde_json::json!(&grant.universe_id),
                        serde_json::json!(&grant.grantee_id),
                        serde_json::json!(&grant.path_prefix),
                        serde_json::json!(grant.can_delegate),
                        serde_json::json!(&grant.granted_by),
                        serde_json::json!(&grant.granted_at),
                    ],
                )
                .await?;
        } else if let Some(ref pool) = self.db_pool {
            sqlx::query(
                "INSERT OR REPLACE INTO path_grants (id, universe_id, grantee_id, path_prefix, can_delegate, granted_by, granted_at) VALUES (?, ?, ?, ?, ?, ?, ?)",
            )
            .bind(&grant.id)
            .bind(&grant.universe_id)
            .bind(&grant.grantee_id)
            .bind(&grant.path_prefix)
            .bind(grant.can_delegate)
            .bind(&grant.granted_by)
            .bind(&grant.granted_at)
            .execute(pool)
            .await?;
        }

        // Update in-memory cache
        let mut grants = self.path_grants.write().await;
        let key = (universe_id.to_string(), grantee_id.to_string());
        grants.entry(key).or_default().push(grant.clone());

        Ok(grant)
    }

    /// Revoke a path grant by ID
    ///
    /// Returns true if the grant was found and revoked, false if not found
    pub async fn revoke_path(
        &self,
        revoker: &UserContext,
        grant_id: &str,
        universe_id: &str,
    ) -> anyhow::Result<bool> {
        // First, find the grant to check permissions
        let grant = self.get_path_grant_by_id(grant_id, universe_id).await?;
        let Some(grant) = grant else {
            return Ok(false);
        };

        // Check if revoker can revoke this grant:
        // - Wizards can revoke any grant
        // - Original grantor can revoke
        // - Someone with delegation rights on the parent path can revoke
        let can_revoke = revoker.access_level >= AccessLevel::Wizard
            || revoker.account_id == grant.granted_by
            || revoker.can_delegate_path(&grant.path_prefix);

        if !can_revoke {
            anyhow::bail!(
                "User {} cannot revoke grant {}",
                revoker.account_id,
                grant_id
            );
        }

        // Delete from database
        if let Some(ref raft_writer) = self.raft_writer {
            raft_writer
                .execute(
                    "DELETE FROM path_grants WHERE id = ? AND universe_id = ?",
                    vec![serde_json::json!(grant_id), serde_json::json!(universe_id)],
                )
                .await?;
        } else if let Some(ref pool) = self.db_pool {
            sqlx::query("DELETE FROM path_grants WHERE id = ? AND universe_id = ?")
                .bind(grant_id)
                .bind(universe_id)
                .execute(pool)
                .await?;
        }

        // Update in-memory cache
        let mut grants = self.path_grants.write().await;
        let key = (universe_id.to_string(), grant.grantee_id.clone());
        if let Some(user_grants) = grants.get_mut(&key) {
            user_grants.retain(|g| g.id != grant_id);
        }

        Ok(true)
    }

    /// Get a specific path grant by ID
    async fn get_path_grant_by_id(
        &self,
        grant_id: &str,
        universe_id: &str,
    ) -> anyhow::Result<Option<PathGrant>> {
        // Try in-memory cache first
        let grants = self.path_grants.read().await;
        for user_grants in grants.values() {
            for grant in user_grants {
                if grant.id == grant_id && grant.universe_id == universe_id {
                    return Ok(Some(grant.clone()));
                }
            }
        }
        drop(grants);

        // Fall back to database
        let Some(ref pool) = self.db_pool else {
            return Ok(None);
        };

        let row: Option<PathGrantRow> = sqlx::query_as(
            "SELECT id, universe_id, grantee_id, path_prefix, can_delegate, granted_by, granted_at FROM path_grants WHERE id = ? AND universe_id = ?",
        )
        .bind(grant_id)
        .bind(universe_id)
        .fetch_optional(&mut *crate::db::run::reader(pool).await?)
        .await?;

        Ok(row.map(|r| r.into_grant()))
    }

    /// Get all path grants for a user in a universe
    pub async fn get_path_grants(&self, account_id: &str, universe_id: &str) -> Vec<PathGrant> {
        // Check in-memory cache first
        let grants = self.path_grants.read().await;
        let key = (universe_id.to_string(), account_id.to_string());
        if let Some(user_grants) = grants.get(&key) {
            return user_grants.clone();
        }
        drop(grants);

        // Fall back to database
        let Some(ref pool) = self.db_pool else {
            return Vec::new();
        };

        let rows: Vec<PathGrantRow> = match sqlx::query_as(
            "SELECT id, universe_id, grantee_id, path_prefix, can_delegate, granted_by, granted_at FROM path_grants WHERE universe_id = ? AND grantee_id = ?",
        )
        .bind(universe_id)
        .bind(account_id)
        .fetch_all(pool)
        .await
        {
            Ok(rows) => rows,
            Err(_) => return Vec::new(),
        };

        let grants: Vec<PathGrant> = rows.into_iter().map(|r| r.into_grant()).collect();

        // Cache for future lookups
        let mut cache = self.path_grants.write().await;
        cache.insert(key, grants.clone());

        grants
    }

    /// Drop every cached level and owner and reload the path grants: the
    /// caches are rebuilt from the tables on becoming primary
    pub async fn reset(&self) -> anyhow::Result<()> {
        self.global_levels.write().await.clear();
        self.member_levels.write().await.clear();
        self.universe_owners.write().await.clear();
        self.load_path_grants().await
    }

    /// Load all path grants from the database (replaces the cache)
    pub async fn load_path_grants(&self) -> anyhow::Result<()> {
        let Some(ref pool) = self.db_pool else {
            return Ok(());
        };

        let rows: Vec<PathGrantRow> = sqlx::query_as(
            "SELECT id, universe_id, grantee_id, path_prefix, can_delegate, granted_by, granted_at FROM path_grants",
        )
        .fetch_all(&mut *crate::db::run::reader(pool).await?)
        .await?;

        // Replaces the cache
        let mut grants = self.path_grants.write().await;
        grants.clear();
        for row in rows {
            let grant = row.into_grant();
            let key = (grant.universe_id.clone(), grant.grantee_id.clone());
            grants.entry(key).or_default().push(grant);
        }

        tracing::debug!("Loaded {} path grants", grants.len());
        Ok(())
    }

    /// Build a user context for permission checks in a specific universe
    pub async fn get_user_context(&self, account_id: &str, universe_id: &str) -> UserContext {
        let access_level = self.get_access_level(universe_id, account_id).await;
        let path_grants = self.get_path_grants(account_id, universe_id).await;

        UserContext {
            account_id: account_id.to_string(),
            universe_id: universe_id.to_string(),
            access_level,
            path_grants,
        }
    }

    /// Check if a user can access a specific path in a universe
    pub async fn can_access_path(
        &self,
        account_id: &str,
        universe_id: &str,
        object_path: &str,
    ) -> bool {
        let ctx = self.get_user_context(account_id, universe_id).await;
        // Wizards can access anything
        if ctx.access_level >= AccessLevel::Wizard {
            return true;
        }
        // Check path grants
        ctx.has_path_access(object_path)
    }

    /// Check if an action is permitted
    ///
    /// Permission check order (first match wins):
    /// 0. AdminConfig/GrantCredits need Admin+; StoreCode needs Wizard+
    ///    (decided here, before any bypass)
    /// 1. Wizard+ bypass: access_level >= Wizard → Allowed
    /// 2. Owner check: object.owner_id == user.account_id → Allowed
    /// 3. Path grant: any grant that covers object.id on a `/` boundary
    ///    (PathGrant::covers_path) → Allowed
    /// 4. Player actions: Read/Execute/Move-non-fixed → Allowed
    /// 5. Default: Denied
    pub fn check_permission(
        &self,
        user: &UserContext,
        action: Action,
        target: &ObjectContext,
    ) -> PermissionResult {
        // Admin-only actions (Admin+ required)
        match action {
            Action::AdminConfig | Action::GrantCredits => {
                if user.access_level >= AccessLevel::Admin {
                    return PermissionResult::Allowed;
                }
                return PermissionResult::Denied("Requires admin access".to_string());
            }
            Action::StoreCode => {
                // Store code is wizard+ only
                if user.access_level >= AccessLevel::Wizard {
                    return PermissionResult::Allowed;
                }
                return PermissionResult::Denied("Requires wizard access".to_string());
            }
            _ => {}
        }

        // 1. Wizard+ bypass - wizards can do everything (like UNIX root)
        if user.access_level >= AccessLevel::Wizard {
            return PermissionResult::Allowed;
        }

        // 2. Owner check - you can always modify your own creations
        if let Some(ref owner_id) = target.owner_id {
            if owner_id == &user.account_id {
                return PermissionResult::Allowed;
            }
        }

        // 3. Path grant check - builders with matching path grants
        if user.has_path_access(&target.object_id) {
            return PermissionResult::Allowed;
        }

        // 4. Player-level default actions
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
            Action::Modify | Action::Delete | Action::Create | Action::StoreCode => {
                // Players cannot modify/delete/create objects directly
                PermissionResult::Denied("Requires builder access or path grant".to_string())
            }
            _ => PermissionResult::Denied("Action not permitted".to_string()),
        }
    }

    /// Check permission for moving an object between two paths
    /// Requires grants on BOTH source and destination paths
    pub fn check_move_permission(
        &self,
        user: &UserContext,
        source_path: &str,
        dest_path: &str,
        owner_id: Option<&str>,
    ) -> PermissionResult {
        // Wizard+ bypass
        if user.access_level >= AccessLevel::Wizard {
            return PermissionResult::Allowed;
        }

        // Owner can move their own objects anywhere
        if let Some(oid) = owner_id {
            if oid == user.account_id {
                return PermissionResult::Allowed;
            }
        }

        // Need path grants on both source and destination
        let has_source = user.has_path_access(source_path);
        let has_dest = user.has_path_access(dest_path);

        if has_source && has_dest {
            PermissionResult::Allowed
        } else if !has_source {
            PermissionResult::Denied(format!("No access to source path: {}", source_path))
        } else {
            PermissionResult::Denied(format!("No access to destination path: {}", dest_path))
        }
    }

    /// Check permission to create an object at a path
    pub fn check_create_permission(
        &self,
        user: &UserContext,
        object_path: &str,
    ) -> PermissionResult {
        // Wizard+ bypass
        if user.access_level >= AccessLevel::Wizard {
            return PermissionResult::Allowed;
        }

        // Need path grant covering the path
        if user.has_path_access(object_path) {
            PermissionResult::Allowed
        } else {
            PermissionResult::Denied(format!("No access to create at path: {}", object_path))
        }
    }
}

/// Row type for path_grants queries
#[derive(sqlx::FromRow)]
struct PathGrantRow {
    id: String,
    universe_id: String,
    grantee_id: String,
    path_prefix: String,
    can_delegate: bool,
    granted_by: String,
    granted_at: String,
}

impl PathGrantRow {
    fn into_grant(self) -> PathGrant {
        PathGrant {
            id: self.id,
            universe_id: self.universe_id,
            grantee_id: self.grantee_id,
            path_prefix: self.path_prefix,
            can_delegate: self.can_delegate,
            granted_by: self.granted_by,
            granted_at: self.granted_at,
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
    fn test_can_set_access_level() {
        use AccessLevel::*;
        // Owners may do anything, including granting admin/owner
        assert!(Owner.can_set_access_level(Player, Admin));
        assert!(Owner.can_set_access_level(Admin, Player));
        assert!(Owner.can_set_access_level(Player, Owner));
        // Admins may manage levels below admin on accounts below them
        assert!(Admin.can_set_access_level(Player, Wizard));
        assert!(Admin.can_set_access_level(Wizard, Player));
        assert!(!Admin.can_set_access_level(Player, Admin));
        assert!(!Admin.can_set_access_level(Player, Owner));
        assert!(!Admin.can_set_access_level(Admin, Player));
        assert!(!Admin.can_set_access_level(Owner, Player));
        // Below admin: never
        assert!(!Wizard.can_set_access_level(Player, Builder));
        assert!(!Builder.can_set_access_level(Player, Player));
        assert!(!Player.can_set_access_level(Player, Player));
    }

    #[test]
    fn test_can_act_as() {
        use AccessLevel::*;
        assert!(Wizard.can_act_as(Player));
        assert!(Wizard.can_act_as(Builder));
        assert!(!Wizard.can_act_as(Wizard));
        assert!(!Wizard.can_act_as(Admin));
        assert!(Owner.can_act_as(Admin));
        assert!(!Builder.can_act_as(Player));
        assert!(!Player.can_act_as(Player));
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
        assert_eq!(pm.get_access_level("u", "user1").await, AccessLevel::Player);

        // Set and get
        pm.set_access_level("u", "user1", AccessLevel::Builder)
            .await
            .unwrap();
        assert_eq!(
            pm.get_access_level("u", "user1").await,
            AccessLevel::Builder
        );

        // Owner is not a member level
        assert!(pm
            .set_access_level("u", "user1", AccessLevel::Owner)
            .await
            .is_err());
    }

    /// Levels are per universe: member levels apply only in their universe,
    /// global admin/owner apply everywhere, lower global levels nowhere, and
    /// a universe's owner is owner inside it
    #[tokio::test]
    async fn test_levels_are_per_universe() {
        let dir = tempfile::TempDir::new().unwrap();
        let db = crate::db::Database::new(Some(dir.path().join("t.db").to_str().unwrap()))
            .await
            .unwrap();
        let pool = db.pool().clone();
        for (id, level) in [("alice", "player"), ("gadmin", "admin"), ("gwiz", "wizard")] {
            sqlx::query(
                "INSERT INTO accounts (id, username, password_hash, access_level, created_at)
                 VALUES (?, ?, 'x', ?, '2026-01-01')",
            )
            .bind(id)
            .bind(id)
            .bind(level)
            .execute(&pool)
            .await
            .unwrap();
        }
        for (u, owner) in [("ua", "alice"), ("ub", "gadmin")] {
            sqlx::query(
                "INSERT INTO universes (id, name, owner_id, config, created_at)
                 VALUES (?, ?, ?, '{}', '2026-01-01')",
            )
            .bind(u)
            .bind(u)
            .bind(owner)
            .execute(&pool)
            .await
            .unwrap();
        }
        let pm = PermissionManager::with_db(pool, None);

        // alice owns ua but is a plain player in ub
        assert_eq!(pm.get_access_level("ua", "alice").await, AccessLevel::Owner);
        assert_eq!(
            pm.get_access_level("ub", "alice").await,
            AccessLevel::Player
        );

        // A member level applies only in its universe
        pm.set_access_level("ub", "alice", AccessLevel::Wizard)
            .await
            .unwrap();
        let fresh = PermissionManager::with_db(pm.db_pool.clone().unwrap(), None);
        assert_eq!(
            fresh.get_access_level("ub", "alice").await,
            AccessLevel::Wizard
        );

        // Global admin applies everywhere; a global wizard means nothing
        assert_eq!(
            pm.get_access_level("ua", "gadmin").await,
            AccessLevel::Admin
        );
        assert_eq!(pm.get_access_level("ua", "gwiz").await, AccessLevel::Player);

        // alice owns ua, but never outranks a global admin there
        assert!(!pm.outranks("ua", "alice", "gadmin").await);
        assert!(!pm.can_act_as("ua", "alice", "gadmin").await);
        assert!(pm.can_act_as("ua", "alice", "gwiz").await);

        // Looking up a universe before it exists does not lock out its owner
        assert_eq!(pm.get_access_level("uc", "gwiz").await, AccessLevel::Player);
        sqlx::query(
            "INSERT INTO universes (id, name, owner_id, config, created_at)
             VALUES ('uc', 'uc', 'gwiz', '{}', '2026-01-01')",
        )
        .execute(pm.db_pool.as_ref().unwrap())
        .await
        .unwrap();
        assert_eq!(pm.get_access_level("uc", "gwiz").await, AccessLevel::Owner);
    }

    #[test]
    fn test_path_grant_covers_path() {
        let grant = PathGrant::new("universe1", "builder1", "/d/forest", true, "admin");

        // Exact match
        assert!(grant.covers_path("/d/forest"));

        // Subdirectory match
        assert!(grant.covers_path("/d/forest/cave"));
        assert!(grant.covers_path("/d/forest/rooms/cave"));

        // Non-match (different prefix)
        assert!(!grant.covers_path("/d/forestville")); // Not a subdirectory!
        assert!(!grant.covers_path("/d/other"));
        assert!(!grant.covers_path("/rooms/forest"));
    }

    #[test]
    fn test_player_permissions() {
        let pm = PermissionManager::new();
        let user = UserContext::player("player1", "universe1");

        let fixed_obj = ObjectContext {
            object_id: "/items/sword1".to_string(),
            owner_id: None,
            is_fixed: true,
        };

        let movable_obj = ObjectContext {
            object_id: "/items/sword2".to_string(),
            owner_id: None,
            is_fixed: false,
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
    fn test_builder_permissions_with_path_grants() {
        let pm = PermissionManager::new();

        // Builder with a path grant for /d/forest
        let grants = vec![PathGrant::new(
            "universe1",
            "builder1",
            "/d/forest",
            false,
            "admin",
        )];
        let user = UserContext::builder("builder1", "universe1", grants);

        let obj_in_path = ObjectContext {
            object_id: "/d/forest/cave".to_string(),
            owner_id: None,
            is_fixed: false,
        };

        let obj_outside_path = ObjectContext {
            object_id: "/d/desert/oasis".to_string(),
            owner_id: None,
            is_fixed: false,
        };

        // Builder can modify objects in their granted path
        assert!(pm
            .check_permission(&user, Action::Modify, &obj_in_path)
            .is_allowed());

        // Builder cannot modify objects outside their granted path
        assert!(!pm
            .check_permission(&user, Action::Modify, &obj_outside_path)
            .is_allowed());
    }

    #[test]
    fn test_owner_can_modify_own_objects() {
        let pm = PermissionManager::new();

        // Player who owns an object
        let user = UserContext::player("player1", "universe1");

        let owned_obj = ObjectContext {
            object_id: "/player-items/sword".to_string(),
            owner_id: Some("player1".to_string()), // Player owns this
            is_fixed: false,
        };

        let not_owned_obj = ObjectContext {
            object_id: "/player-items/shield".to_string(),
            owner_id: Some("player2".to_string()), // Someone else owns this
            is_fixed: false,
        };

        // Owner can modify their own object
        assert!(pm
            .check_permission(&user, Action::Modify, &owned_obj)
            .is_allowed());

        // Cannot modify someone else's object
        assert!(!pm
            .check_permission(&user, Action::Modify, &not_owned_obj)
            .is_allowed());
    }

    #[test]
    fn test_wizard_permissions() {
        let pm = PermissionManager::new();
        let user = UserContext::wizard("wizard1", "universe1");

        let fixed_obj = ObjectContext {
            object_id: "/rooms/statue".to_string(),
            owner_id: None,
            is_fixed: true,
        };

        // Wizard can move fixed objects
        assert!(pm
            .check_permission(&user, Action::Move, &fixed_obj)
            .is_allowed());

        // Wizard can modify anything
        assert!(pm
            .check_permission(&user, Action::Modify, &fixed_obj)
            .is_allowed());

        // Wizard can store code
        assert!(pm
            .check_permission(&user, Action::StoreCode, &fixed_obj)
            .is_allowed());

        // But wizard cannot grant credits (requires admin)
        assert!(!pm
            .check_permission(&user, Action::GrantCredits, &fixed_obj)
            .is_allowed());
    }

    #[test]
    fn test_admin_permissions() {
        let pm = PermissionManager::new();
        let user = UserContext::admin("admin1", "universe1");

        let obj = ObjectContext {
            object_id: "/config".to_string(),
            owner_id: None,
            is_fixed: false,
        };

        // Admin can administer
        assert!(pm
            .check_permission(&user, Action::AdminConfig, &obj)
            .is_allowed());
        assert!(pm
            .check_permission(&user, Action::GrantCredits, &obj)
            .is_allowed());
    }

    #[test]
    fn test_move_permission_requires_both_paths() {
        let pm = PermissionManager::new();

        // Builder with grants for /d/forest but not /d/desert
        let grants = vec![PathGrant::new(
            "universe1",
            "builder1",
            "/d/forest",
            false,
            "admin",
        )];
        let user = UserContext::builder("builder1", "universe1", grants);

        // Can move within granted path
        assert!(pm
            .check_move_permission(&user, "/d/forest/cave", "/d/forest/clearing", None)
            .is_allowed());

        // Cannot move to ungrated path
        assert!(!pm
            .check_move_permission(&user, "/d/forest/cave", "/d/desert/oasis", None)
            .is_allowed());

        // Cannot move from ungrated path
        assert!(!pm
            .check_move_permission(&user, "/d/desert/oasis", "/d/forest/clearing", None)
            .is_allowed());
    }

    #[test]
    fn test_delegation_permissions() {
        // User with can_delegate=true
        let grants_with_delegate = vec![PathGrant::new(
            "universe1",
            "builder1",
            "/d/forest",
            true,
            "admin",
        )];
        let user_can_delegate = UserContext::builder("builder1", "universe1", grants_with_delegate);

        // User with can_delegate=false
        let grants_no_delegate = vec![PathGrant::new(
            "universe1",
            "builder2",
            "/d/desert",
            false,
            "admin",
        )];
        let user_no_delegate = UserContext::builder("builder2", "universe1", grants_no_delegate);

        // User with delegation can delegate subpaths
        assert!(user_can_delegate.can_delegate_path("/d/forest/caves"));
        assert!(user_can_delegate.can_delegate_path("/d/forest"));

        // User without delegation cannot delegate
        assert!(!user_no_delegate.can_delegate_path("/d/desert/oasis"));

        // Neither can delegate outside their paths
        assert!(!user_can_delegate.can_delegate_path("/d/desert"));
        assert!(!user_no_delegate.can_delegate_path("/d/forest"));
    }
}
