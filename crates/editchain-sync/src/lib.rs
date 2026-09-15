//! Bounded, transport-independent replication of exact `EditChain` evidence.
//!
//! A replica is bound to one explicitly approved collaboration space. Network
//! authentication must finish before its replication protocol is exposed.

mod content;
mod identity;
mod ipc;
mod membership;
mod replica;
mod secure;
mod session;
mod transfer;
mod wire;

pub use identity::{DeviceIdentity, PublicDevice};
pub use ipc::{run_worker, MAX_CONTROL_BYTES};
pub use membership::{Membership, MAX_DEVICES};
pub use replica::{RecordKey, Replica, Snapshot};
pub use secure::{SecurePeer, MAX_BRIDGE_BYTES, MAX_BRIDGE_OUTPUT};
pub use session::{Progress, Session};
pub use wire::{decode_message, encode_message, FrameDecoder, Message};

/// Largest operation or blob accepted by the first replication protocol.
pub const MAX_OBJECT_BYTES: usize = 64 * 1024 * 1024;
/// Largest data chunk carried by a peer message.
pub const CHUNK_BYTES: usize = 64 * 1024;
/// Largest encoded peer message, checked before allocation.
pub const MAX_FRAME_BYTES: usize = 128 * 1024;
/// Maximum record identities in one inventory page.
pub const INVENTORY_PAGE: usize = 128;

fn invalid(message: &'static str) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidData, message)
}

// Local capture and peer workers serialize short durable transactions. Wait in
// this native worker, without holding a lock or blocking the extension host.
// A persistently busy writer still fails without an acknowledgment; reconnect
// can repair from the durable inventory.
fn writer(root: &std::path::Path) -> std::io::Result<editchain_store::SegmentStore> {
    let started = std::time::Instant::now();
    loop {
        match editchain_store::SegmentStore::open(root) {
            Err(error)
                if error.kind() == std::io::ErrorKind::WouldBlock
                    && started.elapsed() < std::time::Duration::from_secs(2) =>
            {
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            result => return result,
        }
    }
}

#[cfg(test)]
mod tests;
