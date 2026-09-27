//! Relationships asserted by schema fields, without inferred graph topology.

use std::io;

use serde::{Deserialize, Serialize};

use crate::{GitLinkKind, GitOid, NoteRelationship, OpId, OpKind, RepositoryId, SessionId};

use super::{ChainQueries, ContentStatus, EvidenceRef, HistoryEntry, PageRequest, QueryPage};

/// A recorded operation, session, or repository-qualified Git object identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum EntityRef {
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
    pub evidence: EvidenceRef,
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
    /// Use [`Self::operation`] or [`Self::git`] to inspect endpoint evidence.
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
            evidence: entry.evidence,
            content: entry.content.clone(),
        });
    };
    for parent in &operation.parents {
        add(
            EntityRef::Operation(operation.id),
            EntityRef::Operation(*parent),
            RelationshipKind::CausalParent,
        );
    }
    match &operation.kind {
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
