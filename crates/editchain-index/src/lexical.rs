//! Tantivy-based BM25 lexical index for the edit chain.
//!
//! Uses `RamDirectory` for in-memory indexing with near-real-time commit.
//! Documents are chunked operations with fielded metadata for structured filtering.

// Crate-level dependency markers (used by Cargo for feature resolution).
use editchain_embed as _;
use half as _;
use roaring as _;

use tantivy::collector::TopDocs;
use tantivy::query::{BooleanQuery, Occur, Query, TermQuery};
use tantivy::schema::IndexRecordOption;
use tantivy::schema::{Field, Schema, Term, Value, INDEXED, STORED, STRING, TEXT};
use tantivy::tokenizer::RawTokenizer;
use tantivy::{doc, Index, IndexReader, IndexWriter, ReloadPolicy, TantivyDocument};

use editchain_core::Op;
use editchain_query::hybrid::LexicalSearch;
use editchain_query::search::{ChunkId, ChunkMetadata, ScoredChunk, SearchFilters, TagFilter};

use crate::chunker::{chunk_text, extract_op_text, ChunkRecord, Generation};

// ---------------------------------------------------------------------------
// Schema definition
// ---------------------------------------------------------------------------

/// Fields in the Tantivy schema.
#[derive(Debug)]
pub struct LexicalFields {
    /// Full text body field (TEXT | STORED).
    pub body: Field,
    /// Code body field (STRING | STORED) for exact matching.
    pub body_code: Field,
    /// File path text field for fuzzy matching.
    pub path_text: Field,
    /// File path exact field for literal matching.
    pub path_exact: Field,
    /// Tool name field.
    pub tool_name: Field,
    /// Operation kind field (message, tool, command, etc.).
    pub kind: Field,
    /// Role field (`assistant`, `tool_start`, `tool_result`, etc.).
    pub role: Field,
    /// Actor ID as a u64 field.
    pub actor_id: Field,
    /// Session ID as a u64 field.
    pub session_id: Field,
    /// Node ID as a u64 field.
    pub node_id: Field,
    /// Boot counter as a u64 field.
    pub boot: Field,
    /// Sequence number as a u64 field.
    pub seq: Field,
    /// Op ID string field for exact lookup.
    pub op_id_str: Field,
    /// Chunk ordinal as a u64 field.
    pub chunk_ordinal: Field,
    /// Generation counter as a u64 field.
    pub r#gen: Field,
    /// Clock timestamp in milliseconds as a u64 field.
    pub clock_ms: Field,
}

/// Build the Tantivy schema for edit chain search.
fn build_schema() -> (Schema, LexicalFields) {
    let mut schema_builder = Schema::builder();

    let body = schema_builder.add_text_field("body", TEXT | STORED);
    let body_code = schema_builder.add_text_field("body_code", STRING | STORED);
    let path_text = schema_builder.add_text_field("path_text", STRING);
    let path_exact = schema_builder.add_text_field("path_exact", STRING);
    let tool_name = schema_builder.add_text_field("tool_name", STRING);
    let kind = schema_builder.add_text_field("kind", STRING | STORED);
    let role = schema_builder.add_text_field("role", STRING);
    let actor_id = schema_builder.add_u64_field("actor_id", INDEXED | STORED);
    let session_id = schema_builder.add_u64_field("session_id", INDEXED | STORED);
    let node_id = schema_builder.add_u64_field("node_id", INDEXED | STORED);
    let boot = schema_builder.add_u64_field("boot", INDEXED | STORED);
    let seq = schema_builder.add_u64_field("seq", INDEXED | STORED);
    let op_id_str = schema_builder.add_text_field("op_id_str", STRING | STORED);
    let chunk_ordinal = schema_builder.add_u64_field("chunk_ordinal", INDEXED | STORED);
    let r#gen = schema_builder.add_u64_field("generation", INDEXED | STORED);
    let clock_ms = schema_builder.add_u64_field("clock_ms", INDEXED | STORED);

    let schema = schema_builder.build();
    let fields = LexicalFields {
        body,
        body_code,
        path_text,
        path_exact,
        tool_name,
        kind,
        role,
        actor_id,
        session_id,
        node_id,
        boot,
        seq,
        op_id_str,
        chunk_ordinal,
        r#gen,
        clock_ms,
    };

    (schema, fields)
}

// ---------------------------------------------------------------------------
// LexicalIndex
// ---------------------------------------------------------------------------

/// An in-memory Tantivy BM25 index for edit chain operations.
pub struct LexicalIndex {
    fields: LexicalFields,
    index: Index,
    writer: IndexWriter,
    reader: IndexReader,
}

#[expect(
    clippy::missing_fields_in_debug,
    reason = "IndexWriter and IndexReader do not implement Debug; num_docs() is the meaningful runtime state"
)]
impl std::fmt::Debug for LexicalIndex {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LexicalIndex")
            .field("fields", &self.fields)
            .field("index", &self.index)
            .field("num_docs", &self.num_docs())
            .finish()
    }
}

impl LexicalIndex {
    /// Create a new empty lexical index with `RamDirectory`.
    ///
    /// # Errors
    ///
    /// Returns an error if the Tantivy index writer or reader cannot be created.
    pub fn new() -> Result<Self, Box<dyn std::error::Error>> {
        let (schema, fields) = build_schema();
        let index = Index::create_in_ram(schema);

        // Register custom tokenizers.
        index.tokenizers().register("code", RawTokenizer::default());
        index.tokenizers().register("path", RawTokenizer::default());

        let writer = index.writer(50_000_000)?; // 50MB buffer
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

    /// Index a single operation's text chunks.
    ///
    /// # Errors
    ///
    /// Returns an error if the Tantivy writer fails to add a document.
    #[expect(
        clippy::as_conversions,
        clippy::string_slice,
        reason = "byte_start/byte_end are u32 from chunker; text.len() is usize; floor_char_boundary ensures valid UTF-8 slice boundaries"
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
            // Adjust byte boundaries to valid UTF-8 char boundaries.
            let start = chunk.byte_start as usize;
            let end = (chunk.byte_end as usize).min(text.len());
            let start = text.floor_char_boundary(start);
            let end = text.floor_char_boundary(end);
            let ct = &text[start..end];
            self.index_chunk(op, chunk, ct, generation)?;
        }

        Ok(chunks)
    }

    /// Index a single chunk document.
    fn index_chunk(
        &mut self,
        op: &Op,
        chunk: &ChunkRecord,
        text: &str,
        generation: Generation,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let kind_str = kind_to_string(&op.kind);
        let role_str = role_to_string(&op.kind);

        let _opstamp = self.writer.add_document(doc!(
            self.fields.body => text,
            self.fields.body_code => text,
            self.fields.kind => kind_str,
            self.fields.role => role_str,
            self.fields.actor_id => op.actor.0,
            self.fields.node_id => op.id.node.0,
            self.fields.boot => u64::from(op.id.boot),
            self.fields.seq => op.id.seq,
            self.fields.op_id_str => op.id.to_string(),
            self.fields.chunk_ordinal => u64::from(chunk.chunk_ordinal),
            self.fields.r#gen => generation,
            self.fields.clock_ms => op.clock.as_u64(),
        ))?;

        Ok(())
    }

    /// Commit pending documents to the index.
    ///
    /// # Errors
    ///
    /// Returns an error if the Tantivy commit or reader reload fails.
    pub fn commit(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        let _opstamp = self.writer.commit()?;
        self.reader.reload()?;
        Ok(())
    }

    /// Search the lexical index.
    ///
    /// # Errors
    ///
    /// Returns an error if the Tantivy query parsing or search fails.
    #[expect(
        clippy::as_conversions,
        clippy::cast_possible_truncation,
        reason = "Tantivy stores u64 fields; boot/chunk_ordinal are u32 in the schema; truncation is safe because values originate from u32 sources"
    )]
    pub fn search_internal(
        &self,
        query_str: &str,
        filters: &SearchFilters,
        top_k: usize,
    ) -> Result<Vec<ScoredChunk>, Box<dyn std::error::Error>> {
        let searcher = self.reader.searcher();
        let query_parser = tantivy::query::QueryParser::for_index(
            &self.index,
            vec![self.fields.body, self.fields.body_code],
        );

        let parsed_query = query_parser.parse_query(query_str)?;

        // Build filter subqueries.
        let mut subqueries: Vec<(Occur, Box<dyn Query>)> = Vec::new();
        subqueries.push((Occur::Must, parsed_query));

        // Apply kind filters.
        if let Some(ref kinds) = filters.kinds {
            if !kinds.is_empty() {
                let kind_terms: Vec<(Occur, Box<dyn Query>)> = kinds
                    .iter()
                    .map(|k| -> (Occur, Box<dyn Query>) {
                        let term = Term::from_field_text(self.fields.kind, tag_filter_to_str(k));
                        (
                            Occur::Should,
                            Box::new(TermQuery::new(term, IndexRecordOption::Basic)),
                        )
                    })
                    .collect();
                subqueries.push((Occur::Must, Box::new(BooleanQuery::new(kind_terms))));
            }
        }

        // Apply actor filter.
        if let Some(ref actors) = filters.actors {
            if !actors.is_empty() {
                let actor_terms: Vec<(Occur, Box<dyn Query>)> = actors
                    .iter()
                    .map(|a| -> (Occur, Box<dyn Query>) {
                        let term = Term::from_field_u64(self.fields.actor_id, a.0);
                        (
                            Occur::Should,
                            Box::new(TermQuery::new(term, IndexRecordOption::Basic)),
                        )
                    })
                    .collect();
                subqueries.push((Occur::Must, Box::new(BooleanQuery::new(actor_terms))));
            }
        }

        // Apply session filter.
        if let Some(ref sessions) = filters.sessions {
            if !sessions.is_empty() {
                let session_terms: Vec<(Occur, Box<dyn Query>)> = sessions
                    .iter()
                    .map(|s| -> (Occur, Box<dyn Query>) {
                        let term = Term::from_field_u64(self.fields.session_id, s.0);
                        (
                            Occur::Should,
                            Box::new(TermQuery::new(term, IndexRecordOption::Basic)),
                        )
                    })
                    .collect();
                subqueries.push((Occur::Must, Box::new(BooleanQuery::new(session_terms))));
            }
        }

        // Build the combined boolean query.
        let boolean_query = BooleanQuery::new(subqueries);

        // Collect top-k results.
        let top_docs = searcher.search(&boolean_query, &TopDocs::with_limit(top_k))?;

        let mut results = Vec::with_capacity(top_docs.len());
        for (score, doc_address) in top_docs {
            let doc: TantivyDocument = searcher.doc(doc_address)?;

            let node_val = doc
                .get_first(self.fields.node_id)
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            let boot_val = doc
                .get_first(self.fields.boot)
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            let seq_val = doc
                .get_first(self.fields.seq)
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            let ordinal_val = doc
                .get_first(self.fields.chunk_ordinal)
                .and_then(|v| v.as_u64())
                .unwrap_or(0);

            let op_id = editchain_core::OpId::new(
                editchain_core::NodeId(node_val),
                boot_val.try_into().unwrap_or(0),
                seq_val,
            );

            // Extract stored text from the body field.
            let body_text = doc
                .get_first(self.fields.body)
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();

            results.push(ScoredChunk {
                chunk_id: ChunkId {
                    op_id,
                    chunk_ordinal: ordinal_val as u32,
                },
                op_id,
                score: f64::from(score),
                text: body_text,
                metadata: ChunkMetadata {
                    op_id,
                    chunk_id: ChunkId {
                        op_id,
                        chunk_ordinal: ordinal_val as u32,
                    },
                    session_id: None,
                    actor_id: editchain_core::ActorId(
                        doc.get_first(self.fields.actor_id)
                            .and_then(|v| v.as_u64())
                            .unwrap_or(0),
                    ),
                    kind_tags: 0,
                    timestamp_ms: doc
                        .get_first(self.fields.clock_ms)
                        .and_then(|v| v.as_u64())
                        .unwrap_or(0),
                    generation: doc
                        .get_first(self.fields.r#gen)
                        .and_then(|v| v.as_u64())
                        .unwrap_or(0),
                },
            });
        }

        Ok(results)
    }

    /// Number of indexed documents.
    #[must_use]
    pub fn num_docs(&self) -> usize {
        usize::try_from(self.reader.searcher().num_docs()).unwrap_or(usize::MAX)
    }

    /// Current generation of the index (max committed generation).
    #[must_use]
    pub const fn generation(&self) -> Generation {
        0
    }
}

impl Default for LexicalIndex {
    #[expect(
        clippy::expect_used,
        reason = "Default impl panics on failure; caller must use new() for fallible creation"
    )]
    fn default() -> Self {
        Self::new().expect("Failed to create LexicalIndex")
    }
}

// ---------------------------------------------------------------------------
// LexicalSearch trait impl
// ---------------------------------------------------------------------------

impl LexicalSearch for LexicalIndex {
    fn search(&self, query: &str, filters: &SearchFilters, top_k: usize) -> Vec<ScoredChunk> {
        self.search_internal(query, filters, top_k)
            .unwrap_or_default()
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

const fn kind_to_string(kind: &editchain_core::op::OpKind) -> &'static str {
    use editchain_core::op::OpKind;
    match kind {
        OpKind::Message(_) => "message",
        OpKind::Tool(_) => "tool",
        OpKind::Command(_) => "command",
        OpKind::File(_) => "file",
        OpKind::Reflection(_) => "reflection",
        OpKind::Import(_) => "import",
        OpKind::Note(_) => "note",
        OpKind::Error(_) => "error",
        OpKind::ChainStart(_) | OpKind::Actor(_) | OpKind::Unknown(_) => "unknown",
    }
}

const fn role_to_string(kind: &editchain_core::op::OpKind) -> &'static str {
    use editchain_core::op::OpKind;
    match kind {
        OpKind::Message(_) => "assistant",
        OpKind::Tool(t) => match t.stage {
            editchain_core::op::ToolStage::Start => "tool_start",
            editchain_core::op::ToolStage::Delta => "tool_delta",
            editchain_core::op::ToolStage::Finish => "tool_result",
        },
        OpKind::Command(c) => match c.stage {
            editchain_core::op::CommandStage::Start => "command_start",
            editchain_core::op::CommandStage::Output => "command_output",
            editchain_core::op::CommandStage::Finish => "command_finish",
        },
        OpKind::File(_)
        | OpKind::Reflection(_)
        | OpKind::Import(_)
        | OpKind::Note(_)
        | OpKind::Error(_)
        | OpKind::ChainStart(_)
        | OpKind::Actor(_)
        | OpKind::Unknown(_) => "",
    }
}

const fn tag_filter_to_str(filter: &TagFilter) -> &'static str {
    match filter {
        TagFilter::Message => "message",
        TagFilter::Tool => "tool",
        TagFilter::Command => "command",
        TagFilter::File => "file",
        TagFilter::Reflection => "reflection",
        TagFilter::Import => "import",
        TagFilter::Error => "error",
    }
}
