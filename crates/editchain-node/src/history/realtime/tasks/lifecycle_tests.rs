use super::*;
use editchain_core::provider::{
    CodexDerivationContract, CodexDerivationEvidence, CodexThreadId, ProviderEvidence,
    ProviderEvidenceSchema, ProviderFact,
};
use editchain_core::{
    ActorId, Clock, ImportOp, NodeId, NoteOp, NoteRelationship, Op, OpId, OpKind, ParentSet,
    Payload, ScopeRef, Tags,
};
use editchain_protocol::TaskStatus;

fn record(
    ordinal: u64,
    contract: CodexDerivationContract,
    status: &str,
    authentic_slot: bool,
) -> Vec<Op> {
    let source = OpId::new(NodeId(1), 0, ordinal << 16);
    let raw = Op {
        id: source,
        parents: ParentSet::None,
        actor: ActorId(1),
        clock: Clock::UnixMs(ordinal),
        scope: ScopeRef::None,
        tags: Tags::IMPORT,
        kind: OpKind::Import(ImportOp {
            raw_ref: Payload::Empty,
            raw_hash: Some([1; 32]),
        }),
    };
    let contract_name = match contract {
        CodexDerivationContract::OccurrencesV1 => "codex-occurrences-v1",
        CodexDerivationContract::OccurrencesV2 => "codex-occurrences-v2",
    };
    let slot = serde_json::json!([contract_name, source.node, {"Turn": ["turn", 0]}]);
    let namespace = if authentic_slot {
        editchain_import::derive_node_id(&slot.to_string())
    } else {
        NodeId(3)
    };
    let note = Op {
        id: OpId::new(namespace, 0, source.seq.saturating_add(1)),
        parents: ParentSet::One(source),
        scope: ScopeRef::Turn(editchain_import::derive_turn_id("thread:turn")),
        tags: Tags::NOTE | Tags::IMPORT,
        kind: OpKind::Note(NoteOp {
            target_ids: Vec::new(),
            relationship: NoteRelationship::Explains,
            content: Payload::Inline(format!("turn: {status} (7 items)").into_bytes()),
        }),
        ..raw.clone()
    };
    let evidence = ProviderEvidence {
        schema: ProviderEvidenceSchema::V1,
        source,
        raw_hash: [1; 32],
        fact: ProviderFact::CodexDerivation(CodexDerivationEvidence {
            thread: CodexThreadId("thread".into()),
            contract,
            includes_thinking: false,
            outputs: vec![note.id],
            changes: Vec::new(),
        }),
    };
    let proof = Op {
        id: OpId::new(NodeId(2), 0, source.seq),
        parents: ParentSet::One(source),
        tags: Tags::META | Tags::IMPORT,
        kind: OpKind::Note(NoteOp {
            target_ids: Vec::new(),
            relationship: NoteRelationship::ProviderEvidence,
            content: Payload::Inline(serde_json::to_vec(&evidence).unwrap()),
        }),
        ..raw.clone()
    };
    vec![raw, note, proof]
}

fn task() -> TaskIdentity {
    TaskIdentity {
        key: "task".into(),
        thread: "thread".into(),
        turn: "turn".into(),
        boundary: OpId::new(NodeId(1), 0, 0),
    }
}

#[test]
fn lifecycle_is_native_verified_retractable_and_scoped_to_the_task_incarnation() {
    for contract in [
        CodexDerivationContract::OccurrencesV1,
        CodexDerivationContract::OccurrencesV2,
    ] {
        let mut projection = LiveProjection::default();
        let mut groups = Tasks::default();
        groups.metadata.section(&task(), "section", true);
        for (ordinal, label, expected) in [
            (1, "inProgress", TaskStatus::InProgress),
            (2, "completed", TaskStatus::Completed),
        ] {
            let changes = projection.apply(record(ordinal, contract, label, true), &[]);
            assert_eq!(changes.upserts.len(), 1);
            groups.observe(&changes, &projection);
            assert_eq!(groups.metadata.status(&task()), expected);
            assert!(changes
                .upserts
                .values()
                .all(|row| Tasks::metadata_only(row, &projection)));
            assert!(groups.runs.dirty.contains("section"));
            groups.runs.dirty.clear();
        }
        let mut restored = task();
        restored.boundary.seq = 3 << 16;
        assert_eq!(groups.metadata.status(&restored), TaskStatus::Unknown);
        let changes = projection.apply(Vec::new(), &[OpId::new(NodeId(2), 0, 2 << 16)]);
        groups.observe(&changes, &projection);
        assert_eq!(groups.metadata.status(&task()), TaskStatus::InProgress);
    }
}

#[test]
fn lookalike_notes_do_not_supply_lifecycle_and_empty_failures_remain_visible() {
    let contract = CodexDerivationContract::OccurrencesV2;
    for (status, authentic, expected) in [
        ("completed", false, TaskStatus::Unknown),
        ("failed", true, TaskStatus::Failed),
        ("interrupted", true, TaskStatus::Interrupted),
    ] {
        let mut projection = LiveProjection::default();
        let changes = projection.apply(record(1, contract, status, authentic), &[]);
        let mut groups = Tasks::default();
        groups.observe(&changes, &projection);
        assert_eq!(groups.metadata.status(&task()), expected);
        assert!(changes
            .upserts
            .values()
            .all(|row| !Tasks::metadata_only(row, &projection)));
    }
}
