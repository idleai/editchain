//! Tantivy-backed BM25 index used by VS Code's Find in History feature.

use tantivy::collector::TopDocs;
use tantivy::schema::{Field, Schema, Value, INDEXED, STORED, STRING, TEXT};
use tantivy::tokenizer::RawTokenizer;
use tantivy::{doc, Index, IndexReader, IndexWriter, ReloadPolicy, TantivyDocument};

use editchain_core::{NodeId, Op, OpId, OpKind};

use crate::chunker::{chunk_text, extract_op_text, ChunkRecord, Generation};

/// The identity domain of a lexical hit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LexicalSource {
    /// A persisted operation in the chain.
    EditChain,
    /// A Git commit indexed with a synthetic operation id.
    Git,
}

/// A ranked lexical hit. Row resolution and deduplication happen in the VS
/// Code service. The source distinguishes synthetic Git ids from stored ids.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LexicalHit {
    /// Operation containing the matched chunk.
    pub op_id: OpId,
    /// Identity domain; Git and stored operations can have equal numeric ids.
    pub source: LexicalSource,
    /// Tantivy BM25 score; larger values are more relevant.
    pub score: f64,
}

#[derive(Debug)]
struct LexicalFields {
    body: Field,
    body_code: Field,
    node_id: Field,
    boot: Field,
    seq: Field,
    is_git: Field,
}

fn build_schema() -> (Schema, LexicalFields) {
    let mut builder = Schema::builder();
    let body = builder.add_text_field("body", TEXT);
    let body_code = builder.add_text_field("body_code", STRING);
    let node_id = builder.add_u64_field("node_id", INDEXED | STORED);
    let boot = builder.add_u64_field("boot", INDEXED | STORED);
    let seq = builder.add_u64_field("seq", INDEXED | STORED);
    let is_git = builder.add_bool_field("is_git", STORED);
    let schema = builder.build();
    (
        schema,
        LexicalFields {
            body,
            body_code,
            node_id,
            boot,
            seq,
            is_git,
        },
    )
}

/// In-memory BM25 index for searchable history operations.
pub struct LexicalIndex {
    fields: LexicalFields,
    index: Index,
    writer: IndexWriter,
    reader: IndexReader,
}

#[expect(
    clippy::missing_fields_in_debug,
    reason = "IndexWriter and IndexReader do not implement Debug; num_docs is the useful state"
)]
impl std::fmt::Debug for LexicalIndex {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("LexicalIndex")
            .field("fields", &self.fields)
            .field("index", &self.index)
            .field("num_docs", &self.num_docs())
            .finish()
    }
}

impl LexicalIndex {
    /// Create an empty in-memory index.
    ///
    /// # Errors
    ///
    /// Returns an error when Tantivy cannot create its writer or reader.
    pub fn new() -> Result<Self, Box<dyn std::error::Error>> {
        let (schema, fields) = build_schema();
        let index = Index::create_in_ram(schema);
        index.tokenizers().register("code", RawTokenizer::default());
        let writer = index.writer(50_000_000)?;
        let reader = index
            .reader_builder()
            .reload_policy(ReloadPolicy::OnCommitWithDelay)
            .try_into()?;
        Ok(Self {
            fields,
            index,
            writer,
            reader,
        })
    }

    /// Add every deterministic text chunk from one operation.
    ///
    /// # Errors
    ///
    /// Returns an error when Tantivy rejects a document.
    #[expect(
        clippy::as_conversions,
        clippy::string_slice,
        reason = "chunk offsets originate as bounded u32 values and are clamped to UTF-8 boundaries"
    )]
    pub fn index_op(
        &mut self,
        op: &Op,
        generation: Generation,
    ) -> Result<Vec<ChunkRecord>, Box<dyn std::error::Error>> {
        let Some(text) = extract_op_text(op, false, false) else {
            return Ok(Vec::new());
        };
        let chunks = chunk_text(&text, op.id, generation, 768, 96);
        for chunk in &chunks {
            let start = text.floor_char_boundary(chunk.byte_start as usize);
            let end = text.floor_char_boundary((chunk.byte_end as usize).min(text.len()));
            let chunk_text = &text[start..end];
            let _opstamp = self.writer.add_document(doc!(
                self.fields.body => chunk_text,
                self.fields.body_code => chunk_text,
                self.fields.node_id => op.id.node.0,
                self.fields.boot => u64::from(op.id.boot),
                self.fields.seq => op.id.seq,
                self.fields.is_git => matches!(op.kind, OpKind::GitCommit(_)),
            ))?;
        }
        Ok(chunks)
    }

    /// Commit pending documents and make them searchable.
    ///
    /// # Errors
    ///
    /// Returns an error when Tantivy cannot commit or reload its reader.
    pub fn commit(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        let _opstamp = self.writer.commit()?;
        self.reader.reload()?;
        Ok(())
    }

    /// Search indexed history using BM25.
    ///
    /// # Errors
    ///
    /// Returns an error when Tantivy cannot parse or execute the query.
    #[expect(
        clippy::as_conversions,
        clippy::cast_possible_truncation,
        reason = "stored boot values originate as u32 and are converted back to their source type"
    )]
    pub fn search_internal(
        &self,
        query: &str,
        top_k: usize,
    ) -> Result<Vec<LexicalHit>, Box<dyn std::error::Error>> {
        let searcher = self.reader.searcher();
        let parser = tantivy::query::QueryParser::for_index(
            &self.index,
            vec![self.fields.body, self.fields.body_code],
        );
        let parsed = parser.parse_query(query)?;
        let top_docs = searcher.search(&parsed, &TopDocs::with_limit(top_k))?;
        let mut hits = Vec::with_capacity(top_docs.len());
        for (score, address) in top_docs {
            let document: TantivyDocument = searcher.doc(address)?;
            let node = document
                .get_first(self.fields.node_id)
                .and_then(|value| value.as_u64())
                .unwrap_or(0);
            let boot = document
                .get_first(self.fields.boot)
                .and_then(|value| value.as_u64())
                .unwrap_or(0);
            let seq = document
                .get_first(self.fields.seq)
                .and_then(|value| value.as_u64())
                .unwrap_or(0);
            hits.push(LexicalHit {
                op_id: OpId::new(NodeId(node), boot as u32, seq),
                source: if document
                    .get_first(self.fields.is_git)
                    .and_then(|value| value.as_bool())
                    == Some(true)
                {
                    LexicalSource::Git
                } else {
                    LexicalSource::EditChain
                },
                score: f64::from(score),
            });
        }
        Ok(hits)
    }

    /// Number of committed documents.
    #[must_use]
    pub fn num_docs(&self) -> usize {
        usize::try_from(self.reader.searcher().num_docs()).unwrap_or(usize::MAX)
    }
}

impl Default for LexicalIndex {
    #[expect(
        clippy::expect_used,
        reason = "Default cannot expose initialization failure; callers needing recovery use new"
    )]
    fn default() -> Self {
        Self::new().expect("failed to create lexical index")
    }
}
