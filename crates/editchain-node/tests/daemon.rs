//! Daemon tests.

use clap as _;
use dirs as _;
use editchain_codec as _;
use editchain_core as _;
use editchain_embed as _;
use editchain_import as _;
use editchain_query as _;
use serde as _;
use serde_json as _;
use tempfile as _;

use editchain_index::snapshot::QuerySnapshot;
use editchain_node::daemon::{AppendCoordinator, ProjectionWatermarks, QueryPlane};

#[test]
fn append_coordinator_monotonic() {
    let coord = AppendCoordinator::new("/tmp/test-chain");
    let g1 = coord.next_generation();
    let g2 = coord.next_generation();
    assert!(g2 > g1);
    assert_eq!(coord.current_generation(), g2);
}

#[test]
fn watermarks_defaults() {
    let wm = ProjectionWatermarks::new();
    assert_eq!(wm.log, 0);
    // With log=0, all projections are vacuously caught up.
    assert!(wm.is_fully_consistent());
}

#[test]
fn watermarks_inconsistent_when_log_ahead() {
    let mut wm = ProjectionWatermarks::new();
    wm.log = 10;
    // Projections haven't caught up.
    assert!(!wm.is_fully_consistent());
    assert!(!wm.is_lexical_consistent());
}

#[test]
fn watermarks_consistency() {
    let mut wm = ProjectionWatermarks::new();
    wm.log = 5;
    wm.hydrated = 5;
    wm.graph = 5;
    wm.lexical = 5;
    wm.vector = 5;
    assert!(wm.is_fully_consistent());
    assert!(wm.is_lexical_consistent());

    wm.vector = 3;
    assert!(!wm.is_fully_consistent());
    assert!(wm.is_lexical_consistent());
}

#[test]
fn query_plane_update_and_read() {
    let plane = QueryPlane::new();
    let snap = plane.snapshot();
    assert_eq!(snap.hydrated_generation, 0);

    let mut new_snap = QuerySnapshot::new();
    new_snap.hydrated_generation = 42;
    plane.update(new_snap);

    let snap = plane.snapshot();
    assert_eq!(snap.hydrated_generation, 42);
}
