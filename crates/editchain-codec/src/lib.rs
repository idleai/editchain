#![cfg_attr(not(feature = "use-std"), no_std)]
#![doc = "Editchain binary codec — `no_std` frame encoding via postcard."]
#![cfg_attr(not(feature = "use-std"), allow(unused_extern_crates))]

use serde as _;

#[cfg(test)]
use proptest as _;

/// EC02/EC03 frame-level encoding and decoding.
pub mod frame;

/// EC02 page-level encoding and decoding.
pub mod page;
