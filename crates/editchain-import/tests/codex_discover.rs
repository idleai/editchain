//! Codex rollout discovery tests.

use blake3 as _;
use editchain_core as _;
use editchain_project as _;
use proptest as _;
use serde as _;
use serde_json as _;
use sha2 as _;
use tempfile as _;

use editchain_import::codex::discover::discover_rollouts;

#[test]
fn discover_empty_root() {
    let dir = tempfile::tempdir().unwrap();
    let rollouts = discover_rollouts(dir.path()).unwrap();
    assert!(rollouts.is_empty());
}

#[test]
#[expect(
    clippy::indexing_slicing,
    reason = "test assertions on known-length vec"
)]
fn discover_single_rollout() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("rollout-2026-08-26T12-00-00-000.jsonl");
    std::fs::write(&path, "{}\n").unwrap();

    let rollouts = discover_rollouts(dir.path()).unwrap();
    assert_eq!(rollouts.len(), 1);
    assert_eq!(rollouts[0].path, path);
    assert_eq!(rollouts[0].file_size, 3);
    assert_eq!(rollouts[0].session_id, "rollout-2026-08-26T12-00-00-000");
}

#[test]
#[expect(
    clippy::indexing_slicing,
    reason = "test assertions on known-length vec"
)]
fn discover_recurses_date_trees_deterministically_sorted() {
    let dir = tempfile::tempdir().unwrap();
    let date1 = dir.path().join("2026-08-26");
    let date2 = dir.path().join("2026-08-27");
    let nested = date2.join("nested");
    std::fs::create_dir_all(&date1).unwrap();
    std::fs::create_dir_all(&nested).unwrap();

    // Scrambled creation order — discovery must sort deterministically.
    std::fs::write(nested.join("rollout-c.jsonl"), "{}\n").unwrap();
    std::fs::write(date1.join("rollout-a.jsonl"), "{}\n").unwrap();
    std::fs::write(dir.path().join("rollout-b.jsonl"), "{}\n").unwrap();

    let rollouts = discover_rollouts(dir.path()).unwrap();
    let paths: Vec<_> = rollouts.iter().map(|r| r.path.clone()).collect();
    let mut expected: Vec<_> = paths.clone();
    expected.sort();
    assert_eq!(paths, expected, "rollouts are returned sorted by path");
    assert_eq!(paths.len(), 3);
    assert_eq!(
        rollouts[0].session_id, "rollout-a",
        "date-tree subdirectory rollout uses its filename stem as fallback id"
    );
}

#[test]
#[expect(
    clippy::indexing_slicing,
    reason = "test assertions on known-length vec"
)]
fn discover_ignores_non_rollout_files() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("rollout-1.jsonl"), "{}\n").unwrap();
    std::fs::write(dir.path().join("rollout.jsonl"), "{}\n").unwrap();
    std::fs::write(dir.path().join("rollout-2.jsonl.bak"), "{}\n").unwrap();
    std::fs::write(dir.path().join("other.jsonl"), "{}\n").unwrap();
    std::fs::write(dir.path().join("README.md"), "{}\n").unwrap();

    let rollouts = discover_rollouts(dir.path()).unwrap();
    assert_eq!(rollouts.len(), 1);
    assert!(rollouts[0].path.ends_with("rollout-1.jsonl"));
}
