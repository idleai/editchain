//! Editchain index — text projection, chunking, and query snapshots.

pub mod chunker;
pub mod snapshot;

pub use chunker::*;
pub use snapshot::*;