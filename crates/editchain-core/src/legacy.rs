//! Frozen EC02 operation envelope and deterministic upgrade to canonical IDs.
//!
//! The fourteen `OpKind` discriminants in schema 1 are pinned by wire fixtures.
//! Changing this contract requires a new record schema, not a reordered enum.

use crate::{
    ActorId, Clock, GitCommitEntity, GitLink, NoteOp, Op, OpId, OpKind, ParentSet, ScopeRef,
    SourceId, Tags,
};
use serde::{Deserialize, Serialize};

/// EC02's unversioned postcard envelope. Never add fields to this structure.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LegacyOp {
    /// Original producer address.
    pub id: SourceId,
    /// Original parent addresses.
    pub parents: ParentSet<SourceId>,
    /// Recorded actor.
    pub actor: ActorId,
    /// Recorded clock.
    pub clock: Clock,
    /// Recorded scope.
    pub scope: ScopeRef,
    /// Recorded flags.
    pub tags: Tags,
    /// Schema 1 payload and reference layout.
    pub kind: OpKind<SourceId>,
}

impl LegacyOp {
    /// Upgrade identity and typed references, including dangling targets.
    #[must_use]
    pub fn into_canonical(self) -> Op {
        Op {
            id: self.id.id(),
            source: Some(self.id),
            parents: match self.parents {
                ParentSet::None => ParentSet::None,
                ParentSet::One(id) => ParentSet::One(id.id()),
                ParentSet::Two(a, b) => ParentSet::Two(a.id(), b.id()),
            },
            actor: self.actor,
            clock: self.clock,
            scope: self.scope,
            tags: self.tags,
            kind: map_kind(self.kind, SourceId::id),
        }
    }
}

fn map_kind(kind: OpKind<SourceId>, map: impl Fn(SourceId) -> OpId) -> OpKind {
    match kind {
        OpKind::ChainStart(value) => OpKind::ChainStart(value),
        OpKind::Actor(value) => OpKind::Actor(value),
        OpKind::Message(value) => OpKind::Message(value),
        OpKind::Tool(value) => OpKind::Tool(value),
        OpKind::Command(value) => OpKind::Command(value),
        OpKind::File(value) => OpKind::File(value),
        OpKind::Reflection(value) => OpKind::Reflection(value),
        OpKind::Import(value) => OpKind::Import(value),
        OpKind::Error(value) => OpKind::Error(value),
        OpKind::Unknown(value) => OpKind::Unknown(value),
        OpKind::Session(value) => OpKind::Session(value),
        OpKind::Activity(value) => OpKind::Activity(value),
        OpKind::Note(value) => OpKind::Note(NoteOp {
            target_ids: value.target_ids.into_iter().map(map).collect(),
            relationship: value.relationship,
            content: value.content,
        }),
        OpKind::GitLink(value) => OpKind::GitLink(GitLink {
            source: map(value.source),
            target_repo: value.target_repo,
            target_oid: value.target_oid,
            kind: value.kind,
        }),
        OpKind::GitCommit(value) => OpKind::GitCommit(Box::new(GitCommitEntity {
            repository: value.repository,
            object_format: value.object_format,
            oid: value.oid,
            imported_record: value.imported_record.map(map),
            availability: value.availability,
            tree: value.tree,
            parents: value.parents,
            author: value.author,
            committer: value.committer,
            authored_at: value.authored_at,
            committed_at: value.committed_at,
            message: value.message,
            imported_refs: value.imported_refs,
            live_refs: value.live_refs,
            changed_paths: value.changed_paths,
        })),
    }
}
