use editchain_core::{Clock, ImportOp, Op, OpKind, ParentSet, ScopeRef, Tags};

use super::envelope::{CcContentBlock, CcEnvelope};
use crate::error::ImportError;
use crate::ids::{derive_actor_id, derive_session_id, SourcePosition, SourceStream};
use crate::sink::{payload_for, BlobSink};

/// Semantic class of a Claude Code record, replacing a single `META` boolean so
/// the projection can distinguish content (must stay visible) from bundling
/// metadata (folds under a real turn), structural pointers (visible, graph-
/// shaping), diagnostics (visible), and unknown records (visible standalone).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RecordClass {
    /// User-facing content: user/assistant messages with prose, `tool_use`,
    /// `tool_result`, file content, reflection/boundary that users read.
    Content,
    /// Metadata that belongs beneath a real turn — bundled as a sub-op. Never a
    /// layout anchor on its own.
    BundleMetadata,
    /// Pointer/sidecar records that shape the graph but carry no prose to read.
    StructuralMetadata,
    /// Records that stay visible but are not user turns (errors, informational).
    Diagnostic,
    /// Record type not recognized. Defaults to visible standalone.
    Unknown,
}

/// Classify a Claude Code envelope into a [`RecordClass`].
///
/// `BundleMetadata` records (e.g. `last-prompt`, `permission-mode`,
/// `custom-title`, `mode`, `agent-name`, file-history bookkeeping,
/// `fork-context-ref`, `atis-latch`, `queue-operation`, telemetry `system`
/// subtypes, whitespace-only assistant streaming artifacts, and
/// environment/listing attachments) are bundled as sub-ops of a real
/// turn/tool node rather than occupying their own graph row/lane. They are
/// tagged `META` so the projection can group them without re-parsing the raw
/// JSONL.
#[must_use]
pub fn record_class(env: &CcEnvelope) -> RecordClass {
    match env.record_type.as_str() {
        "last-prompt"
        | "permission-mode"
        | "custom-title"
        | "mode"
        | "agent-name"
        | "file-history-snapshot"
        | "file-history-delta"
        | "fork-context-ref"
        | "atis-latch"
        | "queue-operation"
        | "ai-title" => RecordClass::BundleMetadata,
        // Telemetry system records carry no user-facing prose — bundle.
        "system" => match env.subtype.as_str() {
            "turn_duration" | "local_command" | "scheduled_task_fire" => {
                RecordClass::BundleMetadata
            }
            // API errors and informational events carry prose worth keeping — visible.
            "api_error" | "informational" => RecordClass::Diagnostic,
            // Compaction/checkpoint boundaries shape the graph but aren't content.
            "compact_boundary" | "away_summary" => RecordClass::StructuralMetadata,
            _ => RecordClass::Unknown,
        },
        // A whitespace-only assistant turn (no tool call, no meaningful text) is
        // a streaming artifact with no content — bundle it like metadata.
        "assistant" if is_whitespace_only_assistant(env) => RecordClass::BundleMetadata,
        // Attachment records that carry environment/listing metadata rather than
        // user-facing content — bundle them like metadata.
        "attachment" => matches!(
            env.attachment_type.as_str(),
            "task_reminder"
                | "skill_listing"
                | "agent_listing_delta"
                | "mcp_instructions_delta"
                | "deferred_tools_delta"
                | "command_permissions"
                | "date_change"
                | "nested_memory"
                | "read_truncation_notice"
                | "plan_mode"
                | "plan_mode_exit"
        )
        .then(|| RecordClass::BundleMetadata)
        .unwrap_or(if env.attachment_type == "diagnostics" {
            RecordClass::Diagnostic
        } else {
            RecordClass::Content
        }),
        // Legacy `is_metadata_record` entry point.
        _ => RecordClass::Content,
    }
}

/// Whether a Claude Code record carries only metadata (no user-facing content).
///
/// Retained as a convenience over [`record_class`] so existing callers/tests
/// keep working. Metadata records are bundled as sub-ops of a real turn/tool
/// node rather than occupying their own graph row/lane.
#[must_use]
pub fn is_metadata_record(env: &CcEnvelope) -> bool {
    record_class(env) == RecordClass::BundleMetadata
}

/// Whether an assistant record is a whitespace-only streaming artifact.
///
/// Claude Code splits an assistant turn that ends in a tool call into two
/// records: a text record (possibly whitespace-only) and a `tool_use` record.
/// When the model emits only newlines before calling the tool, the text record
/// carries no user-facing content. Returns true when the message has text blocks
/// that are all whitespace and no `tool_use` block.
#[must_use]
fn is_whitespace_only_assistant(env: &CcEnvelope) -> bool {
    let Some(msg) = &env.message else {
        return false;
    };
    let mut has_text = false;
    for block in &msg.content {
        match block {
            CcContentBlock::Text { text } => {
                if !text.trim().is_empty() {
                    // Meaningful prose — not a whitespace artifact.
                    return false;
                }
                has_text = true;
            }
            CcContentBlock::ToolUse { .. } => {
                // A real tool call — not a degenerate preamble.
                return false;
            }
            CcContentBlock::ToolResult { .. } | CcContentBlock::Thinking { .. } => {}
        }
    }
    has_text
}

/// Normalize a parsed CC envelope into editchain operations.
///
/// Returns (`raw_import_op`, `optional_normalized_ops`).
///
/// # Errors
///
/// Returns [`ImportError`] if payload storage or legacy lane allocation fails.
/// No operations from this record are returned on a storage failure.
///
#[expect(
    clippy::too_many_arguments,
    reason = "all arguments are required for normalization"
)]
pub fn normalize_envelope(
    env: &CcEnvelope,
    line_hash: [u8; 32],
    raw_bytes: &[u8],
    stream: &SourceStream,
    seq: u64,
    options: &NormalizeOptions,
    blobs: &mut dyn BlobSink,
    fallback_session_id: &str,
) -> Result<(Op, Vec<Op>), ImportError> {
    let raw_pos = SourcePosition::raw(seq);
    let op_id = stream.op_from_position(raw_pos)?;
    let timestamp = parse_source_time(&env.timestamp);
    // `parse_source_time` returns `None` when the source timestamp is absent or
    // unparseable. Keep `Clock::UnixMs(0)` for codec/ordering compatibility but
    // set `Tags::SOURCE_TIME_UNKNOWN` so the projection can distinguish "absent"
    // from a confident epoch and never fabricate a borrowed time.
    let clock = Clock::UnixMs(timestamp.unwrap_or(0));
    let source_time_unknown = timestamp.is_none();

    // Some metadata records (e.g. `file-history-snapshot`, `mode`, `agent-name`)
    // carry no `sessionId`/`session_id` field. Without a fallback they would all
    // derive `derive_session_id("")` to one constant scope, collapsing every
    // session's snapshots into a single synthetic session that causally stitches
    // unrelated sessions together. Fall back to the owning source file's session
    // so these records scope to their true session.
    let effective_session_id = if env.session_id.is_empty() {
        fallback_session_id
    } else {
        &env.session_id
    };
    let session_id = derive_session_id(effective_session_id);

    // Derive actor.
    let (actor, _tags) = match env.record_type.as_str() {
        "user" => {
            let actor_key = format!("human:{effective_session_id}");
            (derive_actor_id(&actor_key), Tags::HUMAN | Tags::MESSAGE)
        }
        "assistant" => {
            let model = env.message.as_ref().map_or("", |m| m.model.as_str());
            let actor_key = if env.agent_id.is_empty() {
                format!("model:{effective_session_id}:{model}")
            } else {
                format!("agent:{effective_session_id}:{}", env.agent_id)
            };
            (derive_actor_id(&actor_key), Tags::AGENT | Tags::MESSAGE)
        }
        _ => {
            let actor_key = format!("system:{effective_session_id}");
            (derive_actor_id(&actor_key), Tags::IMPORT)
        }
    };

    // Raw import op.
    let mut raw_tags = Tags::IMPORT;
    if source_time_unknown {
        raw_tags |= Tags::SOURCE_TIME_UNKNOWN;
    }
    match record_class(env) {
        RecordClass::BundleMetadata => raw_tags |= Tags::META,
        RecordClass::StructuralMetadata => raw_tags |= Tags::STRUCTURAL,
        RecordClass::Diagnostic => raw_tags |= Tags::DIAGNOSTIC,
        RecordClass::Content | RecordClass::Unknown => {}
    }
    let raw_op = Op {
        id: op_id,
        parents: ParentSet::None,
        actor,
        clock,
        scope: ScopeRef::Session(session_id),
        tags: raw_tags,
        kind: OpKind::Import(ImportOp {
            raw_ref: payload_for(raw_bytes, blobs)?,
            raw_hash: Some(line_hash),
        }),
    };

    if !options.normalize {
        return Ok((raw_op, vec![]));
    }

    let normalized = super::content::normalize_content(
        env,
        &raw_op,
        options.include_thinking,
        super::content::Contract::Legacy,
        blobs,
    )?;
    Ok((raw_op, normalized))
}

/// Options for controlling normalization behavior.
#[derive(Debug, Clone)]
pub struct NormalizeOptions {
    /// Whether to normalize operation kinds (e.g. split tool calls into start/end).
    pub normalize: bool,
    /// Whether to include thinking blocks in the output.
    pub include_thinking: bool,
}

impl Default for NormalizeOptions {
    fn default() -> Self {
        Self {
            normalize: true,
            include_thinking: false,
        }
    }
}

/// Parse a timestamp string into Unix milliseconds.
///
/// Returns 0 if the string is empty or unparseable (legacy behavior — callers
/// that need to distinguish "absent/invalid" from a confident epoch should use
/// [`parse_source_time`] instead).
#[must_use]
pub fn parse_timestamp(ts_str: &str) -> u64 {
    parse_source_time(ts_str).unwrap_or(0)
}

// Compatibility export for existing provider-specific callers.
pub use crate::source_time::parse_source_time;
