//! Transitional rendering adapter. Its result must never replace stored bytes.

use super::{ChangeState, FileAction, Kind, NoteKind, Operation, Stage};
use crate::{
    ActorId, ActorOp, Clock, CommandOp, CommandStage, ErrorOp, FileOp, FileStage, ImportOp,
    MessageOp, NoteOp, NoteRelationship, Op, OpKind, ParentSet, Payload, ScopeRef, SessionId,
    SessionOp, Tags, ToolOp, ToolStage,
};

impl Operation {
    /// Adapter for existing renderers. Canonical queries return the full record.
    /// Rendering is lossy (for example, attachment previews), so this result
    /// is deliberately separate from the binary encoder and migration API.
    #[must_use]
    pub fn display_op(&self) -> Op {
        let legacy = self.legacy.as_ref();
        Op {
            id: self.id,
            source: None,
            actor: legacy.map_or(ActorId(0), |mapping| mapping.actor),
            clock: self.time_ms.map_or(Clock::None, Clock::UnixMs),
            scope: legacy.map_or(ScopeRef::None, |mapping| mapping.scope),
            tags: legacy.map_or(Tags::NONE, |mapping| mapping.tags),
            parents: match self.parents.as_slice() {
                [] => ParentSet::None,
                [a] => ParentSet::One(*a),
                [a, b, ..] => ParentSet::Two(*a, *b),
            },
            kind: self.display_kind(),
        }
    }

    fn display_kind(&self) -> OpKind {
        match &self.kind {
            Kind::Session(session) => OpKind::Session(SessionOp {
                id: match self.legacy.as_ref().map(|legacy| legacy.scope) {
                    Some(ScopeRef::Session(id)) => id,
                    Some(
                        ScopeRef::None | ScopeRef::Chain(_) | ScopeRef::Turn(_) | ScopeRef::File(_),
                    )
                    | None => SessionId(0),
                },
                parent: None,
                label: session.label.clone(),
                metadata: session.settings.clone(),
            }),
            Kind::Turn(turn) => OpKind::Note(NoteOp {
                target_ids: turn.triggers.clone(),
                relationship: NoteRelationship::Explains,
                content: Payload::Inline(format!("turn {:?}", turn.action).into_bytes()),
            }),
            Kind::Message(message) => {
                let mut bytes = Vec::new();
                for block in &message.blocks {
                    match &block.content {
                        Payload::Inline(content) => bytes.extend_from_slice(content),
                        Payload::Empty => {}
                        Payload::Blob(_) => bytes.extend_from_slice(b"[attachment]"),
                    }
                }
                OpKind::Message(MessageOp {
                    content: Payload::Inline(bytes),
                    content_type: message
                        .blocks
                        .first()
                        .map_or(Payload::Empty, |block| block.media_type.clone()),
                })
            }
            Kind::Tool(tool) => {
                let content = if tool.stage == Stage::Started {
                    tool.arguments.clone()
                } else {
                    tool.output
                        .as_ref()
                        .map_or(Payload::Empty, |output| output.content.clone())
                };
                if tool.terminal.is_some() {
                    OpKind::Command(CommandOp {
                        command_id: tool.native_call.clone(),
                        content,
                        stage: match tool.stage {
                            Stage::Started => CommandStage::Start,
                            Stage::Updated | Stage::Snapshot => CommandStage::Output,
                            Stage::Finished => CommandStage::Finish,
                        },
                    })
                } else {
                    OpKind::Tool(ToolOp {
                        tool_call_id: tool.native_call.clone(),
                        tool_name: tool.name.clone(),
                        content,
                        stage: match tool.stage {
                            Stage::Started => ToolStage::Start,
                            Stage::Updated | Stage::Snapshot => ToolStage::Delta,
                            Stage::Finished => ToolStage::Finish,
                        },
                    })
                }
            }
            Kind::File(file) => OpKind::File(FileOp {
                path: file.path,
                base: file.before,
                after: file.after,
                edit: file.edit.clone(),
                stage: match file.action {
                    FileAction::Delete => FileStage::Deleted,
                    FileAction::Save => FileStage::Saved,
                    FileAction::Create | FileAction::Change | FileAction::Rename => {
                        if file.change == Some(ChangeState::Proposed) {
                            FileStage::Proposed
                        } else {
                            FileStage::Applied
                        }
                    }
                    FileAction::Open
                    | FileAction::Close
                    | FileAction::Read
                    | FileAction::View
                    | FileAction::Snapshot => FileStage::Observed,
                },
            }),
            Kind::Commit(commit) => OpKind::GitCommit(commit.clone()),
            Kind::Note(note) => {
                if note.category == NoteKind::Error {
                    OpKind::Error(ErrorOp {
                        code: note.code.clone(),
                        message: note.content.clone(),
                    })
                } else {
                    OpKind::Note(NoteOp {
                        target_ids: note.targets.clone(),
                        relationship: if note.category == NoteKind::Correction {
                            NoteRelationship::Corrects
                        } else {
                            NoteRelationship::Explains
                        },
                        content: note.content.clone(),
                    })
                }
            }
            Kind::Author(author) => OpKind::Actor(ActorOp {
                label: author.label.clone(),
                role: author.native_role.clone(),
            }),
            Kind::Link(link) => OpKind::Note(NoteOp {
                target_ids: Vec::new(),
                relationship: NoteRelationship::Explains,
                content: link.content.clone(),
            }),
            Kind::Original(original) => OpKind::Import(ImportOp {
                raw_ref: original.bytes.clone(),
                raw_hash: original.hash,
            }),
        }
    }
}
