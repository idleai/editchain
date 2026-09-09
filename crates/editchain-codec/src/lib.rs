#![doc = "Editchain binary codec — frame encoding via postcard."]

use serde as _;

#[cfg(test)]
use proptest as _;

/// EC02/EC03 frame-level encoding and decoding.
pub mod frame;

/// EC02 page-level encoding and decoding.
pub mod page;
