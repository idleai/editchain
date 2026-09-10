//! Typed provider evidence carried by versioned metadata notes.

use serde::{Deserialize, Serialize};

use crate::OpId;

/// A full Codex execution identity, separate from physical operation IDs.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct CodexThreadId(pub String);

/// Version of the typed provider-evidence payload contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProviderEvidenceSchema {
    /// Source extents and occurrence-bound lifecycle observations.
    #[serde(rename = "editchain-provider-evidence-v1")]
    V1,
}

/// Provider evidence bound to an exact physical source occurrence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderEvidence {
    /// Payload schema; unsupported schemas remain opaque metadata.
    pub schema: ProviderEvidenceSchema,
    /// Physical raw operation carrying this observation.
    pub source: OpId,
    /// Hash of the complete physical record, including its newline.
    pub raw_hash: [u8; 32],
    /// Typed provider observation.
    pub fact: ProviderFact,
}

/// An immutable provider observation, independent of endpoint availability.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProviderFact {
    /// One captured complete prefix of a physical Codex source generation.
    CodexSource(Box<CodexSourceEvidence>),
    /// One lifecycle observation carried by a Codex projection change.
    CodexLifecycle(CodexLifecycleEvidence),
}

/// Execution identity and the complete prefix captured from one source.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CodexSourceEvidence {
    /// Full owning execution identity.
    pub thread: CodexThreadId,
    /// Explicit parent execution, when provided.
    pub parent: Option<CodexThreadId>,
    /// Explicit fork source; this alone supplies no visible divergence row.
    pub forked_from: Option<CodexThreadId>,
    /// Provider path, used only for exact legacy lifecycle correlation.
    pub agent_path: Option<String>,
    /// First physical occurrence in this source generation.
    pub first: OpId,
    /// Last complete physical occurrence covered by this prefix.
    pub last: OpId,
    /// Hash of the complete source prefix through `last`.
    pub prefix_hash: [u8; 32],
}

/// A provider lifecycle observation and its logical item identity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CodexLifecycleEvidence {
    /// Execution carrying the observation.
    pub thread: CodexThreadId,
    /// Provider item identity, scoped to the execution and turn.
    pub item_id: String,
    /// Provider turn identity, independent of physical source order.
    pub turn_id: String,
    /// Exact activation or successful completion observation.
    pub event: CodexLifecycleEvent,
}

/// Exact lifecycle fields with the endpoint rules appropriate to their shape.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum CodexLifecycleEvent {
    /// An explicit activation of a named child execution.
    Spawn {
        /// First occurrence of the logical activation item.
        activation: OpId,
        /// Full child execution identity.
        child: CodexThreadId,
        /// Exact legacy path for a later `list_agents` correlation.
        agent_path: Option<String>,
        /// Provider field that establishes activation.
        signal: CodexSpawnSignal,
    },
    /// An explicit completed state naming the child execution.
    Completed {
        /// Full child execution identity.
        child: CodexThreadId,
    },
    /// A legacy completion naming an exact path in its parent execution.
    LegacyCompleted {
        /// Path that must match one unambiguous activation in the same parent.
        agent_path: String,
    },
}

/// Provider signals that establish an exact activation occurrence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CodexSpawnSignal {
    /// The older dedicated subagent activity record.
    SubagentActivity,
    /// A structured collaboration tool activation.
    CollabTool,
}
