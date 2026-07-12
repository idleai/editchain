//! Editchain index — text projection, chunking, and query snapshots.

pub mod chunker;
pub mod lexical;
pub mod snapshot;

pub use chunker::*;
pub use lexical::*;
pub use snapshot::*;