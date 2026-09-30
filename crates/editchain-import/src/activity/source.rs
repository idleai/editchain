//! Provider fields used by the shared conversion pass.

use editchain_core::activity::{
    Completion, ItemId, Kind, MessageKind, NativeId, Operation, Session, SessionAction, Stage,
    Status, Turn, TurnAction,
};
use editchain_core::{NoteRelationship, Op, OpId, OpKind, Payload, ScopeRef, Tags};
use serde_json::Value;

#[derive(Debug)]
pub(super) struct Source {
    pub(super) provider: &'static str,
    pub(super) session: Option<ItemId>,
    turn: Option<ItemId>,
    native: Vec<NativeId>,
    shape: String,
    status: Option<Status>,
    finished: bool,
    exit_code: Option<i32>,
    label: Option<String>,
    parent_session: Option<ItemId>,
    item: Option<ItemId>,
    causes: Vec<ItemId>,
    recorder: Option<ItemId>,
    sequence: Option<u64>,
    slots: std::collections::BTreeMap<u64, String>,
    tool_outcomes: std::collections::BTreeMap<String, Status>,
}

impl Default for Source {
    fn default() -> Self {
        Self {
            provider: "unknown",
            session: None,
            turn: None,
            native: Vec::new(),
            shape: String::new(),
            status: None,
            finished: false,
            exit_code: None,
            label: None,
            parent_session: None,
            item: None,
            causes: Vec::new(),
            recorder: None,
            sequence: None,
            slots: std::collections::BTreeMap::new(),
            tool_outcomes: std::collections::BTreeMap::new(),
        }
    }
}

#[derive(Debug)]
pub(super) struct Derived {
    pub(super) original: OpId,
    pub(super) session: Option<ItemId>,
    pub(super) turn: Option<ItemId>,
    pub(super) item: Option<ItemId>,
    pub(super) incarnation: Option<ItemId>,
}

pub(super) fn identity(namespace: &str, value: &str) -> ItemId {
    ItemId::derive(namespace, value.as_bytes())
}

pub(super) fn legacy_session(value: &str) -> ItemId {
    ItemId::legacy("session", crate::derive_session_id(value).0)
}

pub(super) fn scoped(namespace: &str, parent: ItemId, value: &str) -> ItemId {
    let mut bytes = parent.0.as_bytes().to_vec();
    bytes.extend_from_slice(value.as_bytes());
    ItemId::derive(namespace, &bytes)
}

// Provider parent IDs name logical events, not physical import operations.
// Keep ordinary and logical parent relations distinct, including absent targets.
pub(super) fn parent_link(op: &Op, record: &mut Operation) {
    let (OpKind::Note(note), Kind::Link(link), Some(session)) =
        (&op.kind, &mut record.kind, record.session)
    else {
        return;
    };
    if !matches!(
        note.relationship,
        NoteRelationship::ProviderParent | NoteRelationship::LogicalParent
    ) {
        return;
    }
    let Payload::Inline(bytes) = &note.content else {
        return;
    };
    let Ok(value) = serde_json::from_slice::<Value>(bytes) else {
        return;
    };
    let Some(native) = value.get("externalId").and_then(Value::as_str) else {
        return;
    };
    if value.get("provider").and_then(Value::as_str) != Some("claude-code")
        || value.get("entityKind").and_then(Value::as_str) != Some("event")
        || note.target_ids.as_slice() != [crate::claude_code::topology::event_entity_id(native)]
    {
        return;
    }
    link.to = vec![editchain_core::activity::Entity::Item(scoped(
        "claude.record",
        session,
        native,
    ))];
}

fn text<'a>(value: &'a Value, keys: &[&str]) -> Option<&'a str> {
    keys.iter()
        .find_map(|key| value.get(key).and_then(Value::as_str))
        .filter(|text| !text.is_empty())
}

impl Source {
    pub(super) fn read(op: &Op, bytes: Option<&[u8]>) -> Self {
        let mut info = Self {
            recorder: op
                .source
                .map(|source| ItemId::legacy("legacy-recorder", source.node.0)),
            sequence: op.source.map(|source| source.seq),
            ..Self::default()
        };
        if let ScopeRef::Session(session) = op.scope {
            info.session = Some(ItemId::legacy("session", session.0));
        }
        let Some(value) = bytes.and_then(|bytes| serde_json::from_slice::<Value>(bytes).ok())
        else {
            return info;
        };
        let payload = value.get("payload").unwrap_or(&value);
        info.shape = text(&value, &["type"]).unwrap_or_default().into();
        let native_session = text(&value, &["sessionId", "session_id"]);
        if value.get("format").and_then(Value::as_str) == Some("editchain-human-history")
            || value.get("source").and_then(Value::as_str) == Some("vscode.editor")
        {
            info.provider = "human";
            let event = value.get("event").unwrap_or(&value);
            if let Some(session) = text(event, &["session"]) {
                info.session = Some(identity("human.session", session));
                info.native.push(NativeId {
                    kind: "session".into(),
                    value: session.into(),
                });
            }
        } else if native_session.is_some() || value.get("uuid").is_some() {
            info.provider = "claude";
            info.tool_outcomes = claude_tool_outcomes(&value);
            if let Some(raw) = op.source {
                let count = value
                    .pointer("/message/content")
                    .and_then(Value::as_array)
                    .map_or(0, Vec::len);
                let slots = [
                    Value::String("UserText".into()),
                    Value::String("Record".into()),
                ]
                .into_iter()
                .chain((0..count).flat_map(|index| {
                    [
                        serde_json::json!({"Block": index}),
                        serde_json::json!({"Command": index}),
                    ]
                }));
                for slot in slots {
                    if let Ok(key) = serde_json::to_string(&("claude-blocks-v1", raw.node, &slot)) {
                        let _old = info
                            .slots
                            .insert(crate::derive_node_id(&key).0, slot.to_string());
                    }
                }
            }
            info.session = native_session.map(legacy_session);
            if let Some(uuid) = text(&value, &["uuid"]) {
                info.native.push(NativeId {
                    kind: "record".into(),
                    value: uuid.into(),
                });
                if let Some(session) = info.session {
                    info.item = Some(scoped("claude.record", session, uuid));
                }
            }
            if let (Some(session), Some(parent)) =
                (info.session, text(&value, &["parentUuid", "parent_uuid"]))
            {
                info.causes.push(scoped("claude.record", session, parent));
            }
        } else if matches!(
            info.shape.as_str(),
            "session_meta"
                | "response_item"
                | "event_msg"
                | "turn_context"
                | "compacted"
                | "session_title"
        ) || value.get("provider").and_then(Value::as_str) == Some("codex")
        {
            info.provider = "codex";
            if info.shape == "session_meta" {
                if let Some(session) = text(payload, &["id", "thread_id"]) {
                    info.session = Some(legacy_session(session));
                    info.native.push(NativeId {
                        kind: "session".into(),
                        value: session.into(),
                    });
                }
                info.parent_session =
                    text(payload, &["parent_thread_id", "parentThreadId"]).map(legacy_session);
            }
        }
        if let Some(session) = native_session {
            info.native.push(NativeId {
                kind: "session".into(),
                value: session.into(),
            });
        }
        if info.session.is_none() {
            if let ScopeRef::Session(session) = op.scope {
                info.session = Some(ItemId::legacy("session", session.0));
            }
        }
        if let Some(turn) = text(payload, &["turn_id", "turnId"]) {
            info.native.push(NativeId {
                kind: "turn".into(),
                value: turn.into(),
            });
            info.turn = info
                .session
                .map(|session| scoped("codex.turn", session, turn));
        }
        for key in ["call_id", "tool_use_id", "parentUuid", "parent_uuid"] {
            if let Some(value) = text(payload, &[key]) {
                info.native.push(NativeId {
                    kind: key.into(),
                    value: value.into(),
                });
            }
        }
        info.label = text(&value, &["title", "summary", "aiTitle"]).map(str::to_owned);
        let item = payload.get("item").unwrap_or(payload);
        let status = text(item, &["status", "outcome"]);
        info.status = status.and_then(recorded_status);
        info.finished = status.is_some_and(|status| {
            matches!(
                status,
                "completed"
                    | "complete"
                    | "failed"
                    | "cancelled"
                    | "canceled"
                    | "interrupted"
                    | "success"
                    | "succeeded"
            )
        }) || text(payload, &["type"]).is_some_and(|kind| {
            matches!(kind, "task_complete" | "turn_completed" | "turn_aborted")
        });
        info.exit_code = item
            .get("exit_code")
            .or_else(|| item.get("exitCode"))
            .and_then(Value::as_i64)
            .and_then(|code| i32::try_from(code).ok());
        if let Some(code) = info.exit_code {
            info.status = Some(if code == 0 {
                Status::Success
            } else {
                Status::Failure
            });
        }
        info
    }

    pub(super) fn apply_original(&self, record: &mut Operation) {
        record.session = self.session.or(record.session);
        record.turn = self.turn;
        record.causes.clone_from(&self.causes);
        if let Some(item) = self.item {
            record.item = item;
        }
        if let Kind::Original(original) = &mut record.kind {
            original.provider = self.provider.into();
            original.format = Some(format!("{}.jsonl", self.provider));
            original.native.clone_from(&self.native);
        }
        if self.shape == "session_title" {
            record.item = record.session.unwrap_or(record.item);
            record.kind = Kind::Session(Session {
                action: SessionAction::Changed,
                label: self.label.as_ref().map_or(Payload::Empty, |label| {
                    Payload::Inline(label.as_bytes().to_vec())
                }),
                settings: Payload::Empty,
                participants: Vec::new(),
                parent: self.parent_session,
                initiated_by: None,
            });
        }
    }

    pub(super) fn session_record(&self, original: &Operation) -> Option<Operation> {
        if self.shape != "session_meta" {
            return None;
        }
        let session = self.session?;
        let mut record = original.clone();
        record.id = OpId::from_bytes(blake3::derive_key(
            "editchain.session-snapshot.v1",
            original.id.as_bytes(),
        ));
        record.item = session;
        record.legacy = None;
        record.parents = vec![original.id];
        record.original = Some(editchain_core::activity::OriginalRef {
            operation: original.id,
            converter: super::CONTRACT.into(),
        });
        record.kind = Kind::Session(Session {
            action: SessionAction::Snapshot,
            label: self.label.as_ref().map_or(Payload::Empty, |label| {
                Payload::Inline(label.as_bytes().to_vec())
            }),
            settings: Payload::Empty,
            participants: Vec::new(),
            parent: self.parent_session,
            initiated_by: None,
        });
        Some(record)
    }

    pub(super) fn apply_output(&self, record: &mut Operation, op: &Op) {
        record.session = self.session.or(record.session);
        record.turn = self.turn.or(record.turn);
        record.recorder = self.recorder.unwrap_or(record.recorder);
        record.sequence = self.sequence.or(record.sequence);
        if let Some(item) = self.item {
            // Block positions retain stable source-assigned lanes across capture retries.
            let slot = op
                .source
                .and_then(|source| self.slots.get(&source.node.0))
                .cloned()
                .unwrap_or_else(|| {
                    op.source
                        .map_or(0, |source| source.seq & 0xffff)
                        .to_string()
                });
            record.item = scoped("claude.block", item, &slot);
        }
        if let Kind::Tool(tool) = &mut record.kind {
            if let Some(result) = &mut tool.outcome {
                result.status = self
                    .tool_status(&tool.native_call)
                    .unwrap_or(Status::Unknown);
            }
            if let Some(terminal) = &mut tool.terminal {
                terminal.exit_code = self.exit_code;
            }
        }
    }

    fn tool_status(&self, native: &Payload) -> Option<Status> {
        let call = match native {
            Payload::Inline(bytes) => std::str::from_utf8(bytes).ok(),
            Payload::Empty | Payload::Blob(_) => None,
        };
        call.and_then(|call| {
            let native = serde_json::from_str::<String>(call).unwrap_or_else(|_| call.into());
            self.tool_outcomes.get(&native).copied()
        })
        .or(self.status)
    }
}

fn claude_tool_outcomes(value: &Value) -> std::collections::BTreeMap<String, Status> {
    value
        .pointer("/message/content")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|block| {
            if block.get("type").and_then(Value::as_str) != Some("tool_result") {
                return None;
            }
            let call = block.get("tool_use_id")?.as_str()?;
            let failed = block.get("is_error")?.as_bool()?;
            Some((
                call.into(),
                if failed {
                    Status::Failure
                } else {
                    Status::Success
                },
            ))
        })
        .collect()
}

impl Derived {
    pub(super) fn apply(&self, record: &mut Operation) {
        record.session = self.session.or(record.session);
        record.turn = self.turn.or(record.turn);
        if let Some(item) = self.item {
            record.item = item;
        }
        if let Kind::Tool(tool) = &mut record.kind {
            tool.attempt = self.incarnation.unwrap_or(record.item);
        }
    }
}

fn recorded_status(status: &str) -> Option<Status> {
    match status {
        "success" | "succeeded" => Some(Status::Success),
        "failed" | "failure" | "error" => Some(Status::Failure),
        "cancelled" | "canceled" | "interrupted" => Some(Status::Cancelled),
        "completed" | "complete" => Some(Status::Unknown),
        _ => None,
    }
}

pub(super) fn finish(op: &Op, record: &mut Operation, source: Option<&Source>) {
    if let Kind::Tool(tool) = &mut record.kind {
        if let Payload::Inline(native) = &tool.native_call {
            if !native.is_empty() {
                let native = serde_json::from_slice::<String>(native)
                    .unwrap_or_else(|_| String::from_utf8_lossy(native).into_owned());
                record.item = scoped(
                    "tool.call",
                    record.session.unwrap_or(record.recorder),
                    &native,
                );
            }
        }
        if let Some(output) = &mut tool.output {
            output.block = scoped("tool.output", record.item, &format!("{:?}", tool.channel));
        }
        if tool.attempt == ItemId::derive("legacy-call", op.id.as_bytes())
            || tool.attempt == ItemId::derive("legacy-item", op.id.as_bytes())
        {
            tool.attempt = record.item;
        }
    }
    if let Kind::Message(message) = &mut record.kind {
        if op.tags.matches_any(Tags::PRIVATE)
            && (matches!(op.kind, OpKind::Reflection(_))
                || (matches!(op.kind, OpKind::Message(_))
                    && source.is_some_and(|source| source.provider == "claude")))
        {
            message.category = MessageKind::Reasoning;
        }
        for (index, block) in message.blocks.iter_mut().enumerate() {
            block.block = scoped("message.block", record.item, &index.to_string());
        }
    }
    if let (OpKind::Note(note), ScopeRef::Turn(_)) = (&op.kind, op.scope) {
        if note.relationship == NoteRelationship::Explains
            && note.target_ids.is_empty()
            && source.is_some_and(|source| source.provider == "codex")
        {
            if let (Payload::Inline(summary), Some(session)) = (&note.content, record.session) {
                let summary = String::from_utf8_lossy(summary);
                let (native, tail) = summary.split_once(": ").unwrap_or((&summary, ""));
                let status = tail.split_whitespace().next().and_then(recorded_status);
                let turn = scoped("codex.turn", session, native);
                record.item = turn;
                record.turn = Some(turn);
                let finished = status.is_some() || source.is_some_and(|source| source.finished);
                record.kind = Kind::Turn(Turn {
                    action: if tail.starts_with("removed") {
                        TurnAction::Removed
                    } else if finished {
                        TurnAction::Finished
                    } else {
                        TurnAction::Snapshot
                    },
                    triggers: Vec::new(),
                    attempt: turn,
                    outcome: finished.then(|| Completion {
                        status: status
                            .or_else(|| source.and_then(|source| source.status))
                            .unwrap_or(Status::Unknown),
                        detail: Payload::Empty,
                    }),
                });
            }
        }
    }
    if let Kind::Message(message) = &mut record.kind {
        if source.is_some_and(|source| source.finished) {
            message.stage = Stage::Finished;
        }
    }
}
