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
    Hello {
        node: NodeId,
        protocol: u16,
        frontier: FrontierSet,
    },
    Have {
        frontier: FrontierSet,
    },
    Need {
        ranges: Payload,
    },
    Ops {
        ops: Payload,
    },
    Ack {
        frontier: FrontierSet,
    },
    Error {
        code: u16,
    },
}