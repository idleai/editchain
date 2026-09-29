//! Convert captured records into the ten schema-three operation types.
//!
//! The observation pass reads small identity metadata. Conversion preserves
//! source bytes once, embeds direct references, and assigns new physical IDs.

mod calls;
mod human;
mod migration;
mod source;
pub use migration::{migrate, uses_migration_ids};

use editchain_core::activity::{upgrade_id, ItemId, Kind, Operation, OriginalRef};
use editchain_core::provider::{CodexLogicalChange, ProviderEvidence, ProviderFact};
use editchain_core::{NoteRelationship, Op, OpId, OpKind, Payload, ScopeRef};
use std::collections::{BTreeMap, BTreeSet};

use crate::{BlobSink, ImportError};
use source::{Derived, Source};

/// Version of the deterministic converter, separate from EC03 framing.
pub const CONTRACT: &str = "activity-schema3-v2";

/// Small metadata gathered before a bounded conversion pass.
#[derive(Debug, Default)]
pub struct Converter {
    migration: bool,
    sources: BTreeMap<OpId, Source>,
    derived: BTreeMap<OpId, Derived>,
    folded: BTreeMap<OpId, Vec<OpId>>,
    removed: BTreeSet<OpId>,
    redirects: BTreeMap<OpId, OpId>,
    sessions: BTreeMap<u64, ItemId>,
    retained: BTreeMap<OpId, OpId>,
    blocked: BTreeSet<OpId>,
    calls: calls::Calls,
    files: BTreeMap<OpId, (editchain_core::PathId, Option<OpId>)>,
    paths: BTreeMap<OpId, Payload>,
    path_notes: Vec<(OpId, OpId, Option<OpId>, Payload)>,
}

impl Converter {
    /// Conversion of a frozen old chain, with aliases for folded metadata.
    /// Its ID namespace is separate from new capture, so merging both never
    /// gives changed bytes the same immutable event ID.
    #[must_use]
    pub fn for_migration() -> Self {
        Self {
            migration: true,
            ..Self::default()
        }
    }

    fn protect_conflicts(&mut self, conflicts: BTreeSet<OpId>) {
        for id in &conflicts {
            let _old = self.retained.insert(upgrade_id(*id), *id);
            let _removed = self.removed.remove(id);
            let _redirect = self.redirects.remove(&upgrade_id(*id));
            if let Some(folded) = self.folded.remove(id) {
                for note in folded {
                    let _removed = self.removed.remove(&note);
                    let _redirect = self.redirects.remove(&upgrade_id(note));
                }
            }
        }
        self.blocked = conflicts;
    }
    /// Observe identity metadata before converting any records in the batch.
    /// Blob payloads can be supplied by callers; `None` preserves an opaque source.
    /// # Errors
    /// Reports malformed recognized conversion metadata.
    pub fn observe(&mut self, op: &Op, resolved: Option<&[u8]>) -> Result<(), ImportError> {
        self.calls.observe(op);
        match &op.kind {
            OpKind::Import(raw) => {
                let bytes = match &raw.raw_ref {
                    Payload::Inline(bytes) => Some(bytes.as_slice()),
                    Payload::Empty => Some([].as_slice()),
                    Payload::Blob(_) => resolved,
                };
                let info = Source::read(op, bytes);
                if let (ScopeRef::Session(alias), Some(session)) = (op.scope, info.session) {
                    let _old = self.sessions.insert(alias.0, session);
                }
                let _old = self.sources.insert(op.id, info);
            }
            OpKind::Note(note) => {
                let Payload::Inline(bytes) = &note.content else {
                    return Ok(());
                };
                if note.relationship == NoteRelationship::Explains
                    && note.target_ids.len() == 1
                    && op.tags.matches_all(editchain_core::Tags::IMPORT)
                {
                    if let Some(target) = note.target_ids.first() {
                        self.path_notes.push((
                            op.id,
                            *target,
                            op.parents.iter().next().copied(),
                            note.content.clone(),
                        ));
                    }
                }
                if note.relationship == NoteRelationship::ProviderEvidence {
                    if let Ok(contract) = serde_json::from_slice::<ProviderEvidence>(bytes) {
                        self.contract(op.id, contract);
                    }
                } else if matches!(
                    note.relationship,
                    NoteRelationship::OccurrenceOf
                        | NoteRelationship::Contains
                        | NoteRelationship::ToolResultOf
                ) {
                    if let Some(source) = op.parents.iter().next() {
                        self.fold(op.id, *source);
                    }
                }
            }
            OpKind::File(file) => {
                let _old = self
                    .files
                    .insert(op.id, (file.path, op.parents.iter().next().copied()));
            }
            OpKind::ChainStart(_) => {
                let _old = self.retained.insert(upgrade_id(op.id), op.id);
            }
            OpKind::Actor(_)
            | OpKind::Session(_)
            | OpKind::Message(_)
            | OpKind::Tool(_)
            | OpKind::Command(_)
            | OpKind::Reflection(_)
            | OpKind::Error(_)
            | OpKind::GitCommit(_)
            | OpKind::GitLink(_)
            | OpKind::Unknown(_)
            | OpKind::Activity(_) => {}
        }
        Ok(())
    }

    /// Finish cross-record bookkeeping after observing the complete batch.
    pub fn finish_observations(&mut self) {
        for (note, target, parent, content) in std::mem::take(&mut self.path_notes) {
            if let (Some((path, original)), Payload::Inline(bytes)) =
                (self.files.get(&target), &content)
            {
                if parent == *original
                    && parent.is_some()
                    && std::str::from_utf8(bytes)
                        .ok()
                        .is_some_and(|value| crate::derive_path_id(value) == *path)
                {
                    let _old = self.paths.insert(target, content);
                    self.fold(note, target);
                }
            }
        }
        for (folded, target) in self.calls.folded() {
            self.fold(folded, target);
        }
    }

    fn fold(&mut self, old: OpId, target: OpId) {
        let _inserted = self.removed.insert(old);
        self.folded.entry(target).or_default().push(old);
        let _previous = self.redirects.insert(upgrade_id(old), upgrade_id(target));
    }

    fn contract(&mut self, id: OpId, contract: ProviderEvidence) {
        let source = contract.source.id();
        match contract.fact {
            ProviderFact::CodexDerivation(meta) => {
                let session = source::legacy_session(&meta.thread.0);
                let info = self.sources.entry(source).or_default();
                info.session = Some(session);
                for output in meta.outputs {
                    let _old = self.derived.insert(
                        output.id(),
                        Derived {
                            original: source,
                            session: Some(session),
                            turn: None,
                            item: None,
                            incarnation: None,
                        },
                    );
                }
                for change in meta.changes {
                    if let CodexLogicalChange::Upsert {
                        turn,
                        item,
                        incarnation,
                        outputs,
                    } = change
                    {
                        let turn_id = source::scoped("codex.turn", session, &turn);
                        let item_id = source::scoped("codex.item", turn_id, &item);
                        for output in outputs {
                            let _old = self.derived.insert(
                                output.id(),
                                Derived {
                                    original: source,
                                    session: Some(session),
                                    turn: Some(turn_id),
                                    item: Some(item_id),
                                    incarnation: Some(ItemId::derive(
                                        "codex.attempt",
                                        incarnation.id().as_bytes(),
                                    )),
                                },
                            );
                        }
                    }
                }
                self.fold(id, source);
            }
            ProviderFact::ClaudeDerivation(meta) => {
                for output in meta.outputs {
                    let _old = self.derived.insert(
                        output.id(),
                        Derived {
                            original: source,
                            session: None,
                            turn: None,
                            item: None,
                            incarnation: None,
                        },
                    );
                }
                self.fold(id, source);
            }
            ProviderFact::CodexSource(meta) => {
                let session = source::legacy_session(&meta.thread.0);
                self.sources.entry(meta.first.id()).or_default().session = Some(session);
                self.fold(id, source);
            }
            ProviderFact::CodexLifecycle(_) => {
                // These older contracts contain standalone observations. Keep
                // them as explicit links until their converter can retain every field.
            }
        }
    }

    /// Produce schema-three records after the observation pass.
    /// # Errors
    /// Returns validation, payload storage, or encoding errors.
    pub fn convert(&self, op: &Op, blobs: &mut dyn BlobSink) -> Result<Vec<Op>, ImportError> {
        if matches!(op.kind, OpKind::Activity(_)) || self.blocked.contains(&op.id) {
            return Ok(vec![op.clone()]);
        }
        if self.removed.contains(&op.id) {
            return Ok(Vec::new());
        }
        let Some(mut record) = Operation::upgrade(op) else {
            return Ok(vec![op.clone()]);
        };
        if let Some(mapping) = &mut record.legacy {
            mapping.folded = if self.migration {
                self.folded.get(&op.id).cloned().unwrap_or_default()
            } else {
                Vec::new()
            };
            mapping.folded.sort_unstable();
            mapping.folded.dedup();
        }
        if let ScopeRef::Session(alias) = op.scope {
            record.session = self.sessions.get(&alias.0).copied().or(record.session);
        }
        if let Some(info) = self.sources.get(&op.id) {
            info.apply_original(&mut record);
        }
        let original = self
            .derived
            .get(&op.id)
            .map(|derived| derived.original)
            .or_else(|| {
                op.parents
                    .iter()
                    .find(|parent| self.sources.contains_key(parent))
                    .copied()
            });
        if !matches!(record.kind, Kind::Original(_)) {
            if let Some(original) = original {
                record.original = Some(OriginalRef {
                    operation: upgrade_id(original),
                    converter: CONTRACT.into(),
                });
                if let Some(info) = self.sources.get(&original) {
                    info.apply_output(&mut record, op);
                }
            }
        }
        if let Some(derived) = self.derived.get(&op.id) {
            derived.apply(&mut record);
        }
        source::finish(
            op,
            &mut record,
            original.and_then(|id| self.sources.get(&id)),
        );
        source::parent_link(op, &mut record);
        if let Kind::Tool(tool) = &mut record.kind {
            tool.attempt = self
                .derived
                .get(&op.id)
                .and_then(|derived| derived.incarnation)
                .unwrap_or(record.item);
        }
        self.calls.apply(op.id, &mut record);
        if let Kind::File(file) = &mut record.kind {
            if let Some(path) = self.paths.get(&op.id) {
                file.name.clone_from(path);
            }
        }
        record.map_operation_ids(|id| self.output_id(id));
        let mut output = Vec::new();
        if let Some(info) = self.sources.get(&op.id) {
            if let Some(session) = info.session_record(&record) {
                output.push(
                    session
                        .into_op()
                        .map_err(|error| ImportError::OpSink(error.to_string()))?,
                );
            }
        }
        if let OpKind::Import(raw) = &op.kind {
            let stored = if let Payload::Blob(reference) = &raw.raw_ref {
                blobs.read_blob(reference)?
            } else {
                None
            };
            let bytes = if let Payload::Inline(bytes) = &raw.raw_ref {
                Some(bytes.as_slice())
            } else {
                stored.as_deref()
            };
            if let Some(bytes) = bytes {
                output.extend(human::activities(&record, bytes, blobs)?);
            }
        }
        output.insert(
            0,
            record
                .into_op()
                .map_err(|error| ImportError::OpSink(error.to_string()))?,
        );
        Ok(output)
    }

    fn output_id(&self, id: OpId) -> OpId {
        let target = self.redirects.get(&id).copied().unwrap_or(id);
        self.retained.get(&target).copied().unwrap_or_else(|| {
            if self.migration {
                migration_id(target)
            } else {
                target
            }
        })
    }
}

fn migration_id(id: OpId) -> OpId {
    OpId::from_bytes(blake3::derive_key(
        "editchain.schema3-migration.v2",
        id.as_bytes(),
    ))
}

/// Convert a bounded capture using one observation pass and one output pass.
/// # Errors
/// Returns record validation or blob-storage errors without advancing cursors.
pub fn convert(operations: &[Op], blobs: &mut dyn BlobSink) -> Result<Vec<Op>, ImportError> {
    let mut converter = Converter::default();
    for op in operations {
        let stored = resolve_original(op, blobs)?;
        converter.observe(op, stored.as_deref())?;
    }
    converter.finish_observations();
    let mut output = Vec::new();
    for op in operations {
        output.extend(converter.convert(op, blobs)?);
    }
    Ok(output)
}

/// Resolve source bytes needed by conversion; absent bytes stay absent.
/// # Errors
/// Returns blob-store errors.
pub fn resolve_original(op: &Op, blobs: &dyn BlobSink) -> Result<Option<Vec<u8>>, ImportError> {
    if let OpKind::Import(raw) = &op.kind {
        if let Payload::Blob(reference) = &raw.raw_ref {
            return blobs.read_blob(reference);
        }
    }
    Ok(None)
}
