//! Editchain import adapters — Claude Code history parsers.
//!
//! This crate provides deterministic, idempotent import of Claude Code session
//! files into editchain operations. Every physical JSONL line is preserved as
//! a raw `ImportOp`; normalized operations (messages, tools, commands, files)
//! are derived alongside.

use serde as _;

#[cfg(test)]
use proptest as _;
#[cfg(test)]
use tempfile as _;

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

/// Claude Code session import pipeline.
pub mod claude_code;

pub use cursor::*;
pub use error::*;
pub use ids::*;
pub use model::*;
pub use sink::*;
