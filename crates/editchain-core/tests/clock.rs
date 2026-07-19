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