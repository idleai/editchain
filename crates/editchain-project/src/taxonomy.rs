//! Provider-neutral semantic readability taxonomy for history rows.
//!
//! These enums describe what a history row *is* ([`RecordRole`]), what kind of
//! activity it represents ([`ActivityKind`]), how prominently it should render
//! ([`Visibility`]), how its underlying activity concluded ([`Outcome`]), and
//! how its chain should be presented ([`ChainState`]), without referencing any
//! provider-specific raw format. The projection derives them deterministically
//! from raw/normalized structure; the protocol serializes them as stable
//! lowercase snake_case strings.
//!
//! Forward compatibility: classification enums carry an `Unknown` variant that
//! is both the [`Default`] and the serde catch-all (`#[serde(other)]`).
//! [`ChainState`] instead treats missing or unrecognized values as `Active`, so
//! presentation remains unchanged across protocol versions.

use serde::{Deserialize, Serialize};

/// What a history row contains at the record level.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecordRole {
    /// Readable prose (user/agent messages, plans, reflections).
    Narrative,
    /// An initiating action (tool call, command start, edit proposal).
    Action,
    /// The output of an action (tool result, command output).
    Result,
    /// A persistent artifact (file revision, git commit).
    Artifact,
    /// Lifecycle/transport bookkeeping with no independent content.
    Lifecycle,
    /// A duplicate echo of content that exists elsewhere.
    Echo,
    /// The role could not be determined conservatively.
    #[default]
    #[serde(other)]
    Unknown,
}

/// What kind of activity a history row represents.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActivityKind {
    /// A synthetic aggregate of the work performed between conversational
    /// turns. Individual member rows retain their own concrete activity kind.
    Work,
    /// Conversational exchange (user/agent messages).
    Conversation,
    /// Planning, reasoning, or reflection.
    Plan,
    /// Investigation/exploration (read-only discovery).
    Explore,
    /// Tool or command execution.
    Execute,
    /// File or workspace modification.
    Change,
    /// Verification/checking activity.
    Verify,
    /// Diagnostic/investigation of failures.
    Diagnose,
    /// Coordination between agents (subagent lifecycle).
    Coordinate,
    /// Source-control activity (git).
    SourceControl,
    /// Activity echoed from an external agent.
    External,
    /// System/transport/lifecycle records.
    System,
    /// The activity kind could not be determined conservatively.
    #[default]
    #[serde(other)]
    Unknown,
}

/// How prominently a history row should render.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Visibility {
    /// A first-class content row (narrative, actions, results, artifacts).
    Primary,
    /// A supporting row (bundled metadata revealed on demand).
    Supporting,
    /// A trace row (duplicate/echo/transport noise) — hidden by
    /// `hide_trace` filtering.
    Trace,
    /// Visibility could not be determined.
    #[default]
    #[serde(other)]
    Unknown,
}

/// How the underlying activity concluded.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    /// The activity completed successfully (structured evidence present).
    Success,
    /// The activity completed with a warning.
    Warning,
    /// The activity failed (structured evidence present).
    Failure,
    /// The activity was cancelled/aborted (structured evidence present).
    Cancelled,
    /// The outcome is unknown — never inferred from absence of evidence.
    #[default]
    #[serde(other)]
    Unknown,
}

/// Reusable presentation state for a node and the child-owned edge connecting
/// it to its parent.
///
/// This is deliberately independent of the reason a row is de-emphasized.
/// Import projection currently assigns [`Self::Muted`] to terminal cancelled
/// requests; future projection rules can reuse the same treatment for other
/// abandoned or superseded branches without teaching the renderer about each
/// provider-specific cause.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChainState {
    /// De-emphasized history that remains present, connected, and interactive.
    Muted,
    /// Ordinary active history geometry and row presentation.
    #[default]
    #[serde(other)]
    Active,
}

impl ChainState {
    /// Whether this is the default active state.
    #[must_use]
    pub const fn is_active(&self) -> bool {
        matches!(self, Self::Active)
    }
}
