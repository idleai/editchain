use editchain_core::{Op, OpId};

/// A generation counter for tracking projection freshness.
pub type Generation = u64;

/// A chunk record — a deterministic text segment extracted from an operation.
#[derive(Debug, Clone)]
pub struct ChunkRecord {
    pub chunk_id: ChunkId,
    pub op_id: OpId,
    pub chunk_ordinal: u32,
    pub byte_start: u32,
    pub byte_end: u32,
    pub generation: Generation,
}

/// A chunk identifier — unique within a chain.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ChunkId {
    pub op_id: OpId,
    pub chunk_ordinal: u32,
}

impl ChunkId {
    pub fn new(op_id: OpId, chunk_ordinal: u32) -> Self {
        Self {
            op_id,
            chunk_ordinal,
        }
    }
}

impl core::fmt::Display for ChunkId {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{}:{}", self.op_id, self.chunk_ordinal)
    }
}

/// Default chunking parameters.
pub const DEFAULT_CHUNK_WINDOW_TOKENS: u32 = 768;
pub const DEFAULT_CHUNK_OVERLAP_TOKENS: u32 = 96;

/// Extract searchable text from an operation based on its kind.
///
/// Returns `None` for operations that should not be indexed (e.g. private
/// content when disabled, or raw import ops when raw search is off).
pub fn extract_op_text(op: &Op, include_raw: bool, include_private: bool) -> Option<String> {
    use editchain_core::op::OpKind;

    if !include_private && op.tags.matches_any(editchain_core::tags::Tags::PRIVATE) {
        return None;
    }

    match &op.kind {
        OpKind::Message(msg) => match &msg.content {
            editchain_core::payload::Payload::Inline(bytes) => {
                Some(String::from_utf8_lossy(bytes).to_string())
            }
            _ => None,
        },
        OpKind::Tool(tool) => match &tool.content {
            editchain_core::payload::Payload::Inline(bytes) => {
                Some(String::from_utf8_lossy(bytes).to_string())
            }
            _ => None,
        },
        OpKind::Command(cmd) => match &cmd.content {
            editchain_core::payload::Payload::Inline(bytes) => {
                Some(String::from_utf8_lossy(bytes).to_string())
            }
            _ => None,
        },
        OpKind::File(file) => {
            // File ops carry path info but not always text content.
            // Index the path as searchable text.
            Some(format!("file:{}", file.path.0))
        }
        OpKind::Reflection(_refl) => {
            // Reflection summaries are indexed when available.
            None // Placeholder — will index summary when populated.
        }
        OpKind::Import(_import) if include_raw => {
            // Raw import records indexed only when explicitly requested.
            None // Placeholder — raw_ref payload may be blob.
        }
        _ => None,
    }
}

/// Chunk a text string into overlapping segments.
///
/// Uses a simple token estimate (4 bytes per token) for deterministic
/// chunking without an external tokenizer dependency.
pub fn chunk_text(
    text: &str,
    op_id: OpId,
    generation: Generation,
    window_tokens: u32,
    overlap_tokens: u32,
) -> Vec<ChunkRecord> {
    if text.is_empty() {
        return Vec::new();
    }

    // Rough token estimate: ~4 bytes per token for mixed code/prose.
    let window_bytes = (window_tokens as usize).saturating_mul(4);
    let overlap_bytes = (overlap_tokens as usize).saturating_mul(4);
    let stride = window_bytes.saturating_sub(overlap_bytes);

    if stride == 0 {
        // Window <= overlap — just return one chunk.
        return vec![ChunkRecord {
            chunk_id: ChunkId::new(op_id, 0),
            op_id,
            chunk_ordinal: 0,
            byte_start: 0,
            byte_end: text.len() as u32,
            generation,
        }];
    }

    let bytes = text.as_bytes();
    let mut chunks = Vec::new();
    let mut ordinal = 0u32;
    let mut start = 0usize;

    while start < bytes.len() {
        let end = (start + window_bytes).min(bytes.len());

        chunks.push(ChunkRecord {
            chunk_id: ChunkId::new(op_id, ordinal),
            op_id,
            chunk_ordinal: ordinal,
            byte_start: start as u32,
            byte_end: end as u32,
            generation,
        });

        ordinal += 1;

        if end == bytes.len() {
            break;
        }

        start += stride;
    }

    chunks
}

