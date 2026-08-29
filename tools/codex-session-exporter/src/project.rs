//! Projection of decoded Codex rollout items into the closed editchain-v1 schema.
//!
//! The bridge augments the Codex [`ThreadHistoryBuilder`] change sets with
//! response-derived items:
//!
//! - `response_item` messages/reasoning lines are projected as stable,
//!   sourcePath-scoped items carrying full text (typed `msg_`/`rs_` ids, or a
//!   deterministic `response-<ordinal>` fallback).
//! - Legacy `event_msg`/`response_item` echoes fold to one logical item using
//!   typed ids, turn context, and content correlation — never by hash alone.
//! - `item_completed` markers that the builder does not materialize
//!   (FileChange) are projected; message/reasoning markers only fold onto
//!   already-projected items.

use std::collections::HashMap;

use codex_app_server_protocol::ThreadHistoryBuilder;
use codex_app_server_protocol::ThreadHistoryChangeSet;
use codex_app_server_protocol::ThreadHistoryItemChange;
use codex_app_server_protocol::ThreadHistoryTurnChange;
use codex_app_server_protocol::ThreadItem;
use codex_app_server_protocol::Turn;
use codex_protocol::items::TurnItem;
use codex_protocol::models::AgentMessageInputContent;
use codex_protocol::models::ContentItem;
use codex_protocol::models::InternalChatMessageMetadataPassthrough;
use codex_protocol::models::MessagePhase;
use codex_protocol::models::ReasoningItemContent;
use codex_protocol::models::ReasoningItemReasoningSummary;
use codex_protocol::models::ResponseItem;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::FileChange as CoreFileChange;
use codex_rollout::decode_rollout_line;
use codex_rollout::RolloutItem;
use serde::Serialize;
use serde_json::Value;

use crate::schema::*;

/// FNV-1a 64-bit content fingerprint, hex-encoded lowercase.
///
/// Used as one correlation signal between `event_msg` message lines and their
/// `response_item` echoes. It is a fingerprint, not content, and never the
/// sole dedup identity: folding also requires typed ids, lifecycle position,
/// and turn context.
pub fn fnv1a64_hex(bytes: &[u8]) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        hash ^= u64::from(b);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{hash:016x}")
}

/// Serializes a Codex enum (status/kind/phase) to its wire string when possible.
pub fn enum_str<T: Serialize>(value: &T) -> Option<String> {
    serde_json::to_value(value).ok().and_then(|v| match v {
        Value::String(s) => Some(s),
        Value::Null => None,
        other => Some(other.to_string()),
    })
}

fn opt_value<T: Serialize>(value: &Option<T>) -> Option<Value> {
    serde_json::to_value(value).ok().filter(|v| !v.is_null())
}

/// Per-file projection state: the Codex thread-history builder plus the
/// bridge-owned response-item registry, echo tracker, and turn context.
pub struct SessionProjector {
    builder: ThreadHistoryBuilder,
    registry: ResponseRegistry,
    echo: EchoTracker,
    turn_context: TurnContext,
    response_item_message_count: u64,
    decoded_count: u64,
}

impl Default for SessionProjector {
    fn default() -> Self {
        Self::new()
    }
}

impl SessionProjector {
    pub fn new() -> Self {
        SessionProjector {
            builder: ThreadHistoryBuilder::new(),
            registry: ResponseRegistry::default(),
            echo: EchoTracker::default(),
            turn_context: TurnContext::default(),
            response_item_message_count: 0,
            decoded_count: 0,
        }
    }

    /// Projects one physical input line into a per-line [`LineRecord`].
    ///
    /// Ordering and identity stay deterministic: physical ordinals are 1-based
    /// line numbers; builder-generated item ids are stable counters; response
    /// item ids come from the typed `msg_`/`rs_`/`fc_` ids (or a deterministic
    /// `response-<ordinal>` fallback); turn ids come from the rollout payloads
    /// or the builder's deterministic rollout index.
    pub fn line_record(&mut self, source_path: &str, source_ordinal: u64, raw: &str) -> LineRecord {
        let parsed = serde_json::from_str::<Value>(raw);
        let Ok(value) = parsed else {
            let Err(err) = parsed else {
                unreachable!();
            };
            return LineRecord {
                schema_version: SCHEMA_VERSION.to_string(),
                record_type: RECORD_TYPE_LINE.to_string(),
                source_path: source_path.to_string(),
                source_ordinal,
                decode: DecodeInfo {
                    status: DECODE_ERROR.to_string(),
                    diagnostic: Some(format!("invalid JSON: {err}")),
                    kind: KIND_UNKNOWN_JSON.to_string(),
                    event_type: None,
                    rollout_ordinal: None,
                    timestamp: None,
                },
                projection: ProjectionRecord::default(),
            };
        };

        let hints = extract_wire_hints(&value);
        match decode_rollout_line(value) {
            Err(err) => LineRecord {
                schema_version: SCHEMA_VERSION.to_string(),
                record_type: RECORD_TYPE_LINE.to_string(),
                source_path: source_path.to_string(),
                source_ordinal,
                decode: DecodeInfo {
                    status: DECODE_ERROR.to_string(),
                    diagnostic: Some(err.to_string()),
                    kind: hints.kind.unwrap_or_else(|| KIND_UNKNOWN_JSON.to_string()),
                    event_type: hints.event_type,
                    rollout_ordinal: hints.rollout_ordinal,
                    timestamp: hints.timestamp,
                },
                projection: ProjectionRecord::default(),
            },
            Ok(line) => {
                self.decoded_count += 1;
                let changes = self.builder.handle_rollout_item_with_changes(&line.item);
                self.track_materialized_changes(&changes);
                self.turn_context
                    .observe(&line.item, self.decoded_count.saturating_sub(1));
                let mut projection = project_change_set(changes);
                self.project_response_item(&mut projection, &line.item, source_ordinal);
                self.project_item_completed_supplement(&mut projection, &line.item);
                enrich_projection(&mut projection, &line.item);
                LineRecord {
                    schema_version: SCHEMA_VERSION.to_string(),
                    record_type: RECORD_TYPE_LINE.to_string(),
                    source_path: source_path.to_string(),
                    source_ordinal,
                    decode: DecodeInfo {
                        status: DECODE_OK.to_string(),
                        diagnostic: None,
                        kind: rollout_item_kind(&line.item).to_string(),
                        event_type: if matches!(line.item, RolloutItem::EventMsg(_)) {
                            hints.event_type
                        } else {
                            None
                        },
                        rollout_ordinal: line.ordinal.or(hints.rollout_ordinal),
                        timestamp: Some(line.timestamp),
                    },
                    projection,
                }
            }
        }
    }

    /// Finalizes the underlying thread-history builder into turns, returning
    /// the turns and the response-derived registry for final reconciliation.
    pub fn finish(self) -> (Vec<Turn>, ResponseRegistry) {
        let turns = self.builder.finish();
        (turns, self.registry)
    }

    /// Response-derived items created by this projector (stable per file).
    pub fn registry(&self) -> &ResponseRegistry {
        &self.registry
    }

    /// Number of `response_item` message lines observed (for `--final` counts).
    pub fn response_item_message_count(&self) -> u64 {
        self.response_item_message_count
    }

    /// Registers builder-materialized message/reasoning/tool items so later
    /// `response_item` echoes can fold onto them by typed id and content correlation.
    fn track_materialized_changes(&mut self, changes: &ThreadHistoryChangeSet) {
        for change in &changes.changed_items {
            let tracked = matches!(
                change.item,
                ThreadItem::UserMessage { .. }
                    | ThreadItem::AgentMessage { .. }
                    | ThreadItem::Reasoning { .. }
                    | ThreadItem::McpToolCall { .. }
                    | ThreadItem::DynamicToolCall { .. }
            );
            if !tracked {
                continue;
            }
            let projection = project_thread_item(&change.item);
            let text = match &change.item {
                ThreadItem::McpToolCall { .. } | ThreadItem::DynamicToolCall { .. } => {
                    change.item.id().to_string()
                }
                _ => echo_text(&projection),
            };
            self.echo.upsert(
                change.turn_id.clone(),
                change.item.id().to_string(),
                text,
                projection,
            );
        }
    }

    /// Turn id for a response item: Codex's typed passthrough turn id first,
    /// then the tracked event turn, then a deterministic rollout fallback
    /// matching the builder's implicit turn ids.
    fn response_turn_id(
        &self,
        metadata: Option<&InternalChatMessageMetadataPassthrough>,
    ) -> String {
        metadata
            .and_then(|meta| meta.turn_id.clone())
            .filter(|s| !s.is_empty())
            .or_else(|| self.turn_context.current().map(str::to_string))
            .unwrap_or_else(|| format!("rollout-{}", self.decoded_count.saturating_sub(1)))
    }

    /// Projects one `response_item` line into message/reasoning/tool/inter-agent
    /// items, folding legacy echoes onto their materialized counterparts.
    fn project_response_item(
        &mut self,
        projection: &mut ProjectionRecord,
        item: &RolloutItem,
        source_ordinal: u64,
    ) {
        let RolloutItem::ResponseItem(envelope) = item else {
            return;
        };
        match &envelope.item {
            ResponseItem::Message {
                id,
                role,
                content,
                phase,
                internal_chat_message_metadata_passthrough,
            } => {
                self.response_item_message_count += 1;
                let turn_id =
                    self.response_turn_id(internal_chat_message_metadata_passthrough.as_ref());
                let text = join_message_content(content);
                match role.as_str() {
                    "user" | "developer" => self.upsert_response_message(
                        projection,
                        turn_id,
                        id.as_deref(),
                        text,
                        phase.as_ref(),
                        EchoKind::User,
                        source_ordinal,
                    ),
                    "assistant" => self.upsert_response_message(
                        projection,
                        turn_id,
                        id.as_deref(),
                        text,
                        phase.as_ref(),
                        EchoKind::Agent,
                        source_ordinal,
                    ),
                    other => {
                        projection.changed_items.push(ItemChange {
                            turn_id,
                            item: ItemProjection::Opaque {
                                id: response_item_id(id.as_deref(), source_ordinal),
                                type_name: "ResponseMessageRole".to_string(),
                                note: Some(format!("role: {other}")),
                            },
                            started_at_ms: None,
                            completed_at_ms: None,
                        });
                    }
                }
            }
            ResponseItem::Reasoning {
                id,
                summary,
                content,
                internal_chat_message_metadata_passthrough,
                ..
            } => {
                let turn_id =
                    self.response_turn_id(internal_chat_message_metadata_passthrough.as_ref());
                let summary_texts = summary
                    .iter()
                    .map(|s| match s {
                        ReasoningItemReasoningSummary::SummaryText { text } => text.clone(),
                    })
                    .collect::<Vec<_>>();
                let content_texts = content
                    .as_ref()
                    .map(|items| {
                        items
                            .iter()
                            .map(|c| match c {
                                ReasoningItemContent::ReasoningText { text } => text.clone(),
                                ReasoningItemContent::Text { text } => text.clone(),
                            })
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default();
                self.upsert_response_reasoning(
                    projection,
                    turn_id,
                    id.as_deref(),
                    summary_texts,
                    content_texts,
                    source_ordinal,
                );
            }
            ResponseItem::AgentMessage {
                author,
                recipient,
                content,
                ..
            } => {
                let mut plaintext = Vec::new();
                let mut encrypted_len: usize = 0;
                for part in content {
                    match part {
                        AgentMessageInputContent::InputText { text } => {
                            plaintext.push(text.clone())
                        }
                        AgentMessageInputContent::EncryptedContent { encrypted_content } => {
                            encrypted_len += encrypted_content.chars().count();
                        }
                    }
                }
                projection.inter_agent = Some(InterAgentProjection {
                    id: None,
                    author: Some(author.clone()),
                    recipient: Some(recipient.clone()),
                    other_recipients: Vec::new(),
                    trigger_turn: false,
                    content: plaintext.join("\n"),
                    encrypted_content_length: (encrypted_len > 0).then_some(encrypted_len),
                });
            }
            ResponseItem::FunctionCall {
                id,
                name,
                namespace,
                arguments,
                call_id,
                internal_chat_message_metadata_passthrough,
                ..
            } => self.upsert_response_function_call(
                projection,
                source_ordinal,
                id.as_deref(),
                Some(call_id.as_str()),
                name,
                namespace.as_deref(),
                arguments,
                None,
                internal_chat_message_metadata_passthrough.as_ref(),
            ),
            ResponseItem::CustomToolCall {
                id,
                status,
                call_id,
                name,
                namespace,
                input,
                internal_chat_message_metadata_passthrough,
                ..
            } => self.upsert_response_function_call(
                projection,
                source_ordinal,
                id.as_deref(),
                Some(call_id.as_str()),
                name,
                namespace.as_deref(),
                input,
                status.as_deref(),
                internal_chat_message_metadata_passthrough.as_ref(),
            ),
            ResponseItem::FunctionCallOutput {
                id,
                call_id,
                output,
                internal_chat_message_metadata_passthrough,
                ..
            } => self.upsert_response_function_call_output(
                projection,
                source_ordinal,
                id.as_deref(),
                call_id.as_deref(),
                output,
                internal_chat_message_metadata_passthrough.as_ref(),
            ),
            ResponseItem::CustomToolCallOutput {
                id,
                call_id,
                output,
                internal_chat_message_metadata_passthrough,
                ..
            } => self.upsert_response_function_call_output(
                projection,
                source_ordinal,
                id.as_deref(),
                Some(call_id.as_str()),
                output,
                internal_chat_message_metadata_passthrough.as_ref(),
            ),
            _ => {}
        }
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "response-message upsert needs projection, turn, identity, content, phase, kind, and ordinal fallback"
    )]
    fn upsert_response_message(
        &mut self,
        projection: &mut ProjectionRecord,
        turn_id: String,
        typed_id: Option<&str>,
        text: String,
        phase: Option<&MessagePhase>,
        kind: EchoKind,
        source_ordinal: u64,
    ) {
        let item_id = response_item_id(typed_id, source_ordinal);
        let text = text.trim_end_matches('\n').to_string();
        let (_resolved_id, resolved_turn, resolved_item) =
            if let Some(entry) = self.registry.get(&item_id) {
                (
                    entry.item_id.clone(),
                    entry.turn_id.clone(),
                    entry.projection.clone(),
                )
            } else if let Some(entry) = self.echo.get(&item_id) {
                (
                    entry.item_id.clone(),
                    entry.turn_id.clone(),
                    entry.projection.clone(),
                )
            } else if let Some((fold_id, fold_turn, fold_projection)) =
                self.echo
                    .fold(kind, Some(text.as_str()), self.turn_context.current())
            {
                (fold_id, fold_turn, fold_projection)
            } else {
                let item = match kind {
                    EchoKind::User => ItemProjection::UserMessage {
                        id: item_id.clone(),
                        text: text.clone(),
                        content_hash: content_hash_for(&text),
                        attachments: Vec::new(),
                    },
                    EchoKind::Agent => ItemProjection::AgentMessage {
                        id: item_id.clone(),
                        text: text.clone(),
                        content_hash: content_hash_for(&text),
                        phase: phase.and_then(enum_str),
                    },
                    EchoKind::Reasoning | EchoKind::Tool => unreachable!("kind mismatch"),
                };
                let label = match kind {
                    EchoKind::User => "userMessage",
                    EchoKind::Agent => "agentMessage",
                    EchoKind::Reasoning | EchoKind::Tool => unreachable!("kind mismatch"),
                };
                self.registry
                    .insert(turn_id.clone(), item_id.clone(), label, item.clone());
                (item_id, turn_id, item)
            };
        projection.changed_items.push(ItemChange {
            turn_id: resolved_turn,
            item: resolved_item,
            started_at_ms: None,
            completed_at_ms: None,
        });
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "response-reasoning upsert needs projection, turn, identity, summary/content, and ordinal fallback"
    )]
    fn upsert_response_reasoning(
        &mut self,
        projection: &mut ProjectionRecord,
        turn_id: String,
        typed_id: Option<&str>,
        summary: Vec<String>,
        content: Vec<String>,
        source_ordinal: u64,
    ) {
        let item_id = response_item_id(typed_id, source_ordinal);
        let text = summary.join("\n");
        let text = text.trim_end_matches('\n').to_string();
        let resolved = if let Some(entry) = self.registry.get(&item_id) {
            (
                entry.item_id.clone(),
                entry.turn_id.clone(),
                entry.projection.clone(),
            )
        } else if let Some(entry) = self.echo.get(&item_id) {
            (
                entry.item_id.clone(),
                entry.turn_id.clone(),
                entry.projection.clone(),
            )
        } else if text.is_empty() {
            match self
                .echo
                .fold(EchoKind::Reasoning, None, self.turn_context.current())
            {
                Some(fold) => fold,
                None => return, // empty lifecycle marker with no materialized counterpart
            }
        } else if let Some(fold) = self.echo.fold(
            EchoKind::Reasoning,
            Some(text.as_str()),
            self.turn_context.current(),
        ) {
            fold
        } else {
            let item = ItemProjection::Reasoning {
                id: item_id.clone(),
                summary,
                content,
            };
            self.registry
                .insert(turn_id.clone(), item_id.clone(), "reasoning", item.clone());
            (item_id, turn_id, item)
        };
        projection.changed_items.push(ItemChange {
            turn_id: resolved.1,
            item: resolved.2,
            started_at_ms: None,
            completed_at_ms: None,
        });
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "response tool-call upsert carries projection, identity, call/name/namespace/arguments/status, and passthrough turn metadata"
    )]
    fn upsert_response_function_call(
        &mut self,
        projection: &mut ProjectionRecord,
        source_ordinal: u64,
        typed_id: Option<&str>,
        call_id: Option<&str>,
        name: &str,
        namespace: Option<&str>,
        arguments_text: &str,
        status: Option<&str>,
        metadata: Option<&InternalChatMessageMetadataPassthrough>,
    ) {
        let turn_id = self.response_turn_id(metadata);
        let Some(call_id) = call_id.filter(|s| !s.is_empty()) else {
            return;
        };
        let arguments = parse_arguments(arguments_text);
        let resolved = if let Some(fold) = self.echo.fold_tool(call_id, self.turn_context.current())
        {
            fold
        } else if let Some(fold) = self.registry.find_by_call_id(call_id) {
            fold
        } else {
            let item_id = typed_id
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .unwrap_or_else(|| format!("response-{source_ordinal}"));
            let item = ItemProjection::ToolCall {
                id: item_id.clone(),
                tool: name.to_string(),
                server: None,
                namespace: namespace.map(str::to_string),
                plugin_id: None,
                status: status.map(str::to_string),
                duration_ms: None,
                arguments,
                result: None,
                error_message: None,
            };
            self.registry.insert_with_call_id(
                turn_id.clone(),
                item_id.clone(),
                call_id.to_string(),
                item.clone(),
            );
            (item_id, turn_id, item)
        };
        projection.changed_items.push(ItemChange {
            turn_id: resolved.1,
            item: resolved.2,
            started_at_ms: None,
            completed_at_ms: None,
        });
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "response tool-output upsert carries projection, identity, call id, output body, and passthrough turn metadata"
    )]
    fn upsert_response_function_call_output(
        &mut self,
        projection: &mut ProjectionRecord,
        source_ordinal: u64,
        typed_id: Option<&str>,
        call_id: Option<&str>,
        output: &codex_protocol::models::FunctionCallOutputPayload,
        metadata: Option<&InternalChatMessageMetadataPassthrough>,
    ) {
        let turn_id = self.response_turn_id(metadata);
        let Some(call_id) = call_id.filter(|s| !s.is_empty()) else {
            return;
        };
        let result = serde_json::to_value(&output.body)
            .ok()
            .or_else(|| serde_json::to_value(output).ok());
        let resolved = if let Some(fold) = self.echo.fold_tool(call_id, self.turn_context.current())
        {
            fold
        } else if let Some((entry_id, entry_turn, entry_projection)) =
            self.registry.find_by_call_id(call_id)
        {
            let mut item = entry_projection;
            if let ItemProjection::ToolCall { result: slot, .. } = &mut item {
                *slot = result;
            }
            self.registry.upsert_projection(&entry_id, item.clone());
            (entry_id, entry_turn, item)
        } else {
            let item_id = typed_id
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .unwrap_or_else(|| format!("response-{source_ordinal}"));
            let item = ItemProjection::ToolCall {
                id: item_id.clone(),
                tool: String::new(),
                server: None,
                namespace: None,
                plugin_id: None,
                status: None,
                duration_ms: None,
                arguments: None,
                result,
                error_message: None,
            };
            self.registry.insert_with_call_id(
                turn_id.clone(),
                item_id.clone(),
                call_id.to_string(),
                item.clone(),
            );
            (item_id, turn_id, item)
        };
        projection.changed_items.push(ItemChange {
            turn_id: resolved.1,
            item: resolved.2,
            started_at_ms: None,
            completed_at_ms: None,
        });
    }

    /// Projects `item_completed` markers the Codex builder does not
    /// materialize: FileChange carries real diffs; message/reasoning markers
    /// only fold onto already-projected items (never create).
    fn project_item_completed_supplement(
        &mut self,
        projection: &mut ProjectionRecord,
        item: &RolloutItem,
    ) {
        let RolloutItem::EventMsg(EventMsg::ItemCompleted(payload)) = item else {
            return;
        };
        match &payload.item {
            TurnItem::FileChange(file_change) => {
                let turn_id = payload.turn_id.clone();
                let mut changes = file_change
                    .changes
                    .iter()
                    .map(|(path, change)| FileChangeProjection {
                        path: path.to_string_lossy().into_owned(),
                        kind: core_file_change_kind(change),
                        diff: core_file_change_diff(change),
                    })
                    .collect::<Vec<_>>();
                changes.sort_by(|a, b| a.path.cmp(&b.path));
                let item_projection = ItemProjection::FileChange {
                    id: file_change.id.clone(),
                    status: file_change.status.as_ref().and_then(enum_str),
                    changes,
                };
                self.registry.upsert(
                    turn_id.clone(),
                    file_change.id.clone(),
                    item_projection.clone(),
                );
                projection.changed_items.push(ItemChange {
                    turn_id: turn_id.clone(),
                    item: item_projection,
                    started_at_ms: payload.started_at_ms,
                    completed_at_ms: Some(payload.completed_at_ms),
                });
            }
            TurnItem::UserMessage(_) | TurnItem::AgentMessage(_) | TurnItem::Reasoning(_) => {
                let (kind, text) = match &payload.item {
                    TurnItem::UserMessage(msg) => {
                        let text = msg
                            .content
                            .iter()
                            .filter_map(|input| {
                                serde_json::to_value(input)
                                    .ok()
                                    .map(|v| user_input_text(&v))
                            })
                            .collect::<Vec<_>>()
                            .join("\n");
                        (EchoKind::User, text)
                    }
                    TurnItem::AgentMessage(msg) => {
                        let text = msg
                            .content
                            .iter()
                            .map(|c| match c {
                                codex_protocol::items::AgentMessageContent::Text { text } => {
                                    text.clone()
                                }
                            })
                            .collect::<Vec<_>>()
                            .join("\n");
                        (EchoKind::Agent, text)
                    }
                    TurnItem::Reasoning(reasoning) => {
                        (EchoKind::Reasoning, reasoning.summary_text.join("\n"))
                    }
                    _ => unreachable!("guarded by match"),
                };
                let text = text.trim_end_matches('\n').to_string();
                let text_hash = content_hash_for(&text);
                let turn_id = payload.turn_id.clone();
                let fold = if text.is_empty() {
                    self.registry.fold_by_kind(&turn_id, kind, None)
                } else {
                    self.registry.fold_by_kind(&turn_id, kind, Some(&text_hash))
                };
                let Some((_item_id, marker_projection)) = fold else {
                    return; // marker without a projected counterpart
                };
                projection.changed_items.push(ItemChange {
                    turn_id,
                    item: marker_projection,
                    started_at_ms: payload.started_at_ms,
                    completed_at_ms: Some(payload.completed_at_ms),
                });
            }
            _ => {}
        }
    }
}

/// Coarse echo-correlation kind for materialized and response items.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EchoKind {
    User,
    Agent,
    Reasoning,
    Tool,
}

/// Registry of items created by the bridge (not present in builder turns):
/// response-derived messages/reasoning/tool calls and projected FileChange
/// items. Entries are kept in first-seen order for deterministic reconciliation.
#[derive(Debug, Default)]
pub struct ResponseRegistry {
    entries: Vec<RegistryEntry>,
    by_id: HashMap<String, usize>,
    call_id_to_item: HashMap<String, String>,
}

#[derive(Debug, Clone)]
pub struct RegistryEntry {
    pub turn_id: String,
    pub item_id: String,
    pub kind_label: &'static str,
    pub projection: ItemProjection,
}

impl ResponseRegistry {
    fn insert(
        &mut self,
        turn_id: String,
        item_id: String,
        kind_label: &'static str,
        projection: ItemProjection,
    ) {
        self.by_id.insert(item_id.clone(), self.entries.len());
        self.entries.push(RegistryEntry {
            turn_id,
            item_id,
            kind_label,
            projection,
        });
    }

    /// Inserts a new entry or replaces the projection of an existing one,
    /// keeping the original first-seen position.
    fn upsert(&mut self, turn_id: String, item_id: String, projection: ItemProjection) {
        if let Some(&index) = self.by_id.get(&item_id) {
            self.entries[index].turn_id = turn_id;
            self.entries[index].projection = projection;
        } else {
            let kind_label = projection.kind_label();
            self.insert(turn_id, item_id, kind_label, projection);
        }
    }

    fn insert_with_call_id(
        &mut self,
        turn_id: String,
        item_id: String,
        call_id: String,
        projection: ItemProjection,
    ) {
        self.call_id_to_item.insert(call_id, item_id.clone());
        self.insert(turn_id, item_id, "toolCall", projection);
    }

    fn get(&self, item_id: &str) -> Option<&RegistryEntry> {
        self.by_id.get(item_id).map(|&i| &self.entries[i])
    }

    fn find_by_call_id(&self, call_id: &str) -> Option<(String, String, ItemProjection)> {
        let entry = self
            .call_id_to_item
            .get(call_id)
            .and_then(|id| self.get(id))?;
        Some((
            entry.item_id.clone(),
            entry.turn_id.clone(),
            entry.projection.clone(),
        ))
    }

    fn upsert_projection(&mut self, item_id: &str, projection: ItemProjection) {
        if let Some(&index) = self.by_id.get(item_id) {
            self.entries[index].projection = projection;
        }
    }

    /// Most recent registry entry in `turn_id` of `kind` (optionally matching
    /// a text hash) — used to fold `item_completed` markers onto response items.
    fn fold_by_kind(
        &self,
        turn_id: &str,
        kind: EchoKind,
        text_hash: Option<&str>,
    ) -> Option<(String, ItemProjection)> {
        self.entries
            .iter()
            .rev()
            .filter(|entry| entry.turn_id == turn_id)
            .filter(|entry| {
                let matches_kind = match kind {
                    EchoKind::User => entry.kind_label == "userMessage",
                    EchoKind::Agent => entry.kind_label == "agentMessage",
                    EchoKind::Reasoning => entry.kind_label == "reasoning",
                    EchoKind::Tool => entry.kind_label == "toolCall",
                };
                if !matches_kind {
                    return false;
                }
                match text_hash {
                    None => true,
                    Some(hash) => entry_projection_hash(&entry.projection).as_deref() == Some(hash),
                }
            })
            .map(|entry| (entry.item_id.clone(), entry.projection.clone()))
            .next()
    }

    /// Deterministic ordered view for final reconciliation.
    pub fn entries(&self) -> &[RegistryEntry] {
        &self.entries
    }
}

/// Materialized (builder) message/reasoning/tool items available for echo
/// folding. Text is the current joined content; folds re-emit the latest
/// materialized projection so rich builder state is never regressed by an echo.
#[derive(Debug, Default)]
struct EchoTracker {
    entries: HashMap<String, EchoEntry>,
    order: Vec<String>,
}

#[derive(Debug)]
struct EchoEntry {
    turn_id: String,
    item_id: String,
    kind: EchoKind,
    text: String,
    projection: ItemProjection,
    echoed: bool,
}

impl EchoTracker {
    fn upsert(
        &mut self,
        turn_id: String,
        item_id: String,
        text: String,
        projection: ItemProjection,
    ) {
        if let Some(entry) = self.entries.get_mut(&item_id) {
            entry.turn_id = turn_id;
            entry.text = text;
            entry.projection = projection;
            return;
        }
        self.order.push(item_id.clone());
        self.entries.insert(
            item_id.clone(),
            EchoEntry {
                turn_id,
                item_id,
                kind: echo_kind_for_projection(&projection),
                text,
                projection,
                echoed: false,
            },
        );
    }

    fn get(&self, item_id: &str) -> Option<&EchoEntry> {
        self.entries.get(item_id)
    }

    /// Most recent un-echoed materialized item of `kind` optionally matching
    /// `text` (raw correlation text, not a hash), preferring `prefer_turn`
    /// when given and falling back to the most recent candidate across turns
    /// (needed for rollouts whose turn context is not explicitly opened).
    fn fold(
        &mut self,
        kind: EchoKind,
        text: Option<&str>,
        prefer_turn: Option<&str>,
    ) -> Option<(String, String, ItemProjection)> {
        let mut candidates = self.order.iter().rev().filter(|id| {
            self.entries.get(*id).is_some_and(|entry| {
                entry.kind == kind && !entry.echoed && text.is_none_or(|t| entry.text == t)
            })
        });
        let item_id = match prefer_turn {
            Some(turn) => candidates
                .find(|id| self.entries.get(*id).is_some_and(|e| e.turn_id == turn))
                .or_else(|| candidates.find(|_| true))
                .cloned(),
            None => candidates.find(|_| true).cloned(),
        }?;
        let entry = self.entries.get_mut(&item_id)?;
        entry.echoed = true;
        Some((
            entry.item_id.clone(),
            entry.turn_id.clone(),
            entry.projection.clone(),
        ))
    }

    /// Most recent tool item whose item id equals `call_id`, preferring
    /// `prefer_turn` and falling back across turns.
    fn fold_tool(
        &mut self,
        call_id: &str,
        prefer_turn: Option<&str>,
    ) -> Option<(String, String, ItemProjection)> {
        let mut candidates = self.order.iter().rev().filter(|id| {
            self.entries
                .get(*id)
                .is_some_and(|entry| entry.kind == EchoKind::Tool && entry.item_id == call_id)
        });
        let item_id = match prefer_turn {
            Some(turn) => candidates
                .find(|id| self.entries.get(*id).is_some_and(|e| e.turn_id == turn))
                .or_else(|| candidates.find(|_| true))
                .cloned(),
            None => candidates.find(|_| true).cloned(),
        }?;
        let entry = self.entries.get_mut(&item_id)?;
        entry.echoed = true;
        Some((
            entry.item_id.clone(),
            entry.turn_id.clone(),
            entry.projection.clone(),
        ))
    }
}

/// Tracks the current explicit/implicit turn so response items without a typed
/// passthrough turn id still land in the correct logical turn.
#[derive(Debug, Default)]
struct TurnContext {
    current: Option<String>,
    explicit: bool,
}

impl TurnContext {
    fn observe(&mut self, item: &RolloutItem, line_index: u64) {
        let RolloutItem::EventMsg(event) = item else {
            return;
        };
        match event {
            EventMsg::TurnStarted(payload) => {
                self.current = Some(payload.turn_id.clone());
                self.explicit = true;
            }
            EventMsg::TurnComplete(_) | EventMsg::TurnAborted(_) => {
                self.current = None;
                self.explicit = false;
            }
            EventMsg::UserMessage(_) if !self.explicit => {
                self.current = Some(format!("rollout-{line_index}"));
            }
            _ => {}
        }
    }

    fn current(&self) -> Option<&str> {
        self.current.as_deref()
    }
}

fn response_item_id(typed_id: Option<&str>, source_ordinal: u64) -> String {
    typed_id
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| format!("response-{source_ordinal}"))
}

/// Deterministic text extraction from a Responses API message body.
fn join_message_content(content: &[ContentItem]) -> String {
    content
        .iter()
        .filter_map(|item| match item {
            ContentItem::InputText { text } | ContentItem::OutputText { text } => {
                Some(text.clone())
            }
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Echo-correlation kind derived from a projected item.
fn echo_kind_for_projection(projection: &ItemProjection) -> EchoKind {
    match projection {
        ItemProjection::UserMessage { .. } => EchoKind::User,
        ItemProjection::AgentMessage { .. } => EchoKind::Agent,
        ItemProjection::Reasoning { .. } => EchoKind::Reasoning,
        _ => EchoKind::Tool,
    }
}

/// Correlation text for an already-projected item (message text or joined
/// reasoning summary; tool calls use their call id instead).
fn echo_text(projection: &ItemProjection) -> String {
    match projection {
        ItemProjection::UserMessage { text, .. } | ItemProjection::AgentMessage { text, .. } => {
            text.clone()
        }
        ItemProjection::Reasoning { summary, .. } => {
            summary.join("\n").trim_end_matches('\n').to_string()
        }
        _ => projection.id().to_string(),
    }
}

/// Hash of the projected text used to correlate `item_completed` markers.
fn entry_projection_hash(projection: &ItemProjection) -> Option<String> {
    match projection {
        ItemProjection::UserMessage { content_hash, .. }
        | ItemProjection::AgentMessage { content_hash, .. } => Some(content_hash.clone()),
        ItemProjection::Reasoning { summary, .. } => {
            Some(content_hash_for(summary.join("\n").trim_end_matches('\n')))
        }
        _ => None,
    }
}

fn content_hash_for(text: &str) -> String {
    fnv1a64_hex(text.as_bytes())
}

fn parse_arguments(arguments_text: &str) -> Option<Value> {
    if arguments_text.trim().is_empty() {
        return None;
    }
    serde_json::from_str(arguments_text)
        .ok()
        .or_else(|| Some(Value::String(arguments_text.to_string())))
}

fn core_file_change_kind(change: &CoreFileChange) -> String {
    match change {
        CoreFileChange::Add { .. } => "add".to_string(),
        CoreFileChange::Delete { .. } => "delete".to_string(),
        CoreFileChange::Update { .. } => "update".to_string(),
    }
}

fn core_file_change_diff(change: &CoreFileChange) -> String {
    match change {
        CoreFileChange::Add { content } | CoreFileChange::Delete { content } => content.clone(),
        CoreFileChange::Update { unified_diff, .. } => unified_diff.clone(),
    }
}

struct WireHints {
    kind: Option<String>,
    event_type: Option<String>,
    rollout_ordinal: Option<u64>,
    timestamp: Option<String>,
}

fn extract_wire_hints(value: &Value) -> WireHints {
    let event_type = value
        .get("payload")
        .and_then(|payload| payload.get("type").or_else(|| payload.get("event_type")))
        .and_then(Value::as_str)
        .map(str::to_owned);
    WireHints {
        kind: value.get("type").and_then(Value::as_str).map(str::to_owned),
        event_type,
        rollout_ordinal: value.get("ordinal").and_then(Value::as_u64),
        timestamp: value
            .get("timestamp")
            .and_then(Value::as_str)
            .map(str::to_owned),
    }
}

pub fn rollout_item_kind(item: &RolloutItem) -> &'static str {
    match item {
        RolloutItem::SessionMeta(_) => "sessionMeta",
        RolloutItem::ResponseItem(_) => "responseItem",
        RolloutItem::InterAgentCommunication(_) => "interAgentCommunication",
        RolloutItem::InterAgentCommunicationMetadata { .. } => "interAgentCommunicationMetadata",
        RolloutItem::Compacted(_) => "compacted",
        RolloutItem::TurnContext(_) => "turnContext",
        RolloutItem::WorldState(_) => "worldState",
        RolloutItem::SecurityRiskScore(_) => "securityRiskScore",
        RolloutItem::EventMsg(_) => "eventMsg",
    }
}

fn project_change_set(changes: ThreadHistoryChangeSet) -> ProjectionRecord {
    let changed_items = changes
        .changed_items
        .into_iter()
        .map(
            |ThreadHistoryItemChange {
                 turn_id,
                 item,
                 started_at_ms,
                 completed_at_ms,
             }| ItemChange {
                turn_id,
                item: project_thread_item(&item),
                started_at_ms,
                completed_at_ms,
            },
        )
        .collect();
    let changed_turns = changes
        .changed_turns
        .into_iter()
        .map(
            |ThreadHistoryTurnChange {
                 turn_id,
                 status,
                 error,
                 started_at,
                 completed_at,
                 duration_ms,
             }| {
                TurnChange {
                    turn_id,
                    status: enum_str(&status),
                    error_message: error.as_ref().map(|e| e.message.clone()),
                    started_at,
                    completed_at,
                    duration_ms,
                }
            },
        )
        .collect();
    ProjectionRecord {
        changed_items,
        changed_turns,
        removed_turn_ids: changes.removed_turn_ids,
        ..ProjectionRecord::default()
    }
}

fn enrich_projection(projection: &mut ProjectionRecord, item: &RolloutItem) {
    match item {
        RolloutItem::SessionMeta(meta_line) => {
            let meta = &meta_line.meta;
            projection.session_meta = Some(SessionMetaProjection {
                session_id: meta.session_id.to_string(),
                thread_id: meta.id.to_string(),
                parent_thread_id: meta.parent_thread_id.as_ref().map(ToString::to_string),
                forked_from_id: meta.forked_from_id.as_ref().map(ToString::to_string),
                agent_nickname: meta.agent_nickname.clone(),
                agent_role: meta.agent_role.clone(),
                agent_path: meta.agent_path.clone(),
                thread_source: opt_value(&meta.thread_source),
                source: serde_json::to_value(&meta.source).ok(),
                originator: Some(meta.originator.clone()),
                model_provider: meta.model_provider.clone(),
                cwd: Some(meta.cwd.to_string_lossy().into_owned()),
                cli_version: Some(meta.cli_version.clone()),
                timestamp: Some(meta.timestamp.clone()),
            });
        }
        RolloutItem::InterAgentCommunication(com) => {
            let id = serde_json::to_value(&com.id).ok().and_then(|v| match v {
                Value::String(s) => Some(s),
                _ => None,
            });
            projection.inter_agent = Some(InterAgentProjection {
                id,
                author: Some(com.author.as_str().to_string()),
                recipient: Some(com.recipient.as_str().to_string()),
                other_recipients: com
                    .other_recipients
                    .iter()
                    .map(|p| p.as_str().to_string())
                    .collect(),
                trigger_turn: com.trigger_turn,
                content: com.content.clone(),
                encrypted_content_length: com.encrypted_content.as_ref().map(|s| s.chars().count()),
            });
        }
        RolloutItem::Compacted(payload) => {
            projection.compacted = Some(CompactedProjection {
                message: payload.message.clone(),
                replacement_count: payload
                    .replacement_history
                    .as_ref()
                    .map(Vec::len)
                    .unwrap_or(0),
            });
        }
        _ => {}
    }
}

pub fn project_thread_item(item: &ThreadItem) -> ItemProjection {
    match item {
        ThreadItem::UserMessage { id, content, .. } => {
            let values: Vec<Value> = content
                .iter()
                .map(|input| serde_json::to_value(input).unwrap_or(Value::Null))
                .collect();
            let text = values
                .iter()
                .map(user_input_text)
                .collect::<Vec<_>>()
                .join("\n");
            // Codex's builder persists user text with a trailing newline; strip
            // it so the projected text and content_hash correlate 1:1 with the
            // response_item echo fingerprint.
            let text = text.trim_end_matches('\n').to_string();
            let content_hash = content_hash_for(&text);
            let attachments = values.iter().filter_map(project_attachment).collect();
            ItemProjection::UserMessage {
                id: id.clone(),
                text,
                content_hash,
                attachments,
            }
        }
        ThreadItem::AgentMessage {
            id, text, phase, ..
        } => {
            let text = text.trim_end_matches('\n').to_string();
            ItemProjection::AgentMessage {
                id: id.clone(),
                text: text.clone(),
                content_hash: content_hash_for(&text),
                phase: phase.as_ref().and_then(enum_str),
            }
        }
        ThreadItem::HookPrompt { id, fragments } => ItemProjection::HookPrompt {
            id: id.clone(),
            fragment_count: fragments.len(),
        },
        ThreadItem::Plan { id, text } => ItemProjection::Plan {
            id: id.clone(),
            text: text.clone(),
        },
        ThreadItem::Reasoning {
            id,
            summary,
            content,
        } => ItemProjection::Reasoning {
            id: id.clone(),
            summary: summary.clone(),
            content: content.clone(),
        },
        ThreadItem::CommandExecution {
            id,
            command,
            cwd,
            source,
            status,
            exit_code,
            duration_ms,
            aggregated_output,
            ..
        } => ItemProjection::CommandExecution {
            id: id.clone(),
            command: command.clone(),
            cwd: Some(cwd.as_str().to_string()),
            source: enum_str(source),
            status: enum_str(status),
            exit_code: *exit_code,
            duration_ms: *duration_ms,
            aggregated_output: aggregated_output.clone(),
        },
        ThreadItem::FileChange {
            id,
            changes,
            status,
        } => ItemProjection::FileChange {
            id: id.clone(),
            status: enum_str(status),
            changes: changes
                .iter()
                .map(|change| FileChangeProjection {
                    path: change.path.clone(),
                    kind: serde_json::to_value(&change.kind)
                        .ok()
                        .and_then(|v| v.get("type").and_then(Value::as_str).map(str::to_owned))
                        .unwrap_or_else(|| "unknown".to_string()),
                    diff: change.diff.clone(),
                })
                .collect(),
        },
        ThreadItem::McpToolCall {
            id,
            server,
            tool,
            status,
            arguments,
            plugin_id,
            result,
            error,
            duration_ms,
            ..
        } => ItemProjection::ToolCall {
            id: id.clone(),
            tool: tool.clone(),
            server: Some(server.clone()),
            namespace: None,
            plugin_id: plugin_id.clone(),
            status: enum_str(status),
            duration_ms: *duration_ms,
            arguments: (!arguments.is_null()).then(|| arguments.clone()),
            result: result
                .as_ref()
                .and_then(|r| serde_json::to_value(&**r).ok()),
            error_message: error
                .as_ref()
                .and_then(|e| serde_json::to_value(e).ok())
                .and_then(|v| v.get("message").and_then(Value::as_str).map(str::to_owned)),
        },
        ThreadItem::DynamicToolCall {
            id,
            namespace,
            tool,
            arguments,
            status,
            content_items,
            success,
            duration_ms,
        } => ItemProjection::ToolCall {
            id: id.clone(),
            tool: tool.clone(),
            server: None,
            namespace: namespace.clone(),
            plugin_id: None,
            status: enum_str(status),
            duration_ms: *duration_ms,
            arguments: (!arguments.is_null()).then(|| arguments.clone()),
            result: content_items
                .as_ref()
                .and_then(|items| serde_json::to_value(items).ok()),
            error_message: match (success, content_items) {
                (Some(false), None) => Some("tool call failed".to_string()),
                _ => None,
            },
        },
        ThreadItem::CollabAgentToolCall {
            id,
            tool,
            status,
            sender_thread_id,
            receiver_thread_ids,
            model,
            prompt,
            agents_states,
            ..
        } => ItemProjection::CollabToolCall {
            id: id.clone(),
            tool: enum_str(tool),
            sender_thread_id: Some(sender_thread_id.clone()),
            receiver_thread_ids: receiver_thread_ids.clone(),
            model: model.clone(),
            status: enum_str(status),
            prompt: prompt.clone(),
            agents_states: (!agents_states.is_empty()).then(|| {
                agents_states
                    .iter()
                    .map(|(thread_id, state)| {
                        (
                            thread_id.clone(),
                            CollabAgentStateProjection {
                                status: enum_str(&state.status).unwrap_or_default(),
                                message: state.message.clone(),
                            },
                        )
                    })
                    .collect()
            }),
        },
        ThreadItem::SubAgentActivity {
            id,
            kind,
            agent_thread_id,
            agent_path,
        } => ItemProjection::SubAgentActivity {
            id: id.clone(),
            activity_kind: enum_str(kind),
            agent_thread_id: Some(agent_thread_id.clone()),
            agent_path: Some(agent_path.clone()),
        },
        ThreadItem::WebSearch(_) => opaque(item, "WebSearchItem"),
        ThreadItem::ImageView { id, path } => ItemProjection::ImageView {
            id: id.clone(),
            path: Some(path.as_str().to_string()),
        },
        ThreadItem::Sleep(_) => opaque(item, "SleepItem"),
        ThreadItem::ImageGeneration(_) => opaque(item, "ImageGenerationItem"),
        ThreadItem::EnteredReviewMode { id, review } => ItemProjection::ReviewMode {
            id: id.clone(),
            entered: true,
            review: review.clone(),
        },
        ThreadItem::ExitedReviewMode { id, review } => ItemProjection::ReviewMode {
            id: id.clone(),
            entered: false,
            review: review.clone(),
        },
        ThreadItem::ContextCompaction { id } => {
            ItemProjection::ContextCompaction { id: id.clone() }
        }
    }
}

fn opaque(item: &ThreadItem, type_name: &str) -> ItemProjection {
    ItemProjection::Opaque {
        id: item.id().to_string(),
        type_name: type_name.to_string(),
        note: None,
    }
}

pub fn thread_item_kind_label(item: &ThreadItem) -> &'static str {
    match item {
        ThreadItem::UserMessage { .. } => "userMessage",
        ThreadItem::AgentMessage { .. } => "agentMessage",
        ThreadItem::Reasoning { .. } => "reasoning",
        ThreadItem::Plan { .. } => "plan",
        ThreadItem::CommandExecution { .. } => "commandExecution",
        ThreadItem::FileChange { .. } => "fileChange",
        ThreadItem::McpToolCall { .. } | ThreadItem::DynamicToolCall { .. } => "toolCall",
        ThreadItem::CollabAgentToolCall { .. } => "collabToolCall",
        ThreadItem::SubAgentActivity { .. } => "subAgentActivity",
        ThreadItem::ContextCompaction { .. } => "contextCompaction",
        ThreadItem::HookPrompt { .. } => "hookPrompt",
        ThreadItem::EnteredReviewMode { .. } | ThreadItem::ExitedReviewMode { .. } => "reviewMode",
        ThreadItem::ImageView { .. } => "imageView",
        ThreadItem::WebSearch(_) => "webSearch",
        ThreadItem::Sleep(_) => "sleep",
        ThreadItem::ImageGeneration(_) => "imageGeneration",
    }
}

fn user_input_text(value: &Value) -> String {
    match value.get("type").and_then(Value::as_str) {
        Some("text") => value
            .get("text")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        _ => String::new(),
    }
}

fn project_attachment(value: &Value) -> Option<AttachmentProjection> {
    let ty = value.get("type").and_then(Value::as_str)?;
    let has_data_url = |key: &str| {
        value
            .get(key)
            .and_then(Value::as_str)
            .map(|s| s.starts_with("data:"))
            .unwrap_or(false)
    };
    let path = |key: &str| value.get(key).and_then(Value::as_str).map(str::to_owned);
    match ty {
        "text" => None,
        "image" => Some(AttachmentProjection::Image {
            data_url_present: has_data_url("image_url"),
        }),
        "local_image" | "localImage" => {
            Some(AttachmentProjection::LocalImage { path: path("path") })
        }
        "audio" => Some(AttachmentProjection::Audio {
            data_url_present: has_data_url("audio_url"),
        }),
        "local_audio" | "localAudio" => {
            Some(AttachmentProjection::LocalAudio { path: path("path") })
        }
        "skill" => Some(AttachmentProjection::Skill {
            name: value
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            path: path("path"),
        }),
        "mention" => Some(AttachmentProjection::Mention {
            name: value
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            path: value.get("path").and_then(Value::as_str).map(str::to_owned),
        }),
        other => Some(AttachmentProjection::Opaque {
            type_name: other.to_string(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn project_one(raw: &str) -> (LineRecord, SessionProjector) {
        let mut projector = SessionProjector::new();
        let record = projector.line_record("test.jsonl", 1, raw);
        (record, projector)
    }

    fn project_line(raw: &str) -> LineRecord {
        project_one(raw).0
    }

    fn project_lines(lines: &[&str]) -> (Vec<LineRecord>, SessionProjector) {
        let mut projector = SessionProjector::new();
        let mut out = Vec::new();
        for (index, raw) in lines.iter().enumerate() {
            out.push(projector.line_record("test.jsonl", index as u64 + 1, raw));
        }
        (out, projector)
    }

    #[test]
    fn legacy_session_meta_projects_thread_identity_without_ordinal() {
        let raw = r#"{"timestamp":"2026-08-17T03:25:48.584Z","type":"session_meta","payload":{"session_id":"11111111-1111-7111-8111-111111111111","id":"22222222-2222-7222-8222-222222222222","parent_thread_id":"33333333-3333-7333-8333-333333333333","timestamp":"t","cwd":"/tmp","originator":"test","cli_version":"1.0","source":"vscode"}}"#;
        let record = project_line(raw);
        assert_eq!(record.decode.status, DECODE_OK);
        assert_eq!(record.decode.kind, "sessionMeta");
        assert_eq!(record.decode.rollout_ordinal, None);
        let meta = record.projection.session_meta.expect("session meta");
        assert_eq!(meta.thread_id, "22222222-2222-7222-8222-222222222222");
        assert_eq!(meta.session_id, "11111111-1111-7111-8111-111111111111");
        assert_eq!(
            meta.parent_thread_id.as_deref(),
            Some("33333333-3333-7333-8333-333333333333")
        );
        assert_eq!(meta.source, Some(json!("vscode")));
    }

    #[test]
    fn paginated_session_meta_preserves_rollout_ordinal() {
        let raw = r#"{"timestamp":"t","ordinal":42,"type":"session_meta","payload":{"session_id":"11111111-1111-7111-8111-111111111111","id":"22222222-2222-7222-8222-222222222222","timestamp":"t","cwd":"/tmp","originator":"test","cli_version":"1.0","source":"vscode"}}"#;
        let record = project_line(raw);
        assert_eq!(record.decode.rollout_ordinal, Some(42));
        assert_eq!(record.source_ordinal, 1);
    }

    #[test]
    fn collab_tool_call_projects_per_child_agent_states() {
        use codex_app_server_protocol::{
            CollabAgentState, CollabAgentStatus, CollabAgentTool, CollabAgentToolCallStatus,
        };
        let item = ThreadItem::CollabAgentToolCall {
            id: "call-1".to_string(),
            tool: CollabAgentTool::SpawnAgent,
            status: CollabAgentToolCallStatus::Completed,
            sender_thread_id: "parent-1".to_string(),
            receiver_thread_ids: vec!["child-1".to_string()],
            prompt: Some("go".to_string()),
            model: Some("gpt-5.2-codex".to_string()),
            reasoning_effort: None,
            agents_states: [(
                "child-1".to_string(),
                CollabAgentState {
                    status: CollabAgentStatus::Completed,
                    message: Some("done".to_string()),
                },
            )]
            .into_iter()
            .collect(),
        };
        let projected = project_thread_item(&item);
        let ItemProjection::CollabToolCall { agents_states, .. } = &projected else {
            panic!("expected collabToolCall, got {:?}", projected);
        };
        let states = agents_states.as_ref().expect("agentsStates present");
        let child = states.get("child-1").expect("child state");
        assert_eq!(child.status, "completed");
        assert_eq!(child.message.as_deref(), Some("done"));
        let wire = serde_json::to_value(&projected).unwrap();
        assert_eq!(
            wire["agentsStates"]["child-1"]["status"],
            json!("completed")
        );
        assert_eq!(wire["agentsStates"]["child-1"]["message"], json!("done"));
    }

    #[test]
    fn collab_tool_call_without_agent_states_omits_the_field() {
        use codex_app_server_protocol::{CollabAgentTool, CollabAgentToolCallStatus};
        let item = ThreadItem::CollabAgentToolCall {
            id: "call-1".to_string(),
            tool: CollabAgentTool::SendInput,
            status: CollabAgentToolCallStatus::Completed,
            sender_thread_id: "parent-1".to_string(),
            receiver_thread_ids: vec!["child-1".to_string()],
            prompt: None,
            model: None,
            reasoning_effort: None,
            agents_states: Default::default(),
        };
        let projected = project_thread_item(&item);
        let ItemProjection::CollabToolCall { agents_states, .. } = &projected else {
            panic!("expected collabToolCall, got {:?}", projected);
        };
        assert!(agents_states.is_none(), "empty map must stay absent");
        let wire = serde_json::to_value(&projected).unwrap();
        assert!(
            wire.get("agentsStates").is_none(),
            "old bridge payload shape stays byte-stable"
        );
    }

    #[test]
    fn subagent_session_uses_payload_id_as_owning_thread_and_exposes_parent_metadata() {
        let raw = r#"{"timestamp":"t","type":"session_meta","payload":{"session_id":"11111111-1111-7111-8111-111111111111","id":"22222222-2222-7222-8222-222222222222","parent_thread_id":"11111111-1111-7111-8111-111111111111","timestamp":"t","cwd":"/tmp","originator":"codex","cli_version":"1.0","source":{"subagent":{"thread_spawn":{"parent_thread_id":"11111111-1111-7111-8111-111111111111","depth":1,"agent_path":"/root/a","agent_nickname":"Darwin","agent_role":null}}},"thread_source":"subagent","agent_nickname":"Darwin","agent_path":"/root/a","model_provider":"openai"}}"#;
        let record = project_line(raw);
        let meta = record.projection.session_meta.expect("session meta");
        assert_eq!(meta.thread_id, "22222222-2222-7222-8222-222222222222");
        assert_eq!(meta.session_id, "11111111-1111-7111-8111-111111111111");
        assert_eq!(
            meta.parent_thread_id.as_deref(),
            Some("11111111-1111-7111-8111-111111111111")
        );
        assert_eq!(meta.agent_nickname.as_deref(), Some("Darwin"));
        assert_eq!(meta.agent_path.as_deref(), Some("/root/a"));
        assert_eq!(meta.thread_source, Some(json!("subagent")));
        let source = meta.source.expect("source passthrough");
        assert_eq!(source["subagent"]["thread_spawn"]["depth"], 1);
    }

    #[test]
    fn inter_agent_projection_carries_plaintext_content() {
        let raw = r#"{"timestamp":"t","type":"inter_agent_communication","payload":{"author":"/root","recipient":"/root/sub","other_recipients":[],"content":"hello from parent","trigger_turn":true}}"#;
        let record = project_line(raw);
        assert_eq!(record.decode.status, DECODE_OK);
        let inter = record.projection.inter_agent.as_ref().expect("inter agent");
        assert_eq!(inter.author.as_deref(), Some("/root"));
        assert_eq!(inter.recipient.as_deref(), Some("/root/sub"));
        assert_eq!(inter.content, "hello from parent");
        assert!(inter.trigger_turn);
        assert!(inter.encrypted_content_length.is_none());
    }

    #[test]
    fn response_item_agent_message_projects_inter_agent_text() {
        let raw = r#"{"timestamp":"t","type":"response_item","payload":{"type":"agent_message","id":"am_1","author":"/root/sub","recipient":"/root","content":[{"type":"input_text","text":"status update"}]}}"#;
        let (record, _) = project_one(raw);
        let inter = record.projection.inter_agent.as_ref().expect("inter agent");
        assert_eq!(inter.author.as_deref(), Some("/root/sub"));
        assert_eq!(inter.recipient.as_deref(), Some("/root"));
        assert_eq!(inter.content, "status update");
    }

    #[test]
    fn user_message_with_local_image_projects_text_and_attachment() {
        let raw = r#"{"timestamp":"t","type":"event_msg","payload":{"type":"user_message","message":"look at this","local_images":["/tmp/a.png"],"local_audio":[],"text_elements":[]}}"#;
        let record = project_line(raw);
        let item = &record.projection.changed_items[0].item;
        let ItemProjection::UserMessage {
            text, attachments, ..
        } = item
        else {
            panic!("expected userMessage, got {item:?}");
        };
        assert_eq!(text, "look at this");
        assert_eq!(attachments.len(), 1);
        assert!(matches!(
            attachments[0],
            AttachmentProjection::LocalImage { .. }
        ));
    }

    #[test]
    fn paginated_response_message_projects_full_text_item() {
        let raw = r#"{"timestamp":"t","ordinal":9,"type":"response_item","payload":{"type":"message","id":"msg_66dc19c4429447abba07bafd36adf4d5","role":"assistant","content":[{"type":"output_text","text":"hello world"}],"internal_chat_message_metadata_passthrough":{"turn_id":"turn-1"}}}"#;
        let (record, projector) = project_one(raw);
        assert_eq!(record.decode.status, DECODE_OK);
        assert_eq!(record.decode.kind, "responseItem");
        assert_eq!(record.decode.rollout_ordinal, Some(9));
        assert_eq!(record.projection.changed_items.len(), 1);
        let change = &record.projection.changed_items[0];
        assert_eq!(change.turn_id, "turn-1");
        let ItemProjection::AgentMessage {
            id,
            text,
            content_hash,
            ..
        } = &change.item
        else {
            panic!("expected agentMessage, got {:?}", change.item);
        };
        assert_eq!(id, "msg_66dc19c4429447abba07bafd36adf4d5");
        assert_eq!(text, "hello world");
        assert_eq!(content_hash, &fnv1a64_hex(b"hello world"));
        assert_eq!(projector.response_item_message_count(), 1);
        assert_eq!(projector.registry().entries().len(), 1);
        assert_eq!(projector.registry().entries()[0].kind_label, "agentMessage");
    }

    #[test]
    fn paginated_response_user_message_uses_fallback_id_without_typed_id() {
        let raw = r#"{"timestamp":"t","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"hi"}]}}"#;
        let (record, _) = project_one(raw);
        let change = &record.projection.changed_items[0];
        let ItemProjection::UserMessage { id, text, .. } = &change.item else {
            panic!("expected userMessage, got {:?}", change.item);
        };
        assert_eq!(id, "response-1");
        assert_eq!(text, "hi");
    }

    #[test]
    fn legacy_agent_message_echo_folds_to_one_logical_item() {
        let task_started = r#"{"timestamp":"t","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1","started_at":1786934553,"model_context_window":null,"collaboration_mode_kind":"default"}}"#;
        let agent_message = r#"{"timestamp":"t","type":"event_msg","payload":{"type":"agent_message","message":"same text","phase":null,"memory_citation":null}}"#;
        let echo = r#"{"timestamp":"t","type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"same text"}]}}"#;
        let (records, projector) = project_lines(&[task_started, agent_message, echo]);
        let first = &records[1];
        let second = &records[2];
        assert_eq!(
            first.projection.changed_items[0].item.id(),
            second.projection.changed_items[0].item.id(),
            "echo must reuse the materialized item id"
        );
        assert_eq!(
            first.projection.changed_items[0].item.kind_label(),
            "agentMessage"
        );
        assert_eq!(
            first.projection.changed_items[0].item,
            second.projection.changed_items[0].item
        );
        assert!(projector.registry().entries().is_empty());
    }

    #[test]
    fn repeated_identical_text_is_not_deduped_by_hash() {
        let task_started = r#"{"timestamp":"t","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1","started_at":1786934553,"model_context_window":null,"collaboration_mode_kind":"default"}}"#;
        let first = r#"{"timestamp":"t","type":"event_msg","payload":{"type":"agent_message","message":"same text","phase":null,"memory_citation":null}}"#;
        let second = r#"{"timestamp":"t","type":"event_msg","payload":{"type":"agent_message","message":"same text","phase":null,"memory_citation":null}}"#;
        let (records, _projector) = project_lines(&[task_started, first, second]);
        let id_a = records[1].projection.changed_items[0].item.id().to_string();
        let id_b = records[2].projection.changed_items[0].item.id().to_string();
        assert_ne!(id_a, id_b, "same text twice must produce two logical items");
    }

    #[test]
    fn paginated_user_message_folds_item_completed_echo_by_content() {
        let response_user = r#"{"timestamp":"t","type":"response_item","payload":{"type":"message","id":"msg_user_1","role":"user","content":[{"type":"input_text","text":"audit the tree"}],"internal_chat_message_metadata_passthrough":{"turn_id":"turn-1"}}}"#;
        let marker = r#"{"timestamp":"t","type":"event_msg","payload":{"type":"item_completed","thread_id":"11111111-1111-7111-8111-111111111111","turn_id":"turn-1","item":{"type":"UserMessage","id":"uuid-other","content":[{"type":"text","text":"audit the tree"}]},"started_at_ms":1,"completed_at_ms":2}}"#;
        let (records, projector) = project_lines(&[response_user, marker]);
        assert_eq!(
            records[0].projection.changed_items[0].item.id(),
            "msg_user_1"
        );
        assert_eq!(
            records[1].projection.changed_items[0].item.id(),
            "msg_user_1",
            "item_completed echo must fold onto the response item"
        );
        assert_eq!(projector.registry().entries().len(), 1);
    }

    #[test]
    fn paginated_reasoning_projects_summary_content_and_folds_markers_by_id() {
        let reasoning = r#"{"timestamp":"t","type":"response_item","payload":{"type":"reasoning","id":"rs_1","summary":[{"type":"summary_text","text":"plan step 1"}],"content":[{"type":"reasoning_text","text":"raw chain of thought"}],"internal_chat_message_metadata_passthrough":{"turn_id":"turn-1"}}}"#;
        let marker = r#"{"timestamp":"t","type":"event_msg","payload":{"type":"item_completed","thread_id":"11111111-1111-7111-8111-111111111111","turn_id":"turn-1","item":{"type":"Reasoning","id":"rs_1","summary_text":["plan step 1"]},"started_at_ms":1,"completed_at_ms":2}}"#;
        let (records, _projector) = project_lines(&[reasoning, marker]);
        let ItemProjection::Reasoning {
            id,
            summary,
            content,
            ..
        } = &records[0].projection.changed_items[0].item
        else {
            panic!(
                "expected reasoning, got {:?}",
                records[0].projection.changed_items[0].item
            );
        };
        assert_eq!(id, "rs_1");
        assert_eq!(summary, &vec!["plan step 1".to_string()]);
        assert_eq!(content, &vec!["raw chain of thought".to_string()]);
        assert_eq!(records[1].projection.changed_items[0].item.id(), "rs_1");
    }

    #[test]
    fn legacy_empty_reasoning_marker_without_counterpart_is_ignored() {
        let task_started = r#"{"timestamp":"t","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1","started_at":1786934553,"model_context_window":null,"collaboration_mode_kind":"default"}}"#;
        let empty_reasoning = r#"{"timestamp":"t","type":"response_item","payload":{"type":"reasoning","id":"rs_empty","summary":[],"content":[]}}"#;
        let (records, projector) = project_lines(&[task_started, empty_reasoning]);
        assert!(
            records[1].projection.changed_items.is_empty(),
            "empty marker without counterpart must not create an item"
        );
        assert!(projector.registry().entries().is_empty());
    }

    #[test]
    fn function_call_and_output_fold_into_one_tool_item() {
        let call = r#"{"timestamp":"t","type":"response_item","payload":{"type":"function_call","id":"fc_1","name":"exec_command","arguments":"{\"cmd\":\"git status\"}","call_id":"call_9"}}"#;
        let output = r#"{"timestamp":"t","type":"response_item","payload":{"type":"function_call_output","id":"fco_1","call_id":"call_9","output":" M README.md"}}"#;
        let (records, projector) = project_lines(&[call, output]);
        assert_eq!(records[0].projection.changed_items[0].item.id(), "fc_1");
        assert_eq!(
            records[1].projection.changed_items[0].item.id(),
            "fc_1",
            "output must fold onto the call's item id via call_id"
        );
        let ItemProjection::ToolCall {
            arguments, result, ..
        } = &records[1].projection.changed_items[0].item
        else {
            panic!("expected toolCall");
        };
        assert_eq!(arguments, &Some(json!({"cmd": "git status"})));
        assert_eq!(result, &Some(json!(" M README.md")));
        assert_eq!(projector.registry().entries().len(), 1);
    }

    #[test]
    fn function_call_echo_folds_onto_materialized_tool_item() {
        let task_started = r#"{"timestamp":"t","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1","started_at":1786934553,"model_context_window":null,"collaboration_mode_kind":"default"}}"#;
        let begin = r#"{"timestamp":"t","type":"event_msg","payload":{"type":"mcp_tool_call_begin","turn_id":"turn-1","invocation":{"server":"srv","tool":"read","arguments":{"path":"/tmp/a"}},"call_id":"call_9"}}"#;
        let echo_call = r#"{"timestamp":"t","type":"response_item","payload":{"type":"function_call","id":"fc_9","name":"read","arguments":"{\"path\":\"/tmp/a\"}","call_id":"call_9"}}"#;
        let (records, _projector) = project_lines(&[task_started, begin, echo_call]);
        assert_eq!(
            records[2].projection.changed_items[0].item.id(),
            "call_9",
            "legacy function_call echo must fold onto the builder tool item id"
        );
    }

    #[test]
    fn item_completed_file_change_projects_diff() {
        let raw = r#"{"timestamp":"t","type":"event_msg","payload":{"type":"item_completed","thread_id":"11111111-1111-7111-8111-111111111111","turn_id":"turn-1","item":{"type":"FileChange","id":"file-1","changes":{"/a/b.txt":{"type":"update","unified_diff":"@@ -1 +1 @@\n-old\n+new"}},"status":"completed"},"started_at_ms":1,"completed_at_ms":2}}"#;
        let (record, projector) = project_one(raw);
        let change = &record.projection.changed_items[0];
        assert_eq!(change.turn_id, "turn-1");
        let ItemProjection::FileChange {
            id,
            changes,
            status,
        } = change.item.clone()
        else {
            panic!("expected fileChange, got {:?}", change.item);
        };
        assert_eq!(id, "file-1");
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].path, "/a/b.txt");
        assert_eq!(changes[0].kind, "update");
        assert_eq!(changes[0].diff, "@@ -1 +1 @@\n-old\n+new");
        assert!(status.is_some());
        assert_eq!(projector.registry().entries().len(), 1);
    }

    #[test]
    fn compacted_line_carries_message_text() {
        let raw = r#"{"timestamp":"t","type":"compacted","payload":{"message":"summarized earlier context","replacement_history":[]}}"#;
        let record = project_line(raw);
        let compacted = record.projection.compacted.expect("compacted");
        assert_eq!(compacted.message, "summarized earlier context");
        assert_eq!(compacted.replacement_count, 0);
    }

    #[test]
    fn unknown_future_line_preserves_raw_type_discriminant() {
        let raw = r#"{"timestamp":"t","type":"future_gadget","payload":{"x":1}}"#;
        let record = project_line(raw);
        assert_eq!(record.decode.status, DECODE_ERROR);
        assert_eq!(record.decode.kind, "future_gadget");
        assert!(record.decode.diagnostic.is_some());
        assert!(record.projection.changed_items.is_empty());
    }

    #[test]
    fn content_fingerprint_is_stable_and_case_sensitive() {
        let a = fnv1a64_hex(b"same text");
        let b = fnv1a64_hex(b"same text");
        let c = fnv1a64_hex(b"same Text");
        assert_eq!(a, b);
        assert_ne!(a, c);
        assert_eq!(a.len(), 16);
    }
}
