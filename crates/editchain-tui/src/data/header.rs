use editchain_core::OpId;

/// Compact ordinal used for UI and graph indexing.
pub type OpOrdinal = u32;

/// Compact header for an operation — stored in the in-memory snapshot.
///
/// This is the lightweight representation used for list rendering and filtering.
/// Full operation payloads are decoded lazily from storage.
#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct OpHeader {
    pub id: OpId,
    pub actor: u64,
    pub clock_value: u64,
    pub clock_sub: u16,
    pub scope_discriminant: u8,
    pub scope_value: u64,
    pub tags: u64,
    pub kind_code: u8,
    pub stage_code: Option<u8>,
    pub parent_count: u8,
    pub parent0: Option<OpId>,
    pub parent1: Option<OpId>,
    /// Short preview text (first ~80 chars of content).
    pub preview: Option<Box<str>>,
}

impl OpHeader {
    /// Returns true if this operation has any of the given tag bits set.
    #[allow(dead_code)]
    pub fn has_any_tag(&self, bits: u64) -> bool {
        self.tags & bits != 0
    }

    /// Returns true if this operation has all of the given tag bits set.
    #[allow(dead_code)]
    pub fn has_all_tags(&self, bits: u64) -> bool {
        self.tags & bits == bits
    }

    /// Human-readable kind name.
    pub fn kind_name(&self) -> &'static str {
        match self.kind_code {
            0 => "ChainStart",
            1 => "Actor",
            2 => "Message",
            3 => "Tool",
            4 => "Command",
            5 => "File",
            6 => "Reflection",
            7 => "Import",
            8 => "Note",
            9 => "Error",
            _ => "Unknown",
        }
    }
}