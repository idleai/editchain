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
    walk_history, RefSnapshot, RepositoryCatalog, RepositoryHandle, ResolutionError,
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
    assert_eq!(resolution.availability, GitAvailability::Resolved);
    assert_eq!(resolution.oid, oid);
    assert_eq!(resolution.object_format, GitObjectFormat::Sha1);
    // Message should contain the commit subject.
    match &resolution.message {
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
    let resolved = resolve_commit_prefix(&handle, prefix)
        .expect("read prefix")
        .expect("unique commit prefix");
    assert_eq!(resolved.oid, oid);
    assert!(
        matches!(
            resolve_commit_prefix(&handle, "123"),
            Err(ResolutionError::InvalidPrefix(_))
        ),
        "too-short prefixes are not strong identity evidence"
    );
    assert!(
        matches!(
            resolve_commit_prefix(&handle, "not-hexadecimal"),
            Err(ResolutionError::InvalidPrefix(_))
        ),
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
            Err(ResolutionError::WrongKind {
                expected: "commit",
                ..
            })
        ));
        assert!(matches!(
            resolve_commit_prefix(&handle, &oid.to_hex()),
            Err(ResolutionError::WrongKind {
                expected: "commit",
                ..
            })
        ));
    }
}

#[test]
fn history_observes_refs_once_and_distinguishes_limits_from_exhaustion() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = make_repo(tmp.path());
    let first = head_oid(&repo);
    run(&repo, &["branch", "observed"]);
    run(
        &repo,
        &[
            "-c",
            "user.name=Alice",
            "-c",
            "user.email=alice@example.com",
            "commit",
            "--allow-empty",
            "-q",
            "-m",
            "second",
        ],
    );
    let descriptor = editchain_git::RepositoryDiscovery::from_path(&repo).unwrap();
    let handle = editchain_git::open_repository(&descriptor).unwrap();
    let history = walk_history(&handle, 0).unwrap();
    assert!(history.is_complete());
    assert_eq!(history.commits.len(), 2);
    assert_eq!(history.commits[0].oid, head_oid(&repo));
    assert_eq!(
        history.refs.refs_for(&first),
        &[b"refs/heads/observed".to_vec()]
    );
    assert_eq!(
        history.commits[1].live_refs,
        vec![Payload::Inline(b"refs/heads/observed".to_vec())]
    );

    run(&repo, &["branch", "-f", "observed", "HEAD"]);
    assert_eq!(
        history.refs.refs_for(&first),
        &[b"refs/heads/observed".to_vec()]
    );
    let later = RefSnapshot::capture(&handle).unwrap();
    assert!(later.refs_for(&first).is_empty());
    assert!(later
        .refs_for(&head_oid(&repo))
        .contains(&b"refs/heads/observed".to_vec()));

    let limited = walk_history(&handle, 1).unwrap();
    assert_eq!(limited.commits.len(), 1);
    assert!(limited.truncated);
    assert!(!limited.is_complete());
    assert!(walk_history(&handle, 2).unwrap().is_complete());
}

#[test]
fn history_retains_available_commits_when_an_ancestor_is_missing() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = make_repo(tmp.path());
    let first = head_oid(&repo).to_hex();
    run(
        &repo,
        &[
            "-c",
            "user.name=Alice",
            "-c",
            "user.email=alice@example.com",
            "commit",
            "--allow-empty",
            "-q",
            "-m",
            "second",
        ],
    );
    std::fs::remove_file(
        repo.join(".git/objects")
            .join(first.get(..2).unwrap())
            .join(first.get(2..).unwrap()),
    )
    .unwrap();
    let descriptor = editchain_git::RepositoryDiscovery::from_path(&repo).unwrap();
    let handle = editchain_git::open_repository(&descriptor).unwrap();
    let history = walk_history(&handle, 0).unwrap();
    assert!(!history.is_complete());
    assert!(!history.issues.is_empty());
    assert_eq!(history.commits.len(), 1);
    assert_eq!(history.commits[0].oid, head_oid(&repo));
}

#[test]
fn history_reports_shallow_boundaries_and_ref_failures() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = make_repo(tmp.path());
    run(
        &repo,
        &[
            "-c",
            "user.name=Alice",
            "-c",
            "user.email=alice@example.com",
            "commit",
            "--allow-empty",
            "-q",
            "-m",
            "second",
        ],
    );
    let shallow = tmp.path().join("shallow");
    run(
        tmp.path(),
        &[
            "clone",
            "-q",
            "--no-local",
            "--depth=1",
            repo.to_str().unwrap(),
            shallow.to_str().unwrap(),
        ],
    );
    let descriptor = editchain_git::RepositoryDiscovery::from_path(&shallow).unwrap();
    let handle = editchain_git::open_repository(&descriptor).unwrap();
    let history = walk_history(&handle, 0).unwrap();
    assert!(history.shallow);
    assert!(!history.truncated);
    assert!(history.issues.is_empty());
    assert!(!history.is_complete());
    assert_eq!(history.commits.len(), 1);

    std::fs::write(shallow.join(".git/refs/heads/broken"), b"invalid ref\n").unwrap();
    let handle = editchain_git::open_repository(&descriptor).unwrap();
    let history = walk_history(&handle, 0).unwrap();
    assert!(!history.issues.is_empty());
    assert_eq!(history.commits.len(), 1);
}

#[test]
fn prefix_lookup_distinguishes_absence_ambiguity_and_corruption() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = make_repo(tmp.path());
    let descriptor = editchain_git::RepositoryDiscovery::from_path(&repo).unwrap();
    let handle = editchain_git::open_repository(&descriptor).unwrap();
    let oid = head_oid(&repo).to_hex();
    // A hexadecimal branch name is not object identity evidence.
    run(&repo, &["branch", "0000000"]);
    assert!(resolve_commit_prefix(&handle, "0000000").unwrap().is_none());
    assert!(resolve_commit_prefix(&handle, &"0".repeat(64))
        .unwrap()
        .is_none());

    // A second loose-object name suffices to make the prefix ambiguous; neither
    // matching object's contents may be used to choose an arbitrary winner.
    let other = format!(
        "{}{}",
        oid.get(..39).unwrap(),
        if oid.ends_with('0') { '1' } else { '0' }
    );
    let other_path = repo
        .join(".git/objects")
        .join(other.get(..2).unwrap())
        .join(other.get(2..).unwrap());
    std::fs::write(&other_path, b"unreadable object").unwrap();
    assert!(matches!(
        resolve_commit_prefix(&handle, oid.get(..7).unwrap()),
        Err(ResolutionError::AmbiguousPrefix(_))
    ));
    std::fs::remove_file(other_path).unwrap();

    let object = repo
        .join(".git/objects")
        .join(oid.get(..2).unwrap())
        .join(oid.get(2..).unwrap());
    std::fs::remove_file(&object).unwrap();
    std::fs::write(object, b"unreadable object").unwrap();
    let handle = editchain_git::open_repository(&descriptor).unwrap();
    assert!(matches!(
        resolve_commit_prefix(&handle, oid.get(..7).unwrap()),
        Err(ResolutionError::Decode(_))
    ));
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
