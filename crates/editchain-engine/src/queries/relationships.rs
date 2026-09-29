//! Relationships asserted by schema fields, without inferred graph topology.

use std::io;

use serde::{Deserialize, Serialize};

use crate::{GitLinkKind, GitOid, NoteRelationship, OpId, OpKind, RepositoryId, SessionId};

use super::{ChainQueries, ContentStatus, HistoryEntry, PageRequest, QueryPage, RecordRef};

/// A recorded operation, session, or repository-qualified Git object identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum EntityRef {
    /// Schema-three logical identity.
    Item(editchain_core::activity::ItemId),
    /// Operation identity; the target can be missing or quarantined.
    Operation(OpId),
    /// Producer-assigned session identity.
    Session(SessionId),
    /// Full Git object identity, never conflated across repositories.
    Git {
        /// Recorded repository identity.
        repository: RepositoryId,
        /// Full SHA-1 or SHA-256 object ID.
        oid: GitOid,
    },
}

/// Exact asserted relationship kind, with no controller or rendering semantics.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum RelationshipKind {
    /// Named direct schema-three field or explicit link relation.
    Recorded(String),
    /// The source operation's envelope names the target causal parent.
    CausalParent,
    /// An annotation names a target. The source is the note itself; its causal
    /// parents remain separate and are never substituted as inferred sources.
    Annotation(NoteRelationship),
    /// An explicit session registration names its parent session.
    SessionParent,
    /// A recorded commit lists a parent commit in the same repository.
    GitParent,
    /// A recorded commit names its original importing operation.
    GitImportedRecord,
    /// An explicit Git link retains its original source, target, and kind.
    GitLink(GitLinkKind),
}

/// One asserted relationship with the exact record that supports it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecordedRelationship {
    /// Recorded source identity.
    pub source: EntityRef,
    /// Recorded target identity; existence is not implied by a reference.
    pub target: EntityRef,
    /// Relationship as recorded, without interpretation or edge inversion.
    pub kind: RelationshipKind,
    /// Original operation containing the assertion.
    pub record_ref: RecordRef,
    /// External content availability in the asserting record, including opaque
    /// annotation or custom-relation payloads. Resolve fields through `content`.
    pub content: Vec<ContentStatus>,
}

impl ChainQueries {
    /// Read recorded causal, annotation, session, and Git relationships.
    ///
    /// Optional `entity` selects incident relationships; `page` bounds scanned
    /// operations. Results retain record order and relationship field order,
    /// including repeated assertions. Missing endpoints are never synthesized.
    /// Use [`Self::operation`] or [`Self::git`] to inspect endpoint records.
    ///
    /// # Errors
    /// Returns invalid page limits or index/source IO errors.
    pub fn relationships(
        &self,
        entity: Option<EntityRef>,
        page: PageRequest,
    ) -> io::Result<QueryPage<RecordedRelationship>> {
        let history = self.history(None, page)?;
        let items = history
            .items
            .iter()
            .flat_map(recorded_relationships)
            .filter(|relation| {
                entity.is_none_or(|entity| relation.source == entity || relation.target == entity)
            })
            .collect();
        Ok(QueryPage {
            items,
            next_after: history.next_after,
        })
    }
}

pub(super) fn recorded_relationships(entry: &HistoryEntry) -> Vec<RecordedRelationship> {
    let operation = &entry.operation;
    let mut relations = Vec::new();
    let mut add = |source, target, kind| {
        relations.push(RecordedRelationship {
            source,
            target,
            kind,
            record_ref: entry.record_ref,
            content: entry.content.clone(),
        });
    };
    for parent in operation.parent_ids() {
        add(
            EntityRef::Operation(operation.id),
            EntityRef::Operation(*parent),
            RelationshipKind::CausalParent,
        );
    }
    match &operation.kind {
        OpKind::Activity(record) => {
            modern_relationships(record, &mut add);
        }
        OpKind::Note(note) => {
            for target in &note.target_ids {
                add(
                    EntityRef::Operation(operation.id),
                    EntityRef::Operation(*target),
                    RelationshipKind::Annotation(note.relationship),
                );
            }
        }
        OpKind::Session(session) => {
            if let Some(parent) = session.parent {
                add(
                    EntityRef::Session(session.id),
                    EntityRef::Session(parent),
                    RelationshipKind::SessionParent,
                );
            }
        }
        OpKind::GitCommit(commit) => {
            let source = EntityRef::Git {
                repository: commit.repository,
                oid: commit.oid,
            };
            for parent in &commit.parents {
                add(
                    source,
                    EntityRef::Git {
                        repository: commit.repository,
                        oid: *parent,
                    },
                    RelationshipKind::GitParent,
                );
            }
            if let Some(imported) = commit.imported_record {
                add(
                    source,
                    EntityRef::Operation(imported),
                    RelationshipKind::GitImportedRecord,
                );
            }
        }
        OpKind::GitLink(link) => add(
            EntityRef::Operation(link.source),
            EntityRef::Git {
                repository: link.target_repo,
                oid: link.target_oid,
            },
            RelationshipKind::GitLink(link.kind.clone()),
        ),
        OpKind::ChainStart(_)
        | OpKind::Actor(_)
        | OpKind::Message(_)
        | OpKind::Tool(_)
        | OpKind::Command(_)
        | OpKind::File(_)
        | OpKind::Reflection(_)
        | OpKind::Import(_)
        | OpKind::Error(_)
        | OpKind::Unknown(_) => {}
    }
    relations
}

fn modern_relationships(
    record: &editchain_core::activity::Operation,
    add: &mut impl FnMut(EntityRef, EntityRef, RelationshipKind),
) {
    use editchain_core::activity::{Entity, Kind};
    let operation = EntityRef::Operation(record.id);
    let mut field =
        |source, target, name: &str| add(source, target, RelationshipKind::Recorded(name.into()));
    if let Some(session) = record.session {
        field(operation, EntityRef::Item(session), "session");
    }
    if let Some(turn) = record.turn {
        field(operation, EntityRef::Item(turn), "turn");
    }
    for cause in &record.causes {
        field(operation, EntityRef::Item(*cause), "cause");
    }
    if let Some(original) = &record.original {
        field(
            operation,
            EntityRef::Operation(original.operation),
            "original",
        );
    }
    let entity = |value: &Entity| match value {
        Entity::Operation(id) => EntityRef::Operation(*id),
        Entity::Item(id) => EntityRef::Item(*id),
        Entity::Git { repository, oid } => EntityRef::Git {
            repository: *repository,
            oid: *oid,
        },
    };
    match &record.kind {
        Kind::Session(session) => {
            if let Some(parent) = session.parent {
                field(
                    EntityRef::Item(record.item),
                    EntityRef::Item(parent),
                    "parent_session",
                );
            }
            if let Some(cause) = session.initiated_by {
                field(
                    EntityRef::Item(record.item),
                    EntityRef::Operation(cause),
                    "initiated_by",
                );
            }
        }
        Kind::Turn(turn) => {
            for trigger in &turn.triggers {
                field(operation, EntityRef::Operation(*trigger), "trigger");
            }
        }
        Kind::Message(message) => {
            if let Some(coverage) = &message.coverage {
                for covered in &coverage.operations {
                    field(operation, EntityRef::Operation(*covered), "summarizes");
                }
            }
        }
        Kind::Tool(tool) => {
            if let Some(parent) = tool.parent_call {
                field(
                    EntityRef::Item(record.item),
                    EntityRef::Item(parent),
                    "parent_call",
                );
            }
        }
        Kind::File(file) => {
            if let Some(cause) = file.caused_by {
                field(operation, EntityRef::Item(cause), "caused_by");
            }
        }
        Kind::Note(note) => {
            for item in &note.items {
                field(operation, EntityRef::Item(*item), "annotates");
            }
            for target in &note.targets {
                field(operation, EntityRef::Operation(*target), "annotates");
            }
        }
        Kind::Link(link) => {
            for target in &link.to {
                field(entity(&link.from), entity(target), &link.relation);
            }
        }
        Kind::Commit(commit) => {
            let source = EntityRef::Git {
                repository: commit.repository,
                oid: commit.oid,
            };
            for parent in &commit.parents {
                field(
                    source,
                    EntityRef::Git {
                        repository: commit.repository,
                        oid: *parent,
                    },
                    "git_parent",
                );
            }
            if let Some(original) = commit.imported_record {
                field(source, EntityRef::Operation(original), "original");
            }
        }
        Kind::Author(_) | Kind::Original(_) => {}
    }
}
