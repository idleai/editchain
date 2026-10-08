//! Rebuildable chain indexes over persisted records and referenced content.
//!
//! [`ChainIndex`] maintains operation lookups, encoded record variants, secondary postings,
//! and verified content availability without requiring a viewer. [`ChainIndex::refresh`]
//! reads the append frontier and retries unresolved content; [`ChainIndex::rebuild`]
//! discards derived state and replays storage. Checkpoints never replace source records or blobs.
//!
//! The original page collection API remains available through re-exports. Its
//! implementation lives in `editchain-index-pages`, below both storage and indexes.
//!
//! ```no_run
//! # fn read(chain: &std::path::Path) -> std::io::Result<()> {
//! use editchain_index::ChainIndex;
//!
//! let mut index = ChainIndex::open(chain)?;
//! let changes = index.refresh()?;
//! for id in changes.added.iter().chain(&changes.content_changed) {
//!     let _operation = index.get(*id)?;
//!     let _content = index.content(*id)?;
//! }
//! let _report = index.verify_integrity()?;
//! # Ok(())
//! # }
//! ```

mod chain;
mod changes;
mod content;
mod integrity;
mod keys;
mod references;
mod state;

pub use chain::{ChainIndex, IndexDelta, IndexWork, RecordVariant};
pub use changes::{IndexChange, IndexChangeKind, IndexChanges, IndexRevision};
pub use content::{ContentReference, ContentState, ContentStatus};
pub use integrity::{ContentIssue, IntegrityReport};
pub use keys::IndexKey;

pub use editchain_index_pages::{
    boundary, rank, Entry, Map, OrderedEntry, OrderedMap, OrderedSet, Page, Range, Storage,
};

#[cfg(test)]
mod tests;
