//! Canonical admission, conflict retention, and merge tests.

// Referenced by library derive macros; suppress unused-crate-dependencies lint.
use postcard as _;
use proptest as _;
use serde as _;

use editchain_core::{Admission, NodeId, OpId, OpSet};

#[test]
fn opset_insert_accepts_new() {
    let mut set = OpSet::new();
    let id = OpId::new(NodeId(1), 0, 1);
    assert_eq!(set.insert(id, vec![1, 2, 3]), Admission::Accepted);
    assert!(set.contains(&id));
    assert_eq!(set.len(), 1);
}

#[test]
fn opset_insert_duplicate() {
    let mut set = OpSet::new();
    let id = OpId::new(NodeId(1), 0, 1);
    let _: Admission = set.insert(id, vec![1, 2, 3]);
    assert_eq!(set.insert(id, vec![1, 2, 3]), Admission::Duplicate);
}

#[test]
fn opset_insert_quarantine() {
    let mut set = OpSet::new();
    let id = OpId::new(NodeId(1), 0, 1);
    let _: Admission = set.insert(id, vec![1, 2, 3]);
    let result = set.insert(id, vec![4, 5, 6]);
    assert_eq!(result, Admission::Conflict);
    assert!(!set.contains(&id));
    assert!(set.is_empty());
    assert_eq!(set.evidence().count(), 2);
    assert_eq!(set.conflicts().count(), 1);
}

#[test]
fn opset_merge_counts() {
    let mut a = OpSet::new();
    let mut b = OpSet::new();

    let _: Admission = a.insert(OpId::new(NodeId(1), 0, 1), vec![1]);
    let _: Admission = a.insert(OpId::new(NodeId(1), 0, 2), vec![2]);
    let _: Admission = b.insert(OpId::new(NodeId(1), 0, 2), vec![2]); // duplicate
    let _: Admission = b.insert(OpId::new(NodeId(2), 0, 1), vec![3]); // new

    let (accepted, duplicates, quarantined) = a.merge(&b);
    assert_eq!(accepted, 1);
    assert_eq!(duplicates, 1);
    assert_eq!(quarantined, 0);
}

#[test]
fn conflict_evidence_survives_permutations_empty_merges_and_replays() {
    let id = OpId::new(NodeId(1), 0, 1);
    let permutations = [
        [1, 2, 3],
        [1, 3, 2],
        [2, 1, 3],
        [2, 3, 1],
        [3, 1, 2],
        [3, 2, 1],
    ];
    for versions in permutations {
        let mut source = OpSet::new();
        for version in versions {
            let _: Admission = source.insert(id, vec![version]);
        }
        let mut replica = OpSet::new();
        let _: (usize, usize, usize) = replica.merge(&source);
        assert_eq!(replica, source);
        assert!(replica.is_empty());
        assert_eq!(
            replica
                .evidence()
                .map(|(_, bytes)| bytes)
                .collect::<Vec<_>>(),
            vec![&[1][..], &[2][..], &[3][..]]
        );
        for version in versions {
            assert_eq!(replica.insert(id, vec![version]), Admission::Duplicate);
        }
        assert_eq!(replica, source);
    }
}
