//! Read the importer's persisted turn records, never task guesses from chat text.

use editchain_core::{
    provider::CodexDerivationContract, NoteRelationship, OpId, OpKind, Payload, ScopeRef,
};
use editchain_import::{derive_node_id, derive_turn_id};
use editchain_index::Map as HashMap;
use editchain_index::OrderedMap as BTreeMap;
use editchain_project::live::{LiveProjection, LiveRow, TaskIdentity};
use editchain_protocol::TaskStatus;
use std::collections::BTreeSet;

type Turn = (u64, u32, String, String);

fn turn(task: &TaskIdentity) -> Turn {
    (
        task.boundary.node.0,
        task.boundary.boot,
        task.thread.clone(),
        task.turn.clone(),
    )
}

#[derive(Debug, Default, serde::Serialize, serde::Deserialize)]
pub(super) struct Metadata {
    records: HashMap<String, Vec<(Turn, OpId)>>,
    versions: HashMap<Turn, BTreeMap<OpId, TaskStatus>>,
    sections: HashMap<Turn, editchain_index::OrderedSet<String>>,
    prompts: HashMap<Turn, BTreeMap<OpId, String>>,
    prompt_records: HashMap<String, (Turn, OpId)>,
}

impl Metadata {
    pub(super) fn reset_sections(&mut self) {
        self.sections.clear();
    }
    pub(super) fn title(&self, task: &TaskIdentity) -> Option<String> {
        self.prompts
            .get(&turn(task))?
            .range(task.boundary..)
            .next()
            .map(|(_, title)| title.clone())
    }

    fn prompt(&mut self, row: &LiveRow, dirty: &mut BTreeSet<String>) {
        let Some(task) = &row.task else {
            return;
        };
        let text = row.operations.iter().find_map(|op| {
            if !op.tags.matches_all(editchain_core::Tags::HUMAN) {
                return None;
            }
            let OpKind::Message(message) = &op.kind else {
                return None;
            };
            let Payload::Inline(bytes) = &message.content else {
                return None;
            };
            std::str::from_utf8(bytes).ok()
        });
        let Some(text) = text else {
            return;
        };
        let title: String = text
            .trim()
            .chars()
            .take(160)
            .map(|c| if c.is_whitespace() { ' ' } else { c })
            .collect();
        if title.is_empty() {
            return;
        }
        let turn = turn(task);
        drop(
            self.prompts
                .entry(turn.clone())
                .or_default()
                .insert(row.incarnation, title),
        );
        drop(
            self.prompt_records
                .insert(row.key.clone(), (turn.clone(), row.incarnation)),
        );
        dirty.extend(self.sections.get(&turn).into_iter().flatten().cloned());
    }

    pub(super) fn status(&self, task: &TaskIdentity) -> TaskStatus {
        self.versions
            .get(&turn(task))
            .and_then(BTreeMap::last_key_value)
            .filter(|(source, _)| source.seq > task.boundary.seq)
            .map_or(TaskStatus::Unknown, |(_, status)| *status)
    }

    pub(super) fn section(&mut self, task: &TaskIdentity, key: &str, added: bool) {
        let sections = self.sections.entry(turn(task)).or_default();
        if added {
            let _: bool = sections.insert(key.into());
        } else {
            let _: bool = sections.remove(key);
        }
    }

    pub(super) fn remove(&mut self, key: &str, dirty: &mut BTreeSet<String>) {
        if let Some((turn, source)) = self.prompt_records.remove(key) {
            if let Some(prompts) = self.prompts.get_mut(&turn) {
                drop(prompts.remove(&source));
            }
            dirty.extend(self.sections.get(&turn).into_iter().flatten().cloned());
        }
        for (turn, source) in self.records.remove(key).into_iter().flatten() {
            if let Some(versions) = self.versions.get_mut(&turn) {
                let _: Option<TaskStatus> = versions.remove(&source);
            }
            dirty.extend(self.sections.get(&turn).into_iter().flatten().cloned());
        }
    }

    pub(super) fn observe(
        &mut self,
        row: &LiveRow,
        projection: &LiveProjection,
        dirty: &mut BTreeSet<String>,
    ) {
        self.remove(&row.key, dirty);
        self.prompt(row, dirty);
        for (turn_id, status) in observations(row, projection) {
            let Some(proof) = projection.codex_derivation(row.anchor) else {
                continue;
            };
            let turn = (
                row.anchor.node.0,
                row.anchor.boot,
                proof.thread.0.clone(),
                turn_id,
            );
            let versions = self.versions.entry(turn.clone()).or_default();
            let previous = versions.last_key_value().map(|(_, status)| *status);
            let _: Option<TaskStatus> = versions.insert(row.anchor, status);
            if previous != versions.last_key_value().map(|(_, status)| *status) {
                dirty.extend(self.sections.get(&turn).into_iter().flatten().cloned());
            }
            self.records
                .entry(row.key.clone())
                .or_default()
                .push((turn, row.anchor));
        }
    }
}

/// The old importer already persists native status in a deterministic turn-note
/// slot. Verify that exact slot and scope before decoding its summary grammar.
/// This supports existing chains without rewriting any immutable operation.
pub(super) fn observations(
    row: &LiveRow,
    projection: &LiveProjection,
) -> Vec<(String, TaskStatus)> {
    let Some(proof) = projection.codex_derivation(row.anchor) else {
        return Vec::new();
    };
    let contract = match proof.contract {
        CodexDerivationContract::OccurrencesV1 => "codex-occurrences-v1",
        CodexDerivationContract::OccurrencesV2 => "codex-occurrences-v2",
    };
    row.operations
        .iter()
        .filter_map(|op| {
            let OpKind::Note(note) = &op.kind else {
                return None;
            };
            if note.relationship != NoteRelationship::Explains
                || !note.target_ids.is_empty()
                || !proof.outputs.contains(&op.id)
            {
                return None;
            }
            let Payload::Inline(bytes) = &note.content else {
                return None;
            };
            let content = std::str::from_utf8(bytes).ok()?;
            let (turn, rest) = content.split_once(": ")?;
            if op.scope != ScopeRef::Turn(derive_turn_id(&format!("{}:{turn}", proof.thread.0))) {
                return None;
            }
            let exact_slot = (0..proof.outputs.len()).any(|index| {
                let slot = serde_json::json!([contract, row.anchor.node, {"Turn": [turn, index]}]);
                op.id.node == derive_node_id(&slot.to_string())
                    && Some(op.id.seq) == row.anchor.seq.checked_add(1)
                    && op.id.boot == row.anchor.boot
            });
            if !exact_slot {
                return None;
            }
            let status = match rest.split_whitespace().next()? {
                "inProgress" => TaskStatus::InProgress,
                "completed" => TaskStatus::Completed,
                "failed" => TaskStatus::Failed,
                "interrupted" => TaskStatus::Interrupted,
                _ => TaskStatus::Unknown,
            };
            Some((turn.into(), status))
        })
        .collect()
}
