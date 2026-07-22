use serde::{Deserialize, Serialize};

use editchain_core::ids::NodeId;
use editchain_core::op::FrontierSet;
use editchain_core::payload::Payload;

/// Sync protocol messages for peer-to-peer operation exchange.
///
/// Transport is out of scope. These messages are encoded and sent
/// over whatever transport the gateway/adapter provides.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum SyncMsg {
    /// Handshake message identifying the peer and its known frontier.
    Hello {
        /// The sending peer's node identifier.
        node: NodeId,
        /// Protocol version negotiated by the peers.
        protocol: u16,
        /// The sending peer's current frontier (set of latest ops per lane).
        frontier: FrontierSet,
    },
    /// Notification that the sender has operations up to the given frontier.
    Have {
        /// The sending peer's current frontier.
        frontier: FrontierSet,
    },
    /// Request for operations within the given ranges.
    Need {
        /// Encoded range descriptors identifying which ops the sender needs.
        ranges: Payload,
    },
    /// Batch of serialized operations being transferred.
    Ops {
        /// Encoded sequence of operations.
        ops: Payload,
    },
    /// Acknowledgment that the receiver has incorporated the sent ops.
    Ack {
        /// The receiver's updated frontier after applying received ops.
        frontier: FrontierSet,
    },
    /// Error response with a machine-readable code.
    Error {
        /// Numeric error code indicating the failure reason.
        code: u16,
    },
}
