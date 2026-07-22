#![cfg_attr(not(feature = "use-std"), no_std)]
#![doc = "Editchain core types — `no_std` CRDT schema, IDs, merge, and canonical reducers."]
// Public API types are consumed by other workspace crates; not dead code.
#![allow(
    dead_code,
    reason = "Public API types consumed by other workspace crates"
)]

#[cfg(not(feature = "use-std"))]
extern crate alloc;

// Referenced by sibling crates via serde Serialize/Deserialize derives.
// Import unconditionally to satisfy unused-crate-dependencies lint.
use postcard as _;

#[cfg(test)]
use proptest as _;

/// Clock types for causal ordering.
pub mod clock;
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
pub use clock::*;
pub use ids::*;
pub use op::*;
pub use parents::*;
pub use payload::*;
pub use scope::*;
pub use state::*;
pub use tags::*;
