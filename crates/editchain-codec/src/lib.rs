#![doc = "Editchain binary codec — frame encoding via postcard."]

use serde as _;

#[cfg(test)]
use proptest as _;

/// EC02/EC03 frame-level encoding and decoding.
pub mod frame;

/// EC02 page-level encoding and decoding.
pub mod page;

/// Shared bounded scanning of concatenated EC02 pages and record locations.
pub mod scan;
