//! Deterministic replay of explicitly linked content updates, independent of arrival order.

use std::collections::{BTreeMap, BTreeSet};

use super::{ContentUpdate, ItemId, Kind, Operation, Stage, UpdateMode, ValidationError};
use crate::{OpId, Payload};

/// Rebuilt content of one block in one attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContentState {
    /// Exact available bytes; missing predecessors never become an empty prefix.
    pub bytes: Vec<u8>,
    /// True only when a replacement or known empty start establishes the prefix.
    pub complete: bool,
    /// True when the selected head explicitly finished the item.
    pub finished: bool,
    /// Last contributing event.
    pub head: OpId,
}

/// Replay rejects ambiguity instead of choosing by hash order or arrival time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReplayError {
    /// Invalid standalone event.
    Invalid(ValidationError),
    /// Same operation ID delivered with changed fields.
    Conflict(OpId),
    /// Multiple unjoined content branches remain.
    Ambiguous(Vec<OpId>),
    /// A predecessor belongs to another item, attempt, or block.
    WrongPredecessor(OpId),
    /// Explicit predecessors contain a cycle.
    Cycle(OpId),
    /// The resolver could not supply referenced bytes.
    Unavailable(OpId),
}

impl std::fmt::Display for ReplayError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}

impl std::error::Error for ReplayError {}

/// Rebuildable streaming state. The store remains the immutable source of truth.
#[derive(Debug, Default)]
pub struct StreamState {
    records: BTreeMap<OpId, Operation>,
    items: BTreeMap<ItemId, BTreeSet<OpId>>,
}

impl StreamState {
    /// Accept delivery in any order; exact repeats are idempotent.
    /// # Errors
    /// Rejects invalid records and conflicting event identities.
    pub fn insert(&mut self, operation: Operation) -> Result<bool, ReplayError> {
        operation.validate().map_err(ReplayError::Invalid)?;
        if let Some(existing) = self.records.get(&operation.id) {
            return if existing == &operation {
                Ok(false)
            } else {
                Err(ReplayError::Conflict(operation.id))
            };
        }
        let _inserted = self
            .items
            .entry(operation.item)
            .or_default()
            .insert(operation.id);
        let _previous = self.records.insert(operation.id, operation);
        Ok(true)
    }

    /// Reconstruct one logical block. Calls must specify their attempt ID.
    /// Missing chunks return partial state; divergent branches return an error.
    /// # Errors
    /// Reports branches, cycles, cross-item references, or unavailable payloads.
    pub fn content(
        &self,
        item: ItemId,
        block: ItemId,
        attempt: Option<ItemId>,
        mut resolve: impl FnMut(&Payload) -> Option<Vec<u8>>,
    ) -> Result<Option<ContentState>, ReplayError> {
        let updates: BTreeMap<_, _> = self
            .items
            .get(&item)
            .into_iter()
            .flatten()
            .filter_map(|id| self.records.get(id))
            .filter_map(|record| {
                update(record, block, attempt).map(|value| (record.id, (record, value)))
            })
            .collect();
        let referenced: BTreeSet<_> = updates
            .values()
            .filter_map(|(_, value)| value.previous)
            .collect();
        let heads: Vec<_> = updates
            .keys()
            .filter(|id| !referenced.contains(id))
            .copied()
            .collect();
        let head = match heads.as_slice() {
            [] if updates.is_empty() => return Ok(None),
            [] => {
                return Err(ReplayError::Cycle(
                    updates.keys().next().copied().unwrap_or(item.0),
                ))
            }
            [head] => *head,
            _ => ordered_snapshot(&updates, &heads)
                .ok_or_else(|| ReplayError::Ambiguous(heads.clone()))?,
        };
        let mut cursor = Some(head);
        let mut visited = BTreeSet::new();
        let mut parts = Vec::new();
        let mut complete = false;
        while let Some(id) = cursor {
            if !visited.insert(id) {
                return Err(ReplayError::Cycle(id));
            }
            let Some((_, value)) = updates.get(&id) else {
                if self.records.contains_key(&id) {
                    return Err(ReplayError::WrongPredecessor(id));
                }
                break;
            };
            if value.content == Payload::Empty {
                return Err(ReplayError::Unavailable(id));
            }
            parts.push(resolve(&value.content).ok_or(ReplayError::Unavailable(id))?);
            if value.mode == UpdateMode::Replace {
                complete = true;
                break;
            }
            cursor = value.previous;
        }
        Ok(Some(ContentState {
            bytes: parts.into_iter().rev().flatten().collect(),
            complete,
            finished: self
                .items
                .get(&item)
                .into_iter()
                .flatten()
                .filter_map(|id| self.records.get(id))
                .any(|record| finished(record, attempt)),
            head,
        }))
    }
}

fn update(record: &Operation, block: ItemId, attempt: Option<ItemId>) -> Option<&ContentUpdate> {
    match &record.kind {
        Kind::Message(message) if attempt.is_none() => {
            message.blocks.iter().find(|update| update.block == block)
        }
        Kind::Tool(tool) if attempt == Some(tool.attempt) => {
            tool.output.as_ref().filter(|update| update.block == block)
        }
        Kind::Message(_)
        | Kind::Tool(_)
        | Kind::Session(_)
        | Kind::Turn(_)
        | Kind::File(_)
        | Kind::Commit(_)
        | Kind::Note(_)
        | Kind::Author(_)
        | Kind::Link(_)
        | Kind::Original(_) => None,
    }
}

fn finished(record: &Operation, attempt: Option<ItemId>) -> bool {
    match &record.kind {
        Kind::Message(message) => attempt.is_none() && message.stage == Stage::Finished,
        Kind::Tool(tool) => attempt == Some(tool.attempt) && tool.stage == Stage::Finished,
        Kind::Session(_)
        | Kind::Turn(_)
        | Kind::File(_)
        | Kind::Commit(_)
        | Kind::Note(_)
        | Kind::Author(_)
        | Kind::Link(_)
        | Kind::Original(_) => false,
    }
}

// Whole snapshots may be ordered only by an explicit sequence from one recorder.
// Explicit content branches still need a join, even when timestamps differ.
fn ordered_snapshot(
    updates: &BTreeMap<OpId, (&Operation, &ContentUpdate)>,
    heads: &[OpId],
) -> Option<OpId> {
    let mut ordered = BTreeMap::new();
    let recorder = updates.get(heads.first()?)?.0.recorder;
    for id in heads {
        let (record, value) = updates.get(id)?;
        if record.recorder != recorder
            || value.mode != UpdateMode::Replace
            || value.previous.is_some()
        {
            return None;
        }
        if ordered.insert(record.sequence?, *id).is_some() {
            return None;
        }
    }
    ordered.last_key_value().map(|(_sequence, id)| *id)
}
