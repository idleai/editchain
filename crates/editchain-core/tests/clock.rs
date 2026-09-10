//! Clock ordering tests.

#![expect(
    unused_crate_dependencies,
    reason = "Test file; dependencies used by library macros"
)]

use editchain_core::clock::Clock;

#[test]
fn clock_ordering() {
    let a = Clock::UnixMs(100);
    let b = Clock::UnixMs(200);
    assert!(a < b);

    let c = Clock::Hybrid { ms: 100, ctr: 0 };
    let d = Clock::Hybrid { ms: 100, ctr: 1 };
    assert!(c < d);
}

#[test]
fn observed_time_respects_clock_domain_and_legacy_zero() {
    for clock in [
        Clock::None,
        Clock::Lamport(1_700_000_000_000),
        Clock::UnixMs(0),
        Clock::Hybrid { ms: 0, ctr: 7 },
    ] {
        assert_eq!(clock.observed_unix_ms(), None, "{clock:?}");
    }
    for clock in [Clock::UnixMs(42), Clock::Hybrid { ms: 42, ctr: 7 }] {
        assert_eq!(clock.observed_unix_ms(), Some(42));
    }
}

#[test]
fn clock_wire_variants_retain_their_bytes() {
    for (clock, bytes) in [
        (Clock::None, vec![0]),
        (Clock::Lamport(42), vec![1, 42]),
        (Clock::UnixMs(42), vec![2, 42]),
        (Clock::Hybrid { ms: 42, ctr: 7 }, vec![3, 42, 7]),
    ] {
        assert_eq!(postcard::to_stdvec(&clock).unwrap(), bytes);
        assert_eq!(postcard::from_bytes::<Clock>(&bytes).unwrap(), clock);
    }
}

#[test]
fn explicit_unknown_source_time_overrides_observed_clock() {
    use editchain_core::{
        ActorId, NodeId, Op, OpId, OpKind, ParentSet, Payload, ScopeRef, Tags, UnknownOp,
    };
    let mut op = Op {
        id: OpId::new(NodeId(1), 0, 1),
        parents: ParentSet::None,
        actor: ActorId(1),
        clock: Clock::Hybrid { ms: 42, ctr: 7 },
        scope: ScopeRef::None,
        tags: Tags::NONE,
        kind: OpKind::Unknown(UnknownOp {
            kind_discriminant: 1,
            raw_bytes: Payload::Empty,
        }),
    };
    assert_eq!(op.observed_unix_ms(), Some(42));
    op.tags |= Tags::SOURCE_TIME_UNKNOWN;
    let original = op.clone();
    assert_eq!(op.observed_unix_ms(), None);
    assert_eq!(op, original);
}
