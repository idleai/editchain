//! Immutable normalized revisions at the physical occurrence that witnessed them.

use std::collections::BTreeMap;

use editchain_core::provider::{
    CodexDerivationContract, CodexDerivationEvidence, CodexLogicalChange, CodexThreadId,
    ProviderEvidence, ProviderEvidenceSchema, ProviderFact,
};
use editchain_core::{Clock, Op, OpId, OpKind, ParentSet};
use serde::Serialize;

use super::evidence::evidence_note;
use super::normalize::{
    normalized_ops_for_compaction, normalized_ops_for_inter_agent, normalized_ops_for_occurrence,
    normalized_ops_for_turn, parse_raw_line_meta, raw_clock, NormalizeContext,
};
use super::projection::{CompactedLine, FinalItem, InterAgentLine, Projection, TurnMeta};
use crate::ids::{derive_node_id, SourcePosition, SourceStream};
use crate::sink::{emit_op, EmissionKind, OpSink};
use crate::source_read::SourceReadPlan;
use crate::ImportError;

pub(super) const CONTRACT: &str = "codex-occurrences-v2";

#[derive(Debug, Default)]
struct RecordProjection<'a> {
    items: Vec<&'a FinalItem>,
    turns: Vec<(&'a TurnMeta, usize)>,
    removed: Vec<&'a str>,
    inter_agent: Option<&'a InterAgentLine>,
    compacted: Option<&'a CompactedLine>,
}

impl<'a> RecordProjection<'a> {
    fn index(projection: &'a Projection) -> BTreeMap<u64, Self> {
        let mut records: BTreeMap<u64, Self> = BTreeMap::new();
        for item in &projection.item_occurrences {
            records.entry(item.last_seen).or_default().items.push(item);
        }
        for (ordinal, turn, count) in &projection.turn_occurrences {
            records
                .entry(*ordinal)
                .or_default()
                .turns
                .push((turn, *count));
        }
        for (ordinal, turn) in &projection.removed_turns {
            records.entry(*ordinal).or_default().removed.push(turn);
        }
        for line in &projection.inter_agent_lines {
            records.entry(line.source_ordinal).or_default().inter_agent = Some(line);
        }
        for line in &projection.compacted_lines {
            records.entry(line.source_ordinal).or_default().compacted = Some(line);
        }
        records
    }
}

#[derive(Debug, Serialize)]
enum Slot<'a> {
    Item(&'a str, &'a str, usize),
    Turn(&'a str, usize),
    Removed(&'a str, usize),
    InterAgent,
    Compacted,
}

#[derive(Debug, Default)]
struct RecordOutput {
    ops: Vec<Op>,
    changes: Vec<CodexLogicalChange>,
}

pub(super) fn emit_occurrences(
    projection: &Projection,
    plan: &SourceReadPlan,
    context: &mut NormalizeContext<'_>,
    sink: &mut dyn OpSink,
    replay: bool,
) -> Result<crate::model::ImportReport, ImportError> {
    let historical = (replay && plan.start_seq() > 0)
        .then(|| plan.all_lines())
        .transpose()?;
    let lines = historical.as_deref().unwrap_or_else(|| plan.lines());
    let start = if replay { 0 } else { plan.start_seq() };
    let mut records = RecordProjection::index(projection);
    let mut report = crate::model::ImportReport::default();
    let requested_thinking = context.include_thinking;
    let retained_thinking = plan
        .checkpoint()
        .materialization
        .as_ref()
        .filter(|checkpoint| checkpoint.includes_thinking)
        .map_or(0, |checkpoint| checkpoint.through);
    for (index, line) in lines.iter().enumerate() {
        plan.check_cancellation()?;
        let ordinal = start
            .checked_add(u64::try_from(index).map_err(std::io::Error::other)?)
            .and_then(|value| value.checked_add(1))
            .ok_or_else(|| ImportError::CursorStore("derivation ordinal exhausted".into()))?;
        let source = context
            .stream
            .op_from_position(SourcePosition::raw(ordinal))?;
        let clock = raw_clock(parse_raw_line_meta(&line.data).timestamp.as_deref()).0;
        let record = records.remove(&ordinal).unwrap_or_default();
        context.include_thinking = requested_thinking || ordinal <= retained_thinking;
        let output = materialize_record(&record, ordinal, clock, context, plan)?;
        let proof = evidence_note(
            context.thread,
            &ProviderEvidence {
                schema: ProviderEvidenceSchema::V1,
                source,
                raw_hash: line.hash,
                fact: ProviderFact::CodexDerivation(CodexDerivationEvidence {
                    thread: CodexThreadId(context.thread.to_owned()),
                    contract: CodexDerivationContract::OccurrencesV2,
                    includes_thinking: context.include_thinking,
                    outputs: output.ops.iter().map(|op| op.id).collect(),
                    changes: output.changes,
                }),
            },
        )?;
        for op in &output.ops {
            plan.check_cancellation()?;
            emit_op(op, sink, &mut report, EmissionKind::Derived)?;
        }
        emit_op(&proof, sink, &mut report, EmissionKind::Derived)?;
    }
    context.include_thinking = requested_thinking;
    Ok(report)
}

fn materialize_record(
    record: &RecordProjection<'_>,
    ordinal: u64,
    clock: Clock,
    context: &mut NormalizeContext<'_>,
    plan: &SourceReadPlan,
) -> Result<RecordOutput, ImportError> {
    let mut output = RecordOutput::default();
    for (index, turn) in record.removed.iter().enumerate() {
        plan.check_cancellation()?;
        output.changes.push(CodexLogicalChange::RemoveTurn {
            turn: (*turn).to_owned(),
        });
        let removed = TurnMeta {
            turn_id: (*turn).to_owned(),
            status: Some("removed".into()),
            error_message: None,
            started_at: None,
            completed_at: None,
            duration_ms: None,
        };
        context.lanes.clear();
        let ops = normalized_ops_for_turn(&removed, ordinal, 0, clock, context)?;
        output
            .ops
            .extend(remap(ops, Slot::Removed(turn, index), context.stream)?);
    }
    for (index, item) in record.items.iter().enumerate() {
        plan.check_cancellation()?;
        context.lanes.clear();
        let ops = normalized_ops_for_occurrence(item, clock, context)?;
        let ops = remap(
            ops,
            Slot::Item(&item.turn_id, &item.item_id, index),
            context.stream,
        )?;
        output.changes.push(CodexLogicalChange::Upsert {
            turn: item.turn_id.clone(),
            item: item.item_id.clone(),
            incarnation: context
                .stream
                .op_from_position(SourcePosition::raw(item.first_seen))?,
            outputs: ops.iter().map(|op| op.id).collect(),
        });
        output.ops.extend(ops);
    }
    for (index, (turn, count)) in record.turns.iter().enumerate() {
        plan.check_cancellation()?;
        context.lanes.clear();
        let ops = normalized_ops_for_turn(turn, ordinal, *count, clock, context)?;
        output.ops.extend(remap(
            ops,
            Slot::Turn(&turn.turn_id, index),
            context.stream,
        )?);
    }
    emit_line_content(record, clock, context, &mut output.ops)?;
    Ok(output)
}

fn emit_line_content(
    record: &RecordProjection<'_>,
    clock: Clock,
    context: &mut NormalizeContext<'_>,
    output: &mut Vec<Op>,
) -> Result<(), ImportError> {
    if let Some(line) = record.inter_agent {
        context.lanes.clear();
        let ops = normalized_ops_for_inter_agent(line, clock, context)?;
        output.extend(remap(ops, Slot::InterAgent, context.stream)?);
    }
    if let Some(line) = record.compacted {
        context.lanes.clear();
        let ops = normalized_ops_for_compaction(line, clock, context)?;
        output.extend(remap(ops, Slot::Compacted, context.stream)?);
    }
    Ok(())
}

fn remap(mut ops: Vec<Op>, slot: Slot<'_>, stream: &SourceStream) -> Result<Vec<Op>, ImportError> {
    // The namespace is independent of cursor boundaries and sibling output
    // counts. Adding requested reasoning cannot shift another item's IDs.
    let node = derive_node_id(&serde_json::to_string(&(CONTRACT, stream.node, slot))?);
    let ids: BTreeMap<OpId, OpId> = ops
        .iter()
        .map(|op| (op.id, OpId { node, ..op.id }))
        .collect();
    let mapped = |id: OpId| ids.get(&id).copied().unwrap_or(id);
    for op in &mut ops {
        op.id = mapped(op.id);
        op.parents = match op.parents {
            ParentSet::None => ParentSet::None,
            ParentSet::One(parent) => ParentSet::One(mapped(parent)),
            ParentSet::Two(left, right) => ParentSet::Two(mapped(left), mapped(right)),
        };
        if let OpKind::Note(note) = &mut op.kind {
            for target in &mut note.target_ids {
                *target = mapped(*target);
            }
        }
    }
    Ok(ops)
}
