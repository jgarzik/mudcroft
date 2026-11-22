//! Administration of global access levels (the `mudd_admin` tool)
//!
//! Global admins and owners exist only through this tool (and `mudd_init`
//! for the first admin): there is no Lua or HTTP path to a global level.
//! On a single node it writes the database directly, so run it while mudd
//! is stopped; in a cluster the change goes through the leader (Raft).

use std::path::Path;

use anyhow::{bail, Result};

use crate::auth::accounts::AccountService;
use crate::db::Database;

/// A global level `mudd_admin set-level` can set
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GlobalLevel {
    Admin,
    Owner,
    /// No global level (stored as `player`; per-universe levels still apply)
    None,
}

impl std::str::FromStr for GlobalLevel {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "admin" => Ok(Self::Admin),
            "owner" => Ok(Self::Owner),
            "none" => Ok(Self::None),
            other => Err(format!(
                "unknown level {:?}: use admin, owner or none",
                other
            )),
        }
    }
}

impl GlobalLevel {
    fn stored(self) -> &'static str {
        match self {
            Self::Admin => "admin",
            Self::Owner => "owner",
            Self::None => "player",
        }
    }
}

/// Set `username`'s global level in the database at `db_path` (offline,
/// single node). Refuses a cluster member's database: changing one copy
/// would leave the others different (use `mudd_admin cluster set-level`).
pub async fn set_global_level(db_path: &Path, username: &str, level: GlobalLevel) -> Result<()> {
    let db = Database::open(&db_path.to_string_lossy()).await?;
    let members: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM cluster_nodes")
        .fetch_one(db.pool())
        .await?;
    if members > 0 {
        bail!(
            "this database belongs to a cluster: set levels through it with \
             `mudd_admin cluster ... set-level` instead"
        );
    }
    let service = AccountService::offline(db.pool().clone());
    apply_global_level(&service, db.pool(), username, level).await?;
    db.pool().close().await;
    Ok(())
}

/// Set `username`'s global level through `service` (offline, or through
/// Raft on a cluster's primary). Refuses to remove the last global admin or
/// owner (mudd will not start without one).
pub async fn apply_global_level(
    service: &AccountService,
    pool: &sqlx::SqlitePool,
    username: &str,
    level: GlobalLevel,
) -> Result<()> {
    let Some(account) = service.get_by_username(username).await? else {
        bail!("no account named {:?}", username);
    };

    let is_admin = matches!(account.access_level.as_str(), "admin" | "owner");
    if is_admin && level == GlobalLevel::None {
        let admins: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM accounts WHERE access_level IN ('admin', 'owner')",
        )
        .fetch_one(pool)
        .await?;
        if admins <= 1 {
            bail!(
                "{:?} is the last global admin or owner; promote another account first",
                username
            );
        }
    }

    service
        .set_access_level(&account.id, level.stored())
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    async fn fresh_db(dir: &tempfile::TempDir) -> std::path::PathBuf {
        let path = dir.path().join("admin.db");
        crate::init::init_database(
            &path,
            Some("root"),
            Some("rootpass123"),
            HashMap::new(),
            None,
        )
        .await
        .unwrap();
        let db = Database::open(&path.to_string_lossy()).await.unwrap();
        AccountService::offline(db.pool().clone())
            .create("alice", "alicepass1")
            .await
            .unwrap();
        db.pool().close().await;
        path
    }

    async fn level_of(path: &Path, username: &str) -> String {
        let db = Database::open(&path.to_string_lossy()).await.unwrap();
        let account = AccountService::offline(db.pool().clone())
            .get_by_username(username)
            .await
            .unwrap()
            .unwrap();
        db.pool().close().await;
        account.access_level
    }

    #[tokio::test]
    async fn test_set_global_level() {
        let dir = tempfile::tempdir().unwrap();
        let path = fresh_db(&dir).await;

        set_global_level(&path, "alice", GlobalLevel::Owner)
            .await
            .unwrap();
        assert_eq!(level_of(&path, "alice").await, "owner");
        set_global_level(&path, "alice", GlobalLevel::Admin)
            .await
            .unwrap();
        assert_eq!(level_of(&path, "alice").await, "admin");
        set_global_level(&path, "alice", GlobalLevel::None)
            .await
            .unwrap();
        assert_eq!(level_of(&path, "alice").await, "player");

        assert!(set_global_level(&path, "nobody", GlobalLevel::Admin)
            .await
            .is_err());
    }

    /// The last global admin cannot be removed
    #[tokio::test]
    async fn test_last_admin_kept() {
        let dir = tempfile::tempdir().unwrap();
        let path = fresh_db(&dir).await;
        let err = set_global_level(&path, "root", GlobalLevel::None)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("last global admin"), "{}", err);
        assert_eq!(level_of(&path, "root").await, "admin");

        // With a second admin, the first can step down
        set_global_level(&path, "alice", GlobalLevel::Admin)
            .await
            .unwrap();
        set_global_level(&path, "root", GlobalLevel::None)
            .await
            .unwrap();
        assert_eq!(level_of(&path, "root").await, "player");
    }

    #[test]
    fn test_parse_level() {
        assert_eq!("admin".parse::<GlobalLevel>(), Ok(GlobalLevel::Admin));
        assert_eq!("none".parse::<GlobalLevel>(), Ok(GlobalLevel::None));
        assert!("wizard".parse::<GlobalLevel>().is_err());
    }
}
