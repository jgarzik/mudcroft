//! Snapshot management for Raft state machine
//!
//! Handles creation and restoration of database snapshots.

use std::io;
use std::io::Cursor;
use std::path::Path;

use openraft::storage::Snapshot;
use openraft::{BasicNode, LogId, SnapshotMeta, StorageError, StorageIOError};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tracing::{debug, info, warn};

use super::state_machine::SnapshotData;
use super::types::{NodeId, TypeConfig};

/// Snapshot store for managing snapshot files
#[derive(Debug)]
pub struct SnapshotStore {
    /// Directory for storing snapshots
    snapshot_dir: String,
    /// Current snapshot metadata
    current: tokio::sync::RwLock<Option<SnapshotMeta<NodeId, BasicNode>>>,
}

impl SnapshotStore {
    /// Create a new snapshot store
    pub fn new(snapshot_dir: impl Into<String>) -> Self {
        Self {
            snapshot_dir: snapshot_dir.into(),
            current: tokio::sync::RwLock::new(None),
        }
    }

    /// Create from database path (snapshots go in same directory)
    pub fn from_db_path(db_path: &str) -> Self {
        let path = Path::new(db_path);
        let dir = path
            .parent()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_else(|| ".".to_string());
        Self::new(dir)
    }

    /// Get the snapshot file path for a given snapshot ID
    fn snapshot_path(&self, snapshot_id: &str) -> String {
        format!("{}/snapshot_{}.bin", self.snapshot_dir, snapshot_id)
    }

    /// Ensure snapshot directory exists
    async fn ensure_dir(&self) -> Result<(), StorageError<NodeId>> {
        tokio::fs::create_dir_all(&self.snapshot_dir)
            .await
            .map_err(|e| StorageIOError::write_snapshot(None, &e))?;
        Ok(())
    }

    /// Save a snapshot to disk
    pub async fn save_snapshot(
        &self,
        meta: SnapshotMeta<NodeId, BasicNode>,
        data: &SnapshotData,
    ) -> Result<(), StorageError<NodeId>> {
        self.ensure_dir().await?;

        let path = self.snapshot_path(&meta.snapshot_id);
        let bytes = serde_json::to_vec(data)
            .map_err(|e| StorageIOError::write_snapshot(None, &io::Error::other(e)))?;

        let mut file = tokio::fs::File::create(&path)
            .await
            .map_err(|e| StorageIOError::write_snapshot(None, &e))?;

        file.write_all(&bytes)
            .await
            .map_err(|e| StorageIOError::write_snapshot(None, &e))?;

        file.sync_all()
            .await
            .map_err(|e| StorageIOError::write_snapshot(None, &e))?;

        info!("Saved snapshot {} to {}", meta.snapshot_id, path);
        *self.current.write().await = Some(meta);

        Ok(())
    }

    /// Load a snapshot from disk
    pub async fn load_snapshot(
        &self,
        snapshot_id: &str,
    ) -> Result<Option<SnapshotData>, StorageError<NodeId>> {
        let path = self.snapshot_path(snapshot_id);

        if !Path::new(&path).exists() {
            return Ok(None);
        }

        let mut file = tokio::fs::File::open(&path)
            .await
            .map_err(|e| StorageIOError::read_snapshot(None, &e))?;

        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes)
            .await
            .map_err(|e| StorageIOError::read_snapshot(None, &e))?;

        let data: SnapshotData = serde_json::from_slice(&bytes)
            .map_err(|e| StorageIOError::read_snapshot(None, &io::Error::other(e)))?;

        debug!("Loaded snapshot {} from {}", snapshot_id, path);
        Ok(Some(data))
    }

    /// Get the current snapshot metadata
    pub async fn current_snapshot(&self) -> Option<SnapshotMeta<NodeId, BasicNode>> {
        self.current.read().await.clone()
    }

    /// Get the current snapshot if it exists
    pub async fn get_current_snapshot(
        &self,
    ) -> Result<Option<Snapshot<TypeConfig>>, StorageError<NodeId>> {
        let meta = match self.current.read().await.clone() {
            Some(m) => m,
            None => return Ok(None),
        };

        let data = match self.load_snapshot(&meta.snapshot_id).await? {
            Some(d) => d,
            None => return Ok(None),
        };

        let bytes = serde_json::to_vec(&data)
            .map_err(|e| StorageIOError::read_snapshot(None, &io::Error::other(e)))?;

        Ok(Some(Snapshot {
            meta,
            snapshot: Box::new(Cursor::new(bytes)),
        }))
    }

    /// Delete old snapshots, keeping only the latest N
    pub async fn cleanup_old_snapshots(&self, keep: usize) -> Result<(), StorageError<NodeId>> {
        let mut entries = tokio::fs::read_dir(&self.snapshot_dir)
            .await
            .map_err(|e| StorageIOError::read_snapshot(None, &e))?;

        let mut snapshots = Vec::new();
        while let Some(entry) = entries
            .next_entry()
            .await
            .map_err(|e| StorageIOError::read_snapshot(None, &e))?
        {
            let name = entry.file_name().to_string_lossy().to_string();
            if name.starts_with("snapshot_") && name.ends_with(".bin") {
                snapshots.push((
                    entry.path(),
                    entry
                        .metadata()
                        .await
                        .map(|m| m.modified().ok())
                        .ok()
                        .flatten(),
                ));
            }
        }

        // Sort by modification time, newest first
        snapshots.sort_by(|a, b| b.1.cmp(&a.1));

        // Delete old snapshots beyond the keep count
        for (path, _) in snapshots.into_iter().skip(keep) {
            if let Err(e) = tokio::fs::remove_file(&path).await {
                warn!("Failed to remove old snapshot {:?}: {}", path, e);
            } else {
                debug!("Removed old snapshot {:?}", path);
            }
        }

        Ok(())
    }
}

/// Generate a snapshot ID from log ID
pub fn generate_snapshot_id(log_id: Option<LogId<NodeId>>) -> String {
    log_id
        .map(|id| format!("{}-{}", id.leader_id.term, id.index))
        .unwrap_or_else(|| "0-0".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use openraft::StoredMembership;
    use tempfile::tempdir;

    #[tokio::test]
    async fn test_snapshot_store_creation() {
        let dir = tempdir().unwrap();
        let store = SnapshotStore::new(dir.path().to_string_lossy());
        assert!(store.current_snapshot().await.is_none());
    }

    #[tokio::test]
    async fn test_snapshot_save_and_load() {
        let dir = tempdir().unwrap();
        let store = SnapshotStore::new(dir.path().to_string_lossy());

        let meta = SnapshotMeta {
            last_log_id: Some(LogId {
                leader_id: openraft::LeaderId {
                    term: 1,
                    node_id: 1,
                },
                index: 10,
            }),
            last_membership: StoredMembership::default(),
            snapshot_id: "1-10".to_string(),
        };

        let data = SnapshotData {
            last_applied_log: meta.last_log_id,
            last_membership: meta.last_membership.clone(),
            db_snapshot: vec![1, 2, 3, 4],
        };

        store.save_snapshot(meta.clone(), &data).await.unwrap();

        // Verify current is set
        let current = store.current_snapshot().await;
        assert!(current.is_some());
        assert_eq!(current.unwrap().snapshot_id, "1-10");

        // Load and verify
        let loaded = store.load_snapshot("1-10").await.unwrap();
        assert!(loaded.is_some());
        let loaded = loaded.unwrap();
        assert_eq!(loaded.db_snapshot, vec![1, 2, 3, 4]);
    }

    #[tokio::test]
    async fn test_snapshot_cleanup() {
        let dir = tempdir().unwrap();
        let store = SnapshotStore::new(dir.path().to_string_lossy());

        // Create multiple snapshots
        for i in 1..=5 {
            let meta = SnapshotMeta {
                last_log_id: Some(LogId {
                    leader_id: openraft::LeaderId {
                        term: 1,
                        node_id: 1,
                    },
                    index: i,
                }),
                last_membership: StoredMembership::default(),
                snapshot_id: format!("1-{}", i),
            };

            let data = SnapshotData {
                last_applied_log: meta.last_log_id,
                last_membership: meta.last_membership.clone(),
                db_snapshot: vec![],
            };

            store.save_snapshot(meta, &data).await.unwrap();

            // Small delay to ensure different modification times
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }

        // Keep only 2
        store.cleanup_old_snapshots(2).await.unwrap();

        // Verify only 2 remain
        let mut count = 0;
        let mut entries = tokio::fs::read_dir(dir.path()).await.unwrap();
        while let Some(entry) = entries.next_entry().await.unwrap() {
            if entry.file_name().to_string_lossy().starts_with("snapshot_") {
                count += 1;
            }
        }
        assert_eq!(count, 2);
    }

    #[test]
    fn test_generate_snapshot_id() {
        assert_eq!(generate_snapshot_id(None), "0-0");

        let log_id = LogId {
            leader_id: openraft::LeaderId {
                term: 5,
                node_id: 2,
            },
            index: 100,
        };
        assert_eq!(generate_snapshot_id(Some(log_id)), "5-100");
    }

    #[test]
    fn test_from_db_path() {
        let store = SnapshotStore::from_db_path("/data/game/mudd.db");
        assert_eq!(store.snapshot_dir, "/data/game");
    }
}
