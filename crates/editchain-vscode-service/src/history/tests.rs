use super::files::unified_diff_hunks;
use super::legacy_preview::{
    compact_import_record, compact_structured, compact_text, json_string_field_preview,
    DISPLAY_PREVIEW_CHAR_LIMIT, STRUCTURED_CARRIER_MAX_DEPTH, STRUCTURED_CARRIER_TOTAL_ENTRY_LIMIT,
};
use super::payloads::DISPLAY_PREVIEW_READ_LIMIT;
use super::presentation::{row_content_dto, sub_op_label};
use super::*;
use crate::Server;
use editchain_core::{
    ActorId, BlobRef, Clock, ContentId, ImportOp, MessageOp, NodeId, OpKind, ParentSet, PathId,
    Payload, ScopeRef, SessionId, Tags,
};
use editchain_import::BlobSink as _;
use editchain_import::FsBlobSink;
use editchain_index::{DocumentId, LexicalHit};
use editchain_protocol::{
    ParentRelationDto, ParentRelationKind, Request, RequestBody, ResponseBody,
};
use editchain_store::format::encode_op;
use editchain_store::format::{encode_page, Page};
use std::fs;

/// 2^53 + 1 — the first integer JavaScript's IEEE-754 doubles round.
const OVER_2_53: u64 = 9_007_199_254_740_993;

/// Write ops into a chain directory as a single segment page.
fn write_chain(chain_dir: &Path, ops: &[Op]) {
    let mut page = Page::new(0);
    for op in ops {
        page.add_record(0, encode_op(op).unwrap());
    }
    fs::create_dir_all(chain_dir).unwrap();
    fs::write(chain_dir.join("000000.eclog"), encode_page(&page).unwrap()).unwrap();
}

#[test]
fn concatenated_pages_preserve_all_operations_and_detail_locations() {
    let dir = tempfile::tempdir().unwrap();
    let first = import_op(7, 1, false);
    let second = import_op(7, 2, false);
    let mut bytes = Vec::new();
    for (sequence, op) in [(0, &first), (1, &second)] {
        let mut page = Page::new(sequence);
        page.add_record(0x81, encode_op(op).unwrap());
        bytes.extend(encode_page(&page).unwrap());
    }
    fs::write(dir.path().join("000000.eclog"), bytes).unwrap();
    let (ops, stats, locations) = read_chain_ops(dir.path()).unwrap();
    assert_eq!(ops, vec![first, second]);
    assert_eq!(stats.accepted, 2);
    for (op, locator) in ops.iter().zip(locations) {
        assert_eq!(read_op_at(dir.path(), locator.location).unwrap(), *op);
    }
}

/// Store a blob in a chain's durable blob store, returning its reference.
fn store_blob(chain_dir: &Path, data: &[u8]) -> BlobRef {
    let mut blobs = FsBlobSink::new(chain_dir.join("blobs")).unwrap();
    blobs.put(data).unwrap()
}

#[test]
fn unified_diff_hunks_preserve_headers_and_disconnected_sides() {
    let diff = concat!(
        "diff --git a/src/lib.rs b/src/lib.rs\n",
        "--- a/src/lib.rs\n",
        "+++ b/src/lib.rs\n",
        "@@ -1,3 +1,3 @@ fn first()\n",
        " context one\n",
        "-old one\n",
        "+new one\n",
        " context two\n",
        "@@ -20 +21,2 @@ fn second()\n",
        "-old two\n",
        "+new two\n",
        "+another line\n",
    );

    let hunks = unified_diff_hunks(diff);

    assert_eq!(hunks.len(), 2);
    assert_eq!(hunks[0].header, "@@ -1,3 +1,3 @@ fn first()");
    assert_eq!(hunks[0].before, "context one\nold one\ncontext two");
    assert_eq!(hunks[0].after, "context one\nnew one\ncontext two");
    assert_eq!(hunks[1].header, "@@ -20 +21,2 @@ fn second()");
    assert_eq!(hunks[1].before, "old two");
    assert_eq!(hunks[1].after, "new two\nanother line");
}

/// Wrap `kind` in a standalone operation envelope.
fn op_envelope(node: u64, seq: u64, kind: OpKind) -> Op {
    Op {
        id: OpId::new(NodeId(node), 0, seq),
        parents: ParentSet::None,
        actor: ActorId(1),
        clock: Clock::UnixMs(seq),
        scope: ScopeRef::Session(SessionId(10)),
        tags: Tags::IMPORT,
        kind,
    }
}

/// Build a raw import op.
fn import_op(node: u64, seq: u64, meta: bool) -> Op {
    let mut tags = Tags::IMPORT;
    if meta {
        tags |= Tags::META;
    }
    let record_type = if meta { "last-prompt" } else { "user" };
    Op {
        id: OpId::new(NodeId(node), 0, seq),
        parents: ParentSet::None,
        actor: ActorId(1),
        clock: Clock::UnixMs(seq),
        scope: ScopeRef::Session(SessionId(10)),
        tags,
        kind: OpKind::Import(ImportOp {
            raw_ref: Payload::Inline(
                format!(r#"{{"type":"{record_type}","seq":{seq}}}"#).into_bytes(),
            ),
            raw_hash: None,
        }),
    }
}

/// Build a normalized message op whose parent is `parent`.
fn message_op(node: u64, seq: u64, parent: OpId) -> Op {
    Op {
        id: OpId::new(NodeId(node), 0, seq),
        parents: ParentSet::One(parent),
        actor: ActorId(1),
        clock: Clock::UnixMs(seq),
        scope: ScopeRef::Session(SessionId(10)),
        tags: Tags::HUMAN | Tags::MESSAGE,
        kind: OpKind::Message(MessageOp {
            content: Payload::Inline(b"hello world".to_vec()),
            content_type: Payload::Empty,
        }),
    }
}

/// Build a versioned exact relationship fact: causal parent `parent`,
/// targets `targets`, META-tagged so the projection folds it out of rendered
/// rows and reads it as a virtual edge.
fn structural_note(
    id: OpId,
    parent: OpId,
    targets: Vec<OpId>,
    relationship: editchain_core::NoteRelationship,
    session: u64,
) -> Op {
    Op {
        id,
        parents: ParentSet::One(parent),
        actor: ActorId(0),
        clock: Clock::None,
        scope: ScopeRef::Session(SessionId(session)),
        tags: Tags::META | Tags::IMPORT,
        kind: OpKind::Note(editchain_core::op::NoteOp {
            target_ids: targets,
            relationship,
            content: Payload::Inline(
                br#"{"confidence":"exact","resolver":"service-test-v1"}"#.to_vec(),
            ),
        }),
    }
}

#[test]
fn history_window_exposes_structural_relationship_kinds() {
    // A parent thread (node 1) spawns a subagent thread (node 2) and
    // reconnects into it; a third thread (node 3) forks off the parent.
    // Exact relationship facts drive typed virtual edges in the projection;
    // the service must preserve their provider-neutral kinds.
    let trunk = import_op(1, 1, false);
    let spawn_marker = message_op(1, 3, trunk.id);
    let sub_meta = import_op(2, 1, true);
    let mut sub_first = import_op(2, 2, false);
    sub_first.parents = ParentSet::One(sub_meta.id);
    let sub_last = message_op(2, 5, sub_first.id);
    let completion = message_op(1, 7, spawn_marker.id);
    let branch_first = import_op(3, 1, false);

    let ops = vec![
        trunk.clone(),
        spawn_marker.clone(),
        sub_meta.clone(),
        sub_first.clone(),
        sub_last.clone(),
        completion.clone(),
        branch_first.clone(),
        structural_note(
            OpId::new(NodeId(1), 0, 0xFFFC),
            sub_meta.id,
            vec![spawn_marker.id],
            editchain_core::NoteRelationship::SpawnedBy,
            2,
        ),
        structural_note(
            OpId::new(NodeId(1), 0, 0xFFFB),
            completion.id,
            vec![sub_last.id],
            editchain_core::NoteRelationship::ReconnectsTo,
            1,
        ),
        structural_note(
            OpId::new(NodeId(1), 0, 0xFFFA),
            branch_first.id,
            vec![trunk.id],
            editchain_core::NoteRelationship::ForkOf,
            3,
        ),
    ];
    let projection = HistoryProjection::from_ops(ops);
    let mut ws = Workspace::from_projection(projection);
    let window = ws
        .history_window(HistoryWindowOptions {
            offset: 0,
            limit: 100,
            include_layout: true,
        })
        .unwrap();

    // SpawnedBy: the exact anchor is bundled session metadata, matching a
    // Codex child rollout. Its first surviving row carries the "subagent"
    // relation to the CANONICAL spawn anchor. The raw target (the folded
    // spawn marker op) resolves through the representative map to the
    // trunk's visible import row, which is the parent the row actually
    // renders.
    let sub_row = window
        .rows
        .iter()
        .find(|r| r.op_id.as_deref() == Some(sub_first.id.to_string().as_str()))
        .expect("subagent first op row");
    assert_eq!(sub_row.parents, vec![trunk.id.to_string()]);
    assert_eq!(
        sub_row.parent_relations,
        vec![ParentRelationDto {
            parent: trunk.id.to_string(),
            kind: ParentRelationKind::Subagent,
        }]
    );

    // ReconnectsTo: the parent thread's completion row (a standalone
    // message row on the trunk) carries a "reconnect" relation back into
    // the subagent's last op — again resolved to the visible subagent
    // import row. Its stored parent (the folded spawn marker) lifts to the
    // trunk row, so both parent keys are canonical visible rows.
    let completion_row = window
        .rows
        .iter()
        .find(|r| r.op_id.as_deref() == Some(completion.id.to_string().as_str()))
        .expect("completion row");
    assert_eq!(
        completion_row.parents,
        vec![trunk.id.to_string(), sub_first.id.to_string()]
    );
    assert_eq!(
        completion_row.parent_relations,
        vec![ParentRelationDto {
            parent: sub_first.id.to_string(),
            kind: ParentRelationKind::Reconnect,
        }]
    );

    // ForkOf: the fork thread's first op carries a "fork" relation to the
    // trunk op it branches off.
    let fork_row = window
        .rows
        .iter()
        .find(|r| r.op_id.as_deref() == Some(branch_first.id.to_string().as_str()))
        .expect("fork first op row");
    assert_eq!(fork_row.parents, vec![trunk.id.to_string()]);
    assert_eq!(
        fork_row.parent_relations,
        vec![ParentRelationDto {
            parent: trunk.id.to_string(),
            kind: ParentRelationKind::Fork,
        }]
    );

    // The trunk row itself carries no structural relations (no note is
    // anchored on it), the structural notes never render as rows, and no
    // row carries a stale relation to a folded op id.
    let trunk_row = window
        .rows
        .iter()
        .find(|r| r.op_id.as_deref() == Some(trunk.id.to_string().as_str()))
        .expect("trunk row");
    assert!(trunk_row.parents.is_empty());
    assert!(trunk_row.parent_relations.is_empty());
    assert!(
        window.rows.iter().all(|r| r.kind != "note"),
        "structural notes must never render as rows"
    );
    for row in &window.rows {
        for rel in &row.parent_relations {
            assert!(
                row.parents.contains(&rel.parent),
                "relation.parent {} must be one of the row's parents {:?}",
                rel.parent,
                row.parents
            );
        }
    }
}

#[test]
fn history_window_bundles_meta_subops() {
    // A real turn (import + message), then a META import. The META import
    // bundles into the turn's row as a sub-op; the service emits it as its
    // own expanded row immediately after the parent.
    let turn = import_op(1, 1, false);
    let msg = message_op(1, 2, turn.id);
    let meta = Op {
        parents: ParentSet::One(turn.id),
        ..import_op(1, 3, true)
    };

    let projection = HistoryProjection::from_ops(vec![turn.clone(), msg, meta.clone()]);
    let mut ws = Workspace::from_projection(projection);
    let window = ws
        .history_window(HistoryWindowOptions {
            offset: 0,
            limit: 100,
            include_layout: true,
        })
        .unwrap();

    // Parent row + one expanded sub-op row.
    assert_eq!(window.rows.len(), 2);
    assert_eq!(window.total, 2);
    assert_eq!(window.chain_generation, 3);
    assert_eq!(window.sub_op_counts.as_deref(), Some(&[1][..]));
    // Parent carries the bundled sub-op summary (for the collapsed chevron).
    assert_eq!(window.rows[0].sub_ops.len(), 1);
    assert_eq!(window.rows[0].sub_ops[0].op_id, meta.id.to_string());
    assert_eq!(window.rows[0].sub_ops[0].kind, "last-prompt");
    assert!(!window.rows[0].is_subop);
    assert!(window.rows[0].group_end);
    assert_eq!(window.rows[0].parent_row, None);
    // The expanded sub-op row follows its parent and inherits its lane.
    assert!(window.rows[1].is_subop);
    assert!(!window.rows[1].group_end);
    assert_eq!(window.rows[1].parent_row, Some(0));
    assert_eq!(
        window.rows[1].op_id.as_deref(),
        Some(meta.id.to_string().as_str())
    );
    assert_eq!(window.rows[1].subop_kind.as_deref(), Some("msg"));

    // A page beginning inside an expanded block must still resolve the
    // owning top-level node. Global expansion metadata is sent only on the
    // offset-zero page and retained by the client for later windows.
    let deep = ws
        .history_window(HistoryWindowOptions {
            offset: 1,
            limit: 1,
            include_layout: true,
        })
        .unwrap();
    assert_eq!(deep.rows.len(), 1);
    assert!(deep.rows[0].is_subop);
    assert!(!deep.rows[0].group_end);
    assert_eq!(deep.rows[0].op_id, Some(meta.id.to_string()));
    assert!(deep.sub_op_counts.is_none());
}

#[test]
fn sub_op_rows_draw_pass_through_lanes() {
    // A linear chain where a middle turn carries a bundled META sub-op and
    // has both a child above and a parent below on its own lane. The sub-op
    // row must carry that lane as pass-through (above == below), so the
    // client draws it as a full-height straight line with no dot.
    //
    // Build newest-first by clock:
    //   child (seq high) -> turn+meta (middle) -> parent (low).
    let parent = message_op(5, 4, OpId::new(NodeId(5), 0, 3));
    let turn = import_op(5, 5, false);
    let turn_with_parent = Op {
        parents: ParentSet::One(parent.id),
        ..turn.clone()
    };
    let msg = message_op(5, 6, turn.id); // child of turn
    let meta = Op {
        parents: ParentSet::One(turn.id),
        ..import_op(5, 7, true)
    }; // bundled under turn

    let projection = HistoryProjection::from_ops(vec![
        msg.clone(),
        turn_with_parent.clone(),
        meta.clone(),
        parent.clone(),
    ]);
    let mut ws = Workspace::from_projection(projection);
    let window = ws
        .history_window(HistoryWindowOptions {
            offset: 0,
            limit: 100,
            include_layout: true,
        })
        .unwrap();

    // Find the sub-op row (is_subop).
    let sub = window
        .rows
        .iter()
        .find(|r| r.is_subop)
        .expect("sub-op row present");
    // The sub-op row must have at least one pass-through lane (its own
    // parent's lane), and above == below so the client draws a full line.
    assert!(!sub.above.is_empty());
    assert_eq!(sub.above, sub.below);
}

#[test]
fn sub_op_label_parses_record_type() {
    let op = import_op(1, 1, true);
    let (summary, kind) = sub_op_label(&op);
    assert_eq!(summary, "last-prompt");
    assert_eq!(kind, "last-prompt");
}

#[test]
fn sub_op_label_uses_event_payload_type() {
    let op = op_envelope(
        1,
        1,
        OpKind::Import(ImportOp {
            raw_ref: Payload::Inline(
                br#"{"type":"event_msg","payload":{"type":"task_complete"}}"#.to_vec(),
            ),
            raw_hash: None,
        }),
    );
    let (summary, kind) = sub_op_label(&op);
    assert_eq!(summary, "task_complete");
    assert_eq!(kind, "task_complete");
}

#[test]
fn sub_op_label_formats_token_accounting_as_numbers() {
    let count = op_envelope(
        1,
        1,
        OpKind::Import(ImportOp {
            raw_ref: Payload::Inline(
                br#"{"type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"total_tokens":34652},"last_token_usage":{"total_tokens":17502},"model_context_window":258400}}}"#
                    .to_vec(),
            ),
            raw_hash: None,
        }),
    );
    assert_eq!(
        sub_op_label(&count),
        ("17,502 / 258,400".to_string(), "token_count".to_string())
    );

    let usage = op_envelope(
        1,
        2,
        OpKind::Import(ImportOp {
            raw_ref: Payload::Inline(
                br#"{"type":"token_usage_record","payload":{"usage":{"total_tokens":140635},"turn_token_usage":{"total_tokens":282570},"thread_token_usage":{"total_tokens":900001}}}"#
                    .to_vec(),
            ),
            raw_hash: None,
        }),
    );
    assert_eq!(
        sub_op_label(&usage),
        ("140,635".to_string(), "token_usage_record".to_string())
    );
}

#[test]
fn sub_op_label_preserves_folded_claude_tool_fragment() {
    let op = op_envelope(
        1,
        1,
        OpKind::Import(ImportOp {
            raw_ref: Payload::Inline(
                br#"{"type":"assistant","message":{"id":"msg-1","content":[{"type":"tool_use","name":"Read"}]}}"#
                    .to_vec(),
            ),
            raw_hash: None,
        }),
    );
    let (summary, kind) = sub_op_label(&op);
    assert_eq!(summary, "tool: Read");
    assert_eq!(kind, "tool");
}

#[test]
fn sub_op_label_renders_tool_result_preview() {
    // A tool-result sub-op (Tool, Finish) should render a content preview.
    let op = Op {
        id: OpId::new(NodeId(1), 0, 1),
        parents: ParentSet::None,
        actor: ActorId(1),
        clock: Clock::UnixMs(1),
        scope: ScopeRef::Session(SessionId(10)),
        tags: Tags::TOOL,
        kind: OpKind::Tool(editchain_core::op::ToolOp {
            tool_call_id: Payload::Empty,
            tool_name: Payload::Empty,
            stage: editchain_core::op::ToolStage::Finish,
            content: Payload::Inline(b"1\tline one\n2\tline two".to_vec()),
        }),
    };
    let (summary, kind) = sub_op_label(&op);
    assert_eq!(summary, "line one");
    assert_eq!(kind, "tool_result");
}

#[test]
fn open_previews_blobs_and_hydrates_details_and_search_on_demand() {
    let dir = tempfile::tempdir().unwrap();
    let workspace_path = dir.path().join("workspace");
    fs::create_dir_all(&workspace_path).unwrap();
    let chain_dir = workspace_path.join(".editchain");

    // Payloads well past INLINE_LIMIT (4096) so the importer would spill
    // them into durable blobs.
    let msg_content = format!("needle-hydrated-message {}", "x".repeat(8192)).into_bytes();
    let tool_content = format!("needle-hydrated-tool {}", "y".repeat(8192)).into_bytes();
    let raw_content = format!("needle-hydrated-raw {}", "z".repeat(8192)).into_bytes();
    let msg_ref = store_blob(&chain_dir, &msg_content);
    let tool_ref = store_blob(&chain_dir, &tool_content);
    let raw_ref = store_blob(&chain_dir, &raw_content);

    let msg = op_envelope(
        1,
        1,
        OpKind::Message(MessageOp {
            content: Payload::Blob(msg_ref),
            content_type: Payload::Empty,
        }),
    );
    let tool = op_envelope(
        1,
        2,
        OpKind::Tool(editchain_core::op::ToolOp {
            tool_call_id: Payload::Empty,
            tool_name: Payload::Empty,
            stage: editchain_core::op::ToolStage::Finish,
            content: Payload::Blob(tool_ref),
        }),
    );
    let raw = op_envelope(
        1,
        3,
        OpKind::Import(ImportOp {
            raw_ref: Payload::Blob(raw_ref),
            raw_hash: None,
        }),
    );
    write_chain(&chain_dir, &[msg.clone(), tool.clone(), raw.clone()]);

    // Reopen the workspace over the durable chain.
    let mut ws = Workspace::open(workspace_path.to_str().unwrap(), ".editchain").unwrap();
    assert_eq!(ws.diagnostics.chain.records, 3);
    assert_eq!(ws.diagnostics.chain.accepted, 3);
    assert_eq!(ws.diagnostics.blobs.hydrated, 0);
    assert_eq!(ws.diagnostics.blobs.previewed, 3);
    assert_eq!(ws.diagnostics.blobs.deferred, 3);
    assert_eq!(ws.diagnostics.blobs.missing, 0);
    assert_eq!(ws.diagnostics.blobs.corrupt, 0);
    assert!(ws.diagnostics.warnings().is_empty());

    // NodeDetails hydrates the requested source operation on demand.
    let details = ws.node_details(Some(msg.id.to_string()), None).unwrap();
    assert!(details.body.contains("needle-hydrated-message"));
    let tool_details = ws.node_details(Some(tool.id.to_string()), None).unwrap();
    assert!(tool_details.body.contains("needle-hydrated-tool"));
    let raw_details = ws.node_details(Some(raw.id.to_string()), None).unwrap();
    assert!(raw_details.summary.contains("needle-hydrated-raw"));

    // Search hydrates eligible content fields while lazily building its index.
    let state = build_lexical_index(&mut ws).unwrap();
    let results = state
        .index()
        .candidates("needle-hydrated-message", 5)
        .unwrap()
        .next_page(5)
        .unwrap()
        .hits;
    assert!(!results.is_empty());
    assert!(results
        .iter()
        .any(|r| r.document == DocumentId::Operation(msg.id)));
}

#[test]
fn git_search_hits_retain_real_repository_and_commit_identity() {
    // A real commit in a repository whose id exceeds 2^53, so the identity
    // must round-trip as an exact decimal string.
    let mut bytes = [0u8; 20];
    bytes[0] = 0xaa;
    let oid = GitOid::from_sha1(bytes);
    let commit = editchain_core::GitCommitEntity {
        repository: RepositoryId(OVER_2_53),
        object_format: editchain_core::GitObjectFormat::Sha1,
        oid,
        imported_record: None,
        availability: editchain_core::GitAvailability::Resolved,
        tree: oid,
        parents: Vec::new(),
        author: editchain_core::GitSignature {
            name: Payload::Inline(b"Alice".to_vec()),
            email: Payload::Inline(b"alice@example.com".to_vec()),
            when: 0,
        },
        committer: editchain_core::GitSignature {
            name: Payload::Inline(b"Alice".to_vec()),
            email: Payload::Inline(b"alice@example.com".to_vec()),
            when: 0,
        },
        authored_at: 0,
        committed_at: 0,
        message: Payload::Inline(b"needle-git-identity".to_vec()),
        imported_refs: Vec::new(),
        live_refs: Vec::new(),
        changed_paths: Vec::new(),
    };
    let mut projection = HistoryProjection::new();
    projection.merge_git_commits(vec![commit]);
    let mut ws = Workspace::from_projection(projection);

    let state = build_lexical_index(&mut ws).unwrap();
    let results = state
        .index()
        .candidates("needle-git-identity", 5)
        .unwrap()
        .next_page(5)
        .unwrap()
        .hits;
    let key = editchain_core::GitCommitKey::new(RepositoryId(OVER_2_53), oid);
    assert!(results
        .iter()
        .any(|hit| hit.document == DocumentId::GitCommit(key)));
    let exact = state
        .index()
        .candidates(&oid.to_hex(), 5)
        .unwrap()
        .next_page(5)
        .unwrap();
    assert!(exact
        .hits
        .iter()
        .any(|hit| hit.document == DocumentId::GitCommit(key)));
}

#[test]
fn open_preserves_missing_and_corrupt_blob_refs_and_reports() {
    let dir = tempfile::tempdir().unwrap();
    let workspace_path = dir.path().join("workspace");
    fs::create_dir_all(&workspace_path).unwrap();
    let chain_dir = workspace_path.join(".editchain");

    // Missing: the reference is never stored.
    let missing_data = b"never-stored-content".to_vec();
    let missing_ref = BlobRef {
        id: ContentId::Hash256(hash_raw(&missing_data)),
        len: u32::try_from(missing_data.len()).unwrap(),
    };

    // Corrupt by content: the file exists but holds different bytes.
    let corrupt_data = b"corrupt-original-content".to_vec();
    let corrupt_hash = hash_raw(&corrupt_data);
    let corrupt_ref = BlobRef {
        id: ContentId::Hash256(corrupt_hash),
        len: u32::try_from(corrupt_data.len()).unwrap(),
    };
    let sink = FsBlobSink::new(chain_dir.join("blobs")).unwrap();
    fs::write(sink.path_for(&corrupt_hash), b"corrupted bytes").unwrap();

    // Corrupt by length: correct bytes but a lying declared length.
    let len_data = b"valid-length-content".to_vec();
    let len_ref = BlobRef {
        id: ContentId::Hash256(hash_raw(&len_data)),
        len: u32::try_from(len_data.len()).unwrap().saturating_add(1),
    };
    let _: BlobRef = store_blob(&chain_dir, &len_data);

    // Unresolvable: a local node ref cannot be addressed by this store.
    let local_ref = BlobRef {
        id: ContentId::Local {
            node: NodeId(1),
            seq: 7,
        },
        len: 3,
    };

    let missing_msg = op_envelope(
        1,
        1,
        OpKind::Message(MessageOp {
            content: Payload::Blob(missing_ref),
            content_type: Payload::Empty,
        }),
    );
    let corrupt_msg = op_envelope(
        1,
        2,
        OpKind::Message(MessageOp {
            content: Payload::Blob(corrupt_ref),
            content_type: Payload::Empty,
        }),
    );
    let len_msg = op_envelope(
        1,
        3,
        OpKind::Message(MessageOp {
            content: Payload::Blob(len_ref),
            content_type: Payload::Empty,
        }),
    );
    let local_msg = op_envelope(
        1,
        4,
        OpKind::Message(MessageOp {
            content: Payload::Blob(local_ref),
            content_type: Payload::Empty,
        }),
    );
    write_chain(
        &chain_dir,
        &[
            missing_msg.clone(),
            corrupt_msg.clone(),
            len_msg.clone(),
            local_msg.clone(),
        ],
    );

    // The open succeeds; every unhydrated payload stays a Blob ref.
    let mut server = Server::new();
    let request = Request {
        id: 1,
        body: RequestBody::Open(editchain_protocol::OpenRequest {
            workspace_path: workspace_path.to_string_lossy().to_string(),
            chain_dir: ".editchain".to_string(),
        }),
    };
    let response = server.handle(&request).unwrap();
    assert!(
        matches!(&response.body, ResponseBody::Ok(_)),
        "open unexpectedly failed"
    );
    let value = match response.body {
        ResponseBody::Ok(value) => value,
        ResponseBody::Error(_) => return,
    };
    let diagnostics = value.get("diagnostics").unwrap();
    assert_eq!(diagnostics["blobs"]["hydrated"], 0);
    assert_eq!(diagnostics["blobs"]["missing"], 1);
    assert_eq!(diagnostics["blobs"]["corrupt"], 2);
    assert_eq!(diagnostics["blobs"]["unresolved"], 1);
    assert!(!value["warnings"].as_array().unwrap().is_empty());

    let ws = server.workspace.as_ref().unwrap();
    for op in ws.projection.ops() {
        if let OpKind::Message(message) = &op.kind {
            assert!(matches!(message.content, Payload::Blob(_)));
        }
    }
    // Details must not claim hydrated content for preserved refs (missing
    // payloads surface as empty text, not fabricated content).
    let details = ws
        .node_details(Some(missing_msg.id.to_string()), None)
        .unwrap();
    assert_eq!(details.body, "");
}

#[test]
fn open_canonicalizes_replays_and_quarantines_conflicts() {
    let dir = tempfile::tempdir().unwrap();
    let workspace_path = dir.path().join("workspace");
    fs::create_dir_all(&workspace_path).unwrap();
    let chain_dir = workspace_path.join(".editchain");

    let first = op_envelope(
        1,
        1,
        OpKind::Message(MessageOp {
            content: Payload::Inline(b"first-payload".to_vec()),
            content_type: Payload::Empty,
        }),
    );
    let replayed = first.clone();
    let conflicting = Op {
        kind: OpKind::Message(MessageOp {
            content: Payload::Inline(b"conflicting-payload".to_vec()),
            content_type: Payload::Empty,
        }),
        ..first.clone()
    };
    let second = op_envelope(
        1,
        2,
        OpKind::Message(MessageOp {
            content: Payload::Inline(b"second-payload".to_vec()),
            content_type: Payload::Empty,
        }),
    );
    write_chain(
        &chain_dir,
        &[first.clone(), replayed, conflicting.clone(), second.clone()],
    );

    let ws = Workspace::open(workspace_path.to_str().unwrap(), ".editchain").unwrap();
    assert_eq!(ws.diagnostics.chain.records, 4);
    assert_eq!(ws.diagnostics.chain.accepted, 1);
    assert_eq!(ws.diagnostics.chain.duplicates, 1);
    assert_eq!(ws.diagnostics.chain.quarantined, 2);
    assert_eq!(ws.projection.ops(), vec![second]);
    assert_eq!(ws.diagnostics.warnings().len(), 2);
    assert!(ws.node_details(Some(first.id.to_string()), None).is_none());
}

#[test]
fn hydrate_traverses_every_payload_bearing_field() {
    let dir = tempfile::tempdir().unwrap();
    let chain_dir = dir.path().join("chain");
    let mut blobs = FsBlobSink::new(chain_dir.join("blobs")).unwrap();
    let file_blob_ref = blobs.put(b"full-file-content").unwrap();
    let resolver = BlobResolver::open(&chain_dir).unwrap();
    let mut blob = |data: &[u8]| Payload::Blob(blobs.put(data).unwrap());
    let git_oid = || GitOid::from_sha1([0u8; 20]);

    let mut ops = vec![
        op_envelope(
            1,
            1,
            OpKind::Actor(editchain_core::op::ActorOp {
                label: blob(b"actor-label"),
                role: blob(b"actor-role"),
            }),
        ),
        op_envelope(
            1,
            2,
            OpKind::Message(MessageOp {
                content: blob(b"msg-content"),
                content_type: blob(b"msg-type"),
            }),
        ),
        op_envelope(
            1,
            3,
            OpKind::Tool(editchain_core::op::ToolOp {
                tool_call_id: blob(b"tool-call-id"),
                tool_name: blob(b"tool-name"),
                stage: editchain_core::op::ToolStage::Start,
                content: blob(b"tool-content"),
            }),
        ),
        op_envelope(
            1,
            4,
            OpKind::Command(editchain_core::op::CommandOp {
                command_id: blob(b"cmd-id"),
                content: blob(b"cmd-content"),
                stage: editchain_core::op::CommandStage::Start,
            }),
        ),
        op_envelope(
            1,
            5,
            OpKind::File(editchain_core::op::FileOp {
                path: PathId(1),
                stage: editchain_core::op::FileStage::Observed,
                base: None,
                after: None,
                edit: editchain_core::op::FileEdit::ReplaceBytes {
                    range: editchain_core::op::ByteRange { start: 0, end: 4 },
                    bytes: blob(b"replace-bytes"),
                },
            }),
        ),
        op_envelope(
            1,
            6,
            OpKind::File(editchain_core::op::FileOp {
                path: PathId(2),
                stage: editchain_core::op::FileStage::Observed,
                base: None,
                after: None,
                edit: editchain_core::op::FileEdit::UnifiedDiff(blob(b"unified-diff")),
            }),
        ),
        op_envelope(
            1,
            7,
            OpKind::Reflection(editchain_core::ReflectionOp {
                scope: ScopeRef::None,
                covers: editchain_core::FrontierSet::new(),
                window: editchain_core::WindowRef {
                    start_seq: 0,
                    end_seq: 0,
                },
                summary: blob(b"reflection-summary"),
                anchors: blob(b"reflection-anchors"),
            }),
        ),
        op_envelope(
            1,
            8,
            OpKind::Import(ImportOp {
                raw_ref: blob(b"raw-ref"),
                raw_hash: None,
            }),
        ),
        op_envelope(
            1,
            9,
            OpKind::Note(editchain_core::op::NoteOp {
                target_ids: Vec::new(),
                relationship: editchain_core::op::NoteRelationship::Explains,
                content: blob(b"note-content"),
            }),
        ),
        op_envelope(
            1,
            10,
            OpKind::Error(editchain_core::op::ErrorOp {
                code: blob(b"err-code"),
                message: blob(b"err-message"),
            }),
        ),
        op_envelope(
            1,
            11,
            OpKind::Unknown(editchain_core::op::UnknownOp {
                kind_discriminant: 0xFF,
                raw_bytes: blob(b"unknown-raw"),
            }),
        ),
        op_envelope(
            1,
            12,
            OpKind::GitCommit(Box::new(editchain_core::GitCommitEntity {
                repository: RepositoryId(0),
                object_format: editchain_core::GitObjectFormat::Sha1,
                oid: git_oid(),
                imported_record: None,
                availability: editchain_core::GitAvailability::ImportedOnly,
                tree: git_oid(),
                parents: Vec::new(),
                author: editchain_core::GitSignature {
                    name: blob(b"author-name"),
                    email: blob(b"author-email"),
                    when: 0,
                },
                committer: editchain_core::GitSignature {
                    name: blob(b"committer-name"),
                    email: blob(b"committer-email"),
                    when: 0,
                },
                authored_at: 0,
                committed_at: 0,
                message: blob(b"commit-message"),
                imported_refs: vec![blob(b"imported-ref")],
                live_refs: vec![blob(b"live-ref")],
                changed_paths: Vec::new(),
            })),
        ),
        op_envelope(
            1,
            13,
            OpKind::GitLink(editchain_core::GitLink {
                source: OpId::new(NodeId(1), 0, 0),
                target_repo: RepositoryId(0),
                target_oid: git_oid(),
                kind: editchain_core::GitLinkKind::Custom(blob(b"custom-link")),
            }),
        ),
        op_envelope(
            1,
            14,
            OpKind::File(editchain_core::op::FileOp {
                path: PathId(3),
                stage: editchain_core::op::FileStage::Observed,
                base: None,
                after: None,
                edit: editchain_core::op::FileEdit::Blob(file_blob_ref),
            }),
        ),
    ];

    let stats = hydrate_blob_payloads(&mut ops, &resolver);
    // 2 actor + 2 message + 3 tool + 2 command + 1 replace-bytes + 1 diff
    // + 2 reflection + 1 import + 1 note + 2 error + 1 unknown + 7 commit
    // (message/author/committer/refs) + 1 custom link.
    assert_eq!(stats.hydrated, 26);
    // The file edit blob has no inline representation: validated, preserved.
    assert_eq!(stats.verified_refs, 1);
    assert_eq!(stats.missing, 0);
    assert_eq!(stats.corrupt, 0);
    assert_eq!(stats.unresolved, 0);

    // Spot-check nested hydration: message content inline, and the blob
    // edit variant preserved (validated, never rewritten into a synthetic
    // ReplaceBytes range).
    let message_op = ops.iter().find(|op| op.id.seq == 2).unwrap();
    let message_hydrated = matches!(
        &message_op.kind,
        OpKind::Message(MessageOp { content, .. })
            if content == &Payload::Inline(b"msg-content".to_vec())
    );
    assert!(message_hydrated, "message content not hydrated inline");
    let blob_edit_op = ops.iter().find(|op| op.id.seq == 14).unwrap();
    let blob_edit_preserved = matches!(
        &blob_edit_op.kind,
        OpKind::File(file_op)
            if matches!(
                &file_op.edit,
                editchain_core::op::FileEdit::Blob(blob_ref) if *blob_ref == file_blob_ref
            )
    );
    assert!(
        blob_edit_preserved,
        "valid file blob edit must stay FileEdit::Blob"
    );
}

#[test]
fn projection_previews_defer_full_blob_until_details() {
    let tmp = tempfile::tempdir().unwrap();
    let full_text = "large payload ".repeat(2_000);
    let blob_ref = store_blob(tmp.path(), full_text.as_bytes());
    let source = op_envelope(
        9,
        1,
        OpKind::Message(MessageOp {
            content: Payload::Blob(blob_ref),
            content_type: Payload::Empty,
        }),
    );
    let resolver = BlobResolver::open(tmp.path()).unwrap();
    let (projection_ops, stats, incomplete) =
        projection_ops_with_previews(std::slice::from_ref(&source), &resolver);

    assert!(incomplete.contains(&source.id));
    assert_eq!(stats.hydrated, 0);
    assert_eq!(stats.previewed, 1);
    assert_eq!(stats.deferred, 1);
    assert!(matches!(
        &source.kind,
        OpKind::Message(MessageOp {
            content: Payload::Blob(found),
            ..
        }) if found == &blob_ref
    ));
    let preview_len = if let OpKind::Message(MessageOp {
        content: Payload::Inline(bytes),
        ..
    }) = &projection_ops[0].kind
    {
        String::from_utf8_lossy(bytes).chars().count()
    } else {
        0
    };
    assert_ne!(preview_len, 0, "expected inline projection preview");
    assert!(preview_len <= DISPLAY_PREVIEW_CHAR_LIMIT.saturating_add(1));

    let projection = HistoryProjection::from_preview_ops(projection_ops, &incomplete);
    let preview = row_content_dto(projection.nodes().first().unwrap().display_content());
    assert!(!preview.authored_summary.as_ref().unwrap().complete);
    assert!(preview.is_bounded());
    let mut source_op_index = HashMap::new();
    let _: Option<usize> = source_op_index.insert(source.id, 0);
    let ws = Workspace {
        projection,
        source_ops: vec![source.clone()],
        source_op_index,
        session_metadata: HashMap::new(),
        agent_file_changes: HashMap::new(),
        git_file_changes: HashMap::new(),
        source_op_locations: Vec::new(),
        blob_resolver: Some(resolver),
        repositories: RepositoryCatalog::default(),
        diagnostics: OpenDiagnostics::default(),
        root_path: PathBuf::new(),
        chain_path: tmp.path().to_path_buf(),
        source_identity: None,
        snapshot_id: unique_snapshot_id("memory"),
        backend: WorkspaceBackend::Projected,
        current_view: None,
    };
    let details = ws
        .node_details(Some(source.id.to_string()), None)
        .expect("details");
    assert_eq!(details.body, full_text);
}

#[test]
fn compact_import_record_preserves_bounded_semantic_subset_and_prefix_fallback() {
    // Inline records keep the envelope discriminators plus the bounded
    // semantic subset the classifier and outcome logic read.
    let echo = compact_import_record(
        br#"{"type":"event_msg","payload":{"type":"agent_message","message":"[external_agent_tool_call] {\"tool\":\"Bash\"}"}}"#,
    );
    let echo: serde_json::Value = serde_json::from_slice(&echo).unwrap();
    assert_eq!(echo["type"], "event_msg");
    assert_eq!(echo["payload"]["type"], "agent_message");
    assert_eq!(
        echo["payload"]["message"],
        "[external_agent_tool_call] {\"tool\":\"Bash\"}"
    );

    let item = compact_import_record(
        br#"{"type":"event_msg","payload":{"type":"item_completed","item":{"type":"CommandExecution","id":"call_9","exitCode":1,"status":"completed","errorMessage":"boom","stdout":"actual stdout","formatted_output":"formatted fallback"}}}"#,
    );
    let item: serde_json::Value = serde_json::from_slice(&item).unwrap();
    assert_eq!(item["payload"]["item"]["type"], "CommandExecution");
    assert_eq!(item["payload"]["item"]["exitCode"], 1);
    assert_eq!(item["payload"]["item"]["status"], "completed");
    assert_eq!(item["payload"]["item"]["errorMessage"], "boom");
    assert_eq!(item["payload"]["item"]["stdout"], "actual stdout");
    assert_eq!(
        item["payload"]["item"]["formatted_output"],
        "formatted fallback"
    );

    let response = compact_import_record(
        br#"{"type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"[external_agent_tool_result] done"}]}}"#,
    );
    let response: serde_json::Value = serde_json::from_slice(&response).unwrap();
    assert_eq!(response["payload"]["role"], "assistant");
    assert_eq!(
        response["payload"]["content"][0]["text"],
        "[external_agent_tool_result] done"
    );

    // Session provenance is the only `session_meta` payload copied into
    // the display projection. It survives both complete JSON and the
    // prefix-only path used for large blob-backed records.
    let session = compact_import_record(
        br#"{"type":"session_meta","payload":{"model_provider":"sglang_dsv4","agent_nickname":"Harvey","base_instructions":"large private field"}}"#,
    );
    let session: serde_json::Value = serde_json::from_slice(&session).unwrap();
    assert_eq!(session["payload"]["model_provider"], "sglang_dsv4");
    assert_eq!(session["payload"]["agent_nickname"], "Harvey");
    assert!(session["payload"].get("base_instructions").is_none());
    let session_op = op_envelope(
        90,
        1,
        OpKind::Import(ImportOp {
            raw_ref: Payload::Inline(serde_json::to_vec(&session).unwrap()),
            raw_hash: None,
        }),
    );
    let metadata = session_metadata_index(std::slice::from_ref(&session_op));
    let metadata = metadata.get("session:10").expect("session metadata");
    assert_eq!(metadata.session_title, None);
    assert_eq!(metadata.model_provider.as_deref(), Some("sglang_dsv4"));
    assert_eq!(metadata.agent_nickname.as_deref(), Some("Harvey"));

    // Claude's explicit title and the portable Codex title record retain
    // only their bounded display fields. A duplicate Claude agent-name is
    // removed from the final metadata, while a distinct named subagent is
    // kept for `title · nickname` rendering.
    let custom_title =
        compact_import_record(br#"{"type":"custom-title","customTitle":"q0","private":"discard"}"#);
    let custom_title: serde_json::Value = serde_json::from_slice(&custom_title).unwrap();
    assert_eq!(custom_title["customTitle"], "q0");
    assert!(custom_title.get("private").is_none());
    let codex_title = compact_import_record(
        br#"{"type":"session_title","provider":"codex","title":"r8","updated_at":"discard"}"#,
    );
    let codex_title: serde_json::Value = serde_json::from_slice(&codex_title).unwrap();
    assert_eq!(codex_title["title"], "r8");
    assert!(codex_title.get("updated_at").is_none());

    let metadata = session_metadata_index(&[
        op_envelope(
            91,
            1,
            OpKind::Import(ImportOp {
                raw_ref: Payload::Inline(br#"{"type":"ai-title","aiTitle":"generated"}"#.to_vec()),
                raw_hash: None,
            }),
        ),
        op_envelope(
            91,
            2,
            OpKind::Import(ImportOp {
                raw_ref: Payload::Inline(serde_json::to_vec(&custom_title).unwrap()),
                raw_hash: None,
            }),
        ),
        op_envelope(
            91,
            3,
            OpKind::Import(ImportOp {
                raw_ref: Payload::Inline(br#"{"type":"agent-name","agentName":"q0"}"#.to_vec()),
                raw_hash: None,
            }),
        ),
    ]);
    let metadata = metadata.get("session:10").expect("Claude title metadata");
    assert_eq!(metadata.session_title.as_deref(), Some("q0"));
    assert_eq!(metadata.agent_nickname, None);

    let metadata = session_metadata_index(&[
        op_envelope(
            92,
            1,
            OpKind::Import(ImportOp {
                raw_ref: Payload::Inline(serde_json::to_vec(&codex_title).unwrap()),
                raw_hash: None,
            }),
        ),
        op_envelope(
            92,
            2,
            OpKind::Import(ImportOp {
                raw_ref: Payload::Inline(
                    br#"{"type":"session_meta","payload":{"agent_nickname":"Tesla"}}"#.to_vec(),
                ),
                raw_hash: None,
            }),
        ),
    ]);
    let metadata = metadata.get("session:10").expect("Codex title metadata");
    assert_eq!(metadata.session_title.as_deref(), Some("r8"));
    assert_eq!(metadata.agent_nickname.as_deref(), Some("Tesla"));

    let session_prefix = compact_import_record(
        br#"{"type":"session_meta","payload":{"model_provider":"sglang_dsv4","agent_nickname":"Harvey","base_instructions":"unterminated"#,
    );
    let session_prefix: serde_json::Value = serde_json::from_slice(&session_prefix).unwrap();
    assert_eq!(session_prefix["payload"]["model_provider"], "sglang_dsv4");
    assert_eq!(session_prefix["payload"]["agent_nickname"], "Harvey");

    // Token-accounting imports retain only the compact totals required for
    // numeric subtitles and legacy schema recognition.
    let usage = compact_import_record(
        br#"{"type":"token_usage_record","payload":{"thread_id":"0195cda5-433d-7f9a-9d7b-a9f15b60c2e2","turn_id":"turn-1","session_id":"0195cda5-433d-7f9a-9d7b-a9f15b60c2e2","root_turn_id":"turn-1","response_id":"response-1","usage":{"total_tokens":13},"turn_token_usage":{"total_tokens":13},"thread_token_usage":{"total_tokens":13}}}"#,
    );
    let usage: serde_json::Value = serde_json::from_slice(&usage).unwrap();
    assert_eq!(usage["type"], "token_usage_record");
    assert_eq!(usage["payload"]["turn_id"], "turn-1");
    for field in ["usage", "turn_token_usage", "thread_token_usage"] {
        assert_eq!(
            usage["payload"][field],
            serde_json::json!({ "total_tokens": 13 })
        );
    }

    let count = compact_import_record(
        br#"{"type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":33772,"total_tokens":34652},"last_token_usage":{"input_tokens":17187,"total_tokens":17502},"model_context_window":258400},"rate_limits":{"primary":{"used_percent":2.0}}}}"#,
    );
    let count: serde_json::Value = serde_json::from_slice(&count).unwrap();
    assert_eq!(
        count["payload"]["info"],
        serde_json::json!({
            "total_token_usage": { "total_tokens": 34_652 },
            "last_token_usage": { "total_tokens": 17_502 },
            "model_context_window": 258_400,
        })
    );
    assert!(count["payload"].get("rate_limits").is_none());

    // Prefix-only parsing keeps the same small accounting subset when a
    // later field runs past the bounded preview.
    let count_prefix = compact_import_record(
        br#"{"type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"total_tokens":34652},"last_token_usage":{"total_tokens":17502},"model_context_window":258400},"rate_limits":{"private":"unterminated"#,
    );
    let count_prefix: serde_json::Value = serde_json::from_slice(&count_prefix).unwrap();
    assert_eq!(count_prefix["payload"]["info"], count["payload"]["info"]);

    // Large outputs are bounded, never copied into the projection.
    let huge = format!(
        r#"{{"type":"response_item","payload":{{"type":"function_call_output","output":"{}"}}}}"#,
        "y".repeat(200_000),
    );
    let compacted = compact_import_record(huge.as_bytes());
    let compacted: serde_json::Value = serde_json::from_slice(&compacted).unwrap();
    let output = compacted["payload"]["output"].as_str().unwrap();
    assert!(output.chars().count() <= DISPLAY_PREVIEW_CHAR_LIMIT.saturating_add(1));

    // Truncated blob preview: the prefix fallback still recovers the
    // external-tool marker near the envelope start.
    let truncated = format!(
        r#"{{"type":"event_msg","payload":{{"type":"agent_message","message":"[external_agent_tool_result] {}"}}"#,
        "z".repeat(200_000),
    );
    let compacted = compact_import_record(truncated.as_bytes());
    let compacted: serde_json::Value = serde_json::from_slice(&compacted).unwrap();
    assert_eq!(compacted["type"], "event_msg");
    assert_eq!(compacted["payload"]["type"], "agent_message");
    assert!(
        compacted["payload"]["message"]
            .as_str()
            .unwrap_or_default()
            .starts_with("[external_agent_tool_result]"),
        "marker recovered from truncated blob preview"
    );
}

#[test]
fn compact_import_record_preserves_exact_claude_response_identity() {
    let complete = compact_import_record(
        br#"{"parentUuid":"parent-1","type":"assistant","message":{"id":"msg-response-1","content":[{"type":"tool_use","id":"call-1"}]}}"#,
    );
    let complete: serde_json::Value = serde_json::from_slice(&complete).unwrap();
    assert_eq!(complete["type"], "assistant");
    assert_eq!(complete["message"]["id"], "msg-response-1");

    // Blob previews can end before the large content body closes. Identity
    // is near the envelope start and remains exact in that prefix path.
    let prefix = compact_import_record(
        br#"{"parentUuid":"parent-1","type":"assistant","message":{"id":"msg-response-1","content":[{"type":"tool_use","input":"unterminated"#,
    );
    let prefix: serde_json::Value = serde_json::from_slice(&prefix).unwrap();
    assert_eq!(prefix["message"]["id"], "msg-response-1");
}

#[test]
fn compact_import_record_preserves_claude_interruption_evidence() {
    let complete = compact_import_record(
        br#"{"type":"user","message":{"role":"user","content":[{"type":"text","text":"[Request interrupted by user]"}]},"interruptedMessageId":"msg-cancelled"}"#,
    );
    let complete: serde_json::Value = serde_json::from_slice(&complete).unwrap();
    assert_eq!(complete["type"], "user");
    assert_eq!(complete["text"], "[Request interrupted by user]");
    assert_eq!(complete["interruptedMessageId"], "msg-cancelled");

    // An unclosed blob preview can still recover the exact typed marker
    // even when the trailing interruption id has not arrived yet.
    let prefix = compact_import_record(
        br#"{"type":"user","message":{"role":"user","content":[{"type":"text","text":"[Request interrupted by user for tool use]"},{"type":"tool_result","content":"unterminated"#,
    );
    let prefix: serde_json::Value = serde_json::from_slice(&prefix).unwrap();
    assert_eq!(prefix["text"], "[Request interrupted by user for tool use]");
}

#[test]
fn compact_import_record_preserves_canonical_codex_exec_outcome_header() {
    let raw = format!(
        r#"{{"type":"response_item","payload":{{"type":"custom_tool_call_output","output":[{{"type":"input_text","text":"Script failed\nWall time 0.0 seconds\nOutput:\n"}},{{"type":"input_text","text":"{}"}}]}}}}"#,
        "x".repeat(DISPLAY_PREVIEW_READ_LIMIT.saturating_mul(2)),
    );

    let compacted = compact_import_record(raw.as_bytes());
    let compacted: serde_json::Value = serde_json::from_slice(&compacted).unwrap();
    assert_eq!(
        compacted["payload"]["output"][0]["text"],
        "Script failed\nWall time 0.0 seconds\nOutput:"
    );

    // A truncated blob preview takes the prefix parser but must retain the
    // identical status evidence near the start of the envelope.
    let compacted = compact_import_record(&raw.as_bytes()[..DISPLAY_PREVIEW_READ_LIMIT]);
    let compacted: serde_json::Value = serde_json::from_slice(&compacted).unwrap();
    assert_eq!(
        compacted["payload"]["output"][0]["text"],
        "Script failed\nWall time 0.0 seconds\nOutput:"
    );
}

#[test]
fn compact_import_record_preserves_structured_tool_payload_carriers() {
    // Object/array tool-payload carriers (arguments/input/parameters)
    // keep a bounded structural/content signal so childless tool-like
    // rows are not compacted into empty transport. Large nested strings
    // stay bounded.
    let call = compact_import_record(
        br#"{"type":"response_item","payload":{"type":"function_call","name":"WebSearch","arguments":{"query":"editchain docs"}}}"#,
    );
    let call: serde_json::Value = serde_json::from_slice(&call).unwrap();
    assert_eq!(call["payload"]["arguments"]["query"], "editchain docs");

    let carriers = compact_import_record(
        br#"{"type":"response_item","payload":{"type":"function_call","input":{"path":"/tmp/x"},"parameters":{"depth":2}}}"#,
    );
    let carriers: serde_json::Value = serde_json::from_slice(&carriers).unwrap();
    assert_eq!(carriers["payload"]["input"]["path"], "/tmp/x");
    assert_eq!(carriers["payload"]["parameters"]["depth"], 2);

    // Nested strings inside a structured carrier are bounded like any
    // other display field.
    let huge = format!(
        r#"{{"type":"response_item","payload":{{"type":"function_call","arguments":{{"query":"{}"}}}}}}"#,
        "y".repeat(200_000),
    );
    let compacted = compact_import_record(huge.as_bytes());
    let compacted: serde_json::Value = serde_json::from_slice(&compacted).unwrap();
    let query = compacted["payload"]["arguments"]["query"].as_str().unwrap();
    assert!(query.chars().count() <= DISPLAY_PREVIEW_CHAR_LIMIT.saturating_add(1));

    // Truncated blob preview: the prefix fallback still recovers an
    // object arguments carrier near the envelope start.
    let truncated = format!(
        r#"{{"type":"response_item","payload":{{"type":"function_call","arguments":{{"query":"docs"}},"output":"{}"}}"#,
        "z".repeat(200_000),
    );
    let compacted = compact_import_record(truncated.as_bytes());
    let compacted: serde_json::Value = serde_json::from_slice(&compacted).unwrap();
    assert_eq!(compacted["type"], "response_item");
    assert_eq!(compacted["payload"]["type"], "function_call");
    assert_eq!(compacted["payload"]["arguments"]["query"], "docs");
}

/// Total retained object keys plus array items in a compacted carrier.
fn count_entries(value: &serde_json::Value) -> usize {
    match value {
        serde_json::Value::Object(map) => map
            .iter()
            .map(|(_, child)| count_entries(child).saturating_add(1))
            .sum(),
        serde_json::Value::Array(items) => items
            .iter()
            .map(|child| count_entries(child).saturating_add(1))
            .sum(),
        serde_json::Value::Null
        | serde_json::Value::Bool(_)
        | serde_json::Value::Number(_)
        | serde_json::Value::String(_) => 0,
    }
}

/// Maximum container nesting depth of a value (containers count 1, leaves
/// count 0).
fn depth_of(value: &serde_json::Value) -> usize {
    match value {
        serde_json::Value::Object(map) => map
            .values()
            .map(depth_of)
            .max()
            .map_or(1, |depth| depth.saturating_add(1)),
        serde_json::Value::Array(items) => items
            .iter()
            .map(depth_of)
            .max()
            .map_or(1, |depth| depth.saturating_add(1)),
        serde_json::Value::Null
        | serde_json::Value::Bool(_)
        | serde_json::Value::Number(_)
        | serde_json::Value::String(_) => 0,
    }
}

#[test]
fn compact_structured_globally_bounds_retained_entries() {
    // Two wide sibling objects share one total budget: the retained
    // output never exceeds the total entry limit across the whole carrier
    // (the old per-depth cap would retain 64 entries at every level).
    let value = serde_json::json!({
        "first": (0..128u32)
            .map(|i: u32| (format!("a{i}"), serde_json::Value::from(i)))
            .collect::<serde_json::Map<String, serde_json::Value>>(),
        "second": (0..128u32)
            .map(|i: u32| (format!("b{i}"), serde_json::Value::from(i)))
            .collect::<serde_json::Map<String, serde_json::Value>>(),
    });
    let compacted = compact_structured(&value);
    assert_eq!(
        count_entries(&compacted),
        STRUCTURED_CARRIER_TOTAL_ENTRY_LIMIT,
        "the shared budget must cap the whole carrier"
    );
    let compacted_object = compacted.as_object().unwrap();
    assert!(
        compacted_object.contains_key("first"),
        "the leading sibling keeps the budget"
    );
    assert!(
        !compacted_object.contains_key("second"),
        "the trailing sibling is dropped once the shared budget is spent"
    );
}

#[test]
fn compact_structured_bounds_oversized_multibyte_keys() {
    // Up to STRUCTURED_CARRIER_TOTAL_ENTRY_LIMIT object keys are retained
    // verbatim by key.clone(); oversized multibyte keys would otherwise
    // keep unbounded raw payload despite the boundedness claim. Keys must
    // be cut to the display char limit while staying non-empty.
    let huge_key = "界".repeat(DISPLAY_PREVIEW_CHAR_LIMIT.saturating_mul(8));
    let mut map = serde_json::Map::new();
    for i in 0..STRUCTURED_CARRIER_TOTAL_ENTRY_LIMIT {
        // Distinct leading discriminators keep every truncated bounded key
        // unique, so the budget slots are retained rather than collapsed
        // into one colliding entry.
        drop(map.insert(format!("{i}{huge_key}"), serde_json::Value::from(i)));
    }
    let compacted = compact_structured(&serde_json::Value::Object(map));
    let serialized = serde_json::to_string(&compacted).unwrap();
    assert!(
        !serialized.is_empty(),
        "the compacted carrier must keep a non-empty signal"
    );
    assert_eq!(
        compacted.as_object().unwrap().len(),
        STRUCTURED_CARRIER_TOTAL_ENTRY_LIMIT,
        "every budget slot stays retained with a bounded key"
    );
    // Fixed ceiling: each retained key holds at most
    // DISPLAY_PREVIEW_CHAR_LIMIT chars, worst-case 6 JSON-escaped bytes
    // per char, plus quotes; values and punctuation add a tiny fixed
    // amount. Unbounded keys would blow far past this ceiling.
    let ceiling = STRUCTURED_CARRIER_TOTAL_ENTRY_LIMIT
        .saturating_mul(
            DISPLAY_PREVIEW_CHAR_LIMIT
                .saturating_mul(6)
                .saturating_add(8),
        )
        .saturating_add(256);
    assert!(
        serialized.len() < ceiling,
        "serialized compact output ({} bytes) must stay below the fixed \
         ceiling ({ceiling} bytes)",
        serialized.len()
    );
}

#[test]
fn compact_structured_keeps_first_key_when_truncation_collides() {
    // Two distinct oversized keys sharing one 1024-char prefix truncate to
    // the same bounded key text; the retained object must deterministically
    // keep the first original key instead of silently overwriting it.
    let shared_prefix = "界".repeat(DISPLAY_PREVIEW_CHAR_LIMIT.saturating_mul(8));
    let value = serde_json::json!({
        // Lexicographically first original key: keeps value 1 on collision
        // with first-wins handling.
        format!("{shared_prefix}a"): 1,
        format!("{shared_prefix}b"): 2,
    });
    let compacted = compact_structured(&value);
    let compacted_object = compacted.as_object().unwrap();
    assert_eq!(
        compacted_object.len(),
        1,
        "truncation collision must not produce two identical retained keys"
    );
    let bounded_key = compact_text(&format!("{shared_prefix}a"));
    assert_eq!(
        compacted_object.get(&bounded_key),
        Some(&serde_json::Value::from(1)),
        "the lexicographically first original key wins deterministically"
    );
}

#[test]
fn compact_structured_prunes_nesting_beyond_max_depth() {
    // Pathological nesting is pruned at STRUCTURED_CARRIER_MAX_DEPTH
    // rather than recursing without bound, through the direct compactor
    // and the full compact_import_record path.
    let mut deep = serde_json::Value::Bool(true);
    for _ in 0..(STRUCTURED_CARRIER_MAX_DEPTH.saturating_mul(2)) {
        deep = serde_json::Value::Array(vec![deep]);
    }
    assert!(
        depth_of(&deep) > STRUCTURED_CARRIER_MAX_DEPTH,
        "input must exceed the depth cap"
    );
    assert!(depth_of(&compact_structured(&deep)) <= STRUCTURED_CARRIER_MAX_DEPTH.saturating_add(1));

    let mut inner = String::from("1");
    for _ in 0..(STRUCTURED_CARRIER_MAX_DEPTH.saturating_mul(2)) {
        inner = format!("[{inner}]");
    }
    let raw = format!(
        r#"{{"type":"response_item","payload":{{"type":"function_call","arguments":{inner}}}}}"#
    );
    let compacted = compact_import_record(raw.as_bytes());
    let compacted: serde_json::Value = serde_json::from_slice(&compacted).unwrap();
    assert!(
        depth_of(&compacted["payload"]["arguments"])
            <= STRUCTURED_CARRIER_MAX_DEPTH.saturating_add(1),
        "deep arguments carrier must be pruned through the import path"
    );
}

#[test]
fn compact_import_record_keeps_sentinel_for_unclosed_preview_carrier() {
    // A large blob-backed function call whose arguments object starts
    // inside the bounded preview read window but closes after the cutoff
    // must keep a tiny non-empty signal: it is a genuine tool payload, not
    // empty transport.
    let raw = format!(
        r#"{{"type":"response_item","payload":{{"type":"function_call","name":"Bash","arguments":{{"command":"{}","cwd":"/tmp"}}}}}}"#,
        "x".repeat(200_000),
    );
    let bytes = raw.as_bytes();
    assert!(bytes.len() > DISPLAY_PREVIEW_READ_LIMIT);
    let compacted = compact_import_record(&bytes[..DISPLAY_PREVIEW_READ_LIMIT]);
    let compacted: serde_json::Value = serde_json::from_slice(&compacted).unwrap();
    assert_eq!(compacted["type"], "response_item");
    assert_eq!(compacted["payload"]["type"], "function_call");
    assert_eq!(
        compacted["payload"]["arguments"]["truncated"], true,
        "started-but-incomplete carrier keeps the sentinel"
    );
}

#[test]
fn compact_import_record_keeps_complete_empty_carriers_silent_in_previews() {
    // A complete empty object/array carrier closes inside the preview and
    // must stay silent even when the surrounding record is truncated.
    let cases = [
        r#"{"type":"response_item","payload":{"type":"function_call","arguments":{}},"output":""#,
        r#"{"type":"response_item","payload":{"type":"function_call","parameters":[]},"output":""#,
    ];
    for prefix in cases {
        let full = format!("{prefix}{}\"}}}}", "z".repeat(200_000));
        let bytes = full.as_bytes();
        assert!(bytes.len() > DISPLAY_PREVIEW_READ_LIMIT);
        let compacted = compact_import_record(&bytes[..DISPLAY_PREVIEW_READ_LIMIT]);
        let compacted: serde_json::Value = serde_json::from_slice(&compacted).unwrap();
        assert!(
            compacted["payload"].get("arguments").is_none(),
            "complete empty arguments carrier must stay silent: {prefix}"
        );
        assert!(
            compacted["payload"].get("parameters").is_none(),
            "complete empty parameters carrier must stay silent: {prefix}"
        );
    }
}

#[test]
fn compact_import_record_preserves_scalar_tool_payload_carriers() {
    // Non-empty string and scalar bool/number carriers (arguments/input/
    // parameters) keep a bounded signal; null/empty strings stay silent.
    let call = compact_import_record(
        br#"{"type":"response_item","payload":{"type":"function_call","name":"Bash","arguments":"ls -la"}}"#,
    );
    let call: serde_json::Value = serde_json::from_slice(&call).unwrap();
    assert_eq!(call["payload"]["arguments"], "ls -la");

    let carriers = compact_import_record(
        br#"{"type":"response_item","payload":{"type":"function_call","name":"Read","input":"/tmp/x","parameters":true}}"#,
    );
    let carriers: serde_json::Value = serde_json::from_slice(&carriers).unwrap();
    assert_eq!(carriers["payload"]["input"], "/tmp/x");
    assert_eq!(carriers["payload"]["parameters"], true);

    let number = compact_import_record(
        br#"{"type":"response_item","payload":{"type":"function_call","name":"Tool","parameters":7}}"#,
    );
    let number: serde_json::Value = serde_json::from_slice(&number).unwrap();
    assert_eq!(number["payload"]["parameters"], 7);

    let empty = compact_import_record(
        br#"{"type":"response_item","payload":{"type":"function_call","arguments":"","parameters":null}}"#,
    );
    let empty: serde_json::Value = serde_json::from_slice(&empty).unwrap();
    assert!(empty["payload"].get("arguments").is_none());
    assert!(empty["payload"].get("parameters").is_none());
}

#[test]
fn compact_import_record_prefix_fallback_recovers_scalar_carriers() {
    // Truncated blob preview: string/bool input and parameters carriers
    // near the envelope start survive the prefix fallback.
    let raw = format!(
        r#"{{"type":"response_item","payload":{{"type":"function_call","name":"Read","input":"/tmp/x","parameters":true,"output":"{}"}}}}"#,
        "z".repeat(200_000),
    );
    let bytes = raw.as_bytes();
    assert!(bytes.len() > DISPLAY_PREVIEW_READ_LIMIT);
    let compacted = compact_import_record(&bytes[..DISPLAY_PREVIEW_READ_LIMIT]);
    let compacted: serde_json::Value = serde_json::from_slice(&compacted).unwrap();
    assert_eq!(compacted["payload"]["input"], "/tmp/x");
    assert_eq!(compacted["payload"]["parameters"], true);
}

#[test]
fn compact_import_record_preserves_bounded_reasoning_summary() {
    // Reasoning response items keep the first non-empty summary text so
    // the compact service record can still yield a legible row label.
    let item = compact_import_record(
        br#"{"type":"response_item","payload":{"type":"reasoning","summary":[{"type":"summary_text","text":"Audit the tree layout"},{"type":"summary_text","text":"ignored"}],"content":[{"type":"reasoning","text":"ignored"}]}}"#,
    );
    let item: serde_json::Value = serde_json::from_slice(&item).unwrap();
    assert_eq!(item["payload"]["summary"][0]["type"], "summary_text");
    assert_eq!(
        item["payload"]["summary"][0]["text"],
        "Audit the tree layout"
    );

    // Empty/whitespace-only first summary blocks fall through to a later
    // text-bearing block.
    let skipped = compact_import_record(
        br#"{"type":"response_item","payload":{"type":"reasoning","summary":[{"type":"summary_text","text":"  "},{"type":"summary_text","text":"second block"}]}}"#,
    );
    let skipped: serde_json::Value = serde_json::from_slice(&skipped).unwrap();
    assert_eq!(skipped["payload"]["summary"][0]["text"], "second block");

    // An empty summary array keeps no summary field at all.
    let empty = compact_import_record(
        br#"{"type":"response_item","payload":{"type":"reasoning","summary":[]}}"#,
    );
    let empty: serde_json::Value = serde_json::from_slice(&empty).unwrap();
    assert!(empty["payload"].get("summary").is_none());

    // Large multibyte summary text stays bounded to the display limit.
    let huge = format!(
        r#"{{"type":"response_item","payload":{{"type":"reasoning","summary":[{{"type":"summary_text","text":"{}"}}]}}}}"#,
        "界".repeat(200_000),
    );
    let compacted = compact_import_record(huge.as_bytes());
    let compacted: serde_json::Value = serde_json::from_slice(&compacted).unwrap();
    let text = compacted["payload"]["summary"][0]["text"].as_str().unwrap();
    assert!(text.chars().count() <= DISPLAY_PREVIEW_CHAR_LIMIT.saturating_add(1));
    assert!(text.ends_with('…'), "cut summary keeps the ellipsis marker");
}

#[test]
fn compact_import_record_prefix_fallback_recovers_reasoning_summary() {
    // Truncated blob preview: the first summary text sits near the
    // envelope start and survives the prefix fallback even when a huge
    // trailing output never closes inside the read limit.
    let raw = format!(
        r#"{{"type":"response_item","payload":{{"type":"reasoning","summary":[{{"type":"summary_text","text":"Recovered from prefix"}}],"output":"{}"}}"#,
        "z".repeat(200_000),
    );
    let bytes = raw.as_bytes();
    assert!(bytes.len() > DISPLAY_PREVIEW_READ_LIMIT);
    let compacted = compact_import_record(&bytes[..DISPLAY_PREVIEW_READ_LIMIT]);
    let compacted: serde_json::Value = serde_json::from_slice(&compacted).unwrap();
    assert_eq!(compacted["type"], "response_item");
    assert_eq!(compacted["payload"]["type"], "reasoning");
    assert_eq!(
        compacted["payload"]["summary"][0]["text"],
        "Recovered from prefix"
    );
}

#[test]
fn json_string_field_preview_skips_escaped_quotes() {
    let raw = r#"{"message":"say \"hello\" now","tail":true}"#;
    let (value, cut_by_read_limit) =
        json_string_field_preview(raw, "message", 0).expect("message preview");
    assert_eq!(value, r#"say \"hello\" now"#);
    assert!(!cut_by_read_limit);
}

#[test]
fn json_string_field_preview_accepts_even_backslash_run_before_close() {
    let mut raw = String::from(r#"{"message":"path"#);
    raw.push('\\');
    raw.push('\\');
    raw.push('"');
    raw.push('}');

    let (value, cut_by_read_limit) =
        json_string_field_preview(&raw, "message", 0).expect("message preview");
    let mut expected = String::from("path");
    expected.push('\\');
    expected.push('\\');
    assert_eq!(value, expected);
    assert!(!cut_by_read_limit);
}

#[test]
fn compact_import_record_prefix_fallback_decodes_string_escapes() {
    // The message closes inside the preview, but a later output field is
    // cut so the record takes the prefix-recovery path. Its display text
    // must match serde's fully parsed path, not expose raw `\n` / `\"`.
    let raw = br#"{"type":"event_msg","payload":{"type":"agent_message","message":"line one\n\"quoted\"","output":"unfinished"#;
    let compacted = compact_import_record(raw);
    let compacted: serde_json::Value = serde_json::from_slice(&compacted).unwrap();
    assert_eq!(compacted["payload"]["message"], "line one\n\"quoted\"");
    assert!(compacted["payload"].get("echo_text_truncated").is_none());
}

#[test]
fn compact_import_record_marks_preview_ending_at_escaped_quote_truncated() {
    let mut raw =
        String::from(r#"{"type":"event_msg","payload":{"type":"agent_message","message":"prefix "#);
    raw.push('\\');
    raw.push('"');

    let (_, cut_by_read_limit) =
        json_string_field_preview(&raw, "message", 0).expect("message preview");
    assert!(cut_by_read_limit, "an escaped quote is not a closing quote");

    let compacted = compact_import_record(raw.as_bytes());
    let compacted: serde_json::Value = serde_json::from_slice(&compacted).unwrap();
    assert_eq!(compacted["payload"]["message"], "prefix \"");
    assert_eq!(compacted["payload"]["echo_text_truncated"], true);
}

#[test]
fn compact_import_record_marks_truncated_echo_text() {
    // The classifier must be told explicitly when a service-compacted echo
    // message text was truncated, instead of guessing from an ellipsis:
    // two distinct long texts sharing a display-preview prefix would
    // otherwise compare equal after compaction and be conflated by exact
    // duplicate pairing.

    // Inline event_msg agent_message over the display budget: the message
    // keeps the bounded prefix with an ellipsis and the flag is set.
    let prefix = "shared-prefix-".repeat(200);
    let long_event = format!(
        r#"{{"type":"event_msg","payload":{{"type":"agent_message","message":"{prefix}TAIL-A"}}}}"#,
    );
    let compacted = compact_import_record(long_event.as_bytes());
    let compacted: serde_json::Value = serde_json::from_slice(&compacted).unwrap();
    assert_eq!(compacted["payload"]["echo_text_truncated"], true);
    let message = compacted["payload"]["message"].as_str().unwrap();
    assert!(message.starts_with("shared-prefix-"));
    assert!(
        message.ends_with('…'),
        "cut message keeps the ellipsis marker"
    );

    // A short untruncated message never sets the flag.
    let short_event = compact_import_record(
        br#"{"type":"event_msg","payload":{"type":"agent_message","message":"exact narrative"}}"#,
    );
    let short_event: serde_json::Value = serde_json::from_slice(&short_event).unwrap();
    assert!(short_event["payload"].get("echo_text_truncated").is_none());
    assert_eq!(short_event["payload"]["message"], "exact narrative");

    // Inline response_item assistant message: a long first content text
    // sets the flag; a short one does not.
    let long_response = format!(
        r#"{{"type":"response_item","payload":{{"type":"message","role":"assistant","content":[{{"type":"output_text","text":"{prefix}TAIL-B"}}]}}}}"#,
    );
    let compacted = compact_import_record(long_response.as_bytes());
    let compacted: serde_json::Value = serde_json::from_slice(&compacted).unwrap();
    assert_eq!(compacted["payload"]["echo_text_truncated"], true);
    let text = compacted["payload"]["content"][0]["text"].as_str().unwrap();
    assert!(text.ends_with('…'));

    let short_response = compact_import_record(
        br#"{"type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"exact narrative"}]}}"#,
    );
    let short_response: serde_json::Value = serde_json::from_slice(&short_response).unwrap();
    assert!(short_response["payload"]
        .get("echo_text_truncated")
        .is_none());

    // Blob-backed record over the read budget: the preview window ends
    // inside the huge message value, so the prefix fallback recovers only
    // the bounded prefix (with an ellipsis) and sets the flag.
    let blob_raw = format!(
        r#"{{"type":"event_msg","payload":{{"type":"agent_message","message":"{}"}}}}"#,
        "k".repeat(200_000),
    );
    let bytes = blob_raw.as_bytes();
    assert!(bytes.len() > DISPLAY_PREVIEW_READ_LIMIT);
    let compacted = compact_import_record(&bytes[..DISPLAY_PREVIEW_READ_LIMIT]);
    let compacted: serde_json::Value = serde_json::from_slice(&compacted).unwrap();
    assert_eq!(
        compacted["payload"]["echo_text_truncated"], true,
        "read-limit cut blob preview sets the flag"
    );
    assert!(compacted["payload"]["message"]
        .as_str()
        .unwrap()
        .ends_with('…'));

    // Read-limit cut WITHOUT an ellipsis: a record whose `message` value
    // starts late enough in the preview window that the 4096-byte boundary
    // lands inside the value after fewer than the character-budget chars
    // (here ~622), so `compact_text` appends no ellipsis. Only the flag
    // tells the classifier the text is known-truncated — the ellipsis
    // heuristic alone would miss it.
    let padded_event = format!(
        r#"{{"type":"event_msg","payload":{{"type":"agent_message","pad":"{}","message":"{}"}}}}"#,
        "p".repeat(3400),
        "k".repeat(5000),
    );
    let bytes = padded_event.as_bytes();
    let message_needle = r#""message":""#;
    let message_value_start = bytes
        .windows(message_needle.len())
        .position(|window| window == message_needle.as_bytes())
        .expect("message field")
        .saturating_add(message_needle.len());
    assert_eq!(
        message_value_start, 3474,
        "layout drives the no-ellipsis cut"
    );
    assert!(bytes.len() > DISPLAY_PREVIEW_READ_LIMIT);
    assert!(message_value_start < DISPLAY_PREVIEW_READ_LIMIT);
    let compacted = compact_import_record(&bytes[..DISPLAY_PREVIEW_READ_LIMIT]);
    let compacted: serde_json::Value = serde_json::from_slice(&compacted).unwrap();
    let message = compacted["payload"]["message"].as_str().unwrap();
    assert_eq!(message.chars().count(), 622);
    assert!(
        !message.ends_with('…'),
        "short remainder inside the window gets no appended ellipsis"
    );
    assert_eq!(
        compacted["payload"]["echo_text_truncated"], true,
        "read-limit cut without ellipsis still sets the flag"
    );

    // Same no-ellipsis read-limit cut for a response_item's first content
    // text value (window ends ~675 chars into the value, no ellipsis).
    let padded_response = format!(
        r#"{{"type":"response_item","payload":{{"type":"message","role":"assistant","pad":"{}","content":[{{"type":"output_text","text":"{}"}}]}}}}"#,
        "p".repeat(3300),
        "k".repeat(6000),
    );
    let bytes = padded_response.as_bytes();
    let text_needle = r#""text":""#;
    let text_value_start = bytes
        .windows(text_needle.len())
        .position(|window| window == text_needle.as_bytes())
        .expect("content text field")
        .saturating_add(text_needle.len());
    assert_eq!(text_value_start, 3421, "layout drives the no-ellipsis cut");
    assert!(bytes.len() > DISPLAY_PREVIEW_READ_LIMIT);
    assert!(text_value_start < DISPLAY_PREVIEW_READ_LIMIT);
    let compacted = compact_import_record(&bytes[..DISPLAY_PREVIEW_READ_LIMIT]);
    let compacted: serde_json::Value = serde_json::from_slice(&compacted).unwrap();
    let text = compacted["payload"]["content"][0]["text"].as_str().unwrap();
    assert_eq!(text.chars().count(), 675);
    assert!(!text.ends_with('…'));
    assert_eq!(compacted["payload"]["echo_text_truncated"], true);
}

#[test]
fn compact_import_record_prefix_fallback_recovers_payload_exit_code() {
    // A truncated blob preview may cut the record before its
    // `payload.item` block entirely; payload-level exitCode/status/
    // errorMessage outcome evidence near the envelope start must still
    // survive, exactly as the full-parse path preserves it.
    let failure = format!(
        r#"{{"type":"response_item","payload":{{"type":"function_call_output","exitCode":1,"errorMessage":"boom","status":"failed","output":"{}"}}}}"#,
        "z".repeat(200_000),
    );
    let bytes = failure.as_bytes();
    assert!(bytes.len() > DISPLAY_PREVIEW_READ_LIMIT);
    let compacted = compact_import_record(&bytes[..DISPLAY_PREVIEW_READ_LIMIT]);
    let compacted: serde_json::Value = serde_json::from_slice(&compacted).unwrap();
    assert_eq!(compacted["payload"]["type"], "function_call_output");
    assert_eq!(compacted["payload"]["exitCode"], 1);
    assert_eq!(compacted["payload"]["errorMessage"], "boom");
    assert_eq!(compacted["payload"]["status"], "failed");

    // Success evidence: exitCode 0 is recovered the same way.
    let success = format!(
        r#"{{"type":"response_item","payload":{{"type":"function_call_output","exitCode":0,"output":"{}"}}}}"#,
        "y".repeat(200_000),
    );
    let bytes = success.as_bytes();
    assert!(bytes.len() > DISPLAY_PREVIEW_READ_LIMIT);
    let compacted = compact_import_record(&bytes[..DISPLAY_PREVIEW_READ_LIMIT]);
    let compacted: serde_json::Value = serde_json::from_slice(&compacted).unwrap();
    assert_eq!(compacted["payload"]["exitCode"], 0);

    // The nested item-level copy is still recovered (unchanged behavior),
    // including its bounded command output, when the payload-level field
    // precedes it.
    let nested = format!(
        r#"{{"type":"event_msg","payload":{{"type":"item_completed","exitCode":2,"status":"completed","item":{{"type":"CommandExecution","id":"call_x","exitCode":2,"status":"completed","stdout":"prefix stdout","formatted_output":"prefix formatted"}},"output":"{}"}}}}"#,
        "w".repeat(200_000),
    );
    let bytes = nested.as_bytes();
    assert!(bytes.len() > DISPLAY_PREVIEW_READ_LIMIT);
    let compacted = compact_import_record(&bytes[..DISPLAY_PREVIEW_READ_LIMIT]);
    let compacted: serde_json::Value = serde_json::from_slice(&compacted).unwrap();
    assert_eq!(compacted["payload"]["exitCode"], 2);
    assert_eq!(compacted["payload"]["item"]["type"], "CommandExecution");
    assert_eq!(compacted["payload"]["item"]["exitCode"], 2);
    assert_eq!(compacted["payload"]["item"]["status"], "completed");
    assert_eq!(compacted["payload"]["item"]["stdout"], "prefix stdout");
    assert_eq!(
        compacted["payload"]["item"]["formatted_output"],
        "prefix formatted"
    );
}

#[test]
fn row_first_window_precedes_global_layout() {
    let first = message_op(1, 1, OpId::new(NodeId(0), 0, 0));
    let second = message_op(1, 2, first.id);
    let projection = HistoryProjection::from_ops(vec![first, second]);
    let mut ws = Workspace::from_projection(projection);
    let provisional = ws
        .history_window(HistoryWindowOptions {
            offset: 0,
            limit: 10,
            include_layout: false,
        })
        .unwrap();
    assert!(!provisional.layout_ready);
    assert!(!provisional.rows.is_empty());
    assert!(provisional.rows.iter().all(|row| {
        row.lane == 0 && row.above.is_empty() && row.below.is_empty() && row.transitions.is_empty()
    }));
    assert!(ws
        .current_view
        .as_ref()
        .is_some_and(|snapshot| snapshot.layout().is_none()));

    let laid_out = ws
        .history_window(HistoryWindowOptions {
            offset: 0,
            limit: 10,
            include_layout: true,
        })
        .unwrap();
    assert!(laid_out.layout_ready);
    assert_eq!(laid_out.rows.len(), provisional.rows.len());
    assert_eq!(laid_out.rows[0].node_key, provisional.rows[0].node_key);
    for (before, after) in provisional.rows.iter().zip(&laid_out.rows) {
        assert_eq!(before.parents, after.parents);
        assert_eq!(before.parent_relations, after.parent_relations);
        assert_eq!(before.parent_row, after.parent_row);
        assert_eq!(
            serde_json::to_value(&before.sub_ops).unwrap(),
            serde_json::to_value(&after.sub_ops).unwrap()
        );
    }
    assert!(ws
        .current_view
        .as_ref()
        .is_some_and(|snapshot| snapshot.layout().is_some()));
}

/// Build a ranked lexical hit.
const fn lexical_hit(op_id: OpId, score: f64) -> LexicalHit {
    LexicalHit {
        document: DocumentId::Operation(op_id),
        score,
    }
}

#[test]
fn find_in_history_resolves_top_level_and_folded_children_and_dedupes() {
    // One visible turn row (raw import) with a normalized message child
    // folded into it and a META sub-op bundled under it. Chunks matching
    // the row's own op, the folded child, or the sub-op must all resolve to
    // the single visible top-level row, keeping the best BM25 score.
    let turn = import_op(1, 1, false);
    let msg = message_op(1, 2, turn.id);
    let meta = Op {
        parents: ParentSet::One(turn.id),
        ..import_op(1, 3, true)
    };
    let projection = HistoryProjection::from_ops(vec![turn.clone(), msg.clone(), meta.clone()]);
    let mut ws = Workspace::from_projection(projection);

    let chunks = vec![
        lexical_hit(turn.id, 1.0),
        lexical_hit(msg.id, 3.5),
        lexical_hit(meta.id, 0.5),
    ];
    let matches = ws.find_in_history(&chunks);

    assert_eq!(matches.len(), 1, "all three chunks dedupe into one row");
    assert_eq!(matches[0].node_key, turn.id.to_string());
    assert_eq!(matches[0].row, 0);
    let entries = ws.current_view.as_ref().unwrap().entries().as_ptr();
    let again = ws.find_in_history(&chunks);
    assert_eq!(
        entries,
        ws.current_view.as_ref().unwrap().entries().as_ptr(),
        "Find reuses the opened view"
    );
    assert_eq!(again.len(), 1);
    assert_eq!(again[0].node_key, turn.id.to_string());
}

#[test]
fn find_in_history_maps_bundle_members_and_subops_to_containing_parent() {
    // Build the actual Activity view from two adjacent tool imports and a
    // folded metadata record. Find must resolve every source through the
    // same tree used for history windows.
    let mut member_a = import_op(1, 1, false);
    let mut member_b = import_op(1, 2, false);
    member_b.parents = ParentSet::One(member_a.id);
    for member in [&mut member_a, &mut member_b] {
        member.kind = OpKind::Import(ImportOp {
            raw_ref: Payload::Inline(br#"{"type":"response_item","payload":{"type":"function_call","status":"completed"}}"#.to_vec()),
            raw_hash: None,
        });
    }
    let tool_child = |seq, parent| Op {
        parents: ParentSet::One(parent),
        tags: Tags::AGENT | Tags::TOOL,
        ..op_envelope(
            2,
            seq,
            OpKind::Tool(editchain_core::ToolOp {
                tool_call_id: Payload::Empty,
                tool_name: Payload::Inline(b"Bash".to_vec()),
                stage: editchain_core::ToolStage::Start,
                content: Payload::Empty,
            }),
        )
    };
    let meta_of_b = Op {
        parents: ParentSet::One(member_b.id),
        ..import_op(1, 3, true)
    };
    let projection = HistoryProjection::from_ops(vec![
        member_a.clone(),
        tool_child(1, member_a.id),
        member_b.clone(),
        tool_child(2, member_b.id),
        meta_of_b.clone(),
    ]);
    let mut ws = Workspace::from_projection(projection);
    let window = ws
        .history_window(HistoryWindowOptions {
            offset: 0,
            limit: 20,
            include_layout: false,
        })
        .unwrap();
    let owner = window.rows.iter().find(|row| !row.is_subop).unwrap();
    assert_eq!(
        owner.activity_bundle.as_ref().unwrap().kind,
        editchain_protocol::ActivityBundleKind::WorkGroup
    );
    let chunks = vec![
        lexical_hit(member_a.id, 2.0),
        lexical_hit(member_b.id, 1.0),
        lexical_hit(meta_of_b.id, 0.5),
    ];
    let matches = ws.find_in_history(&chunks);

    assert_eq!(matches.len(), 1, "members dedupe into the bundle row");
    assert_eq!(matches[0].node_key, owner.node_key);
    assert_eq!(matches[0].row, 0, "group parent row offset");
}

#[test]
fn find_in_history_excludes_hits_with_no_row_in_the_active_view() {
    // turn1 is dated and visible; turn2 lives in its own undated session,
    // so Activity omits it (sessions with no dated
    // rows keep `Unknown` time — no BundleAnchor display time is assigned).
    // A hit inside turn2's folded child must be dropped even though the op
    // exists in the projection.
    let turn1 = import_op(1, 1, false);
    let msg1 = message_op(1, 2, turn1.id);
    let mut turn2 = import_op(1, 3, false);
    turn2.scope = ScopeRef::Session(SessionId(20));
    turn2 = {
        let mut op = turn2;
        op.clock = Clock::None;
        op
    };
    let mut msg2 = message_op(1, 4, turn2.id);
    if let OpKind::Message(m) = &mut msg2.kind {
        m.content = Payload::Inline(b"needle-hidden".to_vec());
    }
    msg2.scope = ScopeRef::Session(SessionId(20));
    let projection = HistoryProjection::from_ops(vec![
        turn1.clone(),
        msg1.clone(),
        turn2.clone(),
        msg2.clone(),
    ]);
    let mut ws = Workspace::from_projection(projection);
    let chunks = vec![lexical_hit(msg1.id, 1.0), lexical_hit(msg2.id, 2.0)];
    let matches = ws.find_in_history(&chunks);

    assert_eq!(matches.len(), 1);
    assert_eq!(matches[0].node_key, turn1.id.to_string());
    assert_eq!(matches[0].row, 0);
}

#[test]
fn find_in_history_excludes_nested_repository_git_hits() {
    let mut bytes = [0u8; 20];
    bytes[0] = 0xbb;
    let oid = GitOid::from_sha1(bytes);
    let commit = editchain_core::GitCommitEntity {
        repository: RepositoryId(2),
        object_format: editchain_core::GitObjectFormat::Sha1,
        oid,
        imported_record: None,
        availability: editchain_core::GitAvailability::Resolved,
        tree: oid,
        parents: Vec::new(),
        author: editchain_core::GitSignature {
            name: Payload::Inline(b"Alice".to_vec()),
            email: Payload::Inline(b"alice@example.com".to_vec()),
            when: 0,
        },
        committer: editchain_core::GitSignature {
            name: Payload::Inline(b"Alice".to_vec()),
            email: Payload::Inline(b"alice@example.com".to_vec()),
            when: 0,
        },
        authored_at: 0,
        committed_at: 0,
        message: Payload::Inline(b"needle-git".to_vec()),
        imported_refs: Vec::new(),
        live_refs: Vec::new(),
        changed_paths: Vec::new(),
    };
    let mut projection = HistoryProjection::new();
    projection.merge_git_commits(vec![commit.clone()]);
    let mut ws = Workspace::from_projection(projection);
    // Mark the commit's repository as a nested/submodule repo: the main
    // workspace repo at /ws/.git (id 1) contains /ws/nested/.git (id 2).
    ws.repositories = RepositoryCatalog::from_entries(vec![
        editchain_git::RepositoryDiscovery {
            id: RepositoryId(1),
            marker_path: PathBuf::from("/ws/.git"),
            worktree_root: Some(PathBuf::from("/ws")),
            git_dir: PathBuf::from("/ws/.git"),
            common_dir: PathBuf::from("/ws/.git"),
        },
        editchain_git::RepositoryDiscovery {
            id: RepositoryId(2),
            marker_path: PathBuf::from("/ws/nested/.git"),
            worktree_root: Some(PathBuf::from("/ws/nested")),
            git_dir: PathBuf::from("/ws/nested/.git"),
            common_dir: PathBuf::from("/ws/nested/.git"),
        },
    ]);
    let chunks = vec![LexicalHit {
        document: DocumentId::GitCommit(editchain_core::GitCommitKey::new(
            commit.repository,
            commit.oid,
        )),
        score: 1.0,
    }];

    // The fixed viewer hides nested repositories, so this identity has no
    // row and is dropped rather than mapped to a phantom offset.
    let hidden = ws.find_in_history(&chunks);
    assert!(hidden.is_empty());
}
