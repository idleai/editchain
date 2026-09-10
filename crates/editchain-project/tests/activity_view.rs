//! Tests for fixed Activity-view visibility and edge splicing.

#![expect(
    clippy::indexing_slicing,
    reason = "Tests index into known-length parent vectors"
)]
// Crate-level dependency markers (used by Cargo for feature resolution).
use serde as _;
use serde_json as _;

use editchain_core::{
    ActorId, Clock, GitLink, GitLinkKind, GitOid, ImportOp, MessageOp, NodeId, NoteOp,
    NoteRelationship, Op, OpId, OpKind, ParentSet, Payload, RepositoryId, ScopeRef, Tags,
};
use editchain_project::HistoryProjection;

struct NoDetails;

impl editchain_project::activity_view::ActivityPresentation for NoDetails {
    type Row = ();

    fn activity(&self, _: &editchain_project::HistoryNode) {}

    fn details(&self, _: &editchain_project::HistoryNode) -> Vec<()> {
        Vec::new()
    }
}

#[test]
fn logical_time_remains_unknown_while_visible_edges_cross_it() {
    use editchain_project::activity_view::{OmissionReason, SourceDisposition};
    use editchain_project::NodeKey;

    let parent = msg_op(90, 1, 1_000, None, "observed parent");
    let mut logical = msg_op(90, 2, 0, Some(parent.id), "logical child");
    logical.clock = Clock::Lamport(u64::MAX);
    let mut child = msg_op(90, 3, 500, Some(logical.id), "observed child");
    child.clock = Clock::Hybrid { ms: 500, ctr: 7 };
    let sources = vec![parent.clone(), logical.clone(), child.clone()];
    let projection = HistoryProjection::from_ops(sources.clone());
    let nodes = projection.nodes();
    let logical_node = nodes
        .iter()
        .find(|node| node.node_key() == logical.id.to_string())
        .unwrap();
    assert_eq!(logical_node.timestamp_ms(), 0);
    let view = projection.build_activity_view(|_| true, &NoDetails);
    assert_eq!(
        view.source_disposition(NodeKey::Op(logical.id)),
        Some(SourceDisposition::Omitted(OmissionReason::UnknownTime))
    );
    assert_eq!(
        view.graph().parents(NodeKey::Op(child.id)),
        &[NodeKey::Op(parent.id)]
    );
    assert!(view.source_row(NodeKey::Op(child.id)) < view.source_row(NodeKey::Op(parent.id)));
    assert_eq!(projection.ops(), &sources);
}

#[test]
fn complete_view_retains_explicit_dispositions_for_every_accepted_source() {
    use editchain_project::activity_view::{OmissionReason, SourceDisposition};
    use editchain_project::NodeKey;

    let visible = msg_op(70, 1, 1_000, None, "visible");
    let undated = msg_op(70, 2, 0, Some(visible.id), "undated");
    let trace = Op {
        kind: OpKind::Import(ImportOp {
            raw_ref: Payload::Inline(br#"{"type":"response_item","payload":{}}"#.to_vec()),
            raw_hash: None,
        }),
        tags: Tags::IMPORT,
        ..msg_op(70, 3, 3_000, None, "trace")
    };
    let unresolved = subagent_note(OpId::new(NodeId(998), 0, 1), OpId::new(NodeId(999), 0, 1));
    let sources = vec![
        visible.clone(),
        undated.clone(),
        trace.clone(),
        unresolved.clone(),
    ];
    let projection = HistoryProjection::from_ops(sources.clone());
    let view = projection.build_activity_view(|_| true, &NoDetails);
    assert_eq!(
        view.source_disposition(NodeKey::Op(visible.id)),
        Some(SourceDisposition::Row(0))
    );
    for (source, reason) in [
        (undated.id, OmissionReason::UnknownTime),
        (trace.id, OmissionReason::Trace),
        (unresolved.id, OmissionReason::UnresolvedRelationship),
    ] {
        assert_eq!(
            view.source_disposition(NodeKey::Op(source)),
            Some(SourceDisposition::Omitted(reason))
        );
        assert_eq!(view.source_row(NodeKey::Op(source)), None);
    }
    assert_eq!(
        view.source_disposition(NodeKey::Op(OpId::new(NodeId(500), 0, 1))),
        None
    );
    assert_eq!(projection.ops(), &sources);
}

#[test]
fn complete_view_repository_selection_preserves_qualified_identity_and_graph() {
    use editchain_core::{GitAvailability, GitCommitEntity, GitObjectFormat, GitSignature};
    use editchain_project::activity_view::{OmissionReason, SourceDisposition};
    use editchain_project::NodeKey;

    let signature = GitSignature {
        name: Payload::Empty,
        email: Payload::Empty,
        when: 1,
    };
    let visible = GitCommitEntity {
        repository: RepositoryId(1),
        object_format: GitObjectFormat::Sha1,
        oid: GitOid::from_sha1([7; 20]),
        imported_record: None,
        availability: GitAvailability::LiveOnly,
        tree: GitOid::from_sha1([8; 20]),
        parents: Vec::new(),
        author: signature.clone(),
        committer: signature,
        authored_at: 1,
        committed_at: 1,
        message: Payload::Inline(b"commit".to_vec()),
        imported_refs: Vec::new(),
        live_refs: Vec::new(),
        changed_paths: Vec::new(),
    };
    let hidden = GitCommitEntity {
        repository: RepositoryId(2),
        ..visible.clone()
    };
    let source = msg_op(80, 1, 2_000, None, "based on a hidden repository");
    let link = Op {
        id: OpId::new(NodeId(80), 0, 2),
        clock: Clock::None,
        kind: OpKind::GitLink(GitLink {
            source: source.id,
            target_repo: hidden.repository,
            target_oid: hidden.oid,
            kind: GitLinkKind::BasedOn,
        }),
        ..source.clone()
    };
    let mut projection = HistoryProjection::from_ops(vec![source.clone(), link]);
    projection.merge_git_commits(vec![visible.clone(), hidden.clone()]);
    let view = projection.build_activity_view(|id| id == RepositoryId(1), &NoDetails);
    assert!(view.source_row(NodeKey::Git(visible.key())).is_some());
    assert_eq!(
        view.source_disposition(NodeKey::Git(hidden.key())),
        Some(SourceDisposition::Omitted(OmissionReason::HiddenRepository))
    );
    assert!(view.graph().parents(NodeKey::Op(source.id)).is_empty());
    assert!(!view.graph().keys().contains(&NodeKey::Git(hidden.key())));
    let layout = view.ensure_layout();
    assert!(layout
        .parents
        .get(&source.id.to_string())
        .is_none_or(Vec::is_empty));
    // The built view owns its original observation after its source projection
    // receives a later repository update.
    projection.merge_git_commits(vec![GitCommitEntity {
        oid: GitOid::from_sha1([9; 20]),
        ..visible
    }]);
    assert_eq!(view.entries().len(), 2);
    assert_eq!(view.graph().keys().len(), 2);
}

/// Build a message op with a given clock and parent.
fn msg_op(node: u64, seq: u64, clock_ms: u64, parent: Option<OpId>, text: &str) -> Op {
    Op {
        id: OpId::new(NodeId(node), 0, seq),
        parents: parent.map_or(ParentSet::None, ParentSet::One),
        actor: ActorId(1),
        clock: Clock::UnixMs(clock_ms),
        scope: ScopeRef::None,
        tags: Tags::MESSAGE,
        kind: OpKind::Message(MessageOp {
            content: Payload::Inline(text.as_bytes().to_vec()),
            content_type: Payload::Empty,
        }),
    }
}

/// A `SubagentOf` relationship note: the subagent's first op (`parent_id`) is
/// annotated as branching from the parent thread's spawn marker (`target_id`),
/// mirroring the Codex importer's virtual-edge shape.
fn subagent_note(parent_id: OpId, target_id: OpId) -> Op {
    Op {
        id: OpId::new(NodeId(9), 0, 2),
        parents: ParentSet::One(parent_id),
        actor: ActorId(1),
        clock: Clock::UnixMs(2_000),
        scope: ScopeRef::None,
        tags: Tags::NOTE,
        kind: OpKind::Note(NoteOp {
            target_ids: vec![target_id],
            relationship: NoteRelationship::SubagentOf,
            content: Payload::Empty,
        }),
    }
}

#[test]
fn virtual_subagent_parent_is_not_duplicated_after_activity_projection() {
    // The chain holds exactly one SubagentOf note per child. The default
    // Activity projection retains that target as a derived edge. Re-reading
    // `parent_keys` must not append the same virtual target a second time.
    let spawn_marker = msg_op(2, 1, 1_000, None, "spawned subagent");
    let sub_first = msg_op(3, 1, 2_000, None, "sub work");
    let note = subagent_note(sub_first.id, spawn_marker.id);

    let projection =
        HistoryProjection::from_ops(vec![spawn_marker.clone(), sub_first.clone(), note]);
    let nodes = projection.activity_nodes();
    let sub = nodes
        .iter()
        .find(|n| n.node_key() == sub_first.id.to_string())
        .expect("subagent first op kept");

    let parents = sub.parent_keys(&projection.git().links, projection.relationship_notes());
    assert_eq!(
        parents,
        vec![spawn_marker.id.to_string()],
        "virtual SubagentOf target must appear exactly once, in stable order"
    );

    // The service emits `lifted_parent_keys`; it must be duplicate-free too.
    let lifted = projection.lifted_parent_keys(sub);
    assert_eq!(
        lifted,
        vec![spawn_marker.id.to_string()],
        "lifted parents must not repeat the materialized virtual target"
    );
}

#[test]
fn activity_view_splices_edges_across_undated_rows() {
    // b has clock 0 (undated); c's parent is rewritten to a.
    let a = msg_op(1, 1, 1_000, None, "alpha");
    let b = msg_op(1, 2, 0, Some(a.id), "beta");
    let c = msg_op(1, 3, 3_000, Some(b.id), "gamma");
    let a_id = a.id;
    let original = c.clone();
    let projection = HistoryProjection::from_ops(vec![a, b, c]);

    let nodes = projection.activity_nodes();
    assert_eq!(nodes.len(), 2);
    // c's parent should now be a (the nearest kept ancestor).
    let c_node = nodes
        .iter()
        .find(|n| n.summary() == "gamma")
        .expect("gamma kept");
    let parents = c_node.parent_keys(&projection.git().links, projection.relationship_notes());
    assert_eq!(parents.len(), 1);
    assert_eq!(parents[0], a_id.to_string());
    assert!(
        matches!(c_node, editchain_project::HistoryNode::EditOperation { op, .. } if op.as_ref() == &original)
    );
}

#[test]
fn activity_splices_a_long_hidden_chain_without_recursive_stack_growth() {
    let root = msg_op(77, 1, 1_000, None, "root");
    let mut previous = root.id;
    let mut operations = vec![root.clone()];
    for sequence in 2..20_000 {
        let hidden = msg_op(77, sequence, 0, Some(previous), "hidden");
        previous = hidden.id;
        operations.push(hidden);
    }
    let tip = msg_op(77, 20_000, 2_000, Some(previous), "tip");
    operations.push(tip.clone());
    let projection = HistoryProjection::from_ops(operations);
    let nodes = projection.activity_nodes();
    assert_eq!(nodes.len(), 2);
    let graph = projection.resolved_graph(&nodes);
    assert_eq!(
        graph.parents(editchain_project::NodeKey::Op(tip.id)),
        &[editchain_project::NodeKey::Op(root.id)]
    );
    assert!(
        matches!(nodes.first(), Some(editchain_project::HistoryNode::EditOperation { op, .. }) if op.as_ref() == &tip)
    );
}

#[test]
fn activity_view_keeps_undated_metadata_bundled_as_sub_ops() {
    let turn = Op {
        id: OpId::new(NodeId(1), 0, 1),
        parents: ParentSet::None,
        actor: ActorId(1),
        clock: Clock::UnixMs(1_000),
        scope: ScopeRef::None,
        tags: Tags::IMPORT,
        kind: OpKind::Import(ImportOp {
            raw_ref: Payload::Inline(br#"{"type":"user"}"#.to_vec()),
            raw_hash: None,
        }),
    };
    let metadata = Op {
        id: OpId::new(NodeId(1), 0, 2),
        parents: ParentSet::One(turn.id),
        actor: ActorId(1),
        clock: Clock::UnixMs(0),
        scope: ScopeRef::None,
        tags: Tags::IMPORT | Tags::META,
        kind: OpKind::Import(ImportOp {
            raw_ref: Payload::Inline(br#"{"type":"custom-title"}"#.to_vec()),
            raw_hash: None,
        }),
    };
    let projection = HistoryProjection::from_ops(vec![turn, metadata.clone()]);

    let unfiltered = projection.nodes();
    assert_eq!(unfiltered.len(), 1, "metadata is folded before filtering");
    assert_eq!(unfiltered[0].sub_ops().len(), 1);

    let filtered = projection.activity_nodes();
    assert_eq!(filtered.len(), 1);
    assert_eq!(
        filtered[0]
            .sub_ops()
            .iter()
            .map(|op| op.id)
            .collect::<Vec<_>>(),
        vec![metadata.id],
        "undated top-level rows are omitted while bundled metadata remains inspectable"
    );
}

#[test]
fn activity_view_splices_through_undated_structural_endpoints() {
    // Both exact SubagentOf endpoints are undated: the parent-side spawn row
    // follows a dated trunk row, while the child-side branch anchor precedes a
    // dated work row. The relationship remains in the projection, but neither
    // timestamp-zero carrier may survive presentation. Splicing must lift the
    // branch edge onto the two dated rows.
    let trunk = msg_op(1, 1, 1_000, None, "parent work");
    let spawn = msg_op(1, 2, 0, Some(trunk.id), "spawn marker");
    let branch_anchor = msg_op(2, 1, 0, None, "subagent anchor");
    let branch_work = msg_op(2, 2, 3_000, Some(branch_anchor.id), "subagent work");
    let note = subagent_note(branch_anchor.id, spawn.id);
    let projection = HistoryProjection::from_ops(vec![
        trunk.clone(),
        spawn.clone(),
        branch_anchor.clone(),
        branch_work.clone(),
        note,
    ]);

    let nodes = projection.activity_nodes();
    assert!(
        nodes.iter().all(|node| node.timestamp_ms() != 0),
        "Activity must omit structural timestamp-zero rows too"
    );
    let keys: Vec<String> = nodes
        .iter()
        .map(editchain_project::HistoryNode::node_key)
        .collect();
    assert!(!keys.contains(&spawn.id.to_string()));
    assert!(!keys.contains(&branch_anchor.id.to_string()));

    let work = nodes
        .iter()
        .find(|node| node.node_key() == branch_work.id.to_string())
        .expect("dated subagent work remains visible");
    assert_eq!(
        work.parent_keys(&projection.git().links, projection.relationship_notes()),
        vec![trunk.id.to_string()],
        "the stored structural relationship must splice onto dated endpoints"
    );

    let layout = projection.layout_context(&nodes);
    assert!(
        layout.edges_for_window(0, nodes.len()).iter().any(|edge| {
            edge.child == branch_work.id.to_string() && edge.parent == trunk.id.to_string()
        }),
        "the lifted branch edge must remain drawable between dated rows"
    );
}

#[test]
fn activity_view_preserves_dated_produced_by_sources_without_resolved_targets() {
    for (timestamp, expected_visible) in [(1_000, true), (0, false)] {
        let source = Op {
            id: OpId::new(NodeId(7), 0, 1),
            parents: ParentSet::None,
            actor: ActorId(1),
            clock: Clock::UnixMs(timestamp),
            scope: ScopeRef::None,
            tags: Tags::IMPORT,
            kind: OpKind::Import(ImportOp {
                raw_ref: Payload::Inline(br#"{"type":"response_item","payload":{}}"#.to_vec()),
                raw_hash: None,
            }),
        };
        let link = Op {
            id: OpId::new(NodeId(7), 0, 2),
            parents: ParentSet::None,
            kind: OpKind::GitLink(GitLink {
                source: source.id,
                target_repo: RepositoryId(1),
                target_oid: GitOid::from_sha1([0x55; 20]),
                kind: GitLinkKind::ProducedBy,
            }),
            ..source.clone()
        };
        let projection = HistoryProjection::from_ops(vec![source.clone(), link]);
        let nodes = projection.nodes();
        let source_row = nodes
            .iter()
            .find(|node| node.op_id() == Some(source.id))
            .expect("source in canonical projection");
        assert_eq!(
            source_row.visibility(),
            editchain_project::taxonomy::Visibility::Trace,
            "the fixture exercises structural protection from trace hiding"
        );
        assert_eq!(
            projection
                .activity_nodes()
                .iter()
                .any(|node| node.op_id() == Some(source.id)),
            expected_visible,
            "preserve the r9 fixed view even when the linked Git object is unavailable"
        );
    }
}

#[test]
fn activity_view_removes_undated_leaf_nodes() {
    // A dated root with an undated leaf child (e.g. a `last-prompt` record).
    // The undated leaf must be hidden even though it has no children (it is an
    // endpoint) — it is junk metadata with no chain position to anchor.
    let a = msg_op(1, 1, 1_000, None, "alpha");
    let leaf = msg_op(1, 2, 0, Some(a.id), "last-prompt");
    let projection = HistoryProjection::from_ops(vec![a.clone(), leaf]);

    let nodes = projection.activity_nodes();
    assert_eq!(nodes.len(), 1);
    assert_eq!(nodes[0].summary(), "alpha");
}
