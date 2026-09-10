//! Shared history backend and framed-protocol adapter for `EditChain` viewers.
//!
//! CLI callers use [`history`] directly. The stdio executable uses [`Server`]
//! to dispatch requests through the same backend. Root exports retain the
//! existing Rust API while the backend has its own explicit namespace.

pub mod history;
mod transport;

pub use history::{
    build_lexical_index, hydrate_blob_payloads, parse_git_oid, parse_repository_id,
    prepare_render_snapshot, resolve_git_commit, BlobHydrationStats, BlobResolution, BlobResolver,
    ChainReadStats, GitReadStats, HistoryWindowOptions, OpenDiagnostics, RenderSnapshotReport,
    SearchIndexState, Workspace,
};
pub use transport::Server;
