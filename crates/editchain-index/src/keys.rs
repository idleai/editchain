//! Factual secondary keys; interpretation belongs to consumers.

use editchain_core::{
    ActorId, ChainId, ContentId, Op, OpId, OpKind, PathId, ScopeRef, SessionId, TurnId,
};
use serde::{Deserialize, Serialize};

use crate::ContentReference;

/// A recorded fact used to select accepted operation identities.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum IndexKey {
    /// Schema-three logical item identity.
    Item(editchain_core::activity::ItemId),
    /// Session membership, including records that also name a turn.
    SessionItem(editchain_core::activity::ItemId),
    /// Agent execution membership.
    TurnItem(editchain_core::activity::ItemId),
    /// Recorded author identity.
    AuthorItem(editchain_core::activity::ItemId),
    /// Capturing integration identity.
    Recorder(editchain_core::activity::ItemId),
    /// One of the ten operation categories.
    Kind(editchain_core::activity::KindName),
    /// Previous operation ID after an explicit physical conversion.
    Alias(OpId),
    /// The envelope's author.
    Actor(ActorId),
    /// The envelope's explicit chain scope.
    Chain(ChainId),
    /// The envelope's session scope or a session registration identity.
    Session(SessionId),
    /// The envelope's explicit turn scope.
    Turn(TurnId),
    /// The envelope's file scope, a file operation path, or a Git changed path.
    File(PathId),
    /// An envelope's causal parent (not an inferred relationship).
    Parent(OpId),
    /// A content address referenced by a supported record field.
    Content(ContentId),
}

pub(crate) fn keys(op: &Op, references: &[ContentReference]) -> Vec<IndexKey> {
    let mut keys = vec![IndexKey::Actor(op.actor)];
    keys.extend(op.parent_ids().copied().map(IndexKey::Parent));
    keys.extend(
        references
            .iter()
            .map(|reference| IndexKey::Content(reference.id)),
    );
    keys.extend(match op.scope {
        ScopeRef::None => None,
        ScopeRef::Chain(id) => Some(IndexKey::Chain(id)),
        ScopeRef::Session(id) => Some(IndexKey::Session(id)),
        ScopeRef::Turn(id) => Some(IndexKey::Turn(id)),
        ScopeRef::File(id) => Some(IndexKey::File(id)),
    });
    if let OpKind::Activity(record) = &op.kind {
        keys.push(IndexKey::Item(record.item));
        keys.push(IndexKey::Kind(record.kind.name()));
        keys.push(IndexKey::Recorder(record.recorder));
        keys.extend(record.session.map(IndexKey::SessionItem));
        keys.extend(record.turn.map(IndexKey::TurnItem));
        keys.extend(record.author.map(IndexKey::AuthorItem));
        keys.extend(
            record
                .legacy
                .as_ref()
                .map(|legacy| IndexKey::Alias(legacy.operation)),
        );
        keys.extend(
            record
                .legacy
                .iter()
                .flat_map(|legacy| legacy.folded.iter().copied().map(IndexKey::Alias)),
        );
        if let editchain_core::activity::Kind::File(file) = &record.kind {
            keys.push(IndexKey::File(file.path));
        }
        if let editchain_core::activity::Kind::Commit(commit) = &record.kind {
            keys.extend(commit.changed_paths.iter().copied().map(IndexKey::File));
        }
    }
    if let OpKind::File(file) = &op.kind {
        keys.push(IndexKey::File(file.path));
    }
    if let OpKind::Session(session) = &op.kind {
        keys.push(IndexKey::Session(session.id));
    }
    if let OpKind::GitCommit(commit) = &op.kind {
        keys.extend(commit.changed_paths.iter().copied().map(IndexKey::File));
    }
    keys
}
