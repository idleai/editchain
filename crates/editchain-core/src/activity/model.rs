//! Typed payload contracts shared by archives and live producers.

use crate::{
    ByteRange, ContentId, FileEdit, FrontierSet, GitCommitEntity, GitOid, OpId, PathId, Payload,
    RepositoryId, WindowRef,
};
use serde::{Deserialize, Serialize};

use super::ItemId;

/// The complete schema-three operation inventory.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Kind {
    /// A conversation or editing session observation.
    Session(Session),
    /// A unit of agent execution.
    Turn(Turn),
    /// Communicated text, reasoning, plans, or summaries.
    Message(Message),
    /// An invocation, including terminal commands.
    Tool(Tool),
    /// File or buffer interaction and revision facts.
    File(File),
    /// A Git commit observation.
    Commit(Box<GitCommitEntity>),
    /// An annotation or standalone diagnostic.
    Note(Note),
    /// An identity and its recorded metadata.
    Author(Author),
    /// A separately recorded connection.
    Link(Link),
    /// Exact input retained as received.
    Original(Original),
}

/// Names suitable for indexing and command-line filters.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum KindName {
    /// Session observations.
    Session,
    /// Agent execution units.
    Turn,
    /// All kinds of messages.
    Message,
    /// Calls, commands, and outputs.
    Tool,
    /// File interactions and changes.
    File,
    /// Git commits.
    Commit,
    /// Annotations and diagnostics.
    Note,
    /// Author metadata.
    Author,
    /// Connections.
    Link,
    /// Retained source records.
    Original,
}

impl Kind {
    /// Stable operation category, independent of lifecycle or content subtype.
    #[must_use]
    pub const fn name(&self) -> KindName {
        match self {
            Self::Session(_) => KindName::Session,
            Self::Turn(_) => KindName::Turn,
            Self::Message(_) => KindName::Message,
            Self::Tool(_) => KindName::Tool,
            Self::File(_) => KindName::File,
            Self::Commit(_) => KindName::Commit,
            Self::Note(_) => KindName::Note,
            Self::Author(_) => KindName::Author,
            Self::Link(_) => KindName::Link,
            Self::Original(_) => KindName::Original,
        }
    }
}

/// A session observation, including final-only archive snapshots.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Session {
    /// Recorded lifecycle state.
    pub action: SessionAction,
    /// Recorded name or title.
    pub label: Payload,
    /// Versioned provider or model configuration bytes.
    pub settings: Payload,
    /// Known participants; absence means unrecorded.
    pub participants: Vec<ItemId>,
    /// Parent session explicitly supplied by the source.
    pub parent: Option<ItemId>,
    /// Event that initiated this session, when known.
    pub initiated_by: Option<OpId>,
}

/// Session lifecycle; snapshot makes no claim about a missing start.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SessionAction {
    /// Explicit start.
    Started,
    /// Configuration or label update.
    Changed,
    /// Explicit end.
    Ended,
    /// Captured state with no observed transition.
    Snapshot,
}

/// Recorded execution state and completion.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Turn {
    /// Lifecycle observation.
    pub action: TurnAction,
    /// Accepted prompts or other explicit initiating events.
    pub triggers: Vec<OpId>,
    /// Distinguishes retries of the same logical request.
    pub attempt: ItemId,
    /// A completion outcome, including explicitly unknown outcomes.
    pub outcome: Option<Completion>,
}

/// Agent execution lifecycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TurnAction {
    /// Accepted but not started.
    Queued,
    /// Execution began.
    Started,
    /// Waiting for an input or external event.
    Waiting,
    /// Continued after waiting.
    Resumed,
    /// Terminal observation.
    Finished,
    /// Captured partial state.
    Snapshot,
    /// Source explicitly withdrew this item from its current view.
    Removed,
}

/// Completion is separate from success.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Completion {
    /// Recorded result category.
    pub status: Status,
    /// Error or diagnostic details, when present.
    pub detail: Payload,
}

/// Explicit terminal outcome.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Status {
    /// Source reported success.
    Success,
    /// Source reported failure.
    Failure,
    /// Source reported cancellation or interruption.
    Cancelled,
    /// The result was not recorded or is not understood.
    Unknown,
}

/// Streaming lifecycle shared by messages and calls.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Stage {
    /// Explicit start.
    Started,
    /// Incremental content observation.
    Updated,
    /// Explicit end, which need not include a start record.
    Finished,
    /// Whole captured state without an observed ending.
    Snapshot,
}

/// How this update changes a logical content block.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum UpdateMode {
    /// Append bytes after the declared predecessor.
    Append,
    /// Replace the complete block with these bytes.
    Replace,
}

/// One ordered content update. A predecessor disambiguates reordered delivery.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContentUpdate {
    /// Stable block identity within the logical item.
    pub block: ItemId,
    /// Native block order, if supplied.
    pub position: Option<u32>,
    /// Append or replacement semantics.
    pub mode: UpdateMode,
    /// Previous update to this block; missing appends remain explicitly partial.
    pub previous: Option<OpId>,
    /// MIME type recorded by the producer.
    pub media_type: Payload,
    /// Exact content or attachment reference.
    pub content: Payload,
}

/// A message observation, independent of its speaker or audience.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Message {
    /// Content category, not a guessed intent.
    pub category: MessageKind,
    /// Recorded lifecycle state.
    pub stage: Stage,
    /// Recipients, when explicitly identified.
    pub audience: Vec<ItemId>,
    /// Content updates in recorded block order.
    pub blocks: Vec<ContentUpdate>,
    /// History summarized by this message.
    pub coverage: Option<Coverage>,
    /// Recorded completion; optional for sources without outcomes.
    pub outcome: Option<Completion>,
}

/// Message content category.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MessageKind {
    /// Ordinary communicated content.
    Text,
    /// Captured reasoning explicitly marked by the source.
    Reasoning,
    /// An explicitly identified plan.
    Plan,
    /// A summary of earlier history.
    Summary,
}

/// Explicit summary coverage, including historical frontier contracts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Coverage {
    /// Exact operation references, when known.
    pub operations: Vec<OpId>,
    /// Original sequence frontiers, retained without guessing their expansion.
    pub frontiers: FrontierSet,
    /// Original sequence window, if present.
    pub window: Option<WindowRef>,
    /// Additional recorded anchors.
    pub anchors: Payload,
}

/// An invocation and its progress; terminal commands use the same lifecycle.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Tool {
    /// Stable provider call identity, retained verbatim.
    pub native_call: Payload,
    /// Recorded tool name; outputs may omit it.
    pub name: Payload,
    /// Lifecycle stage.
    pub stage: Stage,
    /// Retry identity; never conflates independently executed attempts.
    pub attempt: ItemId,
    /// Parent call for a wrapper that launches independent calls.
    pub parent_call: Option<ItemId>,
    /// Recorded invocation arguments.
    pub arguments: Payload,
    /// Output channel such as stdout, stderr, or result.
    pub channel: OutputChannel,
    /// Recorded output update.
    pub output: Option<ContentUpdate>,
    /// Terminal-specific fields, when applicable.
    pub terminal: Option<Terminal>,
    /// Finished calls always state an outcome, including unknown.
    pub outcome: Option<Completion>,
}

/// Known output streams.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum OutputChannel {
    /// Structured or unspecified result.
    Result,
    /// Standard output.
    Stdout,
    /// Standard error.
    Stderr,
    /// Producer combined stdout and stderr.
    Combined,
}

/// Recorded terminal execution details.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Terminal {
    /// Exact command text or argv representation.
    pub command: Payload,
    /// Recorded working directory.
    pub cwd: Payload,
    /// Exit status, when supplied.
    pub exit_code: Option<i32>,
}

/// File interaction or revision observation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct File {
    /// Named interaction.
    pub action: FileAction,
    /// Stable path identity, including legacy path aliases.
    pub path: PathId,
    /// Recorded path text, when available.
    pub name: Payload,
    /// New path on a rename.
    pub renamed_to: Option<Payload>,
    /// Revision identity, separate from a content hash.
    pub revision: Option<ItemId>,
    /// Previous observed contents.
    pub before: Option<ContentId>,
    /// Resulting observed contents.
    pub after: Option<ContentId>,
    /// Patch or replacement contract, never inferred from tool success.
    pub edit: FileEdit,
    /// Ordered editor changes expressed in their native UTF-16 coordinate units.
    pub text_edits: Vec<TextEdit>,
    /// Proposed or applied status for creates, changes, renames and deletions.
    pub change: Option<ChangeState>,
    /// Known causing call.
    pub caused_by: Option<ItemId>,
    /// Byte ranges with explicit coordinate units.
    pub ranges: Vec<ByteRange>,
    /// Native editor ranges in line / UTF-16 column coordinates.
    pub text_ranges: Vec<TextRange>,
    /// Recorded duration of visibility or reading in milliseconds.
    pub duration_ms: Option<u64>,
}

/// An explicit editor replacement against the evolving document buffer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TextEdit {
    /// Offset in UTF-16 code units.
    pub offset_utf16: u64,
    /// Replaced length in UTF-16 code units.
    pub length_utf16: u64,
    /// Inserted UTF-8 text; never interpreted as byte offsets.
    pub text: Payload,
}

/// A half-open editor range with explicit coordinate units.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TextRange {
    /// Zero-based line and UTF-16 column.
    pub start: [u32; 2],
    /// Exclusive line and UTF-16 column.
    pub end: [u32; 2],
}

/// File and buffer actions, including unsaved work.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum FileAction {
    /// Editor opened.
    Open,
    /// Editor closed.
    Close,
    /// Range visible; no claim about comprehension.
    View,
    /// Recorded read observation.
    Read,
    /// New file.
    Create,
    /// Buffer or file changed.
    Change,
    /// Persisted to storage.
    Save,
    /// Path changed.
    Rename,
    /// Removed.
    Delete,
    /// Captured revision without a known transition.
    Snapshot,
}

/// Distinguishes a request from an observed change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ChangeState {
    /// Requested and not observed as applied.
    Proposed,
    /// Explicitly recorded as applied.
    Applied,
}

/// Standalone annotation, correction, or diagnostic.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Note {
    /// Structured annotation category.
    pub category: NoteKind,
    /// Targets recorded by the author.
    pub targets: Vec<OpId>,
    /// Logical items addressed by this annotation.
    pub items: Vec<ItemId>,
    /// Version of this note's content contract.
    pub version: u32,
    /// Exact content bytes or reference.
    pub content: Payload,
    /// Optional diagnostic code.
    pub code: Payload,
}

/// Bounded annotation categories.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum NoteKind {
    /// Human or agent annotation.
    Comment,
    /// Consumer classification.
    Label,
    /// Correction of an earlier assertion.
    Correction,
    /// Standalone diagnostic.
    Error,
    /// Known recording gap.
    Gap,
}

/// Metadata observation about the logical author identified by the envelope.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Author {
    /// Recorded display label.
    pub label: Payload,
    /// Person, agent, tool, system, or an explicitly unknown role.
    pub role: AuthorRole,
    /// Original role text, when importing an older representation.
    pub native_role: Payload,
    /// Versioned additional metadata.
    pub metadata: Payload,
}

/// Recorded identity category.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AuthorRole {
    /// A person.
    Person,
    /// An agent.
    Agent,
    /// A tool.
    Tool,
    /// A system integration.
    System,
    /// Unrecorded or unrecognized.
    Unknown,
}

/// A connection learned separately from its endpoints.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Link {
    /// Origin endpoint.
    pub from: Entity,
    /// Explicit relation; legacy names remain distinguishable.
    pub relation: String,
    /// Destination endpoints.
    pub to: Vec<Entity>,
    /// Optional relationship annotation.
    pub content: Payload,
}

/// Typed relationship endpoint.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Entity {
    /// Immutable operation.
    Operation(OpId),
    /// Logical session, message, call, or other item.
    Item(ItemId),
    /// Repository-scoped Git object.
    Git {
        /// Repository identity.
        repository: RepositoryId,
        /// Full Git object ID.
        oid: GitOid,
    },
}

/// Captured input, retained independently of its interpretation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Original {
    /// Source integration or provider.
    pub provider: String,
    /// Input format name and version, if known.
    pub format: Option<String>,
    /// Full source identities, never truncated for storage.
    pub native: Vec<NativeId>,
    /// Physical input location, when recorded.
    pub location: Option<Location>,
    /// Exact bytes including source whitespace and line endings.
    pub bytes: Payload,
    /// Recorded hash of those bytes, stored once.
    pub hash: Option<[u8; 32]>,
}

/// Namespaced native identity without inventing missing identifiers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NativeId {
    /// Provider identity category, such as session, turn, or call.
    pub kind: String,
    /// Unmodified provider value.
    pub value: String,
}

/// Physical location of a captured input record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Location {
    /// Captured source path or URI.
    pub path: String,
    /// One-based physical record ordinal.
    pub record: Option<u64>,
    /// Byte offset, when known.
    pub offset: Option<u64>,
}
