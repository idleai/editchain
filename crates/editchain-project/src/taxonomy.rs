//! Provider-neutral semantic readability taxonomy for history rows.
//!
//! These enums describe what a history row *is* ([`RecordRole`]), what kind of
//! activity it represents ([`ActivityKind`]), how prominently it should render
//! ([`Visibility`]), and how its underlying activity concluded ([`Outcome`]),
//! without referencing any provider-specific raw format. The projection derives
//! them deterministically from raw/normalized structure; the protocol
//! serializes them as stable lowercase snake_case strings.
//!
//! Forward compatibility: every enum carries an `Unknown` variant that is both
//! the [`Default`] and the serde catch-all (`#[serde(other)]`), so a newer
//! service emitting a value this client does not recognize, or an older
//! payload missing the field entirely, never breaks deserialization.

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
