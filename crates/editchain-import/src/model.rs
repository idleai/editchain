use std::path::PathBuf;

use editchain_core::Op;

use crate::ids::SourceStream;

/// Configuration for a discovery request.
#[derive(Debug, Clone)]
pub struct DiscoveryRequest {
    /// Path to the workspace root.
    pub workspace_path: PathBuf,
    /// Path to the Claude Code sessions directory (e.g. `~/.claude/projects/<encoded>`).
    pub sessions_dir: PathBuf,
    /// Path to the output chain directory.
    pub chain_dir: PathBuf,
}

/// Options for the import process.
#[derive(Debug, Clone)]
pub struct ImportOptions {
    /// Whether to emit normalized ops alongside raw ImportOps.
    pub normalize: bool,
    /// Whether to include thinking content (default: false — private).
    pub include_thinking: bool,
    /// Maximum inline payload size before spilling to blob storage.
    pub max_inline_bytes: usize,
}

impl Default for ImportOptions {
    fn default() -> Self {
        Self {
            normalize: true,
            include_thinking: false,
            max_inline_bytes: 4096,
        }
    }
}

/// A report of what happened during an import.
#[derive(Debug, Clone, Default)]
pub struct ImportReport {
    /// Number of source files discovered.
    pub files_discovered: usize,
    /// Number of source files processed.
    pub files_processed: usize,
    /// Number of raw ImportOps emitted.
    pub raw_ops: usize,
    /// Number of normalized ops emitted.
    pub normalized_ops: usize,
    /// Number of duplicate lines skipped.
    pub duplicates: usize,
    /// Number of malformed lines skipped.
    pub malformed: usize,
    /// Number of UUID collisions detected.
    pub uuid_collisions: usize,
}

impl ImportReport {
    pub fn new() -> Self {
        Self::default()
    }
}

/// A raw import operation — one per complete JSONL line.
#[derive(Debug, Clone)]
pub struct RawImport {
    /// The source stream this line belongs to.
    pub stream: SourceStream,
    /// Sequence number within the source stream.
    pub seq: u64,
    /// Blake3 hash of the raw line bytes.
    pub hash: [u8; 32],
    /// The raw line bytes (or a reference if large).
    pub data: Vec<u8>,
}

/// A normalized operation derived from a Claude Code record.
#[derive(Debug, Clone)]
pub enum NormalizedOp {
    /// An editchain Op ready for encoding.
    Op(Op),
}

impl NormalizedOp {
    pub fn into_op(self) -> Op {
        match self {
            NormalizedOp::Op(op) => op,
        }
    }
}