//! Immutable Tantivy BM25 indexes over documents prepared by the application.

use std::io;

use tantivy::collector::{Count, TopDocs};
use tantivy::query::{Query, QueryParser};
use tantivy::schema::{Field, Schema, Value, STORED, STRING, TEXT};
use tantivy::{doc, Index, IndexReader, IndexWriter, ReloadPolicy, Searcher, TantivyDocument};

use editchain_core::{GitCommitKey, OpId};

use crate::chunker::{chunk_text, ChunkOptions};

/// Real source identity, retained exactly through chunking and retrieval.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DocumentId {
    /// A persisted operation in the chain.
    Operation(OpId),
    /// A commit qualified by repository, including its full object format/OID.
    GitCommit(GitCommitKey),
}

/// Searchable text selected and resolved by the application before indexing.
#[derive(Debug)]
pub struct SearchDocument<'a> {
    /// Identity used by the application to resolve a visible row.
    pub id: DocumentId,
    /// Prepared text; privacy and payload access belong to the application.
    pub text: &'a str,
    /// Complete identifiers or paths that also need exact-token matching.
    pub exact_terms: &'a [&'a str],
}

/// A ranked chunk. Visible-row resolution and deduplication belong to the host.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LexicalHit {
    /// Real identity of the document containing this chunk.
    pub document: DocumentId,
    /// Tantivy BM25 score; larger values are more relevant.
    pub score: f64,
}

#[derive(Debug)]
struct LexicalFields {
    body: Field,
    body_code: Field,
    exact: Field,
    document: Field,
}

fn build_schema() -> (Schema, LexicalFields) {
    let mut builder = Schema::builder();
    let body = builder.add_text_field("body", TEXT);
    // Preserve the existing exact whole-chunk field and BM25 query defaults.
    let body_code = builder.add_text_field("body_code", STRING);
    let exact = builder.add_text_field("exact", STRING);
    let document = builder.add_u64_field("document", STORED);
    (
        builder.build(),
        LexicalFields {
            body,
            body_code,
            exact,
            document,
        },
    )
}

/// Fallible construction of an unpublished index; queries require publication.
pub struct LexicalIndexBuilder {
    fields: LexicalFields,
    index: Index,
    writer: IndexWriter,
    documents: Vec<DocumentId>,
    chunks: ChunkOptions,
}

impl std::fmt::Debug for LexicalIndexBuilder {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("LexicalIndexBuilder")
            .field("documents", &self.documents.len())
            .field("chunks", &self.chunks)
            .finish_non_exhaustive()
    }
}

impl LexicalIndexBuilder {
    /// Create a private in-memory writer with validated chunk options.
    ///
    /// # Errors
    ///
    /// Returns an error when Tantivy cannot create its writer.
    pub fn new(chunks: ChunkOptions) -> Result<Self, Box<dyn std::error::Error>> {
        let (schema, fields) = build_schema();
        let index = Index::create_in_ram(schema);
        let writer = index.writer(50_000_000)?;
        Ok(Self {
            fields,
            index,
            writer,
            documents: Vec::new(),
            chunks,
        })
    }

    /// Add one prepared document, keeping chunk ordinals internal to Tantivy.
    ///
    /// # Errors
    ///
    /// Returns an error when a document cannot be represented or indexed.
    pub fn add_document(
        &mut self,
        document: &SearchDocument<'_>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let ordinal = u64::try_from(self.documents.len())?;
        self.documents.push(document.id);
        for range in chunk_text(document.text, self.chunks) {
            let text = document
                .text
                .get(range)
                .ok_or("invalid UTF-8 chunk range")?;
            let mut chunk = doc!(
                self.fields.body => text,
                self.fields.body_code => text,
                self.fields.document => ordinal,
            );
            for term in document.exact_terms {
                chunk.add_text(self.fields.exact, term);
            }
            let _opstamp = self.writer.add_document(chunk)?;
        }
        Ok(())
    }

    /// Commit all documents and consume the writer to publish a read-only index.
    ///
    /// The application pairs this completed value with its source/view version.
    ///
    /// # Errors
    ///
    /// Returns an error when Tantivy cannot commit or open the committed reader.
    pub fn publish(mut self) -> Result<LexicalIndex, Box<dyn std::error::Error>> {
        let _opstamp = self.writer.commit()?;
        let reader = self
            .index
            .reader_builder()
            .reload_policy(ReloadPolicy::Manual)
            .try_into()?;
        Ok(LexicalIndex {
            fields: self.fields,
            index: self.index,
            reader,
            documents: self.documents,
        })
    }
}

/// Read-only BM25 index. There is no partially committed query/update state.
pub struct LexicalIndex {
    fields: LexicalFields,
    index: Index,
    reader: IndexReader,
    documents: Vec<DocumentId>,
}

impl std::fmt::Debug for LexicalIndex {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("LexicalIndex")
            .field("num_docs", &self.num_docs())
            .finish_non_exhaustive()
    }
}

/// Maximum number of ranked chunks one query may retrieve, across all pages.
pub const MAX_CANDIDATES: usize = 65_536;

impl LexicalIndex {
    /// Parse a query once and retain a fixed searcher for bounded continuation.
    ///
    /// # Errors
    ///
    /// Rejects a zero/excessive scan budget or a query Tantivy cannot parse.
    pub fn candidates(
        &self,
        query: &str,
        budget: usize,
    ) -> Result<LexicalQuery<'_>, Box<dyn std::error::Error>> {
        if budget == 0 || budget > MAX_CANDIDATES {
            return Err(io::Error::other("candidate budget must be between 1 and 65536").into());
        }
        let parser = QueryParser::for_index(
            &self.index,
            vec![self.fields.body, self.fields.body_code, self.fields.exact],
        );
        Ok(LexicalQuery {
            parsed: parser.parse_query(query)?,
            searcher: self.reader.searcher(),
            documents: &self.documents,
            document_field: self.fields.document,
            offset: 0,
            budget,
        })
    }

    /// Number of committed chunks.
    #[must_use]
    pub fn num_docs(&self) -> usize {
        usize::try_from(self.reader.searcher().num_docs()).unwrap_or(usize::MAX)
    }
}

/// Continuation over one immutable index and one parsed query.
#[derive(Debug)]
pub struct LexicalQuery<'a> {
    parsed: Box<dyn Query>,
    searcher: Searcher,
    documents: &'a [DocumentId],
    document_field: Field,
    offset: usize,
    budget: usize,
}

/// One candidate page; exhaustion is measured before visible-row filtering.
#[derive(Debug)]
pub struct CandidatePage {
    /// Chunks ranked by descending BM25 score.
    pub hits: Vec<LexicalHit>,
    /// Matching chunks remain, possibly beyond the query's scan budget.
    pub more: bool,
}

impl LexicalQuery<'_> {
    /// Remaining chunks permitted by this query's aggregate scan budget.
    #[must_use]
    pub const fn remaining_budget(&self) -> usize {
        self.budget.saturating_sub(self.offset)
    }

    /// Retrieve another ranked page, capped by the remaining aggregate budget.
    ///
    /// # Errors
    ///
    /// Rejects a zero page size or exhausted budget and reports Tantivy failures
    /// or invalid stored document references as errors rather than guessed IDs.
    pub fn next_page(&mut self, limit: usize) -> Result<CandidatePage, Box<dyn std::error::Error>> {
        let limit = limit.min(self.remaining_budget());
        if limit == 0 {
            return Err(
                io::Error::other("candidate page requires a positive remaining budget").into(),
            );
        }
        let (top_docs, total) = self.searcher.search(
            &self.parsed,
            &(TopDocs::with_limit(limit).and_offset(self.offset), Count),
        )?;
        let mut hits = Vec::with_capacity(top_docs.len());
        for (score, address) in top_docs {
            let stored: TantivyDocument = self.searcher.doc(address)?;
            let ordinal = stored
                .get_first(self.document_field)
                .and_then(|value| value.as_u64())
                .and_then(|value| usize::try_from(value).ok())
                .ok_or("missing lexical document reference")?;
            let document = *self
                .documents
                .get(ordinal)
                .ok_or("unknown lexical document reference")?;
            hits.push(LexicalHit {
                document,
                score: f64::from(score),
            });
        }
        self.offset = self.offset.saturating_add(hits.len());
        Ok(CandidatePage {
            hits,
            more: self.offset < total,
        })
    }
}
