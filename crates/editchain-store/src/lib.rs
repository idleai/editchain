//! Durable EC02 segments and one canonical read contract for every consumer.

mod reader;
mod segment;

pub use reader::{read_op_at, CanonicalChain, ChainReadStats, OpRecordLocation};
pub use segment::SegmentStore;

#[cfg(test)]
use tempfile as _;
