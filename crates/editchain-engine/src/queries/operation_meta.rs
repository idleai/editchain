//! Operation metadata: actor/session records, relationships, and causal parents.

use std::{
    collections::{BTreeMap, BTreeSet},
    io,
};

use serde::{Deserialize, Serialize};

use crate::{OpId, OpKind, ScopeRef};

use super::{
    relationships::recorded_relationships, ChainQueries, EntityRef, HistoryEntry, IndexKey, Lookup,
    RecordedRelationship,
};

/// A referenced operation and its explicit lookup outcome.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OperationLookup {
    /// Referenced operation, retained even when unavailable or conflicted.
    pub operation: OpId,
    /// Recorded fact or the reason no accepted fact can be supplied.
    pub record: Lookup<HistoryEntry>,
}

/// An operation with its recorded metadata: actor/session registrations, direct parents,
/// and relationships.
/// No authorship or comprehension is inferred.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OperationMeta {
    /// Original operation, including raw actor, scope, tags, and source clock.
    pub record: HistoryEntry,
    /// All accepted registrations for its actor, ordered by operation ID. An empty
    /// list means no accepted registration is available, not an actor classification.
    pub actor_records: Vec<HistoryEntry>,
    /// All registrations for its explicit session scope or session record identity.
    /// Empty means no accepted registration; no session is inferred from turn/file scope.
    pub session_records: Vec<HistoryEntry>,
    /// Direct envelope parents in recorded order, with explicit lookup outcomes.
    pub parents: Vec<OperationLookup>,
    /// Accepted relationships incident to this operation, plus assertions carried
    /// by this record. Notes and custom relations retain their opaque meaning.
    pub relationships: Vec<RecordedRelationship>,
}

/// A bounded walk of envelope parent references only.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AncestorGraph {
    /// Requested root, included in the inspected operations.
    pub root: OpId,
    /// Inspected identities in operation-ID order, including explicit missing/conflicts.
    pub operations: Vec<OperationLookup>,
    /// Parent assertions in child-ID and recorded-parent order. Cycles are retained
    /// as recorded, and visited identities are not followed a second time.
    pub relationships: Vec<RecordedRelationship>,
    /// Discovered identities not inspected because of the operation budget, sorted
    /// by ID. A nonempty frontier means traversal is incomplete.
    pub frontier: Vec<OpId>,
}

impl ChainQueries {
    /// Gather an operation's metadata: actor/session records, direct parents, and relationships.
    ///
    /// This complete lookup can scan the chain for incoming annotations and Git
    /// links. Use paged [`Self::relationships`] for bounded retrieval. Actor/session
    /// metadata observations are all retained; no "latest" meaning is assigned.
    ///
    /// # Errors
    /// Returns index or source IO errors.
    pub fn operation_meta(&self, id: OpId) -> io::Result<Lookup<OperationMeta>> {
        self.operation(id)?.try_map(|record| {
            let actor_records = self.all_history(Some(IndexKey::Actor(record.operation.actor)))?
                .into_iter().filter(|entry| matches!(entry.operation.kind, OpKind::Actor(_))).collect();
            let session = if let OpKind::Session(session) = &record.operation.kind {
                Some(session.id)
            } else if let ScopeRef::Session(session) = record.operation.scope {
                Some(session)
            } else {
                None
            };
            let session_records = if let Some(session) = session {
                self.all_history(Some(IndexKey::Session(session)))?.into_iter()
                    .filter(|entry| matches!(&entry.operation.kind, OpKind::Session(value) if value.id == session)).collect()
            } else { Vec::new() };
            let (actor_records, session_records) = if let OpKind::Activity(activity) = &record.operation.kind {
                let author = if let Some(author) = activity.author {
                    self.all_history(Some(IndexKey::Item(author)))?.into_iter().filter(|entry| matches!(&entry.operation.kind, OpKind::Activity(record) if matches!(record.kind, editchain_core::activity::Kind::Author(_)))).collect()
                } else { Vec::new() };
                let sessions = if let Some(session) = activity.session {
                    self.all_history(Some(IndexKey::Item(session)))?.into_iter().filter(|entry| matches!(&entry.operation.kind, OpKind::Activity(record) if matches!(record.kind, editchain_core::activity::Kind::Session(_)))).collect()
                } else { Vec::new() };
                (author, sessions)
            } else { (actor_records, session_records) };
            let parents = record.operation.causal_parents().iter().map(|parent| {
                Ok(OperationLookup { operation: *parent, record: self.operation(*parent)? })
            }).collect::<io::Result<Vec<_>>>()?;
            let target = EntityRef::Operation(id);
            let relationships = self.all_history(None)?.iter().flat_map(recorded_relationships)
                .filter(|relation| relation.source == target || relation.target == target || relation.record_ref.operation == id).collect();
            Ok(OperationMeta { record, actor_records, session_records, parents, relationships })
        })
    }

    /// Follow only explicit envelope parents with a 1..=1000 operation budget.
    ///
    /// Pending identities are visited in ID order. Timestamps, annotations, Git
    /// links, equal content, and session labels never create causal ancestry.
    /// Missing/quarantined parents end their branch and remain in the result.
    ///
    /// # Errors
    /// Returns invalid budgets or index/source IO errors.
    pub fn ancestors(&self, root: OpId, limit: usize) -> io::Result<AncestorGraph> {
        let _page = super::PageRequest { after: None, limit }.validate()?;
        let mut pending = BTreeSet::from([root]);
        let mut visited = BTreeMap::new();
        while visited.len() < limit {
            let Some(id) = pending.pop_first() else {
                break;
            };
            let record = self.operation(id)?;
            if let Lookup::Found(entry) = &record {
                for parent in &entry.operation.causal_parents() {
                    if *parent != id && !visited.contains_key(parent) {
                        let _inserted = pending.insert(*parent);
                    }
                }
            }
            drop(visited.insert(
                id,
                OperationLookup {
                    operation: id,
                    record,
                },
            ));
        }
        let operations: Vec<_> = visited.into_values().collect();
        let relationships = operations
            .iter()
            .filter_map(|operation| match &operation.record {
                Lookup::Found(entry) => Some(entry),
                Lookup::Missing | Lookup::Conflicted(_) => None,
            })
            .flat_map(recorded_relationships)
            .filter(|relation| relation.kind == super::RelationshipKind::CausalParent)
            .collect();
        Ok(AncestorGraph {
            root,
            operations,
            relationships,
            frontier: pending.into_iter().collect(),
        })
    }
}
