//! Legacy native application: viewer ingestion, history queries, and framed stdio.
//!
//! The standalone engine CLI lives in `crates/editchain`. This host backend
//! remains for its existing consumers until the roadmap's app/extension extraction.
//!
//! The two executables share the [`commands`] and [`history`] facades. [`Server`]
//! adapts protocol requests to that history backend. Persistence belongs to
//! `editchain_store`; reconciliation and transport remain internal modules.

pub mod commands;
mod editor;
pub mod history;
mod receipts;
mod reconcile;
mod transport;

pub use transport::Server;

use ctrlc as _;
use serde as _;

#[cfg(test)]
use tempfile as _;
