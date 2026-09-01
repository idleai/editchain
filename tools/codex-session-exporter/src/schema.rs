//! editchain-v1 wire schema for the Codex rollout projection bridge.
//!
//! This is a deliberately small, closed-set projection. It is *not* a mirror of
//! the full Codex `ThreadItem` surface: unrecognized or future Codex item kinds
//! are preserved as [`ItemProjection::Opaque`] with a diagnostic type name so
//! schema evolution is non-fatal for consumers.
//!
//! The projection carries the typed content EditChain needs to build neutral
//! Message/Tool/Command/File/Reflection/Note ops: message text, reasoning
//! summaries and raw content, command output, file diffs, tool arguments,
//! results, and errors, and plan/review/inter-agent text — everything the
//! Codex `ThreadItem`/`ResponseItem` types expose. Only genuinely non-text
//! payloads (encrypted content, image/audio data URIs) stay length/presence-
//! only. The raw rollout JSONL remains canonical for anything not projected.

use serde::Serialize;
use serde_json::Value;
use std::collections::HashMap;

pub const SCHEMA_VERSION: &str = "editchain-v1";
pub const RECORD_TYPE_LINE: &str = "line";
pub const RECORD_TYPE_FINAL: &str = "final";
pub const DECODE_OK: &str = "ok";
pub const DECODE_ERROR: &str = "error";
pub const KIND_UNKNOWN_JSON: &str = "unknownJson";

/// One NDJSON record per non-blank physical input line.
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct LineRecord {
    pub schema_version: String,
    pub record_type: String,
    pub source_path: String,
    /// 1-based physical line ordinal within the source file.
    pub source_ordinal: u64,
    pub decode: DecodeInfo,
    pub projection: ProjectionRecord,
}

/// Per-file synthetic record emitted after EOF when `--final` is requested.
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct FinalRecord {
    pub schema_version: String,
    pub record_type: String,
    pub source_path: String,
    /// EOF anchor: equals `physical_line_count` (0 for an empty file).
    pub source_ordinal: u64,
    pub decode: DecodeInfo,
    pub physical_line_count: u64,
    pub decoded_lines: u64,
    pub failed_lines: u64,
    pub failed_ordinals: Vec<u64>,
    /// Owning thread identity = the physical rollout thread (`session_meta.payload.id`).
    pub thread_id: Option<String>,
    pub session_meta: Option<SessionMetaProjection>,
    /// Final deduplicated per-turn item snapshot, keyed by stable item id.
    /// Includes response-derived items projected from `response_item` lines.
    pub turns: Vec<TurnSummary>,
    pub inter_agent_messages: u64,
    pub response_item_messages: u64,
    /// Items created by the bridge from `response_item` lines or
    /// `item_completed` markers the Codex builder does not materialize.
    pub response_derived_items: u64,
    pub compactions: u64,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct DecodeInfo {
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub diagnostic: Option<String>,
    /// Coarse rollout item kind, or the raw `type` discriminant on error lines.
    pub kind: String,
    /// Event subtype (`payload.type` / legacy `payload.event_type`) when present.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub event_type: Option<String>,
    /// Optional Codex session ordinal from the raw line (absent in legacy rollouts).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rollout_ordinal: Option<u64>,
    /// Raw line timestamp when available (always present on decoded lines).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ProjectionRecord {
    pub changed_items: Vec<ItemChange>,
    pub changed_turns: Vec<TurnChange>,
    pub removed_turn_ids: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_meta: Option<SessionMetaProjection>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub inter_agent: Option<InterAgentProjection>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub compacted: Option<CompactedProjection>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ItemChange {
    pub turn_id: String,
    pub item: ItemProjection,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub started_at_ms: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub completed_at_ms: Option<i64>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct TurnChange {
    pub turn_id: String,
    pub status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_message: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub started_at: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<i64>,
}

/// Closed set of typed item projections. Unknown/future Codex item kinds become
/// [`ItemProjection::Opaque`]; consumers must treat `opaque` as non-fatal.
///
/// Text-bearing fields carry the raw typed content; only encrypted content and
/// image/audio data URIs remain length/presence-only.
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum ItemProjection {
    UserMessage {
        id: String,
        /// Newline-joined text inputs.
        text: String,
        /// FNV-1a 64 fingerprint of `text`; one correlation signal between
        /// `event_msg` and `response_item` echoes (never the sole dedup key).
        content_hash: String,
        attachments: Vec<AttachmentProjection>,
    },
    AgentMessage {
        id: String,
        text: String,
        content_hash: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        phase: Option<String>,
    },
    Reasoning {
        id: String,
        /// Reasoning summary sections (agent-facing titles/blocks).
        summary: Vec<String>,
        /// Raw chain-of-thought content when Codex exposes it.
        content: Vec<String>,
    },
    CommandExecution {
        id: String,
        /// Codex's own secret-redacted command string.
        command: String,
        cwd: Option<String>,
        source: Option<String>,
        status: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        exit_code: Option<i32>,
        #[serde(skip_serializing_if = "Option::is_none")]
        duration_ms: Option<i64>,
        /// Aggregated stdout+stderr output when the item exposes it.
        #[serde(skip_serializing_if = "Option::is_none")]
        aggregated_output: Option<String>,
    },
    FileChange {
        id: String,
        status: Option<String>,
        changes: Vec<FileChangeProjection>,
    },
    ToolCall {
        id: String,
        tool: String,
        server: Option<String>,
        namespace: Option<String>,
        plugin_id: Option<String>,
        status: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        duration_ms: Option<i64>,
        /// Serialized tool arguments (JSON object/array/string) when present.
        #[serde(skip_serializing_if = "Option::is_none")]
        arguments: Option<Value>,
        /// Serialized tool result/output when present.
        #[serde(skip_serializing_if = "Option::is_none")]
        result: Option<Value>,
        #[serde(skip_serializing_if = "Option::is_none")]
        error_message: Option<String>,
    },
    CollabToolCall {
        id: String,
        tool: Option<String>,
        sender_thread_id: Option<String>,
        receiver_thread_ids: Vec<String>,
        model: Option<String>,
        status: Option<String>,
        /// Prompt text sent with the collab call, when Codex exposes it.
        #[serde(skip_serializing_if = "Option::is_none")]
        prompt: Option<String>,
        /// Last known per-child agent status (`agentStates`) keyed by child
        /// thread id, when Codex exposed it. Additive/optional: older bridge
        /// payloads omit the field entirely.
        #[serde(skip_serializing_if = "Option::is_none")]
        agents_states: Option<HashMap<String, CollabAgentStateProjection>>,
    },
    SubAgentActivity {
        id: String,
        /// The activity kind (`kind` is reserved by the internal tag).
        activity_kind: Option<String>,
        agent_thread_id: Option<String>,
        agent_path: Option<String>,
    },
    Plan {
        id: String,
        /// Plan text; the completed plan item is authoritative in Codex.
        text: String,
    },
    ContextCompaction {
        id: String,
    },
    HookPrompt {
        id: String,
        fragment_count: usize,
    },
    ReviewMode {
        id: String,
        entered: bool,
        /// Review-mode text (hint/output) when Codex exposes it.
        review: String,
    },
    ImageView {
        id: String,
        path: Option<String>,
    },
    Opaque {
        id: String,
        /// Diagnostic Rust type name; not part of the closed contract.
        type_name: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        note: Option<String>,
    },
}

impl ItemProjection {
    pub fn id(&self) -> &str {
        match self {
            ItemProjection::UserMessage { id, .. }
            | ItemProjection::AgentMessage { id, .. }
            | ItemProjection::Reasoning { id, .. }
            | ItemProjection::CommandExecution { id, .. }
            | ItemProjection::FileChange { id, .. }
            | ItemProjection::ToolCall { id, .. }
            | ItemProjection::CollabToolCall { id, .. }
            | ItemProjection::SubAgentActivity { id, .. }
            | ItemProjection::Plan { id, .. }
            | ItemProjection::ContextCompaction { id }
            | ItemProjection::HookPrompt { id, .. }
            | ItemProjection::ReviewMode { id, .. }
            | ItemProjection::ImageView { id, .. }
            | ItemProjection::Opaque { id, .. } => id,
        }
    }

    pub fn kind_label(&self) -> &'static str {
        match self {
            ItemProjection::UserMessage { .. } => "userMessage",
            ItemProjection::AgentMessage { .. } => "agentMessage",
            ItemProjection::Reasoning { .. } => "reasoning",
            ItemProjection::CommandExecution { .. } => "commandExecution",
            ItemProjection::FileChange { .. } => "fileChange",
            ItemProjection::ToolCall { .. } => "toolCall",
            ItemProjection::CollabToolCall { .. } => "collabToolCall",
            ItemProjection::SubAgentActivity { .. } => "subAgentActivity",
            ItemProjection::Plan { .. } => "plan",
            ItemProjection::ContextCompaction { .. } => "contextCompaction",
            ItemProjection::HookPrompt { .. } => "hookPrompt",
            ItemProjection::ReviewMode { .. } => "reviewMode",
            ItemProjection::ImageView { .. } => "imageView",
            ItemProjection::Opaque { .. } => "opaque",
        }
    }
}

/// Non-text user input. Image/audio payloads and paths are presence/length
/// signals; the raw JSONL remains canonical for the payloads themselves.
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum AttachmentProjection {
    Image { data_url_present: bool },
    LocalImage { path: Option<String> },
    Audio { data_url_present: bool },
    LocalAudio { path: Option<String> },
    Skill { name: String, path: Option<String> },
    Mention { name: String, path: Option<String> },
    Opaque { type_name: String },
}

/// One child's last known status inside a collab tool call (`agentsStates`).
///
/// The `status` string is the provider's own agent status label (serde
/// camelCase: `completed`, `running`, `interrupted`, `errored`, `pendingInit`,
/// `shutdown`, `notFound`); `message` is the provider's optional per-child
/// completion/error detail. Consumers must treat this map as the only
/// per-child completion signal — the collab tool call's own `status` or tool
/// kind (SpawnAgent/SendInput/CloseAgent/Wait) is never a substitute for a
/// child's explicit `completed` status.
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct CollabAgentStateProjection {
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct FileChangeProjection {
    pub path: String,
    pub kind: String,
    /// Full diff payload (unified diff, added content, or deleted content).
    pub diff: String,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SessionMetaProjection {
    pub session_id: String,
    /// Owning thread identity (`session_meta.payload.id`).
    pub thread_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_thread_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub forked_from_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_nickname: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_role: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thread_source: Option<Value>,
    /// Raw `source` passthrough (string or subagent-spawn object); not secret-bearing.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub originator: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model_provider: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cli_version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<String>,
    /// Git state captured by Codex when the session started. This is a
    /// session-level snapshot, never per-turn state.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub git: Option<SessionGitProjection>,
}

/// Git state recorded on the Codex `session_meta` line.
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SessionGitProjection {
    /// Exact commit checked out when the session started.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub commit_hash: Option<String>,
    /// Branch name observed at session start (provenance only).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    /// Repository remote URL observed at session start (provenance only).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repository_url: Option<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct InterAgentProjection {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    pub author: Option<String>,
    pub recipient: Option<String>,
    pub other_recipients: Vec<String>,
    pub trigger_turn: bool,
    /// Newline-joined plaintext content (legacy `inter_agent_communication`
    /// and `response_item` agent messages).
    pub content: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub encrypted_content_length: Option<usize>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct CompactedProjection {
    /// Compaction summary message text.
    pub message: String,
    pub replacement_count: usize,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct TurnSummary {
    pub turn_id: String,
    pub status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_message: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub started_at: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<i64>,
    pub item_count: usize,
    /// Final deduplicated snapshot; upsert by `item_id` per (sourcePath, turnId).
    /// Response-derived items are merged in by their owning turn.
    pub items: Vec<ItemRef>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ItemRef {
    pub item_id: String,
    pub kind: String,
    pub first_seen_ordinal: u64,
    pub last_seen_ordinal: u64,
    pub seen_line_count: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_record_serializes_to_expected_closed_shape() {
        let record = LineRecord {
            schema_version: SCHEMA_VERSION.to_string(),
            record_type: RECORD_TYPE_LINE.to_string(),
            source_path: "sessions/a.jsonl".to_string(),
            source_ordinal: 7,
            decode: DecodeInfo {
                status: DECODE_OK.to_string(),
                diagnostic: None,
                kind: "eventMsg".to_string(),
                event_type: Some("agent_message".to_string()),
                rollout_ordinal: None,
                timestamp: Some("2026-08-17T03:25:48.586Z".to_string()),
            },
            projection: ProjectionRecord {
                changed_items: vec![ItemChange {
                    turn_id: "turn-1".to_string(),
                    item: ItemProjection::AgentMessage {
                        id: "item-1".to_string(),
                        text: "hi".to_string(),
                        content_hash: "0123456789abcdef".to_string(),
                        phase: None,
                    },
                    started_at_ms: None,
                    completed_at_ms: None,
                }],
                changed_turns: vec![TurnChange {
                    turn_id: "turn-1".to_string(),
                    status: Some("inProgress".to_string()),
                    error_message: None,
                    started_at: Some(1786934553),
                    completed_at: None,
                    duration_ms: None,
                }],
                removed_turn_ids: vec![],
                session_meta: None,
                inter_agent: None,
                compacted: None,
            },
        };
        let json = serde_json::to_string(&record).expect("serialize");
        let expected = "{\"schemaVersion\":\"editchain-v1\",\"recordType\":\"line\",\"sourcePath\":\"sessions/a.jsonl\",\"sourceOrdinal\":7,\"decode\":{\"status\":\"ok\",\"kind\":\"eventMsg\",\"eventType\":\"agent_message\",\"timestamp\":\"2026-08-17T03:25:48.586Z\"},\"projection\":{\"changedItems\":[{\"turnId\":\"turn-1\",\"item\":{\"kind\":\"agentMessage\",\"id\":\"item-1\",\"text\":\"hi\",\"contentHash\":\"0123456789abcdef\"}}],\"changedTurns\":[{\"turnId\":\"turn-1\",\"status\":\"inProgress\",\"startedAt\":1786934553}],\"removedTurnIds\":[]}}";
        assert_eq!(json, expected);
    }

    #[test]
    fn content_carrying_items_serialize_text_and_values() {
        let item = ItemProjection::ToolCall {
            id: "fc_1".to_string(),
            tool: "exec_command".to_string(),
            server: Some("codex".to_string()),
            namespace: None,
            plugin_id: None,
            status: Some("completed".to_string()),
            duration_ms: Some(42),
            arguments: Some(serde_json::json!({"cmd": "git status"})),
            result: Some(serde_json::json!({"stdout": " M README.md"})),
            error_message: None,
        };
        let json = serde_json::to_string(&item).expect("serialize");
        assert!(json.contains("\"arguments\":{\"cmd\":\"git status\"}"));
        assert!(json.contains("\"result\":{\"stdout\":\" M README.md\"}"));
        let file_item = ItemProjection::FileChange {
            id: "file-1".to_string(),
            status: Some("completed".to_string()),
            changes: vec![FileChangeProjection {
                path: "README.md".to_string(),
                kind: "update".to_string(),
                diff: "@@ -1 +1 @@\n-old\n+new".to_string(),
            }],
        };
        let json = serde_json::to_string(&file_item).expect("serialize");
        assert!(json.contains("\"diff\":\"@@ -1 +1 @@\\n-old\\n+new\""));
    }

    #[test]
    fn opaque_item_carries_diagnostic_type_name() {
        let item = ItemProjection::Opaque {
            id: "x".to_string(),
            type_name: "ImageGenerationItem".to_string(),
            note: None,
        };
        let json = serde_json::to_string(&item).expect("serialize");
        assert_eq!(
            json,
            "{\"kind\":\"opaque\",\"id\":\"x\",\"typeName\":\"ImageGenerationItem\"}"
        );
        assert_eq!(item.id(), "x");
        assert_eq!(item.kind_label(), "opaque");
    }
}
