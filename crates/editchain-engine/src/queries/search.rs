//! Literal byte search; ranking and presentation belong to consumers.

use std::io;

use serde::{Deserialize, Serialize};

use crate::{ByteRange, OpId};

use super::{
    fields, ChainQueries, ContentField, ContentResult, EvidenceRef, IndexKey, PageRequest,
};

/// First exact occurrence in one content field, using byte offsets.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FieldMatch {
    /// Field containing the match.
    pub field: ContentField,
    /// Half-open byte range in the exact field content, including for UTF-8 text.
    pub range: ByteRange,
}

/// One recorded operation containing one or more literal field matches.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchHit {
    /// Original record supporting all of these matches.
    pub evidence: EvidenceRef,
    /// First match per field, in schema order; no normalization or relevance score.
    pub fields: Vec<FieldMatch>,
}

/// A bounded search scan, retaining gaps even for operations with no known match.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchPage {
    /// Matching operations in operation-ID order.
    pub hits: Vec<SearchHit>,
    /// Content that could not be searched, including absent revision snapshots.
    pub unavailable: Vec<ContentResult>,
    /// Number of candidate operations searched in this page.
    pub scanned: usize,
    /// Continue after this inspected ID, even when this page has no hits.
    pub next_after: Option<OpId>,
}

impl ChainQueries {
    /// Search recorded payloads and referenced snapshots for a case-sensitive literal.
    ///
    /// `text` must contain 1..=16384 UTF-8 bytes. Matching uses exact byte
    /// sequences, including in binary evidence; it never performs lossy text
    /// decoding or joins adjacent fields. Page limits bound candidate operations,
    /// not hits. An empty hit list with gaps is not proof that text is absent.
    ///
    /// # Errors
    /// Returns invalid text/page limits or index/content IO errors.
    pub fn search(
        &self,
        text: &str,
        key: Option<IndexKey>,
        page: PageRequest,
    ) -> io::Result<SearchPage> {
        if text.is_empty() || text.len() > 16_384 {
            return Err(crate::invalid_input(
                "search text must contain 1 to 16384 bytes",
            ));
        }
        let history = self.history(key, page)?;
        let mut result = SearchPage {
            hits: Vec::new(),
            unavailable: Vec::new(),
            scanned: history.items.len(),
            next_after: history.next_after,
        };
        for entry in history.items {
            let mut matches = Vec::new();
            for (field, source) in fields::fields(&entry.operation.kind) {
                let content = self.resolve_field(&entry, field, source)?;
                if let Some(bytes) = content.value.bytes() {
                    if let Some(start) = bytes
                        .windows(text.len())
                        .position(|window| window == text.as_bytes())
                    {
                        matches.push(FieldMatch {
                            field,
                            range: ByteRange {
                                start: u64::try_from(start).map_err(crate::invalid_input)?,
                                end: u64::try_from(start.saturating_add(text.len()))
                                    .map_err(crate::invalid_input)?,
                            },
                        });
                    }
                } else {
                    result.unavailable.push(content);
                }
            }
            if !matches.is_empty() {
                result.hits.push(SearchHit {
                    evidence: entry.evidence,
                    fields: matches,
                });
            }
        }
        Ok(result)
    }
}
