//! Shared record vocabulary, using the existing immutable operation schema.
//!
//! [`crate::records::OperationRecord`] is the complete envelope: it retains the operation ID,
//! causal parents, actor, clock, scope, tags, and kind. The other record names
//! are its typed payloads, not alternative serialization formats. Wrap them in
//! the corresponding [`crate::OpKind`] variant when recording a fact.
//!
//! A chain's logical identity is [`crate::ChainId`] in [`crate::ScopeRef::Chain`].
//! An actor registration describes the envelope's [`crate::Op::actor`]. Session
//! identity is [`crate::SessionOp::id`], while a revision occurrence is identified
//! by the envelope's [`crate::Op::id`], independently of its content IDs. Equal
//! contents do not merge separate observations or file lifecycle stages.
//!
//! Annotations retain their targets, relationship, and opaque content. Their
//! interpretation belongs to consumers. Reflections retain their recorded
//! coverage, window, anchors, and summary without replacing the covered facts.
//!
//! For imported or replicated operations, retain the original encoded bytes:
//! re-encoding a decoded record is not proof of byte equality. Admission is
//! governed by [`crate::OpSet`], including every conflicting representation.

pub use crate::op::{
    ActorOp as ActorRecord, ChainStart as ChainRecord, FileOp as RevisionRecord,
    NoteOp as AnnotationRecord, Op as OperationRecord, ReflectionOp as ReflectionRecord,
    SessionOp as SessionRecord,
};
