//! Fold unchanged user-message revisions by admitted logical identity and full payload.

use std::collections::{HashMap, HashSet};

use editchain_core::provider::{CodexDerivationEvidence, CodexLogicalChange};
use editchain_core::{Op, OpId, OpKind, SourceId, Tags};

use super::{source_key, SourceKey};

pub(crate) fn source_messages(ops: &[Op], incomplete: &HashSet<OpId>) -> HashMap<OpId, Op> {
    ops.iter()
        .filter(|op| {
            op.tags.matches_all(Tags::HUMAN)
                && matches!(op.kind, OpKind::Message(_))
                && !incomplete.contains(&op.id)
        })
        .map(|op| (op.id, op.clone()))
        .collect()
}

#[derive(Debug, PartialEq, Eq, Hash)]
struct ItemKey {
    source: SourceKey,
    thread: String,
    turn: String,
    item: String,
    incarnation: OpId,
}

#[derive(Default)]
pub(super) struct MessageEchoes {
    last: HashMap<ItemKey, (SourceId, OpId)>,
    echoes: HashMap<SourceId, SourceId>,
}

impl MessageEchoes {
    pub(super) fn observe(
        &mut self,
        source: OpId,
        meta: &CodexDerivationEvidence,
        messages: &HashMap<OpId, Op>,
        ops: &HashMap<OpId, &Op>,
    ) {
        let Some(source) = ops.get(&source).and_then(|op| op.source) else {
            return;
        };
        if meta.changes.is_empty() {
            return;
        }
        let [CodexLogicalChange::Upsert {
            turn,
            item,
            incarnation,
            outputs,
        }] = meta.changes.as_slice()
        else {
            self.last.retain(|key, _| key.source != source_key(source));
            return;
        };
        let key = ItemKey {
            source: source_key(source),
            thread: meta.thread.0.clone(),
            turn: turn.clone(),
            item: item.clone(),
            incarnation: incarnation.id(),
        };
        let [output] = outputs.as_slice() else {
            let _: Option<(SourceId, OpId)> = self.last.remove(&key);
            return;
        };
        let Some(message) = messages.get(&output.id()) else {
            let _: Option<(SourceId, OpId)> = self.last.remove(&key);
            return;
        };
        // Never hide a source occurrence carrying another item or non-metadata
        // output. The complete message payload, not its preview, proves equality.
        if !meta.outputs.iter().all(|id| {
            id == output
                || ops
                    .get(&id.id())
                    .is_some_and(|op| op.tags.matches_all(Tags::META))
        }) {
            let _: Option<(SourceId, OpId)> = self.last.remove(&key);
            return;
        }
        if let Some((previous_source, previous_output)) = self.last.get(&key) {
            if messages.get(previous_output).is_some_and(|previous| {
                previous.kind == message.kind
                    && previous.actor == message.actor
                    && previous.scope == message.scope
                    && previous.tags == message.tags
            }) {
                let _: Option<SourceId> = self.echoes.insert(source, *previous_source);
                return;
            }
        }
        let _: Option<(SourceId, OpId)> = self.last.insert(key, (source, output.id()));
    }

    pub(super) fn finish(self, blocked: &HashSet<SourceKey>) -> HashMap<OpId, OpId> {
        self.echoes
            .into_iter()
            .filter(|(source, _)| !blocked.contains(&source_key(*source)))
            .map(|(source, target)| (source.id(), target.id()))
            .collect()
    }
}
