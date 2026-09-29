//! Reusable Claude, Codex and human history import adapters.
//!
//! This crate provides deterministic, idempotent import of Claude Code session
//! files, Codex rollouts and human archives into editchain operations. Every physical
//! JSONL line is preserved as a raw `ImportOp`; normalized operations
//! (messages, tools, commands, files) are derived alongside.
//!
//! Start with [`capture_import`] and [`ImportSource`]. Inspect the returned
//! [`batch::ImportBatch`], then persist it through a [`batch::DurableOpSink`].
//! [`native`] exposes reconciliation inputs without requiring a native runtime.

use serde as _;

#[cfg(test)]
use editchain_engine as _;
#[cfg(test)]
use proptest as _;

/// Schema-three conversion shared by imports, migration, and live capture.
pub mod activity;
/// Capture batches and ordered operation/checkpoint persistence.
pub mod batch;
/// Cooperative cancellation of source capture and helper execution.
pub mod cancellation;
/// Provider-neutral capture entry point with uncommitted resumable cursors.
pub mod capture;
/// Cursor-based incremental file reading.
pub mod cursor;
/// Import error types.
pub mod error;
/// Discover once and capture individual files with stable provider-relative identities.
pub mod files;
/// Typed provider observations for host-owned Git reconciliation.
pub mod git_evidence;
/// Exact human archive capture and native recorder identity mappings.
pub mod human;
/// Deterministic ID derivation for import.
pub mod ids;
/// Main import orchestrator.
pub mod import;
/// Import data models (request, options, report).
pub mod model;
/// Native provider identities bound to their retained raw evidence.
pub mod native;
/// Pluggable output sinks (ops, blobs, cursors).
pub mod sink;
/// Captured source bytes and shared incremental read plans.
pub mod source_read;
/// Validated, provider-neutral source timestamp parsing.
pub mod source_time;

/// Claude Code session import pipeline.
pub mod claude_code;
/// Codex (OpenAI) session import pipeline.
pub mod codex;

pub use capture::{capture_import, ImportSource};
pub use cursor::*;
pub use error::*;
pub use files::{capture_import_file, discover_import_files, ImportFile};
pub use ids::*;
pub use model::*;
pub use sink::*;
