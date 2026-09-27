//! Explicit full replay and content verification, outside the incremental path.

use std::{io, path::Path};

use editchain_core::OpId;
use editchain_store::{read_op_at, BlobReader, BlobSource, ChainReadStats, OpRecordLocation};

use crate::{
    boundary, references::references, state::State, ChainIndex, ContentReference, ContentState,
};

/// A missing, invalid, or unsupported content reference in a persisted record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContentIssue {
    /// Operation identity, including quarantined identities.
    pub operation: OpId,
    /// Exact record variant containing the reference.
    pub location: OpRecordLocation,
    /// Original reference, including any recorded length.
    pub reference: ContentReference,
    /// Verified outcome; never [`ContentState::Available`].
    pub state: ContentState,
}

/// Comparison with a fresh canonical replay and full referenced-blob verification.
///
/// A stale index may differ simply because records or blobs arrived after its
/// last refresh. Missing content and conflicts are reported without rewriting
/// records or blobs. This report checks supported record fields, not references hidden
/// inside opaque payloads or external Git object databases.
#[derive(Debug, Clone)]
pub struct IntegrityReport {
    /// Fresh counts from canonical records, independent of the checkpoint.
    pub chain: ChainReadStats,
    /// Whether indexed results and retained record variants match current storage.
    pub index_matches: bool,
    /// A derived-page or saved-frontier error encountered during comparison.
    pub index_error: Option<String>,
    /// Content problems from both accepted and quarantined record variants.
    pub content_issues: Vec<ContentIssue>,
}

impl IntegrityReport {
    /// Whether the index agrees and no record or content integrity gaps were found.
    #[must_use]
    pub fn is_clean(&self) -> bool {
        self.index_matches
            && self.index_error.is_none()
            && self.chain.quarantined == 0
            && self.chain.undecodable == 0
            && self.chain.incomplete_tails == 0
            && self.content_issues.is_empty()
    }
}

impl ChainIndex {
    /// Verify canonical framing, admission, referenced blobs, and derived results.
    ///
    /// Performs a full replay without changing the checkpoint or canonical data.
    /// Quiesce writers for a stable audit: records and blobs have independent
    /// publication and can arrive during verification. Available blobs are fully
    /// hashed here, even though incremental refresh assumes they are immutable.
    ///
    /// # Errors
    /// Returns canonical framing, sequence-gap, record, or blob IO errors.
    /// Corrupt derived pages are instead reported in [`IntegrityReport::index_error`].
    pub fn verify_integrity(&self) -> io::Result<IntegrityReport> {
        let blobs = BlobReader::open(&self.root)?;
        let expected = State::build(&self.root, &blobs)?;
        let comparison = boundary(|| {
            // Reload the published root and its cold pages: warmed query caches
            // must not hide corruption that happened after they were decoded.
            let published: State = self.storage.load()?;
            published.tail.resume(&self.root)?;
            Ok(published.matches(&expected) && self.state.matches(&expected))
        })
        .and_then(std::convert::identity);
        let (index_matches, index_error) = match comparison {
            Ok(matches) => (matches, None),
            Err(error) => (false, Some(error.to_string())),
        };
        Ok(IntegrityReport {
            chain: expected.tail.chain().stats(),
            index_matches,
            index_error,
            content_issues: content_issues(&expected, &self.root, &blobs)?,
        })
    }
}

fn content_issues(
    state: &State,
    root: &Path,
    blobs: &impl BlobSource,
) -> io::Result<Vec<ContentIssue>> {
    let chain = state.tail.chain();
    let mut ids: Vec<_> = chain.identities().collect();
    ids.sort_unstable();
    let mut issues = Vec::new();
    for id in ids {
        for location in chain.record_locations(id) {
            let op = read_op_at(root, location)?;
            for reference in references(&op) {
                // Accepted references were fully verified while rebuilding the
                // expected index. Conflict variants are not in query postings.
                let content = match state.content.status(reference) {
                    Some(status) => status.state,
                    None => reference.state(blobs)?,
                };
                if content != ContentState::Available {
                    issues.push(ContentIssue {
                        operation: id,
                        location,
                        reference,
                        state: content,
                    });
                }
            }
        }
    }
    Ok(issues)
}
