//! Integration tests for immutable commit file changes.

#![expect(
    clippy::expect_used,
    clippy::indexing_slicing,
    reason = "test fixtures use fail-fast setup and index only after assertions"
)]

use std::process::Command;

use gix_object as _;
use sha2 as _;

use editchain_core::GitOid;
use editchain_git::{
    commit_file_changes, repository_id_from_path, resolve_blob, GitFileStatus, RepositoryDiscovery,
    RepositoryHandle,
};

fn run(repo: &std::path::Path, args: &[&str]) {
    let status = Command::new("git")
        .current_dir(repo)
        .args(args)
        .status()
        .expect("run git");
    assert!(status.success(), "git {args:?} failed");
}

fn commit(repo: &std::path::Path, message: &str) {
    run(repo, &["add", "-A"]);
    run(
        repo,
        &[
            "-c",
            "user.name=Alice",
            "-c",
            "user.email=alice@example.com",
            "commit",
            "-q",
            "-m",
            message,
        ],
    );
}

fn oid(repo: &std::path::Path, revision: &str) -> GitOid {
    let output = Command::new("git")
        .current_dir(repo)
        .args(["rev-parse", revision])
        .output()
        .expect("rev-parse");
    assert!(output.status.success(), "revision should resolve");
    let hex = String::from_utf8(output.stdout).expect("utf8 oid");
    let mut bytes = [0u8; 20];
    for (index, pair) in hex.trim().as_bytes().chunks_exact(2).enumerate() {
        bytes[index] =
            u8::from_str_radix(std::str::from_utf8(pair).expect("ascii"), 16).expect("hex pair");
    }
    GitOid::from_sha1(bytes)
}

fn handle(repo: &std::path::Path) -> RepositoryHandle {
    RepositoryHandle {
        repo: gix::open(repo).expect("open repository"),
        discovery: RepositoryDiscovery {
            id: repository_id_from_path(repo),
            path: repo.to_path_buf(),
            is_worktree: false,
        },
    }
}

#[test]
fn commit_changes_cover_root_modify_binary_rename_and_delete() {
    let temp = tempfile::tempdir().expect("tempdir");
    let repo = temp.path().join("repo");
    std::fs::create_dir(&repo).expect("create repo");
    run(&repo, &["init", "-q", "-b", "main"]);

    std::fs::write(repo.join("alpha.txt"), b"one\n").expect("write root file");
    commit(&repo, "root");
    let root = commit_file_changes(&handle(&repo), &oid(&repo, "HEAD")).expect("root diff");
    assert_eq!(root.len(), 1);
    assert_eq!(root[0].path, "alpha.txt");
    assert_eq!(root[0].status, GitFileStatus::Added);
    assert!(!root[0].binary);
    let root_blob = resolve_blob(
        &handle(&repo),
        root[0].new_oid.as_ref().expect("new root blob"),
    )
    .expect("resolve root blob");
    assert_eq!(root_blob.bytes, b"one\n");
    assert!(!root_blob.binary);

    std::fs::write(repo.join("alpha.txt"), b"two\n").expect("modify text");
    std::fs::write(repo.join("binary.bin"), [0, 1, 2, 3]).expect("write binary");
    commit(&repo, "modify and add binary");
    let changed = commit_file_changes(&handle(&repo), &oid(&repo, "HEAD")).expect("second diff");
    assert_eq!(changed.len(), 2);
    let modified = changed
        .iter()
        .find(|change| change.path == "alpha.txt")
        .expect("modified path");
    assert_eq!(modified.status, GitFileStatus::Modified);
    assert!(!modified.binary);
    let binary = changed
        .iter()
        .find(|change| change.path == "binary.bin")
        .expect("binary path");
    assert_eq!(binary.status, GitFileStatus::Added);
    assert!(binary.binary);

    run(&repo, &["mv", "alpha.txt", "renamed.txt"]);
    commit(&repo, "rename");
    let renamed = commit_file_changes(&handle(&repo), &oid(&repo, "HEAD")).expect("rename diff");
    let rename = renamed
        .iter()
        .find(|change| change.path == "renamed.txt")
        .expect("renamed path");
    assert_eq!(rename.status, GitFileStatus::Renamed);
    assert_eq!(rename.old_path.as_deref(), Some("alpha.txt"));

    std::fs::remove_file(repo.join("renamed.txt")).expect("delete renamed file");
    commit(&repo, "delete");
    let deleted = commit_file_changes(&handle(&repo), &oid(&repo, "HEAD")).expect("delete diff");
    let deletion = deleted
        .iter()
        .find(|change| change.path == "renamed.txt")
        .expect("deleted path");
    assert_eq!(deletion.status, GitFileStatus::Deleted);
    assert!(deletion.old_oid.is_some());
    assert!(deletion.new_oid.is_none());
}

#[test]
fn merge_commit_uses_first_parent_tree() {
    let temp = tempfile::tempdir().expect("tempdir");
    let repo = temp.path().join("repo");
    std::fs::create_dir(&repo).expect("create repo");
    run(&repo, &["init", "-q", "-b", "main"]);
    std::fs::write(repo.join("base.txt"), b"base\n").expect("write base");
    commit(&repo, "base");

    run(&repo, &["checkout", "-q", "-b", "side"]);
    std::fs::write(repo.join("side.txt"), b"side\n").expect("write side");
    commit(&repo, "side");
    run(&repo, &["checkout", "-q", "main"]);
    std::fs::write(repo.join("main.txt"), b"main\n").expect("write main");
    commit(&repo, "main");
    run(
        &repo,
        &[
            "-c",
            "user.name=Alice",
            "-c",
            "user.email=alice@example.com",
            "merge",
            "-q",
            "--no-ff",
            "side",
            "-m",
            "merge side",
        ],
    );

    let changes = commit_file_changes(&handle(&repo), &oid(&repo, "HEAD")).expect("merge diff");
    assert_eq!(changes.len(), 1);
    assert_eq!(changes[0].path, "side.txt");
    assert_eq!(changes[0].status, GitFileStatus::Added);
}
