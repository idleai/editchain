//! Editchain node — segment files, CLI commands, JSON export, daemon.

/// CLI command implementations.
pub mod commands;
/// Daemon with append coordinator and projector bus.
pub mod daemon;
/// JSON export utilities.
pub mod export;
/// Segment file storage.
pub mod segment;
/// Service layer.
pub mod services;

use serde as _;

#[cfg(test)]
use tempfile as _;
