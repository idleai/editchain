//! Full provider identities mapped to immutable source occurrences.
//!
//! These are reconciliation inputs, not proof that a native runtime operation
//! has been recorded. In particular, Codex item IDs are scoped to their thread
//! and turn; revisions and post-removal incarnations remain separate evidence.

use editchain_core::provider::{CodexLogicalChange, ProviderEvidence, ProviderFact};
use editchain_core::SourceId;
use serde::{Deserialize, Serialize};

/// A full native identifier, never a timestamp or a content-based guess.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(tag = "provider", rename_all = "snake_case")]
pub enum NativeIdentity {
    /// Claude event UUID, including its recorded session context.
    Claude {
        /// Full provider session identifier.
        session: String,
        /// Full provider event UUID.
        uuid: String,
    },
    /// Codex logical item within its owning execution and turn.
    Codex {
        /// Owning thread, never the parent session identifier.
        thread: String,
        /// Full turn identifier.
        turn: String,
        /// Full item identifier supplied by the exporter.
        item: String,
    },
    /// Human recorder incarnation and its native event sequence.
    Human {
        /// Full recorder incarnation, independent of person identity.
        session: String,
        /// One-based sequence within that incarnation.
        sequence: u64,
    },
}

/// A native identity and the exact imported evidence supporting it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NativeMapping {
    /// Unabridged provider identifier.
    pub identity: NativeIdentity,
    /// Raw imported operation, whose bytes remain authoritative.
    pub source: SourceId,
    /// BLAKE3 of the exact complete raw record, including its newline.
    pub raw_hash: [u8; 32],
    /// Materialized operations for this revision, when the provider supplies them.
    pub outputs: Vec<SourceId>,
    /// First occurrence of this Codex item since its turn's last removal.
    pub incarnation: Option<SourceId>,
}

/// Read an exact Claude mapping from a captured record.
///
/// Missing session IDs or event UUIDs remain unmapped. Physical filenames are
/// not substituted for native identity. Copied occurrences may share the UUID
/// while retaining distinct source evidence and recorded session context.
#[must_use]
pub fn claude_mapping(source: SourceId, raw: &[u8]) -> Option<NativeMapping> {
    let envelope = crate::claude_code::envelope::parse_envelope(raw)?;
    if envelope.session_id.is_empty() || envelope.uuid.is_empty() {
        return None;
    }
    Some(NativeMapping {
        identity: NativeIdentity::Claude {
            session: envelope.session_id,
            uuid: envelope.uuid,
        },
        source,
        raw_hash: crate::hash_raw(raw),
        outputs: Vec::new(),
        incarnation: None,
    })
}

/// Read native Codex item mappings from retained derivation evidence.
///
/// Removals remain in [`ProviderEvidence`]; they are not fabricated items.
/// Consumers must replay those changes when reconciling active state. Multiple
/// mappings for an identity are immutable revisions, not duplicate history.
#[must_use]
pub fn codex_mappings(evidence: &ProviderEvidence) -> Vec<NativeMapping> {
    let ProviderFact::CodexDerivation(derivation) = &evidence.fact else {
        return Vec::new();
    };
    derivation
        .changes
        .iter()
        .filter_map(|change| {
            let CodexLogicalChange::Upsert {
                turn,
                item,
                incarnation,
                outputs,
            } = change
            else {
                return None;
            };
            Some(NativeMapping {
                identity: NativeIdentity::Codex {
                    thread: derivation.thread.0.clone(),
                    turn: turn.clone(),
                    item: item.clone(),
                },
                source: evidence.source,
                raw_hash: evidence.raw_hash,
                outputs: outputs.clone(),
                incarnation: Some(*incarnation),
            })
        })
        .collect()
}
