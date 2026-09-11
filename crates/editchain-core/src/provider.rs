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
    /// Versioned semantic materialization of one complete physical record.
    CodexDerivation(CodexDerivationEvidence),
    /// Versioned content-block materialization of one Claude physical record.
    ClaudeDerivation(ClaudeDerivationEvidence),
}

/// Named Claude content derivation contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ClaudeDerivationContract {
    /// Stable block slots, bounded payloads, and explicit reasoning backfill.
    #[serde(rename = "claude-blocks-v1")]
    BlocksV1,
}

/// Complete materialized outputs from one Claude physical occurrence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClaudeDerivationEvidence {
    /// Named derivation semantics and payload representation.
    pub contract: ClaudeDerivationContract,
    /// Whether requested private reasoning is included.
    pub includes_thinking: bool,
    /// Complete operation set in provider content order.
    pub outputs: Vec<OpId>,
}

/// Named semantic derivation contract, independent of metadata migrations.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CodexDerivationContract {
    /// Every upsert and removal is preserved at its witnessing occurrence.
    #[serde(rename = "codex-occurrences-v1")]
    OccurrencesV1,
    /// Occurrences with bidirectional legacy user-message echo correlation.
    #[serde(rename = "codex-occurrences-v2")]
    OccurrencesV2,
}

/// Materialized operations and logical changes from one physical occurrence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CodexDerivationEvidence {
    /// Full owning execution identity.
    pub thread: CodexThreadId,
    /// Named derivation semantics.
    pub contract: CodexDerivationContract,
    /// Whether this materialization includes requested private reasoning.
    pub includes_thinking: bool,
    /// Complete operation set produced for this occurrence.
    pub outputs: Vec<OpId>,
    /// Provider logical changes, in the order reported on this occurrence.
    pub changes: Vec<CodexLogicalChange>,
}

/// An immutable change from which current logical item state can be rebuilt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum CodexLogicalChange {
    /// Remove every active item in this turn; historical revisions remain.
    RemoveTurn {
        /// Full provider turn identity within the execution.
        turn: String,
    },
    /// Replace the active state of one logical item.
    Upsert {
        /// Full provider turn identity within the execution.
        turn: String,
        /// Full provider item identity within the turn.
        item: String,
        /// First occurrence since the most recent removal of this turn.
        incarnation: OpId,
        /// Materialized operations belonging to this revision of the item.
        outputs: Vec<OpId>,
    },
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
