use alloc::vec::Vec;
use serde::{Deserialize, Serialize};

use crate::clock::Clock;
use crate::ids::*;
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
    pub id: OpId,
    pub parents: ParentSet,
    pub actor: ActorId,
    pub clock: Clock,
    pub scope: ScopeRef,
    pub tags: Tags,
    pub kind: OpKind,
}

impl Op {
    /// Create a new operation with the given fields.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
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
    ChainStart(ChainStart),
    Actor(ActorOp),
    Message(MessageOp),
    Tool(ToolOp),
    Command(CommandOp),
    File(FileOp),
    Reflection(ReflectionOp),
    Import(ImportOp),
    Note(NoteOp),
    Error(ErrorOp),
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
    Start,
    Delta,
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
    Start,
    Output,
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
    Observed,
    Proposed,
    Applied,
    Saved,
    Deleted,
}

/// The edit applied to a file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[derive(Default)]
pub enum FileEdit {
    #[default]
    None,
    ReplaceBytes {
        range: ByteRange,
        bytes: Payload,
    },
    UnifiedDiff(Payload),
    Blob(BlobRef),
}


/// A byte range within a file (start offset and length).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ByteRange {
    pub start: u64,
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
    /// Anchored references (OpIds, PathIds, symbols, tasks).
    pub anchors: Payload,
}

/// A set of frontiers — one per node — representing covered operations.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FrontierSet(pub Vec<Frontier>);

impl FrontierSet {
    pub fn new() -> Self {
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
    pub node: NodeId,
    pub boot: u32,
    pub max_seq: u64,
}

/// Window reference for sliding-window context.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct WindowRef {
    pub start_seq: u64,
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
    Corrects,
    Supersedes,
    Rejects,
    Redacts,
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