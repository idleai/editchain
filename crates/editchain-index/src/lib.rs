//! Copy-on-write derived indexes. A checkpoint contains page addresses, not a
//! serialized heap. Readers hydrate only traversed pages; committing writes
//! dirty pages before atomically publishing a new root.

mod map;
mod ordered;
mod page;
pub mod rank;
mod storage;

pub use map::{Entry, Map};
pub use ordered::{OrderedEntry, OrderedMap, OrderedSet, Range};
pub use page::{boundary, Page};
pub use storage::Storage;

#[cfg(test)]
mod tests;
