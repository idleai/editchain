//! Immutable human-work facts derived from retained editor observations.

use crate::{ContentId, Op, OpId, OpKind, Payload};
use serde::{Deserialize, Serialize};

/// Git context observed independently of editing and tool execution.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HumanGitContext {
    /// Repository identity as decimal text, without JavaScript rounding.
    pub repository: String,
    /// Absolute worktree root at observation time.
    pub root: String,
    /// Exact observed HEAD; absent for an unborn or unavailable HEAD.
    pub head: Option<String>,
}

/// A buffer occurrence; equal contents do not collapse distinct revisions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HumanRevision {
    /// Recorder-local document incarnation.
    pub document: String,
    /// Exact VS Code buffer version.
    pub version: u64,
    /// Retained content, independent of occurrence identity.
    pub content: ContentId,
    /// Observation establishing this occurrence, when continuity is known.
    pub occurrence: Option<OpId>,
}

/// What the person contributed, without claiming comprehension.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HumanWorkKind {
    /// An edit with a retained keyboard/undo/redo indicator.
    Edit,
    /// Continuous visibility qualifying under the recorded dwell policy.
    Read,
    /// Shorter visibility, including skimming.
    Exposure,
    /// A known observation gap.
    Gap,
}

/// One immutable work fragment. Turns group fragments without rewriting them.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HumanWorkRecord {
    /// Discriminator, always `vscode.work`.
    pub source: String,
    /// Derivation contract version, currently one.
    pub schema: u32,
    /// Full recorder incarnation; different windows remain distinct.
    pub session: String,
    /// First observation in this bounded work episode.
    pub turn: u64,
    /// Raw event supporting this work fragment.
    pub source_event: OpId,
    /// Activity classification.
    pub kind: HumanWorkKind,
    /// Workspace-relative path; absent for untitled buffers and gaps.
    pub path: Option<String>,
    /// Actual observed application input, including intermediate unsaved edits.
    pub before: Option<HumanRevision>,
    /// Actual observed output or exposed revision.
    pub after: Option<HumanRevision>,
    /// Last captured Git context for this path; never a query-time substitute.
    pub git: Option<HumanGitContext>,
    /// Time at which this Git context was observed.
    pub context_observed_ms: Option<u64>,
    /// User-facing summary derived from the observation.
    pub summary: String,
}

/// Explicit annotation identifying raw editor evidence as supporting activity.
#[must_use]
pub fn is_observation_marker(op: &Op) -> bool {
    matches!(&op.kind, OpKind::Note(note)
        if matches!(&note.content, Payload::Inline(bytes) if bytes == b"vscode.editor.observation.v1"))
}
