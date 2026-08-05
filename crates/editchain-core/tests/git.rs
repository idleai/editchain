//! Git identity, commit, and explicit-link tests.

// Referenced by library derive macros; suppress unused-crate-dependencies lint.
use postcard as _;
use proptest as _;
use serde as _;

use editchain_core::{
    ActorId, Clock, GitAvailability, GitCommitEntity, GitLink, GitLinkKind, GitObjectFormat,
    GitOid, GitProjection, GitSignature, NodeId, Op, OpId, OpKind, ParentSet, PathId, Payload,
    RepositoryId, ScopeRef, Tags,
};

fn sha1(bytes: [u8; 20]) -> GitOid {
    GitOid::from_sha1(bytes)
}

fn sha256(bytes: [u8; 32]) -> GitOid {
    GitOid::from_sha256(bytes)
}

#[test]
fn oid_sha1_pads_to_32_bytes() {
    let oid = sha1([0xab; 20]);
    assert_eq!(oid.format, GitObjectFormat::Sha1);
    assert_eq!(oid.digest_len(), 20);
    // First 20 bytes are the digest; the rest are zero.
    assert_eq!(&oid.bytes[..20], &[0xab; 20]);
    assert_eq!(&oid.bytes[20..], &[0u8; 12]);
}

#[test]
fn oid_sha256_uses_all_bytes() {
    let oid = sha256([0xcd; 32]);
    assert_eq!(oid.format, GitObjectFormat::Sha256);
    assert_eq!(oid.digest_len(), 32);
    assert_eq!(&oid.bytes[..], &[0xcd; 32]);
}

#[test]
fn oid_hex_round_trip() {
    let oid = sha1([0xab; 20]);
    let hex = oid.to_hex();
    assert_eq!(hex.len(), 40);
    assert!(hex.chars().all(|c| c.is_ascii_hexdigit()));
    // 0xab -> "ab"
    assert!(hex.starts_with("ab"));
}

#[test]
fn oid_display_matches_hex() {
    let oid = sha256([0x12; 32]);
    assert_eq!(format!("{oid}"), oid.to_hex());
}

#[test]
#[expect(clippy::panic, reason = "test assertion")]
fn git_commit_op_round_trips() {
    let commit = GitCommitEntity {
        repository: RepositoryId(7),
        object_format: GitObjectFormat::Sha1,
        oid: sha1([0x01; 20]),
        imported_record: Some(OpId::new(NodeId(1), 0, 5)),
        availability: GitAvailability::Resolved,
        tree: sha1([0x02; 20]),
        parents: vec![sha1([0x03; 20]), sha1([0x04; 20])],
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

    let encoded = postcard::to_stdvec(&op).expect("encode failed");
    let decoded: Op = postcard::from_bytes(&encoded).expect("decode failed");

    match (&op.kind, &decoded.kind) {
        (OpKind::GitCommit(a), OpKind::GitCommit(b)) => {
            assert_eq!(a.repository, b.repository);
            assert_eq!(a.oid, b.oid);
            assert_eq!(a.parents, b.parents);
            assert_eq!(a.message, b.message);
            assert_eq!(a.changed_paths, b.changed_paths);
        }
        _ => panic!("kind mismatch"),
    }
}

#[test]
#[expect(clippy::panic, reason = "test assertion")]
fn git_link_op_round_trips() {
    let link = GitLink {
        source: OpId::new(NodeId(1), 0, 5),
        target_repo: RepositoryId(7),
        target_oid: sha1([0x01; 20]),
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

    let encoded = postcard::to_stdvec(&op).expect("encode failed");
    let decoded: Op = postcard::from_bytes(&encoded).expect("decode failed");

    match (&op.kind, &decoded.kind) {
        (OpKind::GitLink(a), OpKind::GitLink(b)) => {
            assert_eq!(a.source, b.source);
            assert_eq!(a.target_repo, b.target_repo);
            assert_eq!(a.target_oid, b.target_oid);
            assert_eq!(a.kind, b.kind);
        }
        _ => panic!("kind mismatch"),
    }
}

#[test]
fn git_link_kind_custom_payload() {
    let link = GitLink {
        source: OpId::new(NodeId(1), 0, 5),
        target_repo: RepositoryId(7),
        target_oid: sha1([0x01; 20]),
        kind: GitLinkKind::Custom(Payload::Inline(b"cherry-picked".to_vec())),
    };
    let encoded = postcard::to_stdvec(&link).expect("encode failed");
    let decoded: GitLink = postcard::from_bytes(&encoded).expect("decode failed");
    assert_eq!(decoded.kind, link.kind);
}

fn commit_op(id: OpId, repo: RepositoryId, oid: GitOid) -> Op {
    let commit = GitCommitEntity {
        repository: repo,
        object_format: oid.format,
        oid,
        imported_record: Some(id),
        availability: GitAvailability::ImportedOnly,
        tree: sha1([0x02; 20]),
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
        message: Payload::Inline(b"commit".to_vec()),
        imported_refs: Vec::new(),
        live_refs: Vec::new(),
        changed_paths: Vec::new(),
    };
    Op {
        id,
        parents: ParentSet::None,
        actor: ActorId(3),
        clock: Clock::UnixMs(1_700_000_000),
        scope: ScopeRef::None,
        tags: Tags::IMPORT,
        kind: OpKind::GitCommit(Box::new(commit)),
    }
}

#[test]
fn projection_dedups_same_commit_by_repo_and_oid() {
    // Two different ops importing the same (repo, oid) collapse to one entity.
    let oid = sha1([0x01; 20]);
    let repo = RepositoryId(7);
    let op_a = commit_op(OpId::new(NodeId(1), 0, 5), repo, oid);
    let op_b = commit_op(OpId::new(NodeId(2), 0, 9), repo, oid);

    let proj = GitProjection::from_ops(&[op_a, op_b]);
    assert_eq!(proj.commits.len(), 1);
    assert!(proj.commit(repo, &oid).is_some());
}

#[test]
fn projection_distinguishes_repositories() {
    // Same OID in two different repositories are distinct entities.
    let oid = sha1([0x01; 20]);
    let op_a = commit_op(OpId::new(NodeId(1), 0, 5), RepositoryId(7), oid);
    let op_b = commit_op(OpId::new(NodeId(2), 0, 9), RepositoryId(8), oid);

    let proj = GitProjection::from_ops(&[op_a, op_b]);
    assert_eq!(proj.commits.len(), 2);
}

#[test]
fn projection_groups_links_by_source() {
    let source = OpId::new(NodeId(1), 0, 5);
    let link_a = GitLink {
        source,
        target_repo: RepositoryId(7),
        target_oid: sha1([0x01; 20]),
        kind: GitLinkKind::CommittedAs,
    };
    let link_b = GitLink {
        source,
        target_repo: RepositoryId(7),
        target_oid: sha1([0x02; 20]),
        kind: GitLinkKind::BasedOn,
    };
    let op_a = Op {
        id: OpId::new(NodeId(2), 0, 10),
        parents: ParentSet::None,
        actor: ActorId(3),
        clock: Clock::UnixMs(1_700_000_000),
        scope: ScopeRef::None,
        tags: Tags::NOTE,
        kind: OpKind::GitLink(link_a),
    };
    let op_b = Op {
        id: OpId::new(NodeId(2), 0, 11),
        parents: ParentSet::None,
        actor: ActorId(3),
        clock: Clock::UnixMs(1_700_000_000),
        scope: ScopeRef::None,
        tags: Tags::NOTE,
        kind: OpKind::GitLink(link_b),
    };

    let proj = GitProjection::from_ops(&[op_a, op_b]);
    assert_eq!(proj.links_from(&source).len(), 2);
}

#[test]
fn projection_link_is_not_a_causal_parent() {
    // A git link op must not appear as a causal parent of the commit it links.
    let source = OpId::new(NodeId(1), 0, 5);
    let link_op = Op {
        id: OpId::new(NodeId(2), 0, 10),
        parents: ParentSet::None,
        actor: ActorId(3),
        clock: Clock::UnixMs(1_700_000_000),
        scope: ScopeRef::None,
        tags: Tags::NOTE,
        kind: OpKind::GitLink(GitLink {
            source,
            target_repo: RepositoryId(7),
            target_oid: sha1([0x01; 20]),
            kind: GitLinkKind::CommittedAs,
        }),
    };

    // The link's own causal parents are empty; the projection stores it under
    // `links`, never under `commits` or as a parent edge.
    assert!(matches!(link_op.parents, ParentSet::None));
    let proj = GitProjection::from_ops(&[link_op]);
    assert!(proj.commits.is_empty());
}
