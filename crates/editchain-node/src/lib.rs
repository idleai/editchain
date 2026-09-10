//! Editchain ingestion CLI, Git reconciliation, and segment storage.

/// CLI command implementations.
pub mod commands;
/// Read-only Git relationship planning over canonical provider evidence.
pub mod reconcile;
/// Segment file storage.
pub mod segment;

use ctrlc as _;
use serde as _;

#[cfg(test)]
use tempfile as _;
