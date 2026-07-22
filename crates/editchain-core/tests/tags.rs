//! Tag bitflag tests.

// Referenced by library derive macros; suppress unused-crate-dependencies lint.
use postcard as _;
use proptest as _;
use serde as _;

use editchain_core::tags::Tags;

#[test]
fn tag_constants() {
    let t = Tags::AGENT | Tags::FILE;
    assert!(t.matches_any(Tags::AGENT));
    assert!(t.matches_any(Tags::FILE));
    assert!(!t.matches_any(Tags::HUMAN));
    assert!(t.matches_all(Tags::AGENT | Tags::FILE));
    assert!(!t.matches_all(Tags::AGENT | Tags::FILE | Tags::HUMAN));
}

#[test]
fn tag_display() {
    assert_eq!(format!("{}", Tags::NONE), "none");
    assert_eq!(format!("{}", Tags::AGENT), "agent");
    let t = Tags::AGENT | Tags::FILE;
    let s = format!("{t}");
    assert!(s.contains("agent"));
    assert!(s.contains("file"));
}
