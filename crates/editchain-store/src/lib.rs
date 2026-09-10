//! Durable EC02 segments, content-addressed blobs, and canonical read contracts.

mod blob;
/// Atomic publication and directory synchronization for durable metadata.
pub mod durable;
mod reader;
mod segment;

pub use blob::{BlobPreviewResolution, BlobReader, BlobResolution, BlobStore};
pub use reader::{read_op_at, CanonicalChain, ChainReadStats, OpRecordLocation};
pub use segment::SegmentStore;

#[cfg(test)]
use tempfile as _;
