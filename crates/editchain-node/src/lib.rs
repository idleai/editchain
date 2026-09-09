//! Editchain ingestion CLI and segment storage.

/// CLI command implementations.
pub mod commands;
/// Segment file storage.
pub mod segment;

use serde as _;

#[cfg(test)]
use tempfile as _;
