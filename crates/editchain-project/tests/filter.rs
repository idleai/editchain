//! Tests for general chain filtering with truncation.

#![expect(
    clippy::indexing_slicing,
    reason = "Tests index into known-length parent vectors"
)]
// Crate-level dependency markers (used by Cargo for feature resolution).
use regex as _;
use serde as _;
use serde_json as _;

use editchain_core::{
    ActorId, Clock, GitAvailability, GitCommitEntity, GitLink, GitLinkKind, GitObjectFormat,
    GitOid, GitSignature, ImportOp, MessageOp, NodeId, NoteOp, NoteRelationship, Op, OpId, OpKind,
    ParentSet, Payload, RepositoryId, ScopeRef, Tags,
};
use editchain_project::filter::ChainFilter;
use editchain_project::{HistoryProjection, ProjectionOptions};

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

/// Build a linear chain of message ops: `a -> b -> c` (a is oldest/root).
fn linear_chain() -> Vec<Op> {
    let a = msg_op(1, 1, 1_000, None, "alpha");
    let b = msg_op(1, 2, 2_000, Some(a.id), "beta");
    let c = msg_op(1, 3, 3_000, Some(b.id), "gamma");
    vec![a, b, c]
}

/// Build a minimal live Git commit for cross-domain filter tests.
fn git_commit(byte: u8, timestamp: i64) -> GitCommitEntity {
    let mut bytes = [0u8; 32];
    bytes[0] = byte;
    let oid = GitOid::new(GitObjectFormat::Sha1, bytes);
    GitCommitEntity {
        repository: RepositoryId(7),
        object_format: GitObjectFormat::Sha1,
        oid,
        imported_record: None,
        availability: GitAvailability::Resolved,
        tree: GitOid::new(GitObjectFormat::Sha1, [0u8; 32]),
        parents: Vec::new(),
        author: GitSignature {
            name: Payload::Empty,
            email: Payload::Empty,
            when: timestamp,
        },
        committer: GitSignature {
            name: Payload::Empty,
            email: Payload::Empty,
            when: timestamp,
        },
        authored_at: timestamp,
        committed_at: timestamp,
        message: Payload::Inline(b"produced commit".to_vec()),
        imported_refs: Vec::new(),
        live_refs: Vec::new(),
        changed_paths: Vec::new(),
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
fn virtual_subagent_parent_not_duplicated_after_filter_materializes_it() {
    // The chain holds exactly one SubagentOf note per child. The default
    // hide-undated filter materializes the virtual target into the filtered
    // clone's stored `Op.parents`; re-reading `parent_keys` (as the service
    // does when emitting `HistoryRow.parents`) must not append the same
    // virtual target a second time.
    let spawn_marker = msg_op(2, 1, 1_000, None, "spawned subagent");
    let sub_first = msg_op(3, 1, 2_000, None, "sub work");
    let note = subagent_note(sub_first.id, spawn_marker.id);

    let projection =
        HistoryProjection::from_ops(vec![spawn_marker.clone(), sub_first.clone(), note]);
    let nodes = projection.filtered_nodes(&ChainFilter::default());
    let sub = nodes
        .iter()
        .find(|n| n.node_key() == sub_first.id.to_string())
        .expect("subagent first op kept");

    let parents = sub.parent_keys(&projection.git.links, projection.relationship_notes());
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
fn empty_filter_keeps_all_nodes() {
    let projection = HistoryProjection::from_ops(linear_chain());
    let filter = ChainFilter::new(
        String::new(),
        String::new(),
        String::new(),
        false,
        false,
        false,
    );
    let nodes = projection.filtered_nodes(&filter);
    assert_eq!(nodes.len(), 3);
}

#[test]
fn hide_undated_removes_clock_zero_nodes() {
    // b has clock 0 (undated); a and c are dated.
    let a = msg_op(1, 1, 1_000, None, "alpha");
    let b = msg_op(1, 2, 0, Some(a.id), "beta");
    let c = msg_op(1, 3, 3_000, Some(b.id), "gamma");
    let projection = HistoryProjection::from_ops(vec![a, b, c]);

    // With splice off, the undated node is dropped but edges are NOT reconnected.
    let filter = ChainFilter::new(
        String::new(),
        String::new(),
        String::new(),
        true,
        false,
        false,
    );
    let nodes = projection.filtered_nodes(&filter);
    assert_eq!(nodes.len(), 2);
    // The kept nodes are a and c; c's parent still points at the hidden b.
    let c_node = nodes
        .iter()
        .find(|n| n.summary() == "gamma")
        .expect("gamma kept");
    assert_eq!(
        c_node
            .parent_keys(&projection.git.links, projection.relationship_notes())
            .len(),
        1
    );
}

#[test]
fn hide_undated_with_splice_reconnects_edges() {
    // b has clock 0 (undated); a and c are dated. With splice on, c's parent
    // should be rewritten to a (skipping the hidden b).
    let a = msg_op(1, 1, 1_000, None, "alpha");
    let b = msg_op(1, 2, 0, Some(a.id), "beta");
    let c = msg_op(1, 3, 3_000, Some(b.id), "gamma");
    let a_id = a.id;
    let projection = HistoryProjection::from_ops(vec![a, b, c]);

    let filter = ChainFilter::new(
        String::new(),
        String::new(),
        String::new(),
        true,
        true,
        false,
    );
    let nodes = projection.filtered_nodes(&filter);
    assert_eq!(nodes.len(), 2);
    // c's parent should now be a (the nearest kept ancestor).
    let c_node = nodes
        .iter()
        .find(|n| n.summary() == "gamma")
        .expect("gamma kept");
    let parents = c_node.parent_keys(&projection.git.links, projection.relationship_notes());
    assert_eq!(parents.len(), 1);
    assert_eq!(parents[0], a_id.to_string());
}

#[test]
fn hide_undated_keeps_metadata_already_bundled_as_sub_ops() {
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
    let projection = HistoryProjection::from_ops_with(
        vec![turn, metadata.clone()],
        ProjectionOptions {
            bundle_metadata: true,
        },
    );

    let unfiltered = projection.nodes();
    assert_eq!(unfiltered.len(), 1, "metadata is folded before filtering");
    assert_eq!(unfiltered[0].sub_ops().len(), 1);

    let filtered = projection.filtered_nodes(&ChainFilter::default());
    assert_eq!(filtered.len(), 1);
    assert_eq!(
        filtered[0]
            .sub_ops()
            .iter()
            .map(|op| op.id)
            .collect::<Vec<_>>(),
        vec![metadata.id],
        "hide_undated only removes top-level rows; bundled metadata remains inspectable"
    );
}

#[test]
fn hide_undated_splices_through_structural_relationship_endpoints() {
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

    let nodes = projection.filtered_nodes(&ChainFilter::default());
    assert!(
        nodes.iter().all(|node| node.timestamp_ms() != 0),
        "hide_undated must remove structural timestamp-zero rows too"
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
        work.parent_keys(&projection.git.links, projection.relationship_notes()),
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
fn hide_undated_removes_undated_leaf_nodes() {
    // A dated root with an undated leaf child (e.g. a `last-prompt` record).
    // The undated leaf must be hidden even though it has no children (it is an
    // endpoint) — it is junk metadata with no chain position to anchor.
    let a = msg_op(1, 1, 1_000, None, "alpha");
    let leaf = msg_op(1, 2, 0, Some(a.id), "last-prompt");
    let projection = HistoryProjection::from_ops(vec![a.clone(), leaf]);

    let filter = ChainFilter::new(
        String::new(),
        String::new(),
        String::new(),
        true,
        true,
        false,
    );
    let nodes = projection.filtered_nodes(&filter);
    assert_eq!(nodes.len(), 1);
    assert_eq!(nodes[0].summary(), "alpha");
}

#[test]
fn summary_pattern_hides_matching_intermediate_nodes() {
    // Hide the middle node by summary; endpoints stay.
    let projection = HistoryProjection::from_ops(linear_chain());
    let filter = ChainFilter::new(
        "beta".to_string(),
        String::new(),
        String::new(),
        false,
        true,
        false,
    );
    let nodes = projection.filtered_nodes(&filter);
    assert_eq!(nodes.len(), 2);
    // The kept nodes are alpha and gamma; gamma's parent is alpha.
    let gamma = nodes
        .iter()
        .find(|n| n.summary() == "gamma")
        .expect("gamma kept");
    assert_eq!(
        gamma.parent_keys(&projection.git.links, projection.relationship_notes()),
        vec![projection.ops[0].id.to_string()]
    );
}

#[test]
fn endpoints_are_preserved_even_when_matching() {
    // Both endpoints match the pattern but must still be kept (they have no
    // parent / no child in the full graph).
    let projection = HistoryProjection::from_ops(linear_chain());
    // Pattern matches alpha (root) and gamma (leaf) but not beta.
    let filter = ChainFilter::new(
        "alpha|gamma".to_string(),
        String::new(),
        String::new(),
        false,
        true,
        false,
    );
    let nodes = projection.filtered_nodes(&filter);
    assert_eq!(nodes.len(), 3);
}

#[test]
fn kind_pattern_hides_matching_nodes() {
    // A chain where the middle node is a tool; hide by kind.
    let a = msg_op(1, 1, 1_000, None, "alpha");
    let tool = Op {
        id: OpId::new(NodeId(1), 0, 2),
        parents: ParentSet::One(a.id),
        actor: ActorId(1),
        clock: Clock::UnixMs(2_000),
        scope: ScopeRef::None,
        tags: Tags::AGENT | Tags::TOOL,
        kind: OpKind::Tool(editchain_core::ToolOp {
            tool_call_id: Payload::Empty,
            tool_name: Payload::Inline(b"Bash".to_vec()),
            stage: editchain_core::ToolStage::Start,
            content: Payload::Empty,
        }),
    };
    let c = msg_op(1, 3, 3_000, Some(tool.id), "gamma");
    let projection = HistoryProjection::from_ops(vec![a.clone(), tool.clone(), c.clone()]);

    let filter = ChainFilter::new(
        String::new(),
        "tool".to_string(),
        String::new(),
        false,
        true,
        false,
    );
    let nodes = projection.filtered_nodes(&filter);
    assert_eq!(nodes.len(), 2);
}

#[test]
fn include_kind_pattern_keeps_only_matching_kinds() {
    // A chain where the middle node is a tool. An INCLUSIVE kind pattern
    // ("messages only") must keep only message nodes — the tool is excluded
    // deterministically even though a hide-pattern would leave it as a
    // non-matching survivor.
    let a = msg_op(1, 1, 1_000, None, "alpha");
    let tool = Op {
        id: OpId::new(NodeId(1), 0, 2),
        parents: ParentSet::One(a.id),
        actor: ActorId(1),
        clock: Clock::UnixMs(2_000),
        scope: ScopeRef::None,
        tags: Tags::AGENT | Tags::TOOL,
        kind: OpKind::Tool(editchain_core::ToolOp {
            tool_call_id: Payload::Empty,
            tool_name: Payload::Inline(b"Bash".to_vec()),
            stage: editchain_core::ToolStage::Start,
            content: Payload::Empty,
        }),
    };
    let c = msg_op(1, 3, 3_000, Some(tool.id), "gamma");
    let a_id = a.id;
    let projection = HistoryProjection::from_ops(vec![a, tool, c]);

    let filter = ChainFilter::new(
        String::new(),
        String::new(),
        "^message$".to_string(),
        false,
        true,
        false,
    );
    let nodes = projection.filtered_nodes(&filter);
    assert_eq!(nodes.len(), 2);
    assert!(nodes.iter().all(|n| n.kind() == "message"));
    // Splice reconnects gamma to alpha across the hidden tool.
    let gamma = nodes
        .iter()
        .find(|n| n.summary() == "gamma")
        .expect("gamma kept");
    assert_eq!(
        gamma.parent_keys(&projection.git.links, projection.relationship_notes()),
        vec![a_id.to_string()]
    );
}

#[test]
fn produced_commit_endpoints_survive_inclusive_kind_filtering() {
    let before = msg_op(1, 1, 1_000, None, "before");
    let producer = Op {
        id: OpId::new(NodeId(1), 0, 2),
        parents: ParentSet::One(before.id),
        actor: ActorId(1),
        clock: Clock::UnixMs(2_000),
        scope: ScopeRef::None,
        tags: Tags::AGENT | Tags::TOOL,
        kind: OpKind::Tool(editchain_core::ToolOp {
            tool_call_id: Payload::Inline(b"commit-call".to_vec()),
            tool_name: Payload::Inline(b"Bash".to_vec()),
            stage: editchain_core::ToolStage::Finish,
            content: Payload::Empty,
        }),
    };
    let after = msg_op(1, 3, 3_000, Some(producer.id), "after");
    let commit = git_commit(9, 2);
    let link = Op {
        id: OpId::new(NodeId(9), 0, 1),
        parents: ParentSet::One(producer.id),
        actor: ActorId(1),
        clock: Clock::UnixMs(2_000),
        scope: ScopeRef::None,
        tags: Tags::IMPORT | Tags::META,
        kind: OpKind::GitLink(GitLink {
            source: producer.id,
            target_repo: commit.repository,
            target_oid: commit.oid,
            kind: GitLinkKind::ProducedBy,
        }),
    };
    let mut projection = HistoryProjection::from_ops(vec![before, producer.clone(), after, link]);
    projection.merge_git_commits(vec![commit.clone()]);

    let filter = ChainFilter::new(
        String::new(),
        String::new(),
        "^message$".to_string(),
        false,
        true,
        false,
    );
    let nodes = projection.filtered_nodes(&filter);
    assert!(
        nodes
            .iter()
            .any(|node| node.node_key() == producer.id.to_string()),
        "the branch point must survive even though it is not a message"
    );
    let commit_node = nodes
        .iter()
        .find(|node| node.node_key() == commit.oid.to_hex())
        .expect("produced commit survives as a structural endpoint");
    assert_eq!(
        commit_node.parent_keys(&projection.git.links, projection.relationship_notes()),
        vec![producer.id.to_string()]
    );
}

#[test]
fn include_kind_pattern_excludes_nonmatching_endpoints() {
    // A dated tool leaf is an endpoint (no children). Hide patterns preserve
    // endpoints, but the INCLUSIVE kind constraint must still exclude it:
    // "messages only" cannot leave a lone tool row as an anchor.
    let a = msg_op(1, 1, 1_000, None, "alpha");
    let tool = Op {
        id: OpId::new(NodeId(1), 0, 2),
        parents: ParentSet::One(a.id),
        actor: ActorId(1),
        clock: Clock::UnixMs(2_000),
        scope: ScopeRef::None,
        tags: Tags::AGENT | Tags::TOOL,
        kind: OpKind::Tool(editchain_core::ToolOp {
            tool_call_id: Payload::Empty,
            tool_name: Payload::Inline(b"Bash".to_vec()),
            stage: editchain_core::ToolStage::Start,
            content: Payload::Empty,
        }),
    };
    let projection = HistoryProjection::from_ops(vec![a, tool]);

    let filter = ChainFilter::new(
        String::new(),
        String::new(),
        "^message$".to_string(),
        false,
        true,
        false,
    );
    let nodes = projection.filtered_nodes(&filter);
    assert_eq!(nodes.len(), 1);
    assert_eq!(nodes[0].kind(), "message");
    assert_eq!(nodes[0].summary(), "alpha");
}

#[test]
fn include_kind_pattern_is_unconditional_with_hide_undated() {
    // The inclusive constraint composes with hide-undated: an undated tool row
    // is excluded by BOTH predicates; a dated tool row only by the inclusive
    // constraint; dated messages survive.
    let a = msg_op(1, 1, 1_000, None, "alpha");
    let tool = Op {
        id: OpId::new(NodeId(1), 0, 2),
        parents: ParentSet::One(a.id),
        actor: ActorId(1),
        clock: Clock::UnixMs(0),
        scope: ScopeRef::None,
        tags: Tags::AGENT | Tags::TOOL,
        kind: OpKind::Tool(editchain_core::ToolOp {
            tool_call_id: Payload::Empty,
            tool_name: Payload::Inline(b"Bash".to_vec()),
            stage: editchain_core::ToolStage::Start,
            content: Payload::Empty,
        }),
    };
    let c = msg_op(1, 3, 3_000, Some(tool.id), "gamma");
    let projection = HistoryProjection::from_ops(vec![a, tool, c]);

    let filter = ChainFilter::new(
        String::new(),
        String::new(),
        "^message$".to_string(),
        true,
        true,
        false,
    );
    let nodes = projection.filtered_nodes(&filter);
    assert_eq!(nodes.len(), 2);
    assert!(nodes.iter().all(|n| n.kind() == "message"));
}

#[test]
fn empty_include_kind_pattern_imposes_no_constraint() {
    // The default/empty inclusive pattern must leave hide semantics untouched
    // (kind_pattern still HIDES matching non-endpoint nodes).
    let a = msg_op(1, 1, 1_000, None, "alpha");
    let tool = Op {
        id: OpId::new(NodeId(1), 0, 2),
        parents: ParentSet::One(a.id),
        actor: ActorId(1),
        clock: Clock::UnixMs(2_000),
        scope: ScopeRef::None,
        tags: Tags::AGENT | Tags::TOOL,
        kind: OpKind::Tool(editchain_core::ToolOp {
            tool_call_id: Payload::Empty,
            tool_name: Payload::Inline(b"Bash".to_vec()),
            stage: editchain_core::ToolStage::Start,
            content: Payload::Empty,
        }),
    };
    let c = msg_op(1, 3, 3_000, Some(tool.id), "gamma");
    let projection = HistoryProjection::from_ops(vec![a.clone(), tool.clone(), c.clone()]);

    // Empty include pattern + empty hide patterns -> nothing hidden.
    let filter = ChainFilter::new(
        String::new(),
        String::new(),
        String::new(),
        false,
        false,
        false,
    );
    assert!(filter.is_empty());
    assert_eq!(projection.filtered_nodes(&filter).len(), 3);

    // Empty include pattern + kind HIDE pattern still hides the tool (middle,
    // non-endpoint) while preserving message endpoints.
    let hide = ChainFilter::new(
        String::new(),
        "tool".to_string(),
        String::new(),
        false,
        true,
        false,
    );
    let nodes = projection.filtered_nodes(&hide);
    assert_eq!(nodes.len(), 2);
    assert!(nodes.iter().all(|n| n.kind() == "message"));
}
