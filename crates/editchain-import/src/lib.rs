//! Editchain import adapters — Claude Code and Codex history importers.
//!
//! This crate provides deterministic, idempotent import of Claude Code session
//! files and Codex rollout files into editchain operations. Every physical
//! JSONL line is preserved as a raw `ImportOp`; normalized operations
//! (messages, tools, commands, files) are derived alongside.

use serde as _;

#[cfg(test)]
use editchain_project as _;
#[cfg(test)]
use proptest as _;

/// Capture batches and ordered operation/checkpoint persistence.
pub mod batch;
/// Cursor-based incremental file reading.
pub mod cursor;
/// Import error types.
pub mod error;
/// Deterministic ID derivation for import.
pub mod ids;
/// Main import orchestrator.
pub mod import;
/// Import data models (request, options, report).
pub mod model;
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

pub use cursor::*;
pub use error::*;
pub use ids::*;
pub use model::*;
pub use sink::*;
