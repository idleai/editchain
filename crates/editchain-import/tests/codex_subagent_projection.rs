//! End-to-end Codex parent/subagent projection regression.
//!
//! One synthetic parent rollout spawns three subagent rollouts (`started`
//! `subAgentActivity` markers), all three complete via one `collabToolCall`
//! whose per-child `agentsStates` is `completed`, and the parent's context
//! compacts. The full importer path (raw rollouts -> `import_codex` ->
//! relationship notes) is then fed through `editchain_project`'s collapsed
//! `HistoryProjection` to prove:
//!
//! - visible rows retain branch (`SpawnedBy`) and reconnect (`ReconnectsTo`)
//!   topology after collapsing raw imports with their normalized children;
//! - every relationship endpoint resolves — to an emitted op, and after
//!   collapsing, to a visible row (the anchor raw op of the folded marker /
//!   tool op);
//! - the graph layout stays compact (bounded lanes) and stable (deterministic
//!   across repeated computations).
//!
//! The expected-correct semantics are written below; where current production
//! code cannot yet express them the failure is reported precisely.
#![cfg(unix)]

#[expect(
    dead_code,
    reason = "the shared harness module contains helpers used by sibling integration-test crates"
)]
mod common;

use blake3 as _;
use process_wrap as _;
use proptest as _;
use serde as _;
use sha2 as _;
use std::collections::{HashMap, HashSet};
use std::path::Path;
use time as _;
use tokio as _;

use editchain_core::op::{NoteRelationship, OpKind};
use editchain_core::parents::ParentSet;
use editchain_core::scope::ScopeRef;
use editchain_core::{Op, OpId};
use editchain_import::codex::HelperCommand;
use editchain_import::ids::{derive_session_id, SourcePosition, SourceStream};
use editchain_project::{HistoryNode, HistoryProjection};

use common::{import, raw_bytes, sh_helper, write_dispatching_helper, write_rollout};

/// (child index, parent spawn-line ordinal, thread id, agent path).
const CHILDREN: [(u32, u64, &str, &str); 3] = [
    (1, 2, "child-1", "/root/c1"),
    (2, 3, "child-2", "/root/c2"),
    (3, 4, "child-3", "/root/c3"),
];

/// Raw `session_meta` line bytes with a timestamp, owning thread id, and the
/// workspace cwd (so the conservative project filter keeps the rollout).
fn session_meta_line(ts: &str, thread: &str, session: &str) -> String {
    format!(
        "{{\"timestamp\":\"{ts}\",\"type\":\"session_meta\",\"payload\":{{\"session_id\":\"{session}\",\"id\":\"{thread}\",\"timestamp\":\"t\",\"cwd\":\"/workspace\"}}}}"
    )
}

/// Raw `event_msg` line bytes carrying an agent-message token.
fn event_line(ts: &str, token: &str) -> String {
    format!(
        "{{\"timestamp\":\"{ts}\",\"type\":\"event_msg\",\"payload\":{{\"type\":\"agent_message\",\"token\":\"{token}\",\"session_id\":\"parent-session\"}}}}"
    )
}

/// Serialize one `editchain-v1` line record built from typed values.
#[expect(
    clippy::needless_pass_by_value,
    reason = "the fixture builder consumes owned JSON values directly into the serialized record"
)]
fn line_record(
    ordinal: u64,
    changed_items: Vec<serde_json::Value>,
    session_meta: Option<serde_json::Value>,
    compacted: Option<serde_json::Value>,
) -> serde_json::Value {
    let mut projection = serde_json::json!({
        "changedItems": changed_items,
        "changedTurns": [],
        "removedTurnIds": [],
    });
    if let serde_json::Value::Object(object) = &mut projection {
        if let Some(meta) = session_meta {
            drop(object.insert("sessionMeta".to_string(), meta));
        }
        if let Some(compacted) = compacted {
            drop(object.insert("compacted".to_string(), compacted));
        }
    }
    serde_json::json!({
        "schemaVersion": "editchain-v1",
        "recordType": "line",
        "sourcePath": "x",
        "sourceOrdinal": ordinal,
        "decode": {"status": "ok"},
        "projection": projection,
    })
}

/// Newline-join projection records into a helper stdout buffer.
#[expect(
    clippy::unwrap_used,
    reason = "serializing in-memory serde_json::Value fixtures cannot fail"
)]
fn projection_bytes(records: &[serde_json::Value]) -> Vec<u8> {
    let mut out = Vec::new();
    for record in records {
        out.extend_from_slice(&serde_json::to_vec(record).unwrap());
        out.push(b'\n');
    }
    out
}

/// A `started` `subAgentActivity` changed item spawning `thread` at `path`.
fn spawn_item(thread: &str, path: &str) -> serde_json::Value {
    serde_json::json!({
        "turnId": "turn-1",
        "item": {
            "kind": "subAgentActivity",
            "id": format!("spawn-{thread}"),
            "activityKind": "started",
            "agentThreadId": thread,
            "agentPath": path,
        }
    })
}

/// One `collabToolCall` completing all three children via `agentsStates`.
fn collab_complete_item() -> serde_json::Value {
    serde_json::json!({
        "turnId": "turn-1",
        "item": {
            "kind": "collabToolCall",
            "id": "call-1",
            "tool": "wait",
            "status": "completed",
            "senderThreadId": "parent-1",
            "receiverThreadIds": ["child-1", "child-2", "child-3"],
            "agentsStates": {
                "child-1": {"status": "completed", "message": "done"},
                "child-2": {"status": "completed", "message": "done"},
                "child-3": {"status": "completed", "message": "done"},
            },
        }
    })
}

/// An `agentMessage` changed item for one child thread.
fn child_message_item(thread: &str) -> serde_json::Value {
    serde_json::json!({
        "turnId": "turn-1",
        "item": {
            "kind": "agentMessage",
            "id": format!("{thread}-msg"),
            "text": format!("{thread} work"),
        }
    })
}

/// Parent bridge `sessionMeta` (owning thread `parent-1`, workspace cwd).
fn parent_session_meta() -> serde_json::Value {
    serde_json::json!({
        "sessionId": "sess-parent",
        "threadId": "parent-1",
        "cwd": "/workspace",
    })
}

/// Child bridge `sessionMeta`: copied-subagent metadata with explicit
/// `parentThreadId` + `agentPath` provenance (suppresses `ForkOf` geometry).
fn child_session_meta(thread: &str, path: &str) -> serde_json::Value {
    serde_json::json!({
        "sessionId": format!("sess-{thread}"),
        "threadId": thread,
        "parentThreadId": "parent-1",
        "forkedFromId": "parent-1",
        "agentPath": path,
        "cwd": "/workspace",
    })
}

/// Bridge `sessionMeta` for a standalone main thread (workspace cwd).
fn main_session_meta(session: &str, thread: &str) -> serde_json::Value {
    serde_json::json!({
        "sessionId": session,
        "threadId": thread,
        "cwd": "/workspace",
    })
}

/// Bridge `sessionMeta` for a subagent thread: copied-subagent metadata with
/// explicit `parentThreadId` + `agentPath` provenance (suppresses `ForkOf`).
fn sub_session_meta(session: &str, thread: &str, parent: &str, path: &str) -> serde_json::Value {
    serde_json::json!({
        "sessionId": session,
        "threadId": thread,
        "parentThreadId": parent,
        "forkedFromId": parent,
        "agentPath": path,
        "cwd": "/workspace",
    })
}

/// Write the 4-rollout fixture (one parent, three children) and return a
/// dispatching fake helper whose per-file projections are exporter-shaped.
fn fixture(root: &Path) -> HelperCommand {
    // Parent rollout: metadata, three spawns, one collab completion, one
    // compaction — 6 physical lines with strictly increasing timestamps.
    write_rollout(
        root,
        "rollout-parent.jsonl",
        &[
            session_meta_line("2026-08-28T12:00:00.000Z", "parent-1", "sess-parent"),
            event_line("2026-08-28T12:00:01.000Z", "P_SPAWN_C1"),
            event_line("2026-08-28T12:00:02.000Z", "P_SPAWN_C2"),
            event_line("2026-08-28T12:00:03.000Z", "P_SPAWN_C3"),
            event_line("2026-08-28T12:00:04.000Z", "P_COLLAB"),
            event_line("2026-08-28T12:00:05.000Z", "P_COMPACT"),
        ],
    );

    let parent_projection = projection_bytes(&[
        line_record(1, Vec::new(), Some(parent_session_meta()), None),
        line_record(2, vec![spawn_item("child-1", "/root/c1")], None, None),
        line_record(3, vec![spawn_item("child-2", "/root/c2")], None, None),
        line_record(4, vec![spawn_item("child-3", "/root/c3")], None, None),
        line_record(5, vec![collab_complete_item()], None, None),
        line_record(
            6,
            vec![serde_json::json!({
                "turnId": "turn-1",
                "item": {"kind": "contextCompaction", "id": "cc-1"},
            })],
            None,
            Some(serde_json::json!({
                "message": "context compacted: 3 subagents summarized",
                "replacementCount": 3,
            })),
        ),
    ]);

    // Child rollouts: session meta + one message each.
    write_rollout(
        root,
        "rollout-child-1.jsonl",
        &[
            session_meta_line("2026-08-28T12:00:01.100Z", "child-1", "sess-child-1"),
            event_line("2026-08-28T12:00:01.500Z", "C1_WORK"),
        ],
    );
    write_rollout(
        root,
        "rollout-child-2.jsonl",
        &[
            session_meta_line("2026-08-28T12:00:02.100Z", "child-2", "sess-child-2"),
            event_line("2026-08-28T12:00:02.500Z", "C2_WORK"),
        ],
    );
    write_rollout(
        root,
        "rollout-child-3.jsonl",
        &[
            session_meta_line("2026-08-28T12:00:03.100Z", "child-3", "sess-child-3"),
            event_line("2026-08-28T12:00:03.500Z", "C3_WORK"),
        ],
    );

    let child1_projection = projection_bytes(&[
        line_record(
            1,
            Vec::new(),
            Some(child_session_meta("child-1", "/root/c1")),
            None,
        ),
        line_record(2, vec![child_message_item("child-1")], None, None),
    ]);
    let child2_projection = projection_bytes(&[
        line_record(
            1,
            Vec::new(),
            Some(child_session_meta("child-2", "/root/c2")),
            None,
        ),
        line_record(2, vec![child_message_item("child-2")], None, None),
    ]);
    let child3_projection = projection_bytes(&[
        line_record(
            1,
            Vec::new(),
            Some(child_session_meta("child-3", "/root/c3")),
            None,
        ),
        line_record(2, vec![child_message_item("child-3")], None, None),
    ]);

    let helper = write_dispatching_helper(
        root,
        "dispatch-helper.sh",
        &[
            ("rollout-parent.jsonl", &parent_projection),
            ("rollout-child-1.jsonl", &child1_projection),
            ("rollout-child-2.jsonl", &child2_projection),
            ("rollout-child-3.jsonl", &child3_projection),
        ],
    );
    sh_helper(&helper, &[])
}

/// Deterministic source stream for a rollout file in the temp root (the
/// harness imports with workspace `/workspace`).
#[expect(
    clippy::unwrap_used,
    reason = "fixture rollout is constructed directly beneath its temp root"
)]
fn stream_for(dir: &Path, name: &str) -> SourceStream {
    let path = dir.join(name);
    let key = editchain_import::cursor::canonical_source_key("codex", dir, &path).unwrap();
    editchain_import::ids::derive_keyed_source_stream(&key, 0)
}

/// Whether an op is a structural note of the given relationship.
fn is_note(op: &Op, relationship: NoteRelationship) -> bool {
    matches!(&op.kind, OpKind::Note(n) if n.relationship == relationship)
}

#[test]
#[expect(
    clippy::indexing_slicing,
    clippy::panic,
    clippy::wildcard_enum_match_arm,
    reason = "this single end-to-end test asserts directly on deterministic, known-shape fixture data"
)]
fn parent_subagent_projection_keeps_branch_and_reconnect_topology_after_collapse() {
    let dir = tempfile::tempdir().unwrap();
    let harness = import(dir.path(), &fixture(dir.path()));

    // Import shape: 4 rollouts, 12 raw lines, 15 normalized ops (8 content +
    // 7 exact relationship facts), no malformed bridge records.
    assert_eq!(
        harness.report.files_discovered, 4,
        "one parent + three children discovered"
    );
    assert_eq!(harness.report.files_processed, 4, "every rollout processed");
    assert_eq!(harness.report.raw_ops, 12, "6 parent lines + 2 per child");
    assert_eq!(
        harness.report.normalized_ops, 15,
        "8 content ops + 3 SpawnedBy + 1 ReconnectsTo + 3 ForkedFrom facts"
    );
    assert_eq!(harness.report.malformed, 0, "no bridge decode errors");
    assert_eq!(
        harness.report.evidence_ops, 10,
        "4 source extents + 6 lifecycle observations"
    );
    assert_eq!(
        harness.ops.ops.len(),
        37,
        "12 raw + 15 normalized + 10 provider evidence ops"
    );

    // Deterministic source streams per physical rollout.
    let parent = stream_for(dir.path(), "rollout-parent.jsonl");
    let child = |n: u32| stream_for(dir.path(), &format!("rollout-child-{n}.jsonl"));

    // --- Op-level topology -------------------------------------------------
    // The importer emits exactly one SpawnedBy fact per child and one grouped
    // ReconnectsTo note for the collab completion.
    let spawned_by: Vec<&Op> = harness
        .ops
        .ops
        .iter()
        .filter(|o| is_note(o, NoteRelationship::SpawnedBy))
        .collect();
    let reconnects_to: Vec<&Op> = harness
        .ops
        .ops
        .iter()
        .filter(|o| is_note(o, NoteRelationship::ReconnectsTo))
        .collect();
    assert_eq!(
        spawned_by.len(),
        3,
        "one exact SpawnedBy fact per spawned child"
    );
    assert_eq!(
        reconnects_to.len(),
        1,
        "one grouped ReconnectsTo note for the completion tool call"
    );

    // SpawnedBy: causal parent = the child's first raw op; target = the parent
    // thread's physical occurrence carrying the real `started` marker.
    for (n, spawn_ordinal, thread, _path) in CHILDREN {
        let child_stream = child(n);
        let expected_parent = child_stream
            .op_from_position(SourcePosition::raw(1))
            .unwrap();
        // Notes are sorted by their causal parent's node id (a path hash), so
        // locate each child's note by its causal parent rather than by index.
        let note = spawned_by
            .iter()
            .find(|note| note.parents == ParentSet::One(expected_parent))
            .unwrap_or_else(|| panic!("missing SpawnedBy fact for {thread}"));
        let expected_target = parent
            .op_from_position(SourcePosition::raw(spawn_ordinal))
            .unwrap();
        match &note.kind {
            OpKind::Note(note) => assert_eq!(
                note.target_ids,
                vec![expected_target],
                "SpawnedBy target is the parent's exact started occurrence for {thread}"
            ),
            _ => panic!("expected a note op"),
        }
    }

    // ReconnectsTo: causal parent = the physical completion occurrence;
    // targets = each child's last raw op, sorted.
    let reconnect = reconnects_to[0];
    assert_eq!(
        reconnect.parents,
        ParentSet::One(parent.op_from_position(SourcePosition::raw(5)).unwrap()),
        "ReconnectsTo causal parent is the exact completion occurrence"
    );
    let mut expected_reconnect_targets: Vec<OpId> = CHILDREN
        .iter()
        .map(|(n, _, _, _)| child(*n).op_from_position(SourcePosition::raw(2)).unwrap())
        .collect();
    expected_reconnect_targets.sort_unstable();
    match &reconnect.kind {
        OpKind::Note(note) => assert_eq!(
            note.target_ids, expected_reconnect_targets,
            "ReconnectsTo targets are each child's last raw op"
        ),
        _ => panic!("expected a note op"),
    }

    // Every relationship endpoint resolves to an emitted op (causal parent,
    // every target, and the note itself).
    let op_ids: HashSet<OpId> = harness.ops.ops.iter().map(|op| op.id).collect();
    for op in harness.ops.ops.iter().filter(|o| {
        matches!(
            &o.kind,
            OpKind::Note(n)
                if matches!(
                    n.relationship,
                    NoteRelationship::SpawnedBy | NoteRelationship::ReconnectsTo
                )
        )
    }) {
        for parent in &op.parents {
            assert!(
                op_ids.contains(parent),
                "relationship causal parent {parent} resolves to an emitted op"
            );
        }
        if let OpKind::Note(note) = &op.kind {
            for target in &note.target_ids {
                assert!(
                    op_ids.contains(target),
                    "relationship target {target} resolves to an emitted op"
                );
            }
        }
        assert!(
            op_ids.contains(&op.id),
            "the relationship note {} itself is emitted",
            op.id
        );
    }

    // --- Collapsed projection rows -----------------------------------------
    let projection = HistoryProjection::from_ops(harness.ops.ops.clone());
    let nodes = projection.nodes();
    assert_eq!(nodes.len(), 9, "27 ops collapse to 9 Activity rows");
    let row_by_key: HashMap<String, &HistoryNode> =
        nodes.iter().map(|n| (n.node_key(), n)).collect();
    for node in &nodes {
        assert!(
            matches!(node, HistoryNode::CollapsedImport { .. }),
            "every visible row is a collapsed import"
        );
    }
    let note_row_keys: HashSet<String> = harness
        .ops
        .ops
        .iter()
        .filter(|o| {
            is_note(o, NoteRelationship::SpawnedBy)
                || is_note(o, NoteRelationship::ReconnectsTo)
                || is_note(o, NoteRelationship::ForkedFrom)
        })
        .map(|o| o.id.to_string())
        .collect();
    for node in &nodes {
        assert!(
            !note_row_keys.contains(&node.node_key()),
            "relationship notes never render as their own rows"
        );
    }

    // Branch topology after collapsing: metadata bundling reconnects each child
    // start to one visible parent row instead of leaving a folded endpoint.
    for (n, _spawn_ordinal, thread, _path) in CHILDREN {
        let child_first = child(n).op_from_position(SourcePosition::raw(1)).unwrap();
        let child_visible = projection
            .visible_op_id(child_first)
            .expect("child first operation has a visible representative");
        let child_row = row_by_key
            .get(&child_visible.to_string())
            .expect("child first row present");
        let lifted = projection.lifted_parent_keys(child_row);
        assert_eq!(
            lifted.len(),
            1,
            "{thread} retains one visible branch parent after canonical collapse"
        );
        assert!(
            row_by_key.contains_key(&lifted[0]),
            "{thread} branch parent resolves to a visible row"
        );
    }

    // Reconnect topology after collapsing: the parent's completion row must
    // retain the ReconnectsTo virtual parents — every child's last op — in
    // addition to its stored chain parent.
    let collab_op = parent.op_from_position(SourcePosition::raw(5)).unwrap();
    let collab_row_key = projection
        .visible_op_id(collab_op)
        .expect("completion operation has a visible representative")
        .to_string();
    let collab_row = row_by_key
        .get(&collab_row_key)
        .expect("parent collab row present");
    let actual_collab_parents =
        collab_row.parent_keys(&projection.git().links, projection.relationship_notes());
    let lifted_collab_parents = projection.lifted_parent_keys(collab_row);
    assert!(
        !actual_collab_parents.is_empty(),
        "completion row retains source and reconnect parents"
    );
    assert!(
        lifted_collab_parents
            .iter()
            .all(|key| row_by_key.contains_key(key)),
        "every completion parent resolves to a visible row"
    );
    for (n, _, thread, _) in CHILDREN {
        let child_last = child(n).op_from_position(SourcePosition::raw(2)).unwrap();
        let child_visible = projection
            .visible_op_id(child_last)
            .expect("child completion has a visible representative")
            .to_string();
        assert!(
            lifted_collab_parents.contains(&child_visible),
            "completion row retains the exact ReconnectsTo edge for {thread}"
        );
    }

    // --- Layout: compact and stable ----------------------------------------
    let graph_a = projection.graph_layout();
    let graph_b = projection.graph_layout();
    let rows_a: Vec<(String, usize)> = graph_a
        .rows
        .iter()
        .map(|r| (r.node.clone(), r.lane))
        .collect();
    let rows_b: Vec<(String, usize)> = graph_b
        .rows
        .iter()
        .map(|r| (r.node.clone(), r.lane))
        .collect();
    assert_eq!(rows_a, rows_b, "lane assignment is stable across runs");
    assert_eq!(
        graph_a.rows.len(),
        9,
        "one layout row per visible collapsed row"
    );
    let lanes_used: HashSet<usize> = graph_a.rows.iter().map(|r| r.lane).collect();
    assert!(
        lanes_used.len() <= 4,
        "layout stays compact: parent lane plus one lane per concurrently-active child ({} lanes used for 9 rows)",
        lanes_used.len()
    );

    // Edge geometry must be deterministic and every endpoint must resolve to a
    // visible row.
    let edges_a: Vec<(String, String)> = graph_a
        .edges
        .iter()
        .map(|e| (e.child.clone(), e.parent.clone()))
        .collect();
    let edges_b: Vec<(String, String)> = graph_b
        .edges
        .iter()
        .map(|e| (e.child.clone(), e.parent.clone()))
        .collect();
    assert_eq!(edges_a, edges_b, "edge geometry is stable across runs");
    let row_keys: HashSet<String> = graph_a.rows.iter().map(|r| r.node.clone()).collect();
    for (child_key, parent_key) in &edges_a {
        assert!(
            row_keys.contains(child_key),
            "edge child {child_key} resolves to a visible row"
        );
        assert!(
            row_keys.contains(parent_key),
            "edge parent {parent_key} resolves to a visible row"
        );
    }

    assert_eq!(
        edges_a.len(),
        11,
        "fixed metadata bundling preserves source, branch, and reconnect edges"
    );
}

#[test]
#[expect(
    clippy::indexing_slicing,
    clippy::panic,
    reason = "this marker-less fixture asserts directly on deterministic, known-shape data"
)]
fn markerless_subagents_stay_unlinked_regardless_of_clocks() {
    let dir = tempfile::tempdir().unwrap();

    // Marker-less parent: 5 raw lines (meta + 4 events) with strictly
    // increasing timestamps and no spawn-marker items in the projection.
    write_rollout(
        dir.path(),
        "rollout-parent-ml.jsonl",
        &[
            session_meta_line("2026-08-28T12:00:00.000Z", "parent-ml", "sess-parent-ml"),
            event_line("2026-08-28T12:00:05.000Z", "P_T1"),
            event_line("2026-08-28T12:00:10.000Z", "P_T2"),
            event_line("2026-08-28T12:00:15.000Z", "P_T3"),
            event_line("2026-08-28T12:00:20.000Z", "P_T4"),
        ],
    );
    let parent_projection = projection_bytes(&[
        line_record(
            1,
            Vec::new(),
            Some(main_session_meta("sess-parent-ml", "parent-ml")),
            None,
        ),
        line_record(2, Vec::new(), None, None),
        line_record(3, Vec::new(), None, None),
        line_record(4, Vec::new(), None, None),
        line_record(5, Vec::new(), None, None),
    ]);

    // Children start between parent events, plus one child whose meta carries
    // no timestamp. None has a structured spawn marker, so none may acquire a
    // branch endpoint from those clocks.
    let children: Vec<(&str, &str, Option<&str>, u64)> = vec![
        (
            "rollout-ml-child-1.jsonl",
            "ml-child-1",
            Some("2026-08-28T12:00:06.000Z"),
            2,
        ),
        (
            "rollout-ml-child-2.jsonl",
            "ml-child-2",
            Some("2026-08-28T12:00:11.000Z"),
            3,
        ),
        (
            "rollout-ml-child-3.jsonl",
            "ml-child-3",
            Some("2026-08-28T12:00:16.000Z"),
            4,
        ),
        (
            "rollout-ml-child-4.jsonl",
            "ml-child-4",
            Some("2026-08-28T12:00:21.000Z"),
            5,
        ),
        (
            "rollout-ml-child-noclock.jsonl",
            "ml-child-noclock",
            None,
            1,
        ),
    ];

    let mut projections: Vec<Vec<u8>> = vec![parent_projection];
    for (file, thread, ts, _ordinal) in &children {
        let mut raw_lines = Vec::new();
        match ts {
            Some(ts) => {
                raw_lines.push(session_meta_line(ts, thread, &format!("sess-{thread}")));
            }
            None => {
                raw_lines.push(format!(
                    "{{\"type\":\"session_meta\",\"payload\":{{\"session_id\":\"sess-{thread}\",\"id\":\"{thread}\",\"cwd\":\"/workspace\"}}}}"
                ));
            }
        }
        raw_lines.push(event_line(
            "2026-08-28T12:30:00.000Z",
            &format!("{thread}_WORK"),
        ));
        write_rollout(dir.path(), file, &raw_lines);
        projections.push(projection_bytes(&[
            line_record(
                1,
                Vec::new(),
                Some(sub_session_meta(
                    &format!("sess-{thread}"),
                    thread,
                    "parent-ml",
                    &format!("/root/{thread}"),
                )),
                None,
            ),
            line_record(2, vec![child_message_item(thread)], None, None),
        ]));
    }
    let mut dispatch: Vec<(&str, &[u8])> = Vec::with_capacity(projections.len());
    dispatch.push(("rollout-parent-ml.jsonl", &projections[0]));
    for (index, (file, _, _, _)) in children.iter().enumerate() {
        dispatch.push((file, &projections[index + 1]));
    }
    let helper = sh_helper(
        &write_dispatching_helper(dir.path(), "dispatch-helper-ml.sh", &dispatch),
        &[],
    );
    let harness = import(dir.path(), &helper);

    assert_eq!(
        harness.report.files_discovered, 6,
        "one parent + five children"
    );
    assert_eq!(harness.report.files_processed, 6, "every rollout processed");
    assert_eq!(harness.report.raw_ops, 15, "5 parent lines + 2 per child");
    assert_eq!(
        harness.report.normalized_ops, 10,
        "5 child messages + 5 hidden ForkedFrom facts"
    );
    assert_eq!(harness.report.malformed, 0, "no bridge decode errors");

    let spawned_by: Vec<&Op> = harness
        .ops
        .ops
        .iter()
        .filter(|o| is_note(o, NoteRelationship::SpawnedBy))
        .collect();
    assert!(spawned_by.is_empty(), "no exact spawn marker means no edge");
    assert_eq!(
        harness
            .ops
            .ops
            .iter()
            .filter(|o| is_note(o, NoteRelationship::ForkedFrom))
            .count(),
        5,
        "exact fork metadata remains inspectable without row geometry"
    );
    assert!(
        !harness
            .ops
            .ops
            .iter()
            .any(|o| is_note(o, NoteRelationship::ForkOf)),
        "no fork geometry"
    );
    assert!(
        !harness
            .ops
            .ops
            .iter()
            .any(|o| is_note(o, NoteRelationship::ReconnectsTo)),
        "no completion evidence in this fixture"
    );

    // Every child remains an independent source root in the visible projection;
    // neither known nor unknown timestamps create provenance.
    let projection = HistoryProjection::from_ops(harness.ops.ops.clone());
    let rows: HashMap<String, HistoryNode> = projection
        .nodes()
        .into_iter()
        .map(|node| (node.node_key(), node))
        .collect();
    for (file, thread, _ts, _ordinal) in &children {
        let child_stream = stream_for(dir.path(), file);
        let child_first = child_stream
            .op_from_position(SourcePosition::raw(1))
            .unwrap();
        let row = rows
            .get(&child_first.to_string())
            .unwrap_or_else(|| panic!("missing first row for {thread}"));
        assert!(
            projection.lifted_parent_keys(row).is_empty(),
            "marker-less child {thread} must remain an unlinked root"
        );
    }
}

#[test]
fn embedded_parent_session_meta_does_not_hijack_child_identity_or_scope() {
    let dir = tempfile::tempdir().unwrap();

    // Marker-less parent: meta at 12:00:00, one event at 12:00:05.
    write_rollout(
        dir.path(),
        "rollout-emb-parent.jsonl",
        &[
            session_meta_line("2026-08-28T12:00:00.000Z", "parent-emb", "sess-parent-emb"),
            event_line("2026-08-28T12:00:05.000Z", "P_WORK"),
        ],
    );
    let parent_projection = projection_bytes(&[
        line_record(
            1,
            Vec::new(),
            Some(main_session_meta("sess-parent-emb", "parent-emb")),
            None,
        ),
        line_record(2, Vec::new(), None, None),
    ]);

    // Child: own meta first, then the parent's session_meta embedded as a
    // later raw line (real Codex subagent files carry the parent's meta), then
    // one work event. The bridge still projects the embedded meta on its own
    // line; the importer keeps the FIRST sessionMeta, so it must not hijack
    // the child's owning thread or session scope.
    let parent_meta_line =
        session_meta_line("2026-08-28T12:00:06.500Z", "parent-emb", "sess-parent-emb");
    write_rollout(
        dir.path(),
        "rollout-emb-child.jsonl",
        &[
            session_meta_line("2026-08-28T12:00:06.000Z", "emb-child", "sess-emb-child"),
            parent_meta_line.clone(),
            event_line("2026-08-28T12:00:07.000Z", "EMB_WORK"),
        ],
    );
    let child_projection = projection_bytes(&[
        line_record(
            1,
            Vec::new(),
            Some(sub_session_meta(
                "sess-emb-child",
                "emb-child",
                "parent-emb",
                "/root/emb",
            )),
            None,
        ),
        line_record(
            2,
            Vec::new(),
            Some(main_session_meta("sess-parent-emb", "parent-emb")),
            None,
        ),
        line_record(3, vec![child_message_item("emb-child")], None, None),
    ]);
    let helper = sh_helper(
        &write_dispatching_helper(
            dir.path(),
            "dispatch-helper-emb.sh",
            &[
                ("rollout-emb-parent.jsonl", &parent_projection),
                ("rollout-emb-child.jsonl", &child_projection),
            ],
        ),
        &[],
    );
    let harness = import(dir.path(), &helper);

    assert_eq!(harness.report.files_discovered, 2);
    assert_eq!(harness.report.files_processed, 2);
    assert_eq!(harness.report.raw_ops, 5, "2 parent lines + 3 child lines");
    assert_eq!(harness.report.malformed, 0);

    let child = stream_for(dir.path(), "rollout-emb-child.jsonl");
    let child_session = ScopeRef::Session(derive_session_id("emb-child"));
    let parent_session = ScopeRef::Session(derive_session_id("parent-emb"));
    let child_first = child.op_from_position(SourcePosition::raw(1)).unwrap();
    let child_embedded_meta = child.op_from_position(SourcePosition::raw(2)).unwrap();

    // Every op emitted from the child file (same node) stays in the child's
    // session scope; the raw lane is never hijacked by the embedded parent
    // meta, and no child op leaks into the parent session.
    let child_node = child_first.node;
    let child_ops: Vec<&Op> = harness
        .ops
        .ops
        .iter()
        .filter(|o| o.id.node == child_node)
        .collect();
    assert_eq!(child_ops.len(), 4, "3 raw + 1 message");
    for op in &child_ops {
        assert_ne!(
            op.scope, parent_session,
            "child file ops never leak into the parent session"
        );
        if matches!(op.kind, OpKind::Import(_)) {
            assert_eq!(
                op.scope, child_session,
                "child raw lane stays scoped to the child's own session"
            );
        }
    }

    // The embedded parent meta is preserved byte-exact in the child's raw lane.
    let embedded = harness
        .ops
        .ops
        .iter()
        .find(|o| o.id == child_embedded_meta)
        .expect("embedded parent meta raw op");
    assert_eq!(
        raw_bytes(embedded, &harness.blobs),
        format!("{parent_meta_line}\n").as_bytes(),
        "embedded parent meta is preserved byte-exact (with its newline) in the child's raw lane"
    );

    // With no exact started marker, parentThreadId does not manufacture a row
    // edge from either the parent clock or the embedded copy.
    assert!(!harness
        .ops
        .ops
        .iter()
        .any(|o| is_note(o, NoteRelationship::SpawnedBy)));

    // The explicit fork field is still retained as a hidden execution fact,
    // anchored to the child's own first occurrence and scope.
    let note = harness
        .ops
        .ops
        .iter()
        .find(|o| is_note(o, NoteRelationship::ForkedFrom))
        .expect("ForkedFrom fact");
    assert_eq!(note.parents, ParentSet::One(child_first));
    assert_eq!(
        note.scope, child_session,
        "relationship note is child-scoped"
    );
    assert!(matches!(&note.kind, OpKind::Note(fact)
        if fact.target_ids.len() == 1
            && !fact.target_ids.contains(&child_embedded_meta)));
}
