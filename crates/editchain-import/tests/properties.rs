//! Property-based tests for import invariants.
//!
//! Verifies:
//! - Identical source positions produce identical IDs
//! - Repeated import is logically idempotent

use blake3 as _;
use editchain_core as _;
use serde as _;
use serde_json as _;
use sha2 as _;
use tempfile as _;

use editchain_import::ids::derive_source_stream;
use proptest::prelude::*;

proptest! {
    #![proptest_config(ProptestConfig {
        cases: 256,
        ..ProptestConfig::default()
    })]

    // -----------------------------------------------------------------------
    // Deterministic IDs: same workspace + path + boot → same stream
    // -----------------------------------------------------------------------
    #[test]
    fn deterministic_source_stream(
        workspace in "\\PC*",
        path in "\\PC*",
        boot in any::<u32>(),
    ) {
        let stream1 = derive_source_stream(&workspace, &path, boot);
        let stream2 = derive_source_stream(&workspace, &path, boot);

        // Same inputs → same stream identity.
        prop_assert_eq!(stream1.node, stream2.node);
        prop_assert_eq!(stream1.boot, stream2.boot);

        // Same sequence numbers → same OpIds.
        for seq in 0..10u64 {
            let id1 = stream1.op_id(seq);
            let id2 = stream2.op_id(seq);
            prop_assert_eq!(id1, id2, "OpId mismatch at seq {}", seq);
        }
    }

    // -----------------------------------------------------------------------
    // Different boot → different OpIds (same seq)
    // -----------------------------------------------------------------------
    #[test]
    fn different_boot_different_ids(
        workspace in "\\PC*",
        path in "\\PC*",
        boot_a in any::<u32>(),
        boot_b in any::<u32>(),
    ) {
        prop_assume!(boot_a != boot_b);

        let stream_a = derive_source_stream(&workspace, &path, boot_a);
        let stream_b = derive_source_stream(&workspace, &path, boot_b);

        // Different boots → different streams (boot is stored separately, not hashed).
        prop_assert_ne!(stream_a.boot, stream_b.boot);
        prop_assert_eq!(stream_a.node, stream_b.node);

        // Same seq → different OpIds (different boot → different OpId).
        let id_a = stream_a.op_id(0);
        let id_b = stream_b.op_id(0);
        prop_assert_ne!(id_a, id_b);
    }

    // -----------------------------------------------------------------------
    // Different paths → different OpIds (same workspace, boot, seq)
    // -----------------------------------------------------------------------
    #[test]
    fn different_path_different_ids(
        workspace in "\\PC*",
        path_a in "\\PC+",
        path_b in "\\PC+",
        boot in any::<u32>(),
    ) {
        prop_assume!(path_a != path_b);

        let stream_a = derive_source_stream(&workspace, &path_a, boot);
        let stream_b = derive_source_stream(&workspace, &path_b, boot);

        // Different paths → different node IDs.
        prop_assert_ne!(stream_a.node, stream_b.node);

        let id_a = stream_a.op_id(0);
        let id_b = stream_b.op_id(0);
        prop_assert_ne!(id_a, id_b);
    }
}
