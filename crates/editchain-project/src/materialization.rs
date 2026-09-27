//! Present shared import reconciliation as row visibility and continuity.

use std::collections::{HashMap, HashSet};

use editchain_core::provider::ProviderFact;
use editchain_core::{Op, OpId, OpKind};
use editchain_engine::imports::ImportState;

use crate::CodexLogicalItem;

mod message_echoes;
pub(super) use message_echoes::source_messages;

type SourceKey = (u64, u32);

#[derive(Debug, Clone, Default)]
pub(super) struct Materialization {
    pub(super) hidden: HashSet<OpId>,
    pub(super) representatives: HashMap<OpId, OpId>,
    pub(super) output_order: HashMap<OpId, usize>,
    pub(super) items: Vec<CodexLogicalItem>,
    pub(super) continuity_keys: HashMap<OpId, String>,
    pub(super) message_echoes: HashMap<OpId, OpId>,
}

impl Materialization {
    pub(super) fn from_ops(
        ops: &[Op],
        messages: &HashMap<OpId, Op>,
        incomplete: &HashSet<OpId>,
    ) -> Self {
        let state = ImportState::from_partial_ops(ops, incomplete);
        let by_id: HashMap<OpId, &Op> = ops.iter().map(|op| (op.id, op)).collect();
        let blocked = state
            .incomplete_sources
            .iter()
            .map(|source| source_key(*source))
            .collect();
        let covered: HashSet<OpId> = state.derivations.iter().map(|entry| entry.source).collect();
        let mut result = Self::default();
        let mut echoes = message_echoes::MessageEchoes::default();
        for entry in state.derivations {
            for output in entry.outputs {
                let _hidden = result.hidden.insert(output);
                let _previous = result.representatives.insert(output, entry.source);
            }
            let outputs = match &entry.selected {
                Some(ProviderFact::CodexDerivation(meta)) => {
                    echoes.observe(entry.source, meta, messages, &by_id);
                    meta.outputs.as_slice()
                }
                Some(ProviderFact::ClaudeDerivation(meta)) => meta.outputs.as_slice(),
                Some(ProviderFact::CodexSource(_) | ProviderFact::CodexLifecycle(_)) | None => &[],
            };
            for (index, output) in outputs.iter().enumerate() {
                let _hidden = result.hidden.remove(output);
                let _previous = result.output_order.insert(*output, index);
            }
        }
        result.message_echoes = echoes.finish(&blocked);
        // A covered record's legacy numeric lanes remain stored and
        // addressable, but their cursor-dependent fold no longer supplies
        // display content. Incomplete replacements do not revive stale data.
        for op in ops {
            if op.id.seq.trailing_zeros() >= 16 {
                continue;
            }
            let raw = OpId {
                seq: op.id.seq & !0xffff,
                ..op.id
            };
            if covered.contains(&raw) {
                let _: bool = result.hidden.insert(op.id);
                let _: Option<OpId> = result.representatives.insert(op.id, raw);
            }
        }
        result.items = state.codex_items;
        for copy in state.copies {
            let _hidden = result.hidden.insert(copy.operation);
            let _previous = result
                .representatives
                .insert(copy.operation, copy.representative);
        }
        for item in &result.items {
            for (index, output) in item.outputs.iter().enumerate() {
                // File order can change when a patch adds another path. A
                // path's identity must not follow its old array position.
                let slot = match by_id.get(output).map(|op| &op.kind) {
                    Some(OpKind::File(file)) => format!("file:{}", file.path.0),
                    _ => format!("output:{index}"),
                };
                let key = format!(
                    "codex:{}:{}:{}:{}:{}:{slot}",
                    item.incarnation,
                    item.turn.len(),
                    item.turn,
                    item.item.len(),
                    item.item
                );
                drop(result.continuity_keys.insert(*output, key));
            }
        }
        result
    }
}

fn source_key(source: OpId) -> SourceKey {
    (source.node.0, source.boot)
}
