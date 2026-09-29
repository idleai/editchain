//! Schema 3: ten operation types, shared logical identities, and explicit updates.
//!
//! Operation IDs identify immutable events. Item IDs identify the object updated
//! by those events. Neither identity depends on presentation or shortened IDs.

mod compatibility;
mod display;
mod fields;
mod model;
mod replay;
mod validation;

pub use compatibility::{upgrade_id, LegacyMapping};
pub use fields::Field;
pub use model::*;
pub use replay::{ContentState, ReplayError, StreamState};
pub use validation::ValidationError;

use crate::{ActorId, Clock, Op, OpId, OpKind, ParentSet, ScopeRef, Tags};
use serde::{Deserialize, Serialize};

/// A full logical identity, separate from an immutable operation ID.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ItemId(pub OpId);

impl ItemId {
    /// Derive an identity from a namespace and unambiguous native bytes.
    #[must_use]
    pub fn derive(namespace: &str, native: &[u8]) -> Self {
        let mut hash = blake3::Hasher::new_derive_key("editchain.logical-item.v1");
        let _hash = hash.update(
            &u64::try_from(namespace.len())
                .unwrap_or(u64::MAX)
                .to_le_bytes(),
        );
        let _hash = hash.update(namespace.as_bytes());
        let _hash = hash.update(native);
        Self(OpId::from_bytes(*hash.finalize().as_bytes()))
    }

    /// Deterministic adapter for historical numeric identities.
    #[must_use]
    pub fn legacy(kind: &str, id: u64) -> Self {
        Self::derive(kind, &id.to_le_bytes())
    }
}

impl std::fmt::Display for ItemId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

/// A direct reference to captured input and the converter that interpreted it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OriginalRef {
    /// Operation retaining the exact input.
    pub operation: OpId,
    /// Versioned converter contract.
    pub converter: String,
}

/// Immutable schema-three envelope. Missing observations remain absent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Operation {
    /// Identity of this exact persisted event.
    pub id: OpId,
    /// Session, turn, message, call, revision, or other object being updated.
    pub item: ItemId,
    /// Recorded author, absent when attribution is unknown.
    pub author: Option<ItemId>,
    /// Integration that captured the event, independent of its author.
    pub recorder: ItemId,
    /// Owning session, including on records that also specify a turn.
    pub session: Option<ItemId>,
    /// Agent execution, when one was recorded. Human edits need no turn.
    pub turn: Option<ItemId>,
    /// Recorded wall time, never an inferred import time.
    pub time_ms: Option<u64>,
    /// Native ordering within the recorder or stream, when supplied.
    pub sequence: Option<u64>,
    /// Explicit causal parents. Array order has no chronological meaning.
    pub parents: Vec<OpId>,
    /// Explicit logical causes, including provider parents known by native ID.
    pub causes: Vec<ItemId>,
    /// Captured input used by a converter; absent for direct structured capture.
    pub original: Option<OriginalRef>,
    /// Old address retained only when a physical conversion assigns a new ID.
    pub legacy: Option<LegacyMapping>,
    /// One of the ten supported operation types.
    pub kind: Kind,
}

impl Operation {
    /// Create a direct structured observation with no invented context.
    #[must_use]
    pub fn new(id: OpId, item: ItemId, recorder: ItemId, kind: Kind) -> Self {
        Self {
            id,
            item,
            recorder,
            kind,
            author: None,
            session: None,
            turn: None,
            time_ms: None,
            sequence: None,
            parents: Vec::new(),
            causes: Vec::new(),
            original: None,
            legacy: None,
        }
    }

    /// Check the store wrapper without copying payloads.
    #[must_use]
    pub fn matches_envelope(&self, op: &Op) -> bool {
        let legacy = self.legacy.as_ref();
        op.id == self.id
            && op.source.is_none()
            && op.actor == legacy.map_or(ActorId(0), |mapping| mapping.actor)
            && op.clock
                == legacy.map_or_else(
                    || self.time_ms.map_or(Clock::None, Clock::UnixMs),
                    |mapping| mapping.clock,
                )
            && op.scope == legacy.map_or(ScopeRef::None, |mapping| mapping.scope)
            && op.tags == legacy.map_or(Tags::NONE, |mapping| mapping.tags)
            && op.parents.iter().eq(self.parents.iter().take(2))
    }

    /// Remap physical references during an explicit conversion. Logical item
    /// IDs and old-address aliases remain unchanged.
    pub fn map_operation_ids(&mut self, map: impl Fn(OpId) -> OpId + Copy) {
        self.id = map(self.id);
        self.parents.iter_mut().for_each(|id| *id = map(*id));
        if let Some(original) = &mut self.original {
            original.operation = map(original.operation);
        }
        match &mut self.kind {
            Kind::Session(session) => session.initiated_by = session.initiated_by.map(map),
            Kind::Turn(turn) => turn.triggers.iter_mut().for_each(|id| *id = map(*id)),
            Kind::Message(message) => {
                for block in &mut message.blocks {
                    block.previous = block.previous.map(map);
                }
                if let Some(coverage) = &mut message.coverage {
                    coverage.operations.iter_mut().for_each(|id| *id = map(*id));
                }
            }
            Kind::Tool(tool) => {
                if let Some(output) = &mut tool.output {
                    output.previous = output.previous.map(map);
                }
            }
            Kind::Commit(commit) => commit.imported_record = commit.imported_record.map(map),
            Kind::Note(note) => note.targets.iter_mut().for_each(|id| *id = map(*id)),
            Kind::Link(link) => {
                for endpoint in std::iter::once(&mut link.from).chain(&mut link.to) {
                    if let Entity::Operation(id) = endpoint {
                        *id = map(*id);
                    }
                }
            }
            Kind::File(_) | Kind::Author(_) | Kind::Original(_) => {}
        }
    }

    /// Wrap a validated record for shared store and query APIs.
    /// # Errors
    /// Rejects inconsistent lifecycle, range, and identity fields.
    pub fn into_op(self) -> Result<Op, ValidationError> {
        self.validate()?;
        let legacy = self.legacy.as_ref();
        Ok(Op {
            id: self.id,
            source: None,
            parents: match self.parents.as_slice() {
                [] => ParentSet::None,
                [first] => ParentSet::One(*first),
                [first, second, ..] => ParentSet::Two(*first, *second),
            },
            actor: legacy.map_or(ActorId(0), |mapping| mapping.actor),
            clock: legacy.map_or_else(
                || self.time_ms.map_or(Clock::None, Clock::UnixMs),
                |mapping| mapping.clock,
            ),
            scope: legacy.map_or(ScopeRef::None, |mapping| mapping.scope),
            tags: legacy.map_or(Tags::NONE, |mapping| mapping.tags),
            kind: OpKind::Activity(Box::new(self)),
        })
    }
}
