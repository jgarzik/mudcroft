//! Resource metering for Lua execution

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

/// Tracks resource usage during Lua execution
#[derive(Debug, Clone)]
pub struct Metering {
    inner: Arc<MeteringInner>,
}

#[derive(Debug)]
struct MeteringInner {
    /// Number of Lua instructions executed
    instructions: AtomicU64,
    /// Number of database read operations
    db_reads: AtomicU64,
    /// Number of database write operations
    db_writes: AtomicU64,
    /// Number of Venice API calls
    venice_calls: AtomicU64,
    /// Memory usage in bytes
    memory_bytes: AtomicU64,
}

impl Default for Metering {
    fn default() -> Self {
        Self::new()
    }
}

impl Metering {
    /// Create a new metering instance
    pub fn new() -> Self {
        Self {
            inner: Arc::new(MeteringInner {
                instructions: AtomicU64::new(0),
                db_reads: AtomicU64::new(0),
                db_writes: AtomicU64::new(0),
                venice_calls: AtomicU64::new(0),
                memory_bytes: AtomicU64::new(0),
            }),
        }
    }

    /// Add to instruction count
    pub fn add_instructions(&self, count: u64) {
        self.inner.instructions.fetch_add(count, Ordering::Relaxed);
    }

    /// Get current instruction count
    pub fn instructions(&self) -> u64 {
        self.inner.instructions.load(Ordering::Relaxed)
    }

    /// Record a database read
    pub fn record_db_read(&self) {
        self.inner.db_reads.fetch_add(1, Ordering::Relaxed);
    }

    /// Get database read count
    pub fn db_reads(&self) -> u64 {
        self.inner.db_reads.load(Ordering::Relaxed)
    }

    /// Record a database write
    pub fn record_db_write(&self) {
        self.inner.db_writes.fetch_add(1, Ordering::Relaxed);
    }

    /// Get database write count
    pub fn db_writes(&self) -> u64 {
        self.inner.db_writes.load(Ordering::Relaxed)
    }

    /// Record a Venice API call
    pub fn record_venice_call(&self) {
        self.inner.venice_calls.fetch_add(1, Ordering::Relaxed);
    }

    /// Get Venice API call count
    pub fn venice_calls(&self) -> u64 {
        self.inner.venice_calls.load(Ordering::Relaxed)
    }

    /// Set current memory usage
    pub fn set_memory(&self, bytes: u64) {
        self.inner.memory_bytes.store(bytes, Ordering::Relaxed);
    }

    /// Get current memory usage
    pub fn memory_bytes(&self) -> u64 {
        self.inner.memory_bytes.load(Ordering::Relaxed)
    }

    /// Calculate estimated cost in credits (millicredits)
    /// Based on design.md pricing
    pub fn estimated_cost_millicredits(&self) -> u64 {
        // Instructions: $0.0001 per 1M = 0.1 millicredits per 1M
        let instr_cost = self.instructions() / 10_000_000;
        // DB reads: $0.001 each = 1 millicredit
        let read_cost = self.db_reads();
        // DB writes: $0.01 each = 10 millicredits
        let write_cost = self.db_writes() * 10;
        // Venice calls: ~$0.02-$0.20 each, use average $0.10 = 100 millicredits
        let venice_cost = self.venice_calls() * 100;

        instr_cost + read_cost + write_cost + venice_cost
    }

    /// Reset all counters
    pub fn reset(&self) {
        self.inner.instructions.store(0, Ordering::Relaxed);
        self.inner.db_reads.store(0, Ordering::Relaxed);
        self.inner.db_writes.store(0, Ordering::Relaxed);
        self.inner.venice_calls.store(0, Ordering::Relaxed);
        self.inner.memory_bytes.store(0, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_metering_basic() {
        let m = Metering::new();
        assert_eq!(m.instructions(), 0);

        m.add_instructions(1000);
        assert_eq!(m.instructions(), 1000);

        m.record_db_read();
        m.record_db_read();
        assert_eq!(m.db_reads(), 2);

        m.record_db_write();
        assert_eq!(m.db_writes(), 1);
    }

    #[test]
    fn test_metering_clone_shares_state() {
        let m1 = Metering::new();
        let m2 = m1.clone();

        m1.add_instructions(100);
        assert_eq!(m2.instructions(), 100);
    }

    #[test]
    fn test_metering_reset() {
        let m = Metering::new();
        m.add_instructions(1000);
        m.record_db_read();
        m.record_db_write();

        m.reset();
        assert_eq!(m.instructions(), 0);
        assert_eq!(m.db_reads(), 0);
        assert_eq!(m.db_writes(), 0);
    }
}
