#![doc = "Editchain immutable operation schema, identities, and canonical admission."]

#[cfg(test)]
use postcard as _;
#[cfg(test)]
use proptest as _;
#[cfg(test)]
use serde_json as _;

/// Schema-three operations for imported and streaming activity.
pub mod activity;
/// Canonical admission and retained conflict evidence.
pub mod admission;
/// Clock types for causal ordering.
pub mod clock;
/// Git identity, commit, and explicit-link types.
pub mod git;
/// Human work episodes and exact observed buffer revisions.
pub mod human;
/// Query prefixes over canonical operation identities.
pub mod id_query;
/// Identifier types (`NodeId`, `ActorId`, `OpId`, etc.).
pub mod ids;
pub use id_query::IdQuery;
/// Frozen legacy wire types for explicit migration.
pub mod legacy;
/// Operation envelope and all operation kinds.
pub mod op;
/// Parent reference types for causal DAG ordering.
pub mod parents;
/// Payload types (`ContentId`, `BlobRef`, Payload).
pub mod payload;
/// Typed provider identity and immutable source/lifecycle evidence.
pub mod provider;
/// Shared record names over the canonical operation envelope and payloads.
pub mod records;
/// Scope reference types (chain, session, turn, file).
pub mod scope;
/// Tag bitflags for operation filtering.
pub mod tags;
/// Shared provider-neutral history classifications; no projection algorithms.
pub mod taxonomy;

// Re-exports for convenience.
pub use admission::*;
pub use clock::*;
pub use git::*;
pub use ids::*;
pub use op::*;
pub use parents::*;
pub use payload::*;
pub use scope::*;
pub use tags::*;
