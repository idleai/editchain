//! Editchain daemon — warm process with append coordinator, projector bus,
//! and Unix-socket IPC for online consistency.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use editchain_index::snapshot::{LexicalSnapshot, QuerySnapshot};
use editchain_index::chunker::Generation;

// ---------------------------------------------------------------------------
// Append coordinator
// ---------------------------------------------------------------------------

/// Single append coordinator per chain directory.
///
/// Ensures monotonic commit generations and provides read-your-writes
/// consistency by tracking the latest committed generation.
pub struct AppendCoordinator {
    chain_dir: String,
    next_generation: AtomicU64,
}

impl AppendCoordinator {
    pub fn new(chain_dir: &str) -> Self {
        Self {
            chain_dir: chain_dir.to_string(),
            next_generation: AtomicU64::new(1),
        }
    }

    /// Reserve the next commit generation (monotonic).
    pub fn next_generation(&self) -> Generation {
        self.next_generation.fetch_add(1, Ordering::SeqCst)
    }

    /// Get the current committed generation.
    pub fn current_generation(&self) -> Generation {
        self.next_generation.load(Ordering::SeqCst).saturating_sub(1)
    }

    pub fn chain_dir(&self) -> &str {
        &self.chain_dir
    }
}

// ---------------------------------------------------------------------------
// Projection watermarks
// ---------------------------------------------------------------------------

/// Tracks how current each projection is relative to the append log.
#[derive(Debug, Clone, Copy)]
pub struct ProjectionWatermarks {
    pub log: Generation,
    pub hydrated: Generation,
    pub graph: Generation,
    pub lexical: Generation,
    pub vector: Generation,
}

impl ProjectionWatermarks {
    pub fn new() -> Self {
        Self {
            log: 0,
            hydrated: 0,
            graph: 0,
            lexical: 0,
            vector: 0,
        }
    }

    /// Returns true if all projections are caught up to the log.
    pub fn is_fully_consistent(&self) -> bool {
        self.hydrated >= self.log
            && self.graph >= self.log
            && self.lexical >= self.log
            && self.vector >= self.log
    }

    /// Returns true if lexical and graph are caught up (vector may lag).
    pub fn is_lexical_consistent(&self) -> bool {
        self.hydrated >= self.log && self.graph >= self.log && self.lexical >= self.log
    }
}

impl Default for ProjectionWatermarks {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Projector bus
// ---------------------------------------------------------------------------

/// A notification sent when new data is committed to the chain.
#[derive(Debug, Clone)]
pub struct CommitNotification {
    pub generation: Generation,
}

/// Trait for chain projectors (graph, lexical, vector).
pub trait Projector: Send + 'static {
    /// Called when a new commit is available.
    fn on_commit(&mut self, notification: &CommitNotification);
}

/// A bus that distributes commit notifications to registered projectors.
pub struct ProjectorBus {
    projectors: Vec<Box<dyn Projector>>,
}

impl ProjectorBus {
    pub fn new() -> Self {
        Self {
            projectors: Vec::new(),
        }
    }

    /// Register a projector.
    pub fn register(&mut self, projector: Box<dyn Projector>) {
        self.projectors.push(projector);
    }

    /// Notify all projectors of a new commit.
    pub fn notify(&mut self, notification: &CommitNotification) {
        for projector in &mut self.projectors {
            projector.on_commit(notification);
        }
    }
}

impl Default for ProjectorBus {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Query plane — ArcSwap snapshot holder
// ---------------------------------------------------------------------------

/// Holds the current query snapshot behind an ArcSwap for lock-free reads.
pub struct QueryPlane {
    snapshot: Arc<std::sync::RwLock<QuerySnapshot>>,
}

impl QueryPlane {
    pub fn new() -> Self {
        Self {
            snapshot: Arc::new(std::sync::RwLock::new(QuerySnapshot::new())),
        }
    }

    /// Get the current snapshot (acquires read lock).
    pub fn snapshot(&self) -> QuerySnapshot {
        self.snapshot.read().unwrap().clone()
    }

    /// Update the snapshot (acquires write lock).
    pub fn update(&self, new_snapshot: QuerySnapshot) {
        *self.snapshot.write().unwrap() = new_snapshot;
    }
}

impl Default for QueryPlane {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Daemon config
// ---------------------------------------------------------------------------

/// Configuration for the editchain daemon.
#[derive(Debug, Clone)]
pub struct DaemonConfig {
    /// Path to the chain directory.
    pub chain_dir: String,
    /// Unix socket path for IPC.
    pub socket_path: String,
    /// Lexical commit interval in milliseconds.
    pub lexical_commit_interval_ms: u64,
}

impl Default for DaemonConfig {
    fn default() -> Self {
        Self {
            chain_dir: ".editchain".to_string(),
            socket_path: "/tmp/editchain.sock".to_string(),
            lexical_commit_interval_ms: 100,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn append_coordinator_monotonic() {
        let coord = AppendCoordinator::new("/tmp/test-chain");
        let g1 = coord.next_generation();
        let g2 = coord.next_generation();
        assert!(g2 > g1);
        assert_eq!(coord.current_generation(), g2);
    }

    #[test]
    fn watermarks_defaults() {
        let wm = ProjectionWatermarks::new();
        assert_eq!(wm.log, 0);
        // With log=0, all projections are vacuously caught up.
        assert!(wm.is_fully_consistent());
    }

    #[test]
    fn watermarks_inconsistent_when_log_ahead() {
        let mut wm = ProjectionWatermarks::new();
        wm.log = 10;
        // Projections haven't caught up.
        assert!(!wm.is_fully_consistent());
        assert!(!wm.is_lexical_consistent());
    }

    #[test]
    fn watermarks_consistency() {
        let mut wm = ProjectionWatermarks::new();
        wm.log = 5;
        wm.hydrated = 5;
        wm.graph = 5;
        wm.lexical = 5;
        wm.vector = 5;
        assert!(wm.is_fully_consistent());
        assert!(wm.is_lexical_consistent());

        wm.vector = 3;
        assert!(!wm.is_fully_consistent());
        assert!(wm.is_lexical_consistent());
    }

    #[test]
    fn query_plane_update_and_read() {
        let plane = QueryPlane::new();
        let snap = plane.snapshot();
        assert_eq!(snap.hydrated_generation, 0);

        let mut new_snap = QuerySnapshot::new();
        new_snap.hydrated_generation = 42;
        plane.update(new_snap);

        let snap = plane.snapshot();
        assert_eq!(snap.hydrated_generation, 42);
    }
}