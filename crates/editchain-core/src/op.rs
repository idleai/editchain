#[cfg(not(feature = "use-std"))]
use alloc::vec::Vec;
use serde::{Deserialize, Serialize};

use crate::clock::Clock;
use crate::ids::{ActorId, NodeId, OpId, PathId};
use crate::parents::ParentSet;
use crate::payload::{BlobRef, ContentId, Payload};
use crate::scope::ScopeRef;
use crate::tags::Tags;

// ---------------------------------------------------------------------------
// Full operation envelope
// ---------------------------------------------------------------------------

/// A single operation in the edit chain.
///
/// Operations are immutable records. The envelope carries identity,
/// causal parents, actor, clock, scope, and tags. The `kind` field
/// holds the domain-specific payload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Op {
    /// Globally unique operation identifier.
    pub id: OpId,
    /// Causal parent references for DAG ordering.
    pub parents: ParentSet,
    /// Actor that created this operation.
    pub actor: ActorId,
    /// Clock value for causal ordering.
    pub clock: Clock,
    /// Scoping reference (chain, session, turn, or file).
    pub scope: ScopeRef,
    /// Bitflag tags for filtering.
    pub tags: Tags,
    /// Domain-specific operation payload.
    pub kind: OpKind,
}

impl Op {
    /// Create a new operation with the given fields.
    #[expect(
        clippy::too_many_arguments,
        reason = "Op struct has 7 fields; constructor takes all of them"
    )]
    #[must_use]
    pub const fn new(
        id: OpId,
        parents: ParentSet,
        actor: ActorId,
        clock: Clock,
        scope: ScopeRef,
        tags: Tags,
        kind: OpKind,
    ) -> Self {
        Self {
            id,
            parents,
            actor,
            clock,
            scope,
            tags,
            kind,
        }
    }
}

// ---------------------------------------------------------------------------
// Operation kinds
// ---------------------------------------------------------------------------

/// All supported operation kinds.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum OpKind {
    /// Chain initialization operation.
    ChainStart(ChainStart),
    /// Actor registration or metadata update.
    Actor(ActorOp),
    /// A message in the conversation.
    Message(MessageOp),
    /// A tool call lifecycle operation.
    Tool(ToolOp),
    /// A shell command lifecycle operation.
    Command(CommandOp),
    /// A file revision fact.
    File(FileOp),
    /// Agent context reflection.
    Reflection(ReflectionOp),
    /// Imported external record.
    Import(ImportOp),
    /// Relationship note between operations.
    Note(NoteOp),
    /// Diagnostic error fact.
    Error(ErrorOp),
    /// Opaque preserved fact (unrecognized kind).
    Unknown(UnknownOp),
}

// ---------------------------------------------------------------------------
// ChainStart
// ---------------------------------------------------------------------------

/// Chain initialization — the first operation in a chain.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChainStart {
    /// Human-readable chain name.
    pub name: Vec<u8>,
    /// Protocol version.
    pub version: u16,
}

// ---------------------------------------------------------------------------
// Actor
// ---------------------------------------------------------------------------

/// Actor registration or metadata update.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActorOp {
    /// Actor display name or label.
    pub label: Payload,
    /// Actor role (e.g. "agent", "human", "tool").
    pub role: Payload,
}

// ---------------------------------------------------------------------------
// Message
// ---------------------------------------------------------------------------

/// A message in the conversation (agent or human).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MessageOp {
    /// Message content.
    pub content: Payload,
    /// Content type (e.g. "text/markdown", "text/plain").
    pub content_type: Payload,
}

// ---------------------------------------------------------------------------
// Tool
// ---------------------------------------------------------------------------

/// Tool call lifecycle operation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolOp {
    /// Tool call identifier for correlating start/delta/finish.
    pub tool_call_id: Payload,
    /// Tool name.
    pub tool_name: Payload,
    /// Lifecycle stage.
    pub stage: ToolStage,
    /// Tool input arguments or output content.
    pub content: Payload,
}

/// Lifecycle stage of a tool call.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ToolStage {
    /// Tool call initiated.
    Start,
    /// Streaming delta output.
    Delta,
    /// Tool call completed.
    Finish,
}

// ---------------------------------------------------------------------------
// Command
// ---------------------------------------------------------------------------

/// Shell command lifecycle operation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommandOp {
    /// Command identifier for correlating start/output/finish.
    pub command_id: Payload,
    /// Command string or output content.
    pub content: Payload,
    /// Lifecycle stage.
    pub stage: CommandStage,
}

/// Lifecycle stage of a command execution.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CommandStage {
    /// Command execution started.
    Start,
    /// Streaming output from command.
    Output,
    /// Command execution finished.
    Finish,
}

// ---------------------------------------------------------------------------
// File
// ---------------------------------------------------------------------------

/// File revision fact.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileOp {
    /// File path identifier.
    pub path: PathId,
    /// Stage of the file lifecycle.
    pub stage: FileStage,
    /// Base content (before edit), if known.
    pub base: Option<ContentId>,
    /// Result content (after edit), if materializing.
    pub after: Option<ContentId>,
    /// The edit operation itself.
    pub edit: FileEdit,
}

/// Stage of a file in its lifecycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum FileStage {
    /// File was observed (read or stat'd).
    Observed,
    /// File edit was proposed but not yet applied.
    Proposed,
    /// File edit was applied to the working copy.
    Applied,
    /// File was saved/persisted.
    Saved,
    /// File was deleted.
    Deleted,
}

/// The edit applied to a file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum FileEdit {
    /// No edit (observation only).
    #[default]
    None,
    /// Replace a byte range with new content.
    ReplaceBytes {
        /// Byte range to replace.
        range: ByteRange,
        /// Replacement bytes.
        bytes: Payload,
    },
    /// Unified diff patch payload.
    UnifiedDiff(Payload),
    /// Reference to an external blob containing the full content.
    Blob(BlobRef),
}

/// A byte range within a file (start offset and length).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ByteRange {
    /// Start offset in bytes.
    pub start: u64,
    /// End offset in bytes (exclusive).
    pub end: u64,
}

// ---------------------------------------------------------------------------
// Reflection
// ---------------------------------------------------------------------------

/// Agent context reflection — a summary of covered operations.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReflectionOp {
    /// The scope this reflection applies to.
    pub scope: ScopeRef,
    /// The frontier of operations covered by this reflection.
    pub covers: FrontierSet,
    /// Window reference for sliding-window context management.
    pub window: WindowRef,
    /// Summary text/content.
    pub summary: Payload,
    /// Anchored references (`OpId`s, `PathId`s, symbols, tasks).
    pub anchors: Payload,
}

/// A set of frontiers — one per node — representing covered operations.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FrontierSet(pub Vec<Frontier>);

impl FrontierSet {
    /// Create an empty frontier set.
    #[must_use]
    pub const fn new() -> Self {
        Self(Vec::new())
    }
}

impl Default for FrontierSet {
    fn default() -> Self {
        Self::new()
    }
}

/// A frontier — the highest known sequence from a given node+boot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Frontier {
    /// Node identifier.
    pub node: NodeId,
    /// Boot counter of the node.
    pub boot: u32,
    /// Maximum sequence number seen from this node+boot.
    pub max_seq: u64,
}

/// Window reference for sliding-window context.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct WindowRef {
    /// Start sequence number (inclusive).
    pub start_seq: u64,
    /// End sequence number (exclusive).
    pub end_seq: u64,
}

// ---------------------------------------------------------------------------
// Import
// ---------------------------------------------------------------------------

/// Imported external record (e.g. from Claude Code history).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImportOp {
    /// Reference to the raw external record.
    pub raw_ref: Payload,
    /// Optional hash of the raw record.
    pub raw_hash: Option<[u8; 32]>,
}

// ---------------------------------------------------------------------------
// Note
// ---------------------------------------------------------------------------

/// A relationship note between operations — corrects/supersedes/rejects/redacts/explains.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NoteOp {
    /// The target operation(s) this note applies to.
    pub target_ids: Vec<OpId>,
    /// The relationship kind.
    pub relationship: NoteRelationship,
    /// Note content / explanation.
    pub content: Payload,
}

/// Relationship kinds for notes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum NoteRelationship {
    /// This note corrects an error in the target operation(s).
    Corrects,
    /// This note supersedes the target operation(s).
    Supersedes,
    /// This note rejects the target operation(s).
    Rejects,
    /// This note redacts information from the target operation(s).
    Redacts,
    /// This note provides additional explanation for the target operation(s).
    Explains,
}

// ---------------------------------------------------------------------------
// Error
// ---------------------------------------------------------------------------

/// A diagnostic error fact.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ErrorOp {
    /// Error code or identifier.
    pub code: Payload,
    /// Error message / description.
    pub message: Payload,
}

// ---------------------------------------------------------------------------
// Unknown
// ---------------------------------------------------------------------------

/// An opaque preserved fact — kind not recognized by this version of the core.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnknownOp {
    /// The raw kind discriminant value that was not recognized.
    pub kind_discriminant: u8,
    /// The raw encoded bytes of the operation payload.
    pub raw_bytes: Payload,
}
