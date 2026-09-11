//! Property-based tests for CRDT merge invariants.
//!
//! Verifies that `OpSet` merge is:
//! - Commutative:  merge(A, B) == merge(B, A)
//! - Associative:  merge(merge(A, B), C) == merge(A, merge(B, C))
//! - Idempotent:   merge(A, A) == A

#![expect(
    unused_crate_dependencies,
    reason = "Test file; dependencies used by library macros"
)]

use editchain_core::{Admission, NodeId, OpId, OpSet};
use proptest::prelude::*;

/// Generate an arbitrary `OpId`.
fn arb_opid() -> impl Strategy<Value = OpId> {
    prop_oneof![
        4 => (0u64..2, 0u32..2, 0u64..3),
        1 => (any::<u64>(), any::<u32>(), any::<u64>()),
    ]
    .prop_map(|(node, boot, seq)| OpId::new(NodeId(node), boot, seq))
}

/// Generate an arbitrary encoded operation (just random bytes keyed by `OpId`).
fn arb_op_entry() -> impl Strategy<Value = (OpId, Vec<u8>)> {
    arb_opid().prop_flat_map(|id| {
        let id_clone = id;
        (
            Just(id_clone),
            proptest::collection::vec(any::<u8>(), 1..64),
        )
    })
}

/// Generate a small `OpSet` from a list of entries.
fn arb_opset(max_ops: usize) -> impl Strategy<Value = OpSet> {
    proptest::collection::vec(arb_op_entry(), 0..max_ops).prop_map(|entries| {
        let mut opset = OpSet::new();
        for (id, bytes) in entries {
            // Retain conflicts too: merge laws must cover all evidence.
            let _: Admission = opset.insert(id, bytes);
        }
        opset
    })
}

proptest! {
    #![proptest_config(ProptestConfig {
        cases: 256,
        ..ProptestConfig::default()
    })]

    // -----------------------------------------------------------------------
    // Commutativity: merge(A, B) should produce the same state as merge(B, A)
    // -----------------------------------------------------------------------
    #[expect(
        clippy::let_underscore_untyped,
        reason = "Test helper; discarding merge result is intentional"
    )]
    #[test]
    fn merge_commutative(
        a in arb_opset(10),
        b in arb_opset(10),
    ) {
        let mut merged_ab = OpSet::new();
        let _ = merged_ab.merge(&a);
        let _ = merged_ab.merge(&b);

        let mut merged_ba = OpSet::new();
        let _ = merged_ba.merge(&b);
        let _ = merged_ba.merge(&a);

        // Both orders should result in the same set of accepted ops.
        prop_assert_eq!(&merged_ab, &merged_ba);
        prop_assert_eq!(merged_ab.len(), merged_ba.len());
        prop_assert_eq!(merged_ab.conflicts().count(), merged_ba.conflicts().count());

        // Every op in merged_ab should be in merged_ba and vice versa.
        for (id, bytes) in merged_ab.iter() {
            let id_str = format!("{id}");
            prop_assert!(merged_ba.contains(id), "op {} missing from BA merge", id_str);
            // Check bytes match (same OpId → same bytes in both orders).
            let ba_bytes = merged_ba.iter().find(|(k, _)| *k == id).map(|(_, v)| v);
            prop_assert_eq!(Some(bytes), ba_bytes, "bytes differ for op {}", id_str);
        }
    }

    // -----------------------------------------------------------------------
    // Associativity: merge(merge(A, B), C) == merge(A, merge(B, C))
    // -----------------------------------------------------------------------
    #[expect(
        clippy::let_underscore_untyped,
        reason = "Test helper; discarding merge result is intentional"
    )]
    #[test]
    fn merge_associative(
        a in arb_opset(8),
        b in arb_opset(8),
        c in arb_opset(8),
    ) {
        // Left-associative: (A ∪ B) ∪ C
        let mut left = OpSet::new();
        let _ = left.merge(&a);
        let _ = left.merge(&b);
        let _ = left.merge(&c);

        // Right-associative: A ∪ (B ∪ C)
        let mut bc = OpSet::new();
        let _ = bc.merge(&b);
        let _ = bc.merge(&c);
        let mut right = OpSet::new();
        let _ = right.merge(&a);
        let _ = right.merge(&bc);

        prop_assert_eq!(&left, &right);
        prop_assert_eq!(left.len(), right.len());
        prop_assert_eq!(left.conflicts().count(), right.conflicts().count());

        for (id, bytes) in left.iter() {
            let id_str = format!("{id}");
            prop_assert!(right.contains(id), "op {} missing from right-assoc merge", id_str);
            let r_bytes = right.iter().find(|(k, _)| *k == id).map(|(_, v)| v);
            prop_assert_eq!(Some(bytes), r_bytes);
        }
    }

    // -----------------------------------------------------------------------
    // Idempotency: merge(A, A) == A
    // -----------------------------------------------------------------------
    #[expect(
        clippy::let_underscore_untyped,
        reason = "Test helper; discarding merge result is intentional"
    )]
    #[test]
    fn merge_idempotent(
        a in arb_opset(10),
    ) {
        let mut merged = OpSet::new();
        let _ = merged.merge(&a);

        // Merging again should not change anything.
        let evidence_before = merged.evidence().count();
        let (accepted, duplicates, quarantined) = merged.merge(&a);
        prop_assert_eq!(accepted, 0);
        prop_assert_eq!(duplicates, evidence_before);
        prop_assert_eq!(quarantined, 0);
        prop_assert_eq!(merged, a);
    }

    // -----------------------------------------------------------------------
    // Every record survives; visibility requires unambiguous bytes.
    // -----------------------------------------------------------------------
    #[test]
    fn insert_retains_evidence_and_excludes_conflicts(
        entries in proptest::collection::vec(arb_op_entry(), 0..20),
    ) {
        let mut opset = OpSet::new();
        for (id, bytes) in &entries {
            let _: Admission = opset.insert(*id, bytes.clone());
        }
        for (id, bytes) in &entries {
            prop_assert!(opset.evidence().any(|(key, value)| key == id && value == bytes));
            let has_conflict = entries.iter().any(|(other_id, other_bytes)| other_id == id && other_bytes != bytes);
            prop_assert_eq!(opset.contains(id), !has_conflict);
        }
        prop_assert_eq!(opset.len(), opset.iter().count());
    }
}
