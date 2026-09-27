//! Factual secondary keys; interpretation belongs to consumers.

use editchain_core::{
    ActorId, ChainId, ContentId, Op, OpId, OpKind, PathId, ScopeRef, SessionId, TurnId,
};
use serde::{Deserialize, Serialize};

use crate::ContentReference;

/// A recorded fact used to select accepted operation identities.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum IndexKey {
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
    keys.extend(op.parents.iter().copied().map(IndexKey::Parent));
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
