//! Durable EC03 segments with EC02 read compatibility, content-addressed blobs, and canonical read contracts.

mod blob;
mod blob_adapter;
/// Atomic publication and directory synchronization for durable metadata.
pub mod durable;
/// Operation frames and durable page formats used by storage writers/readers.
pub mod format;
mod indexed;
mod log;
mod reader;
mod segment;
mod tail;
mod visit;

pub use blob::{BlobPreviewResolution, BlobReader, BlobResolution, BlobStore};
pub use blob_adapter::{BlobSource, BlobStorage};
pub use indexed::IndexedChain;
pub use log::{AppendLog, LogReadStats, LogStore, RecordVisitor};
pub use reader::{read_encoded_at, read_op_at, CanonicalChain, ChainReadStats, OpRecordLocation};
pub use segment::{SegmentOptions, SegmentStore};
pub use tail::{CanonicalTail, ChainDelta, IndexedTail, Tail, TailCorpus, TailWork};
pub use visit::visit_records;

#[cfg(test)]
use tempfile as _;

/// Resumable migration into a new EC03 chain with original evidence retained.
pub mod migration;
