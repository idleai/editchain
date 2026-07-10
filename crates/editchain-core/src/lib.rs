#![cfg_attr(not(feature = "use-std"), no_std)]
#![doc = "Editchain core types — no_std CRDT schema, IDs, merge, and canonical reducers."]

extern crate alloc;

pub mod ids;
pub mod payload;
pub mod tags;
pub mod clock;
pub mod scope;
pub mod parents;
pub mod op;
pub mod state;

// Re-exports for convenience.
pub use ids::*;
pub use payload::*;
pub use tags::*;
pub use clock::*;
pub use scope::*;
pub use parents::*;
pub use op::*;
pub use state::*;