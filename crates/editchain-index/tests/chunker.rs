#![doc = "Tests for the chunker module."]

// Crate-level dependency markers (used by Cargo for feature resolution).
use editchain_embed as _;
use editchain_query as _;
use half as _;
use roaring as _;
use tantivy as _;

use editchain_core::*;
use editchain_index::chunker::{chunk_text, extract_op_text};

#[test]
fn extract_message_text() {
    let op = Op {
        id: OpId::new(NodeId(1), 0, 1),
        parents: ParentSet::None,
        actor: ActorId(0),
        clock: Clock::UnixMs(1000),
        scope: ScopeRef::None,
        tags: Tags::MESSAGE,
        kind: OpKind::Message(MessageOp {
            content: Payload::Inline(b"hello world".to_vec()),
            content_type: Payload::Empty,
        }),
    };

    let text = extract_op_text(&op, false, false);
    assert_eq!(text, Some("hello world".to_string()));
}

#[test]
fn extract_private_content_blocked() {
    let op = Op {
        id: OpId::new(NodeId(1), 0, 1),
        parents: ParentSet::None,
        actor: ActorId(0),
        clock: Clock::UnixMs(1000),
        scope: ScopeRef::None,
        tags: Tags::PRIVATE | Tags::MESSAGE,
        kind: OpKind::Message(MessageOp {
            content: Payload::Inline(b"secret".to_vec()),
            content_type: Payload::Empty,
        }),
    };

    assert!(extract_op_text(&op, false, false).is_none());
    assert!(extract_op_text(&op, false, true).is_some());
}

#[test]
#[expect(
    clippy::indexing_slicing,
    reason = "Test assertions on known-valid indices; panic is acceptable in tests"
)]
fn chunk_short_text() {
    let text = "short";
    let op_id = OpId::new(NodeId(1), 0, 1);
    let chunks = chunk_text(text, op_id, 0, 768, 96);
    assert_eq!(chunks.len(), 1);
    assert_eq!(chunks[0].byte_start, 0);
    assert_eq!(chunks[0].byte_end, 5);
}

#[test]
fn extract_git_commit_text() {
    let commit = GitCommitEntity {
        repository: RepositoryId(7),
        object_format: GitObjectFormat::Sha1,
        oid: GitOid::from_sha1([0x01; 20]),
        imported_record: None,
        availability: GitAvailability::Resolved,
        tree: GitOid::from_sha1([0x02; 20]),
        parents: Vec::new(),
        author: GitSignature {
            name: Payload::Inline(b"Alice".to_vec()),
            email: Payload::Inline(b"alice@example.com".to_vec()),
            when: 1_700_000_000,
        },
        committer: GitSignature {
            name: Payload::Inline(b"Alice".to_vec()),
            email: Payload::Inline(b"alice@example.com".to_vec()),
            when: 1_700_000_100,
        },
        authored_at: 1_700_000_000,
        committed_at: 1_700_000_100,
        message: Payload::Inline(b"feat: add git support".to_vec()),
        imported_refs: vec![Payload::Inline(b"refs/heads/main".to_vec())],
        live_refs: Vec::new(),
        changed_paths: vec![PathId(9)],
    };

    let op = Op {
        id: OpId::new(NodeId(2), 0, 10),
        parents: ParentSet::None,
        actor: ActorId(3),
        clock: Clock::UnixMs(1_700_000_000),
        scope: ScopeRef::None,
        tags: Tags::IMPORT,
        kind: OpKind::GitCommit(Box::new(commit)),
    };

    let text = extract_op_text(&op, false, false).expect("git commit should be indexed");
    assert!(text.contains("feat: add git support"));
    assert!(text.contains("refs/heads/main"));
}

#[test]
fn extract_git_link_text() {
    let link = GitLink {
        source: OpId::new(NodeId(1), 0, 5),
        target_repo: RepositoryId(7),
        target_oid: GitOid::from_sha1([0x01; 20]),
        kind: GitLinkKind::CommittedAs,
    };

    let op = Op {
        id: OpId::new(NodeId(2), 0, 11),
        parents: ParentSet::None,
        actor: ActorId(3),
        clock: Clock::UnixMs(1_700_000_000),
        scope: ScopeRef::None,
        tags: Tags::NOTE,
        kind: OpKind::GitLink(link),
    };

    let text = extract_op_text(&op, false, false).expect("git link should be indexed");
    assert!(text.contains("git:"));
}
