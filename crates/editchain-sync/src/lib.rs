#![cfg_attr(not(feature = "use-std"), no_std)]
#![doc = "Editchain sync state machine — `no_std` frontiers and sync messages."]

/// Sync protocol message types for peer-to-peer operation exchange.
pub mod msg;

/// Retain postcard as a required dependency for downstream encoding/decoding.
use postcard as _;
