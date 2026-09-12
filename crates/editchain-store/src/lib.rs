//! Durable EC02 segments, content-addressed blobs, and canonical read contracts.

mod blob;
/// Atomic publication and directory synchronization for durable metadata.
pub mod durable;
/// Operation frames and durable page formats used by storage writers/readers.
pub mod format;
mod indexed;
mod reader;
mod segment;
mod tail;

pub use blob::{BlobPreviewResolution, BlobReader, BlobResolution, BlobStore};
pub use indexed::IndexedChain;
pub use reader::{read_op_at, CanonicalChain, ChainReadStats, OpRecordLocation};
pub use segment::SegmentStore;
pub use tail::{CanonicalTail, ChainDelta, IndexedTail, Tail, TailCorpus, TailWork};

#[cfg(test)]
use tempfile as _;
