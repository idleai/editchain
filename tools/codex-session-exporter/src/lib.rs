//! Isolated Codex rollout projection bridge.
//!
//! Reads Codex rollout JSONL and emits a typed, versioned, deterministic
//! NDJSON projection (`editchain-v1`) that EditChain adapters can map to
//! neutral Message/Tool/Command ops without linking Codex crates.

pub mod bridge;
pub mod project;
pub mod schema;
pub mod stream;
