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

use editchain_core::{GitAvailability, GitObjectFormat, GitOid, Payload};
use editchain_git::{
    discover_repositories, repository_id_from_path, resolve_commit, resolve_commit_prefix,
    RepositoryCatalog, RepositoryHandle,
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
    assert!(discoveries.iter().any(|d| d.marker_path.ends_with(".git")));
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
        discovery: editchain_git::RepositoryDiscovery::from_path(&repo)
            .expect("describe repository"),
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
        discovery: editchain_git::RepositoryDiscovery::from_path(&repo)
            .expect("describe repository"),
    };

    // A non-existent OID (all zeros).
    let missing = GitOid::from_sha1([0u8; 20]);
    let result = resolve_commit(&handle, &missing);
    assert!(result.is_err(), "missing object should error");
}

#[test]
fn resolve_commit_prefix_requires_an_unambiguous_commit_object() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let repo = make_repo(tmp.path());
    let oid = head_oid(&repo);
    let handle = RepositoryHandle {
        repo: gix::open(&repo).expect("open repo"),
        discovery: editchain_git::RepositoryDiscovery::from_path(&repo)
            .expect("describe repository"),
    };

    let full = oid.to_hex();
    let prefix = full.get(..7).expect("seven-character prefix");
    let resolved = resolve_commit_prefix(&handle, prefix).expect("unique commit prefix");
    assert_eq!(resolved.commit.oid, oid);
    assert!(
        resolve_commit_prefix(&handle, "123").is_none(),
        "too-short prefixes are not strong identity evidence"
    );
    assert!(
        resolve_commit_prefix(&handle, "not-hexadecimal").is_none(),
        "non-hexadecimal strings are not object prefixes"
    );
}

#[test]
fn full_commit_lookup_rejects_blob_and_tree_objects_without_panicking() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = make_repo(tmp.path());
    let descriptor = editchain_git::RepositoryDiscovery::from_path(&repo).unwrap();
    let handle = editchain_git::open_repository(&descriptor).unwrap();
    for revision in ["HEAD:file.txt", "HEAD^{tree}"] {
        let output = Command::new("git")
            .current_dir(&repo)
            .args(["rev-parse", revision])
            .output()
            .unwrap();
        assert!(output.status.success());
        let hex = String::from_utf8(output.stdout).unwrap();
        let oid = GitOid::from_hex(hex.trim()).unwrap();
        assert!(matches!(
            resolve_commit(&handle, &oid),
            Err(editchain_git::ResolutionError::WrongKind {
                expected: "commit",
                ..
            })
        ));
    }
}

#[test]
fn catalog_describes_worktrees_and_bare_repositories_without_changing_ids() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = make_repo(tmp.path());
    let linked = tmp.path().join("linked");
    run(
        &repo,
        &[
            "worktree",
            "add",
            "--detach",
            "-q",
            linked.to_str().unwrap(),
        ],
    );
    let bare = tmp.path().join("bare.git");
    run(
        tmp.path(),
        &[
            "clone",
            "--bare",
            "-q",
            repo.to_str().unwrap(),
            bare.to_str().unwrap(),
        ],
    );
    let catalog = RepositoryCatalog::discover(tmp.path()).unwrap();
    assert!(catalog.is_complete());
    assert_eq!(catalog.len(), 3);
    let main = catalog.repository_for_path(&repo).unwrap();
    let linked_repo = catalog.repository_for_path(&linked).unwrap();
    assert!(!main.is_linked_worktree());
    assert!(linked_repo.is_linked_worktree());
    assert_eq!(main.common_dir, linked_repo.common_dir);
    assert_ne!(main.git_dir, linked_repo.git_dir);
    assert_eq!(main.id, repository_id_from_path(&repo.join(".git")));
    assert_eq!(
        linked_repo.id,
        repository_id_from_path(&linked.join(".git"))
    );
    assert_ne!(main.id, linked_repo.id);
    let bare_repo = catalog
        .entries()
        .iter()
        .find(|entry| entry.worktree_root.is_none())
        .unwrap();
    assert_eq!(bare_repo.marker_path, bare.canonicalize().unwrap());
    let handle = editchain_git::open_repository(linked_repo).unwrap();
    assert_eq!(
        resolve_commit(&handle, &head_oid(&repo))
            .unwrap()
            .commit
            .repository,
        linked_repo.id
    );
}

#[test]
fn configured_worktree_does_not_change_the_discovered_marker_identity() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = make_repo(tmp.path());
    let worktree = tmp.path().join("configured-worktree");
    std::fs::create_dir_all(&worktree).unwrap();
    run(
        &repo,
        &["config", "core.worktree", worktree.to_str().unwrap()],
    );
    let marker = repo.join(".git");
    let descriptor = editchain_git::RepositoryDiscovery::from_path(&marker).unwrap();
    assert_eq!(descriptor.id, repository_id_from_path(&marker));
    assert_eq!(descriptor.marker_path, marker);
    assert_eq!(
        descriptor.worktree_root,
        Some(worktree.canonicalize().unwrap())
    );
    let handle = editchain_git::open_repository(&descriptor).unwrap();
    assert_eq!(handle.repo.git_dir(), descriptor.git_dir);
    assert_eq!(
        editchain_git::RepositoryDiscovery::from_path(&repo)
            .unwrap()
            .id,
        descriptor.id
    );
}

#[test]
fn catalog_uses_component_containment_and_retains_discovery_failures() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = make_repo(tmp.path());
    let sibling = tmp.path().join("repo-copy");
    run(
        tmp.path(),
        &[
            "clone",
            "-q",
            repo.to_str().unwrap(),
            sibling.to_str().unwrap(),
        ],
    );
    let nested = repo.join("nested");
    std::fs::create_dir_all(&nested).unwrap();
    run(&nested, &["init", "-q"]);
    let broken = tmp.path().join("broken");
    std::fs::create_dir_all(&broken).unwrap();
    std::fs::write(
        broken.join(".git"),
        "gitdir: /nonexistent/editchain-test-repo",
    )
    .unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink(tmp.path(), repo.join("cycle")).unwrap();

    let catalog = RepositoryCatalog::discover(tmp.path()).unwrap();
    assert_eq!(catalog.len(), 3);
    assert!(!catalog.is_complete());
    assert_eq!(catalog.issues().len(), 1);
    assert_eq!(catalog.issues().first().unwrap().path, broken.join(".git"));
    assert!(discover_repositories(tmp.path()).is_err());
    let main = catalog.repository_for_path(&repo).unwrap();
    let sibling_repo = catalog.repository_for_path(&sibling).unwrap();
    let nested_repo = catalog.repository_for_path(&nested).unwrap();
    assert!(!catalog.is_nested(main.id));
    assert!(!catalog.is_nested(sibling_repo.id));
    assert!(catalog.is_nested(nested_repo.id));
    assert!(main
        .relative_worktree_path(std::path::Path::new("../repo-copy/file.txt"), Some(&repo))
        .is_none());
    assert_eq!(
        main.relative_worktree_path(std::path::Path::new("src/../file.txt"), Some(&repo)),
        Some(std::path::PathBuf::from("file.txt"))
    );
}
