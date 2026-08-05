//! Integration tests for live git repository resolution.

#![expect(
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::wildcard_enum_match_arm,
    reason = "Test helpers and assertions; panics and expects are acceptable in tests"
)]

use std::process::Command;

// Crate-level dependency markers (used by Cargo for feature resolution).
use gix_object as _;
use sha2 as _;

use editchain_core::{
    GitAvailability, GitCommitEntity, GitObjectFormat, GitOid, GitSignature, Payload, RepositoryId,
};
use editchain_git::{
    discover_repositories, merge_commit_entities, repository_id_from_path, resolve_commit,
    RepositoryHandle,
};

/// Create a temporary git repository with one commit and return its path.
fn make_repo(dir: &std::path::Path) -> std::path::PathBuf {
    let repo = dir.join("repo");
    std::fs::create_dir_all(&repo).expect("create repo dir");
    run(&repo, &["init", "-q"]);
    std::fs::write(repo.join("file.txt"), b"hello\n").expect("write file");
    run(&repo, &["add", "file.txt"]);
    run(
        &repo,
        &[
            "-c",
            "user.name=Alice",
            "-c",
            "user.email=alice@example.com",
            "commit",
            "-q",
            "-m",
            "initial commit",
        ],
    );
    repo
}

fn run(dir: &std::path::Path, args: &[&str]) {
    let status = Command::new("git")
        .current_dir(dir)
        .args(args)
        .status()
        .expect("run git");
    assert!(status.success(), "git {args:?} failed");
}

fn head_oid(repo: &std::path::Path) -> GitOid {
    let out = Command::new("git")
        .current_dir(repo)
        .args(["rev-parse", "HEAD"])
        .output()
        .expect("rev-parse");
    let hex = String::from_utf8(out.stdout).expect("utf8");
    let hex = hex.trim();
    let mut bytes = [0u8; 32];
    for (i, ch) in hex.as_bytes().chunks(2).enumerate() {
        bytes[i] = u8::from_str_radix(std::str::from_utf8(ch).expect("ascii"), 16).expect("hex");
    }
    GitOid::from_sha1(bytes[..20].try_into().expect("20 bytes"))
}

#[test]
fn discover_finds_repository() {
    let tmp = tempfile::tempdir().expect("tempdir");
    drop(make_repo(tmp.path()));

    let discoveries = discover_repositories(tmp.path()).expect("discover");
    assert!(!discoveries.is_empty(), "should find at least one repo");
    assert!(discoveries.iter().any(|d| d.path.ends_with(".git")));
}

#[test]
fn repository_id_is_deterministic() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let repo = make_repo(tmp.path());

    let id1 = repository_id_from_path(&repo);
    let id2 = repository_id_from_path(&repo);
    assert_eq!(id1, id2);
}

#[test]
fn resolve_commit_reads_fields() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let repo = make_repo(tmp.path());
    let oid = head_oid(&repo);

    let gix_repo = gix::open(&repo).expect("open repo");
    let handle = RepositoryHandle {
        repo: gix_repo,
        discovery: editchain_git::RepositoryDiscovery {
            id: repository_id_from_path(&repo),
            path: repo.clone(),
            is_worktree: false,
        },
    };

    let resolution = resolve_commit(&handle, &oid).expect("resolve");
    assert!(resolution.found);
    assert_eq!(resolution.commit.availability, GitAvailability::Resolved);
    assert_eq!(resolution.commit.oid, oid);
    assert_eq!(resolution.commit.object_format, GitObjectFormat::Sha1);
    // Message should contain the commit subject.
    match &resolution.commit.message {
        Payload::Inline(b) => {
            assert!(String::from_utf8_lossy(b).contains("initial commit"));
        }
        _ => panic!("expected inline message"),
    }
}

#[test]
fn resolve_missing_object_reports_not_found() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let repo = make_repo(tmp.path());

    let gix_repo = gix::open(&repo).expect("open repo");
    let handle = RepositoryHandle {
        repo: gix_repo,
        discovery: editchain_git::RepositoryDiscovery {
            id: repository_id_from_path(&repo),
            path: repo.clone(),
            is_worktree: false,
        },
    };

    // A non-existent OID (all zeros).
    let missing = GitOid::from_sha1([0u8; 20]);
    let result = resolve_commit(&handle, &missing);
    assert!(result.is_err(), "missing object should error");
}

#[test]
fn merge_prefers_live_and_keeps_imported_record() {
    let imported = GitCommitEntity {
        repository: RepositoryId(7),
        object_format: GitObjectFormat::Sha1,
        oid: GitOid::from_sha1([0x01; 20]),
        imported_record: Some(editchain_core::OpId::new(editchain_core::NodeId(1), 0, 5)),
        availability: GitAvailability::ImportedOnly,
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
        message: Payload::Inline(b"imported msg".to_vec()),
        imported_refs: vec![Payload::Inline(b"refs/heads/main".to_vec())],
        live_refs: Vec::new(),
        changed_paths: Vec::new(),
    };

    // Live version has a different message (authoritative).
    let mut live = imported.clone();
    live.message = Payload::Inline(b"live msg".to_vec());
    live.availability = GitAvailability::Resolved;

    let outcome = merge_commit_entities(Some(imported), Some(live)).expect("merge");
    assert!(outcome.used_live);
    // Live message wins.
    match &outcome.merged.message {
        Payload::Inline(b) => assert_eq!(b, b"live msg"),
        _ => panic!("expected inline"),
    }
    // Imported record link retained.
    assert!(outcome.merged.imported_record.is_some());
}

#[test]
fn merge_imported_only_when_no_live() {
    let imported = GitCommitEntity {
        repository: RepositoryId(7),
        object_format: GitObjectFormat::Sha1,
        oid: GitOid::from_sha1([0x01; 20]),
        imported_record: None,
        availability: GitAvailability::ImportedOnly,
        tree: GitOid::from_sha1([0x02; 20]),
        parents: Vec::new(),
        author: GitSignature {
            name: Payload::Inline(b"Alice".to_vec()),
            email: Payload::Inline(b"alice@example.com".to_vec()),
            when: 0,
        },
        committer: GitSignature {
            name: Payload::Inline(b"Alice".to_vec()),
            email: Payload::Inline(b"alice@example.com".to_vec()),
            when: 0,
        },
        authored_at: 0,
        committed_at: 0,
        message: Payload::Inline(b"msg".to_vec()),
        imported_refs: Vec::new(),
        live_refs: Vec::new(),
        changed_paths: Vec::new(),
    };

    let outcome = merge_commit_entities(Some(imported.clone()), None).expect("merge");
    assert!(!outcome.used_live);
    assert_eq!(outcome.merged.message, imported.message);
}
