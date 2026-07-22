//! Flat f16 vector index with `RoaringBitmap` filters and exact scan.
//!
//! Uses half-precision (f16) vectors stored row-major in aligned segments.
//! Filters use `RoaringBitmap` for fast pre-filtering before dot product scan.

// Crate-level dependency markers (used by Cargo for feature resolution).
use half as _;

use half::f16;
use roaring::RoaringBitmap;

use editchain_core::{NodeId, OpId};
use editchain_embed::{Embedder, EmbeddingManifest};
use editchain_query::hybrid::VectorSearch;
use editchain_query::search::{ChunkId, ChunkMetadata, ScoredChunk, SearchFilters};

use crate::chunker::Generation;

// ---------------------------------------------------------------------------
// FlatVectorSegment
// ---------------------------------------------------------------------------

/// A sealed segment of f16 vectors with associated doc ordinals.
#[derive(Debug, Clone)]
pub struct FlatVectorSegment {
    /// Generation range this segment covers (start).
    pub generation_start: Generation,
    /// Generation range this segment covers (end).
    pub generation_end: Generation,
    /// Vector dimensionality.
    pub dimensions: usize,
    /// Doc ordinals in insertion order (maps to external doc IDs).
    pub doc_ordinals: Vec<DocOrdinal>,
    /// Original `OpIds` for each vector (same order as `doc_ordinals`).
    pub op_ids: Vec<OpId>,
    /// Chunk ordinals for each vector.
    pub chunk_ordinals: Vec<u32>,
    /// Row-major f16 vectors, length = `doc_ordinals.len()` * dimensions.
    pub vectors: Vec<f16>,
}

/// A document ordinal — index into the vector segment.
pub type DocOrdinal = u32;

impl FlatVectorSegment {
    /// Create a new empty vector segment with the given dimensionality.
    #[must_use]
    pub const fn new(dimensions: usize) -> Self {
        Self {
            generation_start: 0,
            generation_end: 0,
            dimensions,
            doc_ordinals: Vec::new(),
            op_ids: Vec::new(),
            chunk_ordinals: Vec::new(),
            vectors: Vec::new(),
        }
    }

    /// Number of vectors in this segment.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.doc_ordinals.len()
    }

    /// Returns `true` if this segment contains no vectors.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.doc_ordinals.is_empty()
    }

    /// Push a vector with an ordinal (without explicit `op_id` / `chunk_ordinal`).
    #[expect(
        clippy::missing_assert_message,
        reason = "debug_assert_eq with just len/dimensions is self-explanatory"
    )]
    pub fn push(&mut self, ordinal: DocOrdinal, vec: &[f16]) {
        debug_assert_eq!(vec.len(), self.dimensions);
        self.doc_ordinals.push(ordinal);
        self.op_ids
            .push(OpId::new(NodeId(0), 0, u64::from(ordinal)));
        self.chunk_ordinals.push(0);
        self.vectors.extend_from_slice(vec);
    }

    /// Push a vector with full metadata (ordinal, `op_id`, `chunk_ordinal`).
    #[expect(
        clippy::missing_assert_message,
        reason = "debug_assert_eq with just len/dimensions is self-explanatory"
    )]
    pub fn push_with_op(
        &mut self,
        ordinal: DocOrdinal,
        vec: &[f16],
        op_id: OpId,
        chunk_ordinal: u32,
    ) {
        debug_assert_eq!(vec.len(), self.dimensions);
        self.doc_ordinals.push(ordinal);
        self.op_ids.push(op_id);
        self.chunk_ordinals.push(chunk_ordinal);
        self.vectors.extend_from_slice(vec);
    }

    /// Get the vector at the given index, or `None` if out of bounds.
    #[expect(
        clippy::arithmetic_side_effects,
        clippy::indexing_slicing,
        reason = "Bounds check is performed before arithmetic; dimensions is non-zero for any real index"
    )]
    #[must_use]
    pub fn get(&self, index: usize) -> Option<&[f16]> {
        if index >= self.doc_ordinals.len() {
            return None;
        }
        let start = index * self.dimensions;
        Some(&self.vectors[start..start + self.dimensions])
    }

    /// Seal this segment with a generation range (prevents further pushes).
    pub const fn seal(&mut self, gen_start: Generation, gen_end: Generation) {
        self.generation_start = gen_start;
        self.generation_end = gen_end;
    }
}

// ---------------------------------------------------------------------------
// VectorIndex
// ---------------------------------------------------------------------------

/// A flat f16 vector index with `RoaringBitmap` filters and exact scan.
pub struct VectorIndex {
    manifest: EmbeddingManifest,
    active: FlatVectorSegment,
    sealed: Vec<FlatVectorSegment>,
    filters: VectorFilters,
    next_ordinal: DocOrdinal,
}

#[expect(
    clippy::missing_fields_in_debug,
    reason = "VectorFilters and sealed segments are large internal state; active_len and next_ordinal are sufficient for debugging"
)]
impl std::fmt::Debug for VectorIndex {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VectorIndex")
            .field("manifest", &self.manifest)
            .field("active_len", &self.active.len())
            .field("sealed_count", &self.sealed.len())
            .field("next_ordinal", &self.next_ordinal)
            .finish()
    }
}

/// `RoaringBitmap` filters for vector index pre-filtering.
#[derive(Debug, Clone)]
pub struct VectorFilters {
    /// Bitmaps keyed by operation kind (message, tool, command, etc.).
    pub kind_bitmaps: std::collections::HashMap<String, RoaringBitmap>,
    /// Bitmaps keyed by session ID.
    pub session_bitmaps: std::collections::HashMap<u64, RoaringBitmap>,
}

impl VectorFilters {
    fn new() -> Self {
        Self {
            kind_bitmaps: std::collections::HashMap::new(),
            session_bitmaps: std::collections::HashMap::new(),
        }
    }
}

impl VectorIndex {
    /// Create a new empty vector index with the given embedding manifest.
    #[expect(
        clippy::as_conversions,
        reason = "manifest.dimensions is u32 from wire format; usize conversion is safe for any realistic dimension count (<65536)"
    )]
    #[must_use]
    pub fn new(manifest: EmbeddingManifest) -> Self {
        let dims = manifest.dimensions as usize;
        Self {
            manifest,
            active: FlatVectorSegment::new(dims),
            sealed: Vec::new(),
            filters: VectorFilters::new(),
            next_ordinal: 0,
        }
    }

    /// Add a vector to the active segment with its metadata.
    #[expect(
        clippy::too_many_arguments,
        clippy::arithmetic_side_effects,
        clippy::let_underscore_untyped,
        reason = "all parameters are required for vector indexing; next_ordinal increment is bounded; bitmap insert discards old value"
    )]
    pub fn add_vector(
        &mut self,
        op_id: OpId,
        chunk_ordinal: u32,
        vec: &[f16],
        kind: &str,
        session_id: Option<u64>,
        _generation: Generation,
    ) {
        let ordinal = self.next_ordinal;
        self.next_ordinal += 1;
        self.active.push_with_op(ordinal, vec, op_id, chunk_ordinal);

        let _ = self
            .filters
            .kind_bitmaps
            .entry(kind.to_string())
            .or_default()
            .insert(ordinal);

        if let Some(sid) = session_id {
            let _ = self
                .filters
                .session_bitmaps
                .entry(sid)
                .or_default()
                .insert(ordinal);
        }
    }

    /// Seal the active segment and start a new one at the given generation.
    #[expect(
        clippy::as_conversions,
        reason = "manifest.dimensions is u32 from wire format; usize conversion is safe for any realistic dimension count (<65536)"
    )]
    pub fn seal_active(&mut self, generation: Generation) {
        if !self.active.is_empty() {
            self.active.seal(self.active.generation_start, generation);
            let sealed = std::mem::replace(
                &mut self.active,
                FlatVectorSegment::new(self.manifest.dimensions as usize),
            );
            self.sealed.push(sealed);
        }
        self.active.generation_start = generation;
    }

    /// Search the vector index with the given query vector and filters.
    #[must_use]
    pub fn search(
        &self,
        query_vec: &[f16],
        filters: &SearchFilters,
        top_k: usize,
    ) -> Vec<ScoredChunk> {
        let filter_mask = self.build_filter_mask(filters);
        let mut candidates = Vec::new();

        self.scan_segment(
            &self.active,
            query_vec,
            filter_mask.as_ref(),
            &mut candidates,
        );
        for segment in &self.sealed {
            self.scan_segment(segment, query_vec, filter_mask.as_ref(), &mut candidates);
        }

        candidates.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
        candidates
            .into_iter()
            .take(top_k)
            .map(|(_, chunk)| chunk)
            .collect()
    }

    /// Scan a single segment, computing dot products and collecting candidates.
    #[expect(
        clippy::indexing_slicing,
        clippy::arithmetic_side_effects,
        clippy::unused_self,
        reason = "Segment vectors are guaranteed to be dimension-aligned; i*dims is bounded by segment length; self is retained for future extensibility"
    )]
    fn scan_segment(
        &self,
        segment: &FlatVectorSegment,
        query_vec: &[f16],
        filter_mask: Option<&RoaringBitmap>,
        results: &mut Vec<(f32, ScoredChunk)>,
    ) {
        let dims = segment.dimensions;
        for i in 0..segment.len() {
            let ordinal = segment.doc_ordinals[i];
            if let Some(mask) = filter_mask {
                if !mask.contains(ordinal) {
                    continue;
                }
            }
            let start = i * dims;
            let vec = &segment.vectors[start..start + dims];
            let score = dot_product_f16(query_vec, vec);
            if score <= 0.0 {
                continue;
            }
            let op_id = segment.op_ids[i];
            let chunk_ord = segment.chunk_ordinals[i];
            results.push((
                score,
                ScoredChunk {
                    chunk_id: ChunkId {
                        op_id,
                        chunk_ordinal: chunk_ord,
                    },
                    op_id,
                    score: f64::from(score),
                    text: String::new(),
                    metadata: ChunkMetadata {
                        op_id,
                        chunk_id: ChunkId {
                            op_id,
                            chunk_ordinal: chunk_ord,
                        },
                        session_id: None,
                        actor_id: editchain_core::ActorId(0),
                        kind_tags: 0,
                        timestamp_ms: 0,
                        generation: segment.generation_end,
                    },
                },
            ));
        }
    }

    /// Build a combined filter mask from search filters, intersecting kind bitmaps.
    fn build_filter_mask(&self, filters: &SearchFilters) -> Option<RoaringBitmap> {
        let mut mask: Option<RoaringBitmap> = None;
        if let Some(ref kinds) = filters.kinds {
            for kind in kinds {
                let kind_str = match kind {
                    editchain_query::search::TagFilter::Message => "message",
                    editchain_query::search::TagFilter::Tool => "tool",
                    editchain_query::search::TagFilter::Command => "command",
                    editchain_query::search::TagFilter::File => "file",
                    editchain_query::search::TagFilter::Reflection => "reflection",
                    editchain_query::search::TagFilter::Import => "import",
                    editchain_query::search::TagFilter::Error => "error",
                };
                if let Some(bitmap) = self.filters.kind_bitmaps.get(kind_str) {
                    match &mut mask {
                        Some(m) => *m &= bitmap.clone(),
                        None => mask = Some(bitmap.clone()),
                    }
                } else {
                    return Some(RoaringBitmap::new());
                }
            }
        }
        mask
    }

    /// Number of vectors across all segments (active + sealed).
    #[expect(
        clippy::arithmetic_side_effects,
        reason = "Segment lengths are bounded by memory; addition is safe"
    )]
    #[must_use]
    pub fn num_vectors(&self) -> usize {
        let mut count = self.active.len();
        for seg in &self.sealed {
            count += seg.len();
        }
        count
    }

    /// The embedding manifest describing vector dimensionality and model.
    #[must_use]
    pub const fn manifest(&self) -> &EmbeddingManifest {
        &self.manifest
    }
}

// ---------------------------------------------------------------------------
// VectorSearch trait impl
// ---------------------------------------------------------------------------

/// A vector search backend that embeds queries via an `Embedder` and searches
/// a `VectorIndex`.
pub struct VectorSearchWrapper {
    index: VectorIndex,
    embedder: Box<dyn Embedder>,
}

#[expect(
    clippy::missing_fields_in_debug,
    reason = "Box<dyn Embedder> does not implement Debug; index state is sufficient for debugging"
)]
impl std::fmt::Debug for VectorSearchWrapper {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VectorSearchWrapper")
            .field("index", &self.index)
            .finish()
    }
}

impl VectorSearchWrapper {
    /// Create a new vector search wrapper with the given index and embedder.
    #[must_use]
    pub fn new(index: VectorIndex, embedder: Box<dyn Embedder>) -> Self {
        Self { index, embedder }
    }

    /// Embed a batch of texts and add them to the index.
    ///
    /// # Errors
    ///
    /// Returns `EmbedError` if the embedder fails to produce embeddings.
    #[expect(
        clippy::type_complexity,
        reason = "tuple is internal to this module and not exposed publicly"
    )]
    pub fn add_texts(
        &mut self,
        texts: &[(OpId, u32, String, String, Option<u64>)],
        generation: u64,
    ) -> Result<(), editchain_embed::EmbedError> {
        let batch: Vec<String> = texts.iter().map(|(_, _, t, _, _)| t.clone()).collect();
        let vectors = self.embedder.embed(&batch)?;
        for ((op_id, chunk_ordinal, _, kind, session_id), vec) in texts.iter().zip(vectors.iter()) {
            let f16v = f32_to_f16_vec(vec);
            self.index
                .add_vector(*op_id, *chunk_ordinal, &f16v, kind, *session_id, generation);
        }
        Ok(())
    }

    /// Access the embedder mutably (for batch embedding).
    pub fn embedder_mut(&mut self) -> &mut Box<dyn Embedder> {
        &mut self.embedder
    }

    /// Access the vector index mutably.
    pub const fn index_mut(&mut self) -> &mut VectorIndex {
        &mut self.index
    }
}

impl VectorSearch for VectorSearchWrapper {
    fn search(&self, query: &str, filters: &SearchFilters, top_k: usize) -> Vec<ScoredChunk> {
        let Ok(query_vec) = self.embedder.embed_query(query) else {
            return Vec::new();
        };
        let f16q = f32_to_f16_vec(&query_vec);
        self.index.search(&f16q, filters, top_k)
    }
}

// ---------------------------------------------------------------------------
// Math helpers
// ---------------------------------------------------------------------------

#[expect(
    clippy::missing_assert_message,
    reason = "debug_assert_eq with just len comparison is self-explanatory"
)]
fn dot_product_f16(a: &[f16], b: &[f16]) -> f32 {
    debug_assert_eq!(a.len(), b.len());
    a.iter()
        .zip(b.iter())
        .map(|(x, y)| x.to_f32() * y.to_f32())
        .sum()
}

/// Convert a slice of f32 values to a vector of f16 values.
#[must_use]
pub fn f32_to_f16_vec(vec: &[f32]) -> Vec<f16> {
    vec.iter().map(|&x| f16::from_f32(x)).collect()
}

/// Normalize an f32 vector in-place to unit length.
pub fn normalize_f32(vec: &mut [f32]) {
    let norm_sq: f32 = vec.iter().map(|x| x * x).sum();
    if norm_sq > 0.0 {
        let norm = norm_sq.sqrt();
        for x in vec.iter_mut() {
            *x /= norm;
        }
    }
}
