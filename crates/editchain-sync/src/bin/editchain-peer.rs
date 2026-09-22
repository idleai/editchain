//! Dedicated native peer worker; stdin is local control, never a network socket.

// Dependencies belong to the library; keep the workspace's binary dependency
// audit explicit without broad lint suppressions.
use base64 as _;
use blake3 as _;
use editchain_core as _;
use editchain_store as _;
use postcard as _;
use rcgen as _;
use rustls as _;
use serde as _;
use serde_json as _;
#[cfg(test)]
use tempfile as _;
// The editor recorder is a dev-only dependency of this crate's tests.
#[cfg(test)]
use editchain_node as _;
#[cfg(test)]
use editchain_protocol as _;

fn main() -> std::io::Result<()> {
    editchain_sync::run_worker(&mut std::io::stdin().lock(), &mut std::io::stdout().lock())
}
