//! Bounded, transport-independent replication of exact `EditChain` evidence.
//!
//! A replica is bound to one explicitly approved collaboration space. Network
//! authentication must finish before its replication protocol is exposed.

mod content;
mod replica;
mod session;
mod transfer;
mod wire;

pub use replica::{RecordKey, Replica, Snapshot};
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

#[cfg(test)]
mod tests;
