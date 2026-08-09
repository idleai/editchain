//! Tests for history linking — stitching sessions and git into one chain.

#![expect(
    clippy::indexing_slicing,
    reason = "Test fixtures use fixed small vectors; indices are known to be in bounds"
)]

use editchain_core::{
    ActorId, Clock, GitAvailability, GitCommitEntity, GitObjectFormat, GitOid, GitSignature,
    MessageOp, NodeId, Op, OpId, OpKind, ParentSet, Payload, ScopeRef, SessionId, Tags,
};
use editchain_project::link::link_history;

fn op(node: u64, seq: u64, session: Option<u64>, clock: u64) -> Op {
    Op {
        id: OpId::new(NodeId(node), 0, seq),
        parents: ParentSet::None,
        actor: ActorId(1),
        clock: Clock::UnixMs(clock),
        scope: session.map_or(ScopeRef::None, |s| ScopeRef::Session(SessionId(s))),
        tags: Tags::MESSAGE,
        kind: OpKind::Message(MessageOp {
            content: Payload::Inline(b"msg".to_vec()),
            content_type: Payload::Empty,
        }),
    }
}

fn git_commit(oid_byte: u8, committed_at: i64) -> GitCommitEntity {
    let oid = |b: u8| {
        let mut bytes = [0u8; 32];
        bytes[0] = b;
        GitOid::new(GitObjectFormat::Sha1, bytes)
    };
    GitCommitEntity {
        repository: editchain_core::RepositoryId(1),
        object_format: GitObjectFormat::Sha1,
        oid: oid(oid_byte),
        imported_record: None,
        availability: GitAvailability::Resolved,
        tree: oid(0),
        parents: Vec::new(),
        author: GitSignature {
            name: Payload::Empty,
            email: Payload::Empty,
            when: committed_at,
        },
        committer: GitSignature {
            name: Payload::Empty,
            email: Payload::Empty,
            when: committed_at,
        },
        authored_at: committed_at,
        committed_at,
        message: Payload::Empty,
        imported_refs: Vec::new(),
        live_refs: Vec::new(),
        changed_paths: Vec::new(),
    }
}

#[test]
fn sessions_stitch_into_one_chain() {
    // Two sessions, each with two ops. After linking, session 2's first op
    // should parent to session 1's last op.
    let mut ops = vec![
        op(1, 1, Some(10), 1000), // session A, op 1
        op(1, 2, Some(10), 2000), // session A, op 2 (last)
        op(1, 3, Some(20), 3000), // session B, op 1 (first)
        op(1, 4, Some(20), 4000), // session B, op 2
    ];
    let result = link_history(&ops, &[]);
    ops = result.ops;

    // Session B's first op (index 2) should now have a parent = session A's last op.
    assert_eq!(ops[2].parents.iter().count(), 1);
    assert_eq!(ops[2].parents.iter().next(), Some(&ops[1].id));
}

#[test]
fn git_command_links_to_closest_commit() {
    // A commit at t=5000. An op at t=5100 running a git command should link to it.
    let commits = vec![git_commit(1, 5000)];
    let mut ops = vec![op(1, 1, Some(10), 5100)];
    // Make it a git command op.
    ops[0].kind = OpKind::Command(editchain_core::CommandOp {
        command_id: Payload::Empty,
        content: Payload::Inline(b"git commit -m x".to_vec()),
        stage: editchain_core::CommandStage::Finish,
    });

    let result = link_history(&ops, &commits);
    assert!(!result.git_links.is_empty());
    assert_eq!(result.git_links[0].source, ops[0].id);
}

#[test]
fn session_links_to_closest_commit() {
    // A commit at t=5000. A session with no git command should link its last op
    // to the closest commit.
    let commits = vec![git_commit(1, 5000)];
    let ops = vec![
        op(1, 1, Some(10), 4000),
        op(1, 2, Some(10), 6000), // last op
    ];
    let result = link_history(&ops, &commits);
    assert!(!result.git_links.is_empty());
    assert_eq!(result.git_links[0].source, ops[1].id);
}

#[test]
fn stitch_skips_backward_edge_when_seq_ranges_overlap() {
    // Two sessions whose seq ranges overlap (seq is a per-file counter, not a
    // global timestamp). Session A spans seq 100..200; session B spans seq
    // 150..250. Sorting by first-op seq puts A before B, but B's last op (250)
    // has a HIGHER seq than A's first op (100) — a naive stitch would add a
    // backward edge A.first -> B.last that closes a cycle. The stitch must
    // skip it.
    let mut ops = vec![
        op(1, 100, Some(10), 1000), // session A first
        op(1, 200, Some(10), 2000), // session A last
        op(1, 150, Some(20), 3000), // session B first (seq < A.last)
        op(1, 250, Some(20), 4000), // session B last
    ];
    let result = link_history(&ops, &[]);
    ops = result.ops;

    // Session B's first op (index 2) must NOT gain a parent to session A's last
    // op (index 1), because that would be a backward edge (150 < 200).
    let b_first_parents: Vec<_> = ops[2].parents.iter().collect();
    assert!(
        !b_first_parents.iter().any(|p| **p == ops[1].id),
        "stitch must not create a backward edge from seq {} to seq {}",
        ops[2].id.seq,
        ops[1].id.seq
    );
}
