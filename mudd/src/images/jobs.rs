//! Room image jobs in flight, so one room gets one generation at a time

use std::collections::HashSet;
use std::sync::{Arc, Mutex};

/// The rooms (per universe) with an image being generated
#[derive(Debug, Default, Clone)]
pub struct ImageJobs {
    running: Arc<Mutex<HashSet<(String, String)>>>,
}

/// A started job; dropping it (however the job ends) frees the room
#[derive(Debug)]
pub struct ImageJob {
    jobs: ImageJobs,
    key: (String, String),
}

impl ImageJobs {
    pub fn new() -> Self {
        Self::default()
    }

    /// Start a job for a room, or None if one is already running
    pub fn try_start(&self, universe_id: &str, room_id: &str) -> Option<ImageJob> {
        let key = (universe_id.to_string(), room_id.to_string());
        let mut running = self.running.lock().unwrap_or_else(|e| e.into_inner());
        running.insert(key.clone()).then(|| ImageJob {
            jobs: self.clone(),
            key,
        })
    }
}

impl Drop for ImageJob {
    fn drop(&mut self) {
        let mut running = self.jobs.running.lock().unwrap_or_else(|e| e.into_inner());
        running.remove(&self.key);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_one_job_per_room() {
        let jobs = ImageJobs::new();
        let job = jobs.try_start("u1", "/rooms/a").expect("first starts");
        assert!(jobs.try_start("u1", "/rooms/a").is_none(), "a duplicate");
        // Other rooms, and the same path in another universe, are separate
        assert!(jobs.try_start("u1", "/rooms/b").is_some());
        assert!(jobs.try_start("u2", "/rooms/a").is_some());
        drop(job);
        assert!(jobs.try_start("u1", "/rooms/a").is_some(), "freed on drop");
    }
}
