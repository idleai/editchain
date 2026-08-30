//! Tests for history linking — git/commit links; sessions stay separate chains.

#![expect(
    clippy::indexing_slicing,
    reason = "Test fixtures use fixed small vectors; indices are known to be in bounds"
)]
// Crate-level dependency markers (used by Cargo for feature resolution).
use regex as _;
use serde as _;
use serde_json as _;

use editchain_core::{
    ActorId, Clock, GitAvailability, GitCommitEntity, GitLinkKind, GitObjectFormat, GitOid,
    GitSignature, MessageOp, NodeId, Op, OpId, OpKind, ParentSet, Payload, ScopeRef, SessionId,
    Tags,
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
fn sessions_stay_as_separate_chains_not_stitched() {
    // Two unrelated sessions. There is NO blanket session-to-session stitching:
    // each source session is its own chain (the chain count maps 1:1 to source
    // sessions). Session B's first op must NOT gain a parent to session A's last
    // op just because they're adjacent. Cross-session linkage comes only from
    // explicit fork/subagent relationship notes and op→git links.
    let mut ops = vec![
        op(1, 1, Some(10), 1000), // session A, op 1
        op(1, 2, Some(10), 2000), // session A, op 2 (last)
        op(1, 3, Some(20), 3000), // session B, op 1 (first)
        op(1, 4, Some(20), 4000), // session B, op 2
    ];
    let result = link_history(&ops, &[]);
    ops = result.ops;

    // Session B's first op stays rootless (no parent added by stitching).
    assert_eq!(ops[2].parents.iter().count(), 0);
    assert!(result.git_links.is_empty());
}

#[test]
fn git_command_links_to_closest_commit_across_timestamp_units() {
    // Git timestamps are seconds while operation clocks are milliseconds. The
    // op is 100ms after commit 1 and almost 1,000s before commit 2, so commit 1
    // must win. Comparing the raw numbers incorrectly picks the newest commit 2.
    let commits = vec![git_commit(1, 1_000), git_commit(2, 2_000)];
    let mut ops = vec![op(1, 1, Some(10), 1_000_100)];
    // Make it a git command op.
    ops[0].kind = OpKind::Command(editchain_core::CommandOp {
        command_id: Payload::Empty,
        content: Payload::Inline(b"git commit -m x".to_vec()),
        stage: editchain_core::CommandStage::Finish,
    });

    let result = link_history(&ops, &commits);
    let produced = result
        .git_links
        .iter()
        .find(|link| matches!(&link.kind, GitLinkKind::ProducedBy))
        .expect("git command must produce a Git link");
    assert_eq!(produced.source, ops[0].id);
    assert_eq!(produced.target_oid, commits[0].oid);
}

#[test]
fn session_links_to_closest_commit_across_timestamp_units() {
    // The session's final operation is 100ms after commit 1. The weak fallback
    // must normalize Git seconds to operation milliseconds before matching.
    let commits = vec![git_commit(1, 1_000), git_commit(2, 2_000)];
    let ops = vec![
        op(1, 1, Some(10), 900_000),
        op(1, 2, Some(10), 1_000_100), // last op
    ];
    let result = link_history(&ops, &commits);
    let based_on = result
        .git_links
        .iter()
        .find(|link| matches!(&link.kind, GitLinkKind::BasedOn))
        .expect("session must retain weak Git provenance");
    assert_eq!(based_on.source, ops[1].id);
    assert_eq!(based_on.target_oid, commits[0].oid);
}

#[test]
fn non_wall_clock_ops_do_not_infer_git_links() {
    let commits = vec![git_commit(1, 1_000)];
    let mut logical = op(1, 1, Some(10), 0);
    logical.clock = Clock::Lamport(1_000_100);
    logical.kind = OpKind::Command(editchain_core::CommandOp {
        command_id: Payload::Empty,
        content: Payload::Inline(b"git commit -m x".to_vec()),
        stage: editchain_core::CommandStage::Finish,
    });

    let result = link_history(&[logical], &commits);
    assert!(
        result.git_links.is_empty(),
        "Lamport clocks cannot be compared to Git wall-clock timestamps"
    );
}

#[test]
fn sessions_are_never_force_stitched() {
    // Sessions are never stitched together by sequence number. Even two sessions
    // whose seq ranges overlap (seq is a per-file counter, not a global
    // timestamp) must NOT be parent-linked: session B's first op stays rootless.
    // This guards against regressions reintroducing blanket session stitching.
    let mut ops = vec![
        op(1, 100, Some(10), 1000), // session A first
        op(1, 200, Some(10), 2000), // session A last
        op(1, 150, Some(20), 3000), // session B first (seq < A.last)
        op(1, 250, Some(20), 4000), // session B last
    ];
    let result = link_history(&ops, &[]);
    ops = result.ops;

    // Session B's first op (index 2) must NOT gain a parent to session A's last
    // op (index 1) — sessions are their own chains.
    let b_first_parents: Vec<_> = ops[2].parents.iter().collect();
    assert!(
        b_first_parents.iter().all(|p| **p != ops[1].id),
        "sessions must not be force-stitched (seq {} has a parent to {})",
        ops[2].id.seq,
        ops[1].id.seq
    );
}
