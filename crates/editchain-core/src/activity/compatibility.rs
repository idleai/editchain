//! Read adapters never rewrite the old record. Physical upgrades assign new IDs.

use crate::{
    ActorId, Clock, CommandStage, FileStage, GitLinkKind, NoteRelationship, Op, OpId, OpKind,
    Payload, ScopeRef, SourceId, Tags, ToolStage,
};
use serde::{Deserialize, Serialize};

use super::{
    Author, AuthorRole, ChangeState, Completion, ContentUpdate, Coverage, Entity, File, FileAction,
    ItemId, Kind, Link, Message, MessageKind, Note, NoteKind, Operation, Original, OriginalRef,
    OutputChannel, Session, SessionAction, Stage, Status, Terminal, Tool, UpdateMode,
};

/// Compact old-address mapping retained on a physically converted operation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LegacyMapping {
    /// Previous immutable record address. Its bytes remain in the old chain.
    pub operation: OpId,
    /// Old metadata records incorporated into this record's direct fields.
    pub folded: Vec<OpId>,
    /// Original producer tuple, when available.
    pub source: Option<SourceId>,
    /// Historical numeric author alias.
    pub actor: ActorId,
    /// Historical clock, including non-wall-clock values.
    pub clock: Clock,
    /// Historical scope alias.
    pub scope: ScopeRef,
    /// Original filtering bits.
    pub tags: Tags,
}

/// Stable new address for a schema-three conversion of an old operation.
#[must_use]
pub fn upgrade_id(id: OpId) -> OpId {
    OpId::from_bytes(blake3::derive_key(
        "editchain.operation-schema3.v3",
        id.as_bytes(),
    ))
}

impl Operation {
    /// Present an old operation in the new vocabulary without changing its ID.
    /// Chain initialization belongs to chain metadata and has no activity row.
    #[must_use]
    pub fn view(op: &Op) -> Option<Self> {
        if let OpKind::Activity(record) = &op.kind {
            return Some((**record).clone());
        }
        let session = match op.scope {
            ScopeRef::Session(id) => Some(ItemId::legacy("session", id.0)),
            ScopeRef::None | ScopeRef::Chain(_) | ScopeRef::Turn(_) | ScopeRef::File(_) => None,
        };
        let turn = match op.scope {
            ScopeRef::Turn(id) => Some(ItemId::legacy("turn", id.0)),
            ScopeRef::None | ScopeRef::Chain(_) | ScopeRef::Session(_) | ScopeRef::File(_) => None,
        };
        let mut record = Self {
            id: op.id,
            item: ItemId::derive("legacy-item", op.id.as_bytes()),
            author: (op.actor.0 != 0).then(|| ItemId::legacy("author", op.actor.0)),
            recorder: ItemId::legacy(
                "legacy-recorder",
                op.source.map_or(0, |source| source.node.0),
            ),
            session,
            turn,
            time_ms: op.observed_unix_ms(),
            sequence: op.source.map(|source| source.seq),
            parents: op.parents.iter().copied().collect(),
            causes: Vec::new(),
            original: None,
            legacy: None,
            kind: kind(op)?,
        };
        if op.tags.matches_any(Tags::IMPORT) && !matches!(op.kind, OpKind::Import(_)) {
            record.original = op.parents.iter().next().map(|id| OriginalRef {
                operation: *id,
                converter: "legacy-to-schema3-v3".into(),
            });
        }
        if let (OpKind::Reflection(reflection), Kind::Message(message)) =
            (&op.kind, &mut record.kind)
        {
            if op.tags.matches_any(Tags::PRIVATE) {
                message.category = MessageKind::Reasoning;
                message.coverage = None;
                if reflection.anchors != Payload::Empty {
                    let mut content = block(
                        op.id,
                        reflection.anchors.clone(),
                        Payload::Empty,
                        UpdateMode::Replace,
                    );
                    content.position = Some(1);
                    message.blocks.push(content);
                }
            }
        }
        record.item = logical_item(&record, op);
        match &mut record.kind {
            Kind::Tool(tool) => tool.attempt = record.item,
            Kind::Message(message) => {
                for (position, block) in message.blocks.iter_mut().enumerate() {
                    let mut key = record.item.0.as_bytes().to_vec();
                    key.extend_from_slice(position.to_string().as_bytes());
                    block.block = ItemId::derive("message-block", &key);
                }
            }
            Kind::Session(_)
            | Kind::Turn(_)
            | Kind::File(_)
            | Kind::Commit(_)
            | Kind::Note(_)
            | Kind::Author(_)
            | Kind::Link(_)
            | Kind::Original(_) => {}
        }
        Some(record)
    }

    /// Convert to a new physical record with an explicit old-address mapping.
    /// This never permits replacement bytes to use the original operation ID.
    #[must_use]
    pub fn upgrade(op: &Op) -> Option<Self> {
        if let OpKind::Activity(record) = &op.kind {
            return Some((**record).clone());
        }
        let mut record = Self::view(op)?;
        record.id = upgrade_id(op.id);
        record.parents = record.parents.into_iter().map(upgrade_id).collect();
        record.legacy = Some(LegacyMapping {
            operation: op.id,
            folded: Vec::new(),
            source: op.source,
            actor: op.actor,
            clock: op.clock,
            scope: op.scope,
            tags: op.tags,
        });
        if let Some(original) = &mut record.original {
            original.operation = upgrade_id(original.operation);
        }
        match &mut record.kind {
            Kind::Note(note) => {
                note.targets = note.targets.iter().copied().map(upgrade_id).collect();
            }
            Kind::Link(link) => {
                remap_entity(&mut link.from);
                for target in &mut link.to {
                    remap_entity(target);
                }
            }
            Kind::Commit(commit) => commit.imported_record = commit.imported_record.map(upgrade_id),
            Kind::Session(session) => session.initiated_by = session.initiated_by.map(upgrade_id),
            Kind::Message(message) => {
                if let Some(coverage) = &mut message.coverage {
                    coverage.operations = coverage
                        .operations
                        .iter()
                        .copied()
                        .map(upgrade_id)
                        .collect();
                }
            }
            Kind::Turn(_) | Kind::Tool(_) | Kind::File(_) | Kind::Author(_) | Kind::Original(_) => {
            }
        }
        Some(record)
    }
}

fn remap_entity(entity: &mut Entity) {
    if let Entity::Operation(id) = entity {
        *id = upgrade_id(*id);
    }
}

fn logical_item(record: &Operation, op: &Op) -> ItemId {
    match &op.kind {
        OpKind::Actor(_) => ItemId::legacy("author", op.actor.0),
        OpKind::Session(session) => ItemId::legacy("session", session.id.0),
        OpKind::Tool(tool) => call_item(record, &tool.tool_call_id),
        OpKind::Command(command) => call_item(record, &command.command_id),
        OpKind::ChainStart(_)
        | OpKind::Message(_)
        | OpKind::File(_)
        | OpKind::Reflection(_)
        | OpKind::Import(_)
        | OpKind::Note(_)
        | OpKind::Error(_)
        | OpKind::GitCommit(_)
        | OpKind::GitLink(_)
        | OpKind::Unknown(_)
        | OpKind::Activity(_) => record.item,
    }
}

fn call_item(record: &Operation, native: &Payload) -> ItemId {
    let Payload::Inline(bytes) = native else {
        return record.item;
    };
    if bytes.is_empty() {
        return record.item;
    }
    let mut key = record
        .session
        .or(record.turn)
        .unwrap_or(record.recorder)
        .0
        .as_bytes()
        .to_vec();
    key.extend_from_slice(bytes);
    ItemId::derive("call", &key)
}

fn block(id: OpId, content: Payload, media_type: Payload, mode: UpdateMode) -> ContentUpdate {
    ContentUpdate {
        block: ItemId::derive("legacy-block", id.as_bytes()),
        position: Some(0),
        mode,
        previous: None,
        media_type,
        content,
    }
}

fn completion() -> Completion {
    Completion {
        status: Status::Unknown,
        detail: Payload::Empty,
    }
}

fn kind(op: &Op) -> Option<Kind> {
    Some(match &op.kind {
        OpKind::ChainStart(_) => return None,
        OpKind::Actor(value) => Kind::Author(Author {
            label: value.label.clone(),
            role: AuthorRole::Unknown,
            native_role: value.role.clone(),
            metadata: Payload::Empty,
        }),
        OpKind::Session(value) => Kind::Session(Session {
            action: SessionAction::Snapshot,
            label: value.label.clone(),
            settings: value.metadata.clone(),
            participants: Vec::new(),
            parent: value.parent.map(|id| ItemId::legacy("session", id.0)),
            initiated_by: None,
        }),
        OpKind::Message(value) => Kind::Message(Message {
            category: MessageKind::Text,
            stage: Stage::Snapshot,
            audience: Vec::new(),
            blocks: vec![block(
                op.id,
                value.content.clone(),
                value.content_type.clone(),
                UpdateMode::Replace,
            )],
            coverage: None,
            outcome: None,
        }),
        OpKind::Reflection(value) => Kind::Message(Message {
            category: MessageKind::Summary,
            stage: Stage::Snapshot,
            audience: Vec::new(),
            blocks: vec![block(
                op.id,
                value.summary.clone(),
                Payload::Empty,
                UpdateMode::Replace,
            )],
            coverage: Some(Coverage {
                operations: Vec::new(),
                frontiers: value.covers.clone(),
                window: Some(value.window),
                anchors: value.anchors.clone(),
            }),
            outcome: None,
        }),
        OpKind::Tool(value) => tool(
            op.id,
            (&value.tool_call_id, &value.tool_name),
            &value.content,
            match value.stage {
                ToolStage::Start => Stage::Started,
                ToolStage::Delta => Stage::Updated,
                ToolStage::Finish => Stage::Finished,
            },
            false,
        ),
        OpKind::Command(value) => tool(
            op.id,
            (&value.command_id, &Payload::Inline(b"terminal".to_vec())),
            &value.content,
            match value.stage {
                CommandStage::Start => Stage::Started,
                CommandStage::Output => Stage::Updated,
                CommandStage::Finish => Stage::Finished,
            },
            true,
        ),
        OpKind::File(value) => Kind::File(File {
            action: match value.stage {
                FileStage::Observed => FileAction::Snapshot,
                FileStage::Proposed | FileStage::Applied => FileAction::Change,
                FileStage::Saved => FileAction::Save,
                FileStage::Deleted => FileAction::Delete,
            },
            path: value.path,
            name: Payload::Empty,
            renamed_to: None,
            revision: Some(ItemId::derive("revision", op.id.as_bytes())),
            before: value.base,
            after: value.after,
            edit: value.edit.clone(),
            change: match value.stage {
                FileStage::Proposed => Some(ChangeState::Proposed),
                FileStage::Applied | FileStage::Deleted => Some(ChangeState::Applied),
                FileStage::Observed | FileStage::Saved => None,
            },
            text_edits: Vec::new(),
            caused_by: None,
            ranges: Vec::new(),
            text_ranges: Vec::new(),
            duration_ms: None,
        }),
        OpKind::Import(value) => Kind::Original(Original {
            provider: "unknown".into(),
            format: None,
            native: Vec::new(),
            location: None,
            bytes: value.raw_ref.clone(),
            hash: value.raw_hash,
        }),
        OpKind::Unknown(value) => Kind::Original(Original {
            provider: "unknown".into(),
            format: Some(format!("legacy-kind-{}", value.kind_discriminant)),
            native: Vec::new(),
            location: None,
            bytes: value.raw_bytes.clone(),
            hash: None,
        }),
        OpKind::Note(value) => match value.relationship {
            NoteRelationship::Explains | NoteRelationship::Corrects => Kind::Note(Note {
                category: if value.relationship == NoteRelationship::Corrects {
                    NoteKind::Correction
                } else {
                    NoteKind::Comment
                },
                targets: value.target_ids.clone(),
                items: Vec::new(),
                version: 1,
                content: value.content.clone(),
                code: Payload::Empty,
            }),
            NoteRelationship::Supersedes
            | NoteRelationship::Rejects
            | NoteRelationship::Redacts
            | NoteRelationship::ForkOf
            | NoteRelationship::SubagentOf
            | NoteRelationship::ReconnectsTo
            | NoteRelationship::OccurrenceOf
            | NoteRelationship::ProviderParent
            | NoteRelationship::LogicalParent
            | NoteRelationship::ForkedFrom
            | NoteRelationship::SpawnedBy
            | NoteRelationship::Contains
            | NoteRelationship::ToolResultOf
            | NoteRelationship::ProviderEvidence => Kind::Link(Link {
                from: Entity::Operation(op.parents.iter().next().copied().unwrap_or(op.id)),
                relation: format!("{:?}", value.relationship),
                to: value
                    .target_ids
                    .iter()
                    .copied()
                    .map(Entity::Operation)
                    .collect(),
                content: value.content.clone(),
            }),
        },
        OpKind::Error(value) => Kind::Note(Note {
            category: NoteKind::Error,
            targets: Vec::new(),
            items: Vec::new(),
            version: 1,
            content: value.message.clone(),
            code: value.code.clone(),
        }),
        OpKind::GitCommit(value) => Kind::Commit(value.clone()),
        OpKind::GitLink(value) => Kind::Link(Link {
            from: Entity::Operation(value.source),
            relation: match value.kind {
                GitLinkKind::BasedOn => "based_on",
                GitLinkKind::Checkpoint => "checkpoint",
                GitLinkKind::CommittedAs => "committed_as",
                GitLinkKind::ProducedBy => "produced_by",
                GitLinkKind::Mentions => "mentions",
                GitLinkKind::Custom(_) => "custom",
            }
            .into(),
            to: vec![Entity::Git {
                repository: value.target_repo,
                oid: value.target_oid,
            }],
            content: match &value.kind {
                GitLinkKind::Custom(bytes) => bytes.clone(),
                GitLinkKind::BasedOn
                | GitLinkKind::Checkpoint
                | GitLinkKind::CommittedAs
                | GitLinkKind::ProducedBy
                | GitLinkKind::Mentions => Payload::Empty,
            },
        }),
        OpKind::Activity(value) => return Some(value.kind.clone()),
    })
}

fn tool(
    id: OpId,
    identity: (&Payload, &Payload),
    content: &Payload,
    stage: Stage,
    terminal: bool,
) -> Kind {
    let (native, name) = identity;
    let started = stage == Stage::Started;
    Kind::Tool(Tool {
        native_call: native.clone(),
        name: name.clone(),
        stage,
        attempt: ItemId::derive("legacy-call", id.as_bytes()),
        parent_call: None,
        arguments: if started {
            content.clone()
        } else {
            Payload::Empty
        },
        channel: OutputChannel::Result,
        output: (!started).then(|| {
            block(
                id,
                content.clone(),
                Payload::Empty,
                if stage == Stage::Updated {
                    UpdateMode::Append
                } else {
                    UpdateMode::Replace
                },
            )
        }),
        terminal: terminal.then(|| Terminal {
            command: if started {
                content.clone()
            } else {
                Payload::Empty
            },
            cwd: Payload::Empty,
            exit_code: None,
        }),
        outcome: (stage == Stage::Finished).then(completion),
    })
}
