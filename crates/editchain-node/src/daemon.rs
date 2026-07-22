//! Editchain daemon — warm process with append coordinator, projector bus,
//! and Unix-socket IPC for online consistency.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use clap as _;
use dirs as _;
use editchain_codec as _;
use editchain_core as _;
use editchain_embed as _;
use editchain_import as _;
use editchain_query as _;
use serde as _;
use serde_json as _;

#[cfg(test)]
use tempfile as _;

use editchain_index::chunker::Generation;
use editchain_index::snapshot::QuerySnapshot;

// ---------------------------------------------------------------------------
// Append coordinator
// ---------------------------------------------------------------------------

/// Single append coordinator per chain directory.
///
/// Ensures monotonic commit generations and provides read-your-writes
/// consistency by tracking the latest committed generation.
#[derive(Debug)]
pub struct AppendCoordinator {
    chain_dir: String,
    next_generation: AtomicU64,
}

impl AppendCoordinator {
    /// Create a new append coordinator for the given chain directory.
    #[must_use]
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
        self.next_generation
            .load(Ordering::SeqCst)
            .saturating_sub(1)
    }

    /// Get the chain directory path.
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
    /// Log generation watermark.
    pub log: Generation,
    /// Hydrated projection watermark.
    pub hydrated: Generation,
    /// Graph projection watermark.
    pub graph: Generation,
    /// Lexical index watermark.
    pub lexical: Generation,
    /// Vector index watermark.
    pub vector: Generation,
}

impl ProjectionWatermarks {
    /// Create a new projection watermarks with all generations set to 0.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            log: 0,
            hydrated: 0,
            graph: 0,
            lexical: 0,
            vector: 0,
        }
    }

    /// Returns true if all projections are caught up to the log.
    #[must_use]
    pub const fn is_fully_consistent(&self) -> bool {
        self.hydrated >= self.log
            && self.graph >= self.log
            && self.lexical >= self.log
            && self.vector >= self.log
    }

    /// Returns true if lexical and graph are caught up (vector may lag).
    #[must_use]
    pub const fn is_lexical_consistent(&self) -> bool {
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
    /// The generation that was committed.
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

impl std::fmt::Debug for ProjectorBus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProjectorBus")
            .field("projector_count", &self.projectors.len())
            .finish()
    }
}

impl ProjectorBus {
    /// Create a new empty projector bus.
    #[must_use]
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

/// Holds the current query snapshot behind an `ArcSwap` for lock-free reads.
#[derive(Debug)]
pub struct QueryPlane {
    snapshot: Arc<std::sync::RwLock<QuerySnapshot>>,
}

impl QueryPlane {
    /// Create a new query plane with an empty snapshot.
    #[must_use]
    pub fn new() -> Self {
        Self {
            snapshot: Arc::new(std::sync::RwLock::new(QuerySnapshot::new())),
        }
    }

    /// Get the current snapshot (acquires read lock).
    ///
    /// # Panics
    ///
    /// Panics if the read lock is poisoned (another thread panicked while holding it).
    #[expect(
        clippy::unwrap_used,
        reason = "RwLock poison is a fatal error; unwrap is appropriate"
    )]
    #[must_use]
    pub fn snapshot(&self) -> QuerySnapshot {
        self.snapshot.read().unwrap().clone()
    }

    /// Update the snapshot (acquires write lock).
    ///
    /// # Panics
    ///
    /// Panics if the write lock is poisoned (another thread panicked while holding it).
    #[expect(
        clippy::unwrap_used,
        reason = "RwLock poison is a fatal error; unwrap is appropriate"
    )]
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
