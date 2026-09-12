//! Retained logical identities. Only changed turns/items enter each output batch.

use super::{
    parse_record, FinalItem, ParsedRecord, Projection, ProjectionError, ProjectionRecord,
    SessionMeta,
};
use serde_json::Value;
use std::collections::HashMap;

/// Stateful fold of the exporter's ordered occurrence deltas.
#[derive(Debug, Default)]
pub struct LiveProjection {
    items: HashMap<String, HashMap<String, FinalItem>>,
    owning_thread: Option<String>,
    session_meta: Option<SessionMeta>,
    session_meta_source_ordinal: Option<u64>,
}

impl LiveProjection {
    /// Validate an entire reply before mutating the retained fold. The caller
    /// supplies the exact nonblank physical ordinals from its captured batch.
    ///
    /// # Errors
    /// Returns a protocol error for missing, extra or invalid records.
    pub fn apply(
        &mut self,
        records: &[Value],
        ordinals: &[u64],
        through: u64,
    ) -> Result<Projection, ProjectionError> {
        if records.len() != ordinals.len() {
            return Err(ProjectionError::Protocol(
                "live record count mismatch".into(),
            ));
        }
        let parsed = records
            .iter()
            .zip(ordinals)
            .map(|(value, ordinal)| {
                let bytes = serde_json::to_vec(value)
                    .map_err(|error| ProjectionError::Protocol(error.to_string()))?;
                match parse_record(&bytes, *ordinal, through).map_err(ProjectionError::Protocol)? {
                    ParsedRecord::Line(record) if record.source_ordinal == *ordinal => Ok(*record),
                    ParsedRecord::Line(_) | ParsedRecord::Final(_) => Err(
                        ProjectionError::Protocol("live source ordinal mismatch".into()),
                    ),
                }
            })
            .collect::<Result<Vec<_>, _>>()?;
        let mut output = Projection::default();
        for record in parsed {
            self.reduce(record, &mut output);
        }
        output.owning_thread.clone_from(&self.owning_thread);
        output.session_meta.clone_from(&self.session_meta);
        output.session_meta_source_ordinal = self.session_meta_source_ordinal;
        Ok(output)
    }

    fn reduce(&mut self, record: ProjectionRecord, output: &mut Projection) {
        output.line_ordinals.push(record.source_ordinal);
        output.malformed = output.malformed.saturating_add(record.malformed_items);
        if record.decode_error {
            output.malformed = output.malformed.saturating_add(1);
            return;
        }
        if self.owning_thread.is_none() {
            self.owning_thread = record.thread_id;
        }
        if self.session_meta.is_none() {
            self.session_meta_source_ordinal =
                record.session_meta.as_ref().map(|_| record.source_ordinal);
            self.session_meta = record.session_meta;
        }
        for turn in record.removed_turn_ids {
            drop(self.items.remove(&turn));
            output.removed_turns.push((record.source_ordinal, turn));
        }
        for change in record.changed_items {
            let turn = self.items.entry(change.turn_id.clone()).or_default();
            let first_seen = turn
                .get(&change.item_id)
                .map_or(record.source_ordinal, |item| item.first_seen);
            let item = FinalItem {
                item_id: change.item_id,
                turn_id: change.turn_id,
                first_seen,
                last_seen: record.source_ordinal,
                kind: change.kind,
                actor: change.actor,
                payload: change.payload,
            };
            drop(turn.insert(item.item_id.clone(), item.clone()));
            output.item_occurrences.push(item);
        }
        for turn in record.changed_turns {
            let count = self.items.get(&turn.turn_id).map_or(0, HashMap::len);
            output
                .turn_occurrences
                .push((record.source_ordinal, turn, count));
        }
        output.inter_agent_lines.extend(record.inter_agent);
        output.compacted_lines.extend(record.compacted);
    }
}
