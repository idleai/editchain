use super::*;
use editchain_core::provider::{
    CodexLifecycleEvidence, CodexSpawnSignal, ProviderEvidence, ProviderEvidenceSchema,
};
use editchain_core::{
    ActorId, Clock, ImportOp, NodeId, NoteOp, ParentSet, Payload, SessionId, Tags,
};

fn raw(stream: u64, ordinal: u64) -> Arc<Op> {
    Arc::new(Op {
        id: OpId::new(NodeId(stream), 0, ordinal << 16),
        parents: if ordinal > 1 {
            ParentSet::One(OpId::new(
                NodeId(stream),
                0,
                ordinal.saturating_sub(1) << 16,
            ))
        } else {
            ParentSet::None
        },
        actor: ActorId(1),
        clock: Clock::UnixMs(1000),
        scope: ScopeRef::Session(SessionId(stream)),
        tags: Tags::IMPORT,
        kind: OpKind::Import(ImportOp {
            raw_ref: Payload::Empty,
            raw_hash: Some([1; 32]),
        }),
    })
}

fn fact(source: &Op, slot: u64, fact: ProviderFact) -> Arc<Op> {
    let evidence = ProviderEvidence {
        schema: ProviderEvidenceSchema::V1,
        source: source.id,
        raw_hash: [1; 32],
        fact,
    };
    let mut op = source.clone();
    op.id.seq = op.id.seq.saturating_add(slot);
    op.parents = ParentSet::One(source.id);
    op.tags = Tags::META | Tags::IMPORT;
    op.kind = OpKind::Note(NoteOp {
        relationship: NoteRelationship::ProviderEvidence,
        target_ids: Vec::new(),
        content: Payload::Inline(serde_json::to_vec(&evidence).unwrap()),
    });
    Arc::new(op)
}

fn prefix(last: &Op, parent: Option<&str>) -> Arc<Op> {
    fact(
        last,
        1,
        ProviderFact::CodexSource(Box::new(CodexSourceEvidence {
            thread: CodexThreadId(last.id.node.0.to_string()),
            parent: parent.map(|parent| CodexThreadId(parent.into())),
            forked_from: None,
            agent_path: None,
            first: OpId {
                seq: 1 << 16,
                ..last.id
            },
            last: last.id,
            prefix_hash: [2; 32],
        })),
    )
}

fn lifecycle(at: &Op, child: u64, spawn: bool) -> Arc<Op> {
    let child = CodexThreadId(child.to_string());
    fact(
        at,
        2,
        ProviderFact::CodexLifecycle(CodexLifecycleEvidence {
            thread: CodexThreadId(at.id.node.0.to_string()),
            item_id: format!("event-{}", at.id),
            turn_id: "turn".into(),
            event: if spawn {
                CodexLifecycleEvent::Spawn {
                    activation: at.id,
                    child,
                    agent_path: None,
                    signal: CodexSpawnSignal::CollabTool,
                }
            } else {
                CodexLifecycleEvent::Completed { child }
            },
        }),
    )
}

fn fixture() -> Vec<Arc<Op>> {
    let mut ops = Vec::new();
    for index in 1..=7 {
        let op = raw(1, index);
        if (2..=4).contains(&index) {
            ops.push(lifecycle(&op, index, true));
        }
        if (5..=7).contains(&index) {
            ops.push(lifecycle(&op, index.saturating_sub(3), false));
        }
        if index == 7 {
            ops.push(prefix(&op, None));
        }
        ops.push(op);
    }
    for child in 2..=4 {
        for index in 1..=3 {
            let op = raw(child, index);
            if index == 3 {
                ops.push(prefix(&op, Some("1")));
            }
            ops.push(op);
        }
    }
    ops
}

fn oracle(ops: &Map<OpId, Arc<Op>>) -> BTreeSet<RelationEdge> {
    let ops: Vec<_> = ops.values().map(|op| op.as_ref().clone()).collect();
    crate::provider::resolve(&ops)
        .notes
        .into_iter()
        .flat_map(|op| {
            let OpKind::Note(note) = op.kind else {
                return Vec::new();
            };
            op.parents
                .iter()
                .flat_map(|anchor| {
                    note.target_ids.iter().map(|target| RelationEdge {
                        anchor: *anchor,
                        target: *target,
                        spawn: note.relationship == NoteRelationship::SpawnedBy,
                    })
                })
                .collect::<Vec<_>>()
        })
        .collect()
}

fn apply(
    topology: &mut Topology,
    ops: &mut Map<OpId, Arc<Op>>,
    added: &[Arc<Op>],
    removed: &[OpId],
    edges: &mut BTreeSet<RelationEdge>,
) {
    for id in removed.iter().chain(added.iter().map(|op| &op.id)) {
        if let Some(old) = ops.remove(id) {
            topology.observe(&old, false);
        }
    }
    for op in added {
        topology.observe(op, true);
        drop(ops.insert(op.id, Arc::clone(op)));
    }
    let changes = topology.resolve(ops);
    for edge in changes.removed {
        let _: bool = edges.remove(&edge);
    }
    edges.extend(changes.added);
    assert_eq!(*edges, oracle(ops));
}

#[test]
fn sibling_spawns_and_completions_match_activity_for_every_arrival_batch() {
    for reverse in [false, true] {
        for size in [1, 2, 7, 100] {
            let mut fixture = fixture();
            if reverse {
                fixture.reverse();
            }
            let (mut topology, mut ops, mut edges) =
                (Topology::default(), Map::default(), BTreeSet::new());
            for batch in fixture.chunks(size) {
                apply(&mut topology, &mut ops, batch, &[], &mut edges);
            }
            assert_eq!(edges.iter().filter(|edge| edge.spawn).count(), 3);
            assert_eq!(edges.iter().filter(|edge| !edge.spawn).count(), 3);
        }
    }
}

#[test]
fn gaps_conflicting_spawns_hashes_and_late_terminal_repair_match_activity() {
    let (mut topology, mut ops, mut edges) = (Topology::default(), Map::default(), BTreeSet::new());
    apply(&mut topology, &mut ops, &fixture(), &[], &mut edges);
    let missing = raw(2, 2);
    apply(&mut topology, &mut ops, &[], &[missing.id], &mut edges);
    apply(&mut topology, &mut ops, &[missing], &[], &mut edges);
    let ambiguous = lifecycle(&raw(1, 4), 2, true);
    let previous = ops.get(&ambiguous.id).unwrap().clone();
    apply(&mut topology, &mut ops, &[ambiguous], &[], &mut edges);
    apply(&mut topology, &mut ops, &[previous], &[], &mut edges);
    let mut corrupt = raw(2, 3).as_ref().clone();
    if let OpKind::Import(raw) = &mut corrupt.kind {
        raw.raw_hash = Some([9; 32]);
    }
    apply(
        &mut topology,
        &mut ops,
        &[Arc::new(corrupt)],
        &[],
        &mut edges,
    );
    apply(&mut topology, &mut ops, &[raw(2, 3)], &[], &mut edges);
    let next = raw(2, 4);
    apply(
        &mut topology,
        &mut ops,
        &[Arc::clone(&next)],
        &[],
        &mut edges,
    );
    apply(
        &mut topology,
        &mut ops,
        &[prefix(&next, Some("1"))],
        &[],
        &mut edges,
    );
    assert!(edges
        .iter()
        .any(|edge| !edge.spawn && edge.target == next.id));
}

#[test]
fn ordinary_append_reads_bounded_structural_evidence_after_a_long_prefix() {
    let mut topology = Topology::default();
    let mut ops = Map::default();
    for index in 1..=10_000 {
        let op = raw(2, index);
        topology.observe(&op, true);
        drop(ops.insert(op.id, op));
    }
    for op in [
        prefix(&raw(2, 10_000), Some("1")),
        raw(1, 1),
        prefix(&raw(1, 1), None),
        lifecycle(&raw(1, 1), 2, true),
    ] {
        topology.observe(&op, true);
        drop(ops.insert(op.id, op));
    }
    let before = topology.resolve(&ops);
    assert_eq!(before.added.len(), 1);
    for op in [raw(2, 10_001), prefix(&raw(2, 10_001), Some("1"))] {
        topology.observe(&op, true);
        drop(ops.insert(op.id, op));
    }
    assert_eq!(topology.evidence(&CodexThreadId("1".into())).len(), 3);
    let after = topology.resolve(&ops);
    assert!(after.added.is_empty() && after.removed.is_empty());
}

#[test]
fn invalid_prefix_identity_cannot_unblock_an_ambiguous_child() {
    let (mut topology, mut ops, mut edges) = (Topology::default(), Map::default(), BTreeSet::new());
    apply(&mut topology, &mut ops, &fixture(), &[], &mut edges);
    let alternate = raw(5, 1);
    let alternate_meta = CodexSourceEvidence {
        thread: CodexThreadId("2".into()),
        parent: Some(CodexThreadId("1".into())),
        forked_from: None,
        agent_path: None,
        first: alternate.id,
        last: alternate.id,
        prefix_hash: [2; 32],
    };
    apply(
        &mut topology,
        &mut ops,
        &[
            Arc::clone(&alternate),
            fact(
                &alternate,
                1,
                ProviderFact::CodexSource(Box::new(alternate_meta)),
            ),
        ],
        &[],
        &mut edges,
    );
    let mut broken = raw(2, 4).as_ref().clone();
    if let OpKind::Import(raw) = &mut broken.kind {
        raw.raw_hash = Some([9; 32]);
    }
    let meta = CodexSourceEvidence {
        thread: CodexThreadId("foreign".into()),
        parent: Some(CodexThreadId("1".into())),
        forked_from: None,
        agent_path: None,
        first: raw(2, 1).id,
        last: broken.id,
        prefix_hash: [2; 32],
    };
    let evidence = fact(&broken, 1, ProviderFact::CodexSource(Box::new(meta)));
    apply(
        &mut topology,
        &mut ops,
        &[Arc::new(broken), evidence],
        &[],
        &mut edges,
    );
    assert!(!edges
        .iter()
        .any(|edge| edge.spawn && edge.anchor == alternate.id));
}
