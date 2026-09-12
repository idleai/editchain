//! Native `EditChain` application: ingestion, history queries, and framed stdio.
//!
//! The two executables share the [`commands`] and [`history`] facades. [`Server`]
//! adapts protocol requests to that history backend. Persistence belongs to
//! `editchain_store`; reconciliation and transport remain internal modules.

pub mod commands;
mod editor;
pub mod history;
mod reconcile;
mod transport;

pub use transport::Server;

use ctrlc as _;
use serde as _;

#[cfg(test)]
use tempfile as _;
