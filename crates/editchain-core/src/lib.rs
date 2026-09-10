#![doc = "Editchain core types — CRDT schema, IDs, merge, and canonical reducers."]
// Public API types are consumed by other workspace crates; not dead code.
#![allow(
    dead_code,
    reason = "Public API types consumed by other workspace crates"
)]

#[cfg(test)]
use postcard as _;
#[cfg(test)]
use proptest as _;

/// Canonical admission and retained conflict evidence.
pub mod admission;
/// Clock types for causal ordering.
pub mod clock;
/// Git identity, commit, and explicit-link types.
pub mod git;
/// Identifier types (`NodeId`, `ActorId`, `OpId`, etc.).
pub mod ids;
/// Operation envelope and all operation kinds.
pub mod op;
/// Parent reference types for causal DAG ordering.
pub mod parents;
/// Payload types (`ContentId`, `BlobRef`, Payload).
pub mod payload;
/// Scope reference types (chain, session, turn, file).
pub mod scope;
/// State types (`OpSet`, `BlobSet`, `ChainState`, reducers).
pub mod state;
/// Tag bitflags for operation filtering.
pub mod tags;

// Re-exports for convenience.
pub use admission::*;
pub use clock::*;
pub use git::*;
pub use ids::*;
pub use op::*;
pub use parents::*;
pub use payload::*;
pub use scope::*;
pub use state::*;
pub use tags::*;
