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
    /// Whether to emit normalized ops alongside raw `ImportOps`.
    pub normalize: bool,
    /// Whether to include thinking content (default: false — private).
    pub include_thinking: bool,
    /// Maximum inline payload size before spilling to blob storage.
    pub max_inline_bytes: usize,
    /// Bounds on captured sources and individual physical records.
    pub source_limits: crate::source_read::SourceReadLimits,
    /// Bounds on helper output and elapsed execution time.
    pub helper_limits: crate::codex::helper::HelperLimits,
    /// Aggregate operation count and encoded-byte bounds for one capture batch.
    pub batch_limits: crate::sink::BatchLimits,
    /// Shared cancellation signal for capture and derivation.
    pub cancellation: crate::cancellation::ImportCancellation,
}

impl Default for ImportOptions {
    fn default() -> Self {
        Self {
            normalize: true,
            include_thinking: false,
            max_inline_bytes: 4096,
            source_limits: crate::source_read::SourceReadLimits::default(),
            helper_limits: crate::codex::helper::HelperLimits::default(),
            batch_limits: crate::sink::BatchLimits::default(),
            cancellation: crate::cancellation::ImportCancellation::default(),
        }
    }
}

impl ImportOptions {
    pub(crate) fn source_control(&self) -> crate::source_read::SourceReadControl {
        crate::source_read::SourceReadControl {
            limits: self.source_limits,
            cancellation: self.cancellation.clone(),
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
    /// Number of distinct raw operation variants retained by the capture sink.
    pub raw_ops: usize,
    /// Number of distinct normalized variants retained by the capture sink.
    pub normalized_ops: usize,
    /// Number of distinct typed provider evidence variants retained.
    pub evidence_ops: usize,
    /// Number of exact operation variants already retained by the capture sink.
    pub duplicates: usize,
    /// Number of malformed lines skipped.
    pub malformed: usize,
    /// New conflicting operation variants retained by the capture sink.
    pub op_conflicts: usize,
}

impl ImportReport {
    /// Create a new empty import report.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub(crate) fn merge_emissions(&mut self, other: &Self) {
        self.raw_ops = self.raw_ops.saturating_add(other.raw_ops);
        self.normalized_ops = self.normalized_ops.saturating_add(other.normalized_ops);
        self.evidence_ops = self.evidence_ops.saturating_add(other.evidence_ops);
        self.duplicates = self.duplicates.saturating_add(other.duplicates);
        self.op_conflicts = self.op_conflicts.saturating_add(other.op_conflicts);
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
    /// Convert this normalized op into an editchain `Op`.
    #[must_use]
    pub fn into_op(self) -> Op {
        match self {
            Self::Op(op) => op,
        }
    }
}
