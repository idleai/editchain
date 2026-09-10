//! Git identity, commit, and explicit-link tests.

// Referenced by library derive macros; suppress unused-crate-dependencies lint.
use postcard as _;
use proptest as _;
use serde as _;

use editchain_core::{
    ActorId, Clock, GitAvailability, GitCommitEntity, GitLink, GitLinkKind, GitObjectFormat,
    GitOid, GitSignature, NodeId, Op, OpId, OpKind, ParentSet, PathId, Payload, RepositoryId,
    ScopeRef, Tags,
};

fn sha1(bytes: [u8; 20]) -> GitOid {
    GitOid::from_sha1(bytes)
}

fn sha256(bytes: [u8; 32]) -> GitOid {
    GitOid::from_sha256(bytes)
}

#[test]
fn qualified_commit_keys_preserve_full_repository_identity() {
    use editchain_core::GitCommitKey;
    let oid = sha1([0xab; 20]);
    let key = GitCommitKey::new(RepositoryId(u64::MAX), oid);
    assert_eq!(GitCommitKey::from_display_str(&key.to_string()), Some(key));
    assert!(GitCommitKey::from_display_str(&oid.to_hex()).is_none());
    assert_ne!(
        key.to_string(),
        GitCommitKey::new(RepositoryId(1), oid).to_string()
    );
}

#[test]
fn oid_sha1_pads_to_32_bytes() {
    let oid = sha1([0xab; 20]);
    assert_eq!(oid.format(), GitObjectFormat::Sha1);
    assert_eq!(oid.digest_len(), 20);
    // First 20 bytes are the digest; the rest are zero.
    assert_eq!(&oid.as_bytes()[..20], &[0xab; 20]);
    assert_eq!(&oid.as_bytes()[20..], &[0u8; 12]);
}

#[test]
fn oid_sha256_uses_all_bytes() {
    let oid = sha256([0xcd; 32]);
    assert_eq!(oid.format(), GitObjectFormat::Sha256);
    assert_eq!(oid.digest_len(), 32);
    assert_eq!(&oid.as_bytes()[..], &[0xcd; 32]);
}

#[test]
fn oid_decode_preserves_canonical_wire_bytes_and_rejects_sha1_aliases() {
    for (format_tag, oid) in [(0, sha1([0xab; 20])), (1, sha256([0xcd; 32]))] {
        let mut bytes = vec![format_tag];
        bytes.extend_from_slice(oid.as_bytes());
        assert_eq!(postcard::to_stdvec(&oid).unwrap(), bytes);
        assert_eq!(postcard::from_bytes::<GitOid>(&bytes).unwrap(), oid);
        assert_eq!(GitOid::new(oid.format(), *oid.as_bytes()), Some(oid));
    }
    for padding_index in 20..32 {
        let mut storage = *sha1([0xab; 20]).as_bytes();
        *storage.get_mut(padding_index).unwrap() = 1;
        assert_eq!(GitOid::new(GitObjectFormat::Sha1, storage), None);
        let bytes = postcard::to_stdvec(&(GitObjectFormat::Sha1, storage)).unwrap();
        assert!(postcard::from_bytes::<GitOid>(&bytes).is_err());
        // The same bytes are significant and valid under SHA-256.
        assert_eq!(
            GitOid::new(GitObjectFormat::Sha256, storage),
            Some(sha256(storage))
        );
    }
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
