//! Flat f16 vector index with RoaringBitmap filters and exact scan.
//!
//! Uses half-precision (f16) vectors stored row-major in aligned segments.
//! Filters use RoaringBitmap for fast pre-filtering before dot product scan.

use std::sync::Arc;

use half::f16;
use roaring::RoaringBitmap;

use editchain_core::{NodeId, OpId};
use editchain_embed::EmbeddingManifest;
use editchain_query::search::{ChunkId, ChunkMetadata, ScoredChunk, SearchFilters};

use crate::chunker::Generation;

// ---------------------------------------------------------------------------
// FlatVectorSegment
// ---------------------------------------------------------------------------

/// A sealed segment of f16 vectors with associated doc ordinals.
#[derive(Debug, Clone)]
pub struct FlatVectorSegment {
    /// Generation range this segment covers.
    pub generation_start: Generation,
    pub generation_end: Generation,
    /// Vector dimensionality.
    pub dimensions: usize,
    /// Doc ordinals in insertion order (maps to external doc IDs).
    pub doc_ordinals: Vec<DocOrdinal>,
    /// Original OpIds for each vector (same order as doc_ordinals).
    pub op_ids: Vec<OpId>,
    /// Chunk ordinals for each vector.
    pub chunk_ordinals: Vec<u32>,
    /// Row-major f16 vectors, length = doc_ordinals.len() * dimensions.
    pub vectors: Vec<f16>,
}

/// A document ordinal — index into the vector segment.
pub type DocOrdinal = u32;

impl FlatVectorSegment {
    pub fn new(dimensions: usize) -> Self {
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

    pub fn len(&self) -> usize {
        self.doc_ordinals.len()
    }

    pub fn is_empty(&self) -> bool {
        self.doc_ordinals.is_empty()
    }

    pub fn push(&mut self, ordinal: DocOrdinal, vec: &[f16]) {
        debug_assert_eq!(vec.len(), self.dimensions);
        self.doc_ordinals.push(ordinal);
        self.op_ids.push(OpId::new(NodeId(0), 0, ordinal as u64));
        self.chunk_ordinals.push(0);
        self.vectors.extend_from_slice(vec);
    }

    pub fn push_with_op(&mut self, ordinal: DocOrdinal, vec: &[f16], op_id: OpId, chunk_ordinal: u32) {
        debug_assert_eq!(vec.len(), self.dimensions);
        self.doc_ordinals.push(ordinal);
        self.op_ids.push(op_id);
        self.chunk_ordinals.push(chunk_ordinal);
        self.vectors.extend_from_slice(vec);
    }

    pub fn get(&self, index: usize) -> Option<&[f16]> {
        if index >= self.doc_ordinals.len() {
            return None;
        }
        let start = index * self.dimensions;
        Some(&self.vectors[start..start + self.dimensions])
    }

    pub fn seal(&mut self, gen_start: Generation, gen_end: Generation) {
        self.generation_start = gen_start;
        self.generation_end = gen_end;
    }
}

// ---------------------------------------------------------------------------
// VectorIndex
// ---------------------------------------------------------------------------

pub struct VectorIndex {
    manifest: EmbeddingManifest,
    active: FlatVectorSegment,
    sealed: Vec<FlatVectorSegment>,
    filters: VectorFilters,
    next_ordinal: DocOrdinal,
}

#[derive(Debug, Clone)]
pub struct VectorFilters {
    pub kind_bitmaps: std::collections::HashMap<String, RoaringBitmap>,
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

    pub fn add_vector(
        &mut self,
        op_id: OpId,
        chunk_ordinal: u32,
        vec: &[f16],
        kind: &str,
        session_id: Option<u64>,
        generation: Generation,
    ) {
        let ordinal = self.next_ordinal;
        self.next_ordinal += 1;
        self.active.push_with_op(ordinal, vec, op_id, chunk_ordinal);

        self.filters
            .kind_bitmaps
            .entry(kind.to_string())
            .or_default()
            .insert(ordinal);

        if let Some(sid) = session_id {
            self.filters
                .session_bitmaps
                .entry(sid)
                .or_default()
                .insert(ordinal);
        }
    }

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

    pub fn search(
        &self,
        query_vec: &[f16],
        filters: &SearchFilters,
        top_k: usize,
    ) -> Vec<ScoredChunk> {
        let filter_mask = self.build_filter_mask(filters);
        let mut candidates = Vec::new();

        self.scan_segment(&self.active, query_vec, &filter_mask, &mut candidates);
        for segment in &self.sealed {
            self.scan_segment(segment, query_vec, &filter_mask, &mut candidates);
        }

        candidates.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
        candidates.into_iter().take(top_k).map(|(_, chunk)| chunk).collect()
    }

    fn scan_segment(
        &self,
        segment: &FlatVectorSegment,
        query_vec: &[f16],
        filter_mask: &Option<RoaringBitmap>,
        results: &mut Vec<(f32, ScoredChunk)>,
    ) {
        let dims = segment.dimensions;
        for i in 0..segment.len() {
            let ordinal = segment.doc_ordinals[i];
            if let Some(ref mask) = filter_mask {
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
                    chunk_id: ChunkId { op_id, chunk_ordinal: chunk_ord },
                    op_id,
                    score: score as f64,
                    text: String::new(),
                    metadata: ChunkMetadata {
                        op_id,
                        chunk_id: ChunkId { op_id, chunk_ordinal: chunk_ord },
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
                        Some(ref mut m) => *m &= bitmap.clone(),
                        None => mask = Some(bitmap.clone()),
                    }
                } else {
                    return Some(RoaringBitmap::new());
                }
            }
        }
        mask
    }

    pub fn num_vectors(&self) -> usize {
        let mut count = self.active.len();
        for seg in &self.sealed {
            count += seg.len();
        }
        count
    }

    pub fn manifest(&self) -> &EmbeddingManifest {
        &self.manifest
    }
}

// ---------------------------------------------------------------------------
// Math helpers
// ---------------------------------------------------------------------------

fn dot_product_f16(a: &[f16], b: &[f16]) -> f32 {
    debug_assert_eq!(a.len(), b.len());
    a.iter().zip(b.iter()).map(|(x, y)| x.to_f32() * y.to_f32()).sum()
}

pub fn f32_to_f16_vec(vec: &[f32]) -> Vec<f16> {
    vec.iter().map(|&x| f16::from_f32(x)).collect()
}

pub fn normalize_f32(vec: &mut [f32]) {
    let norm_sq: f32 = vec.iter().map(|x| x * x).sum();
    if norm_sq > 0.0 {
        let norm = norm_sq.sqrt();
        for x in vec.iter_mut() {
            *x /= norm;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use editchain_embed::EmbeddingManifest;

    #[test]
    fn normalize_and_convert() {
        let mut v = vec![3.0, 4.0];
        normalize_f32(&mut v);
        assert!((v[0] - 0.6).abs() < 0.001);
        assert!((v[1] - 0.8).abs() < 0.001);

        let f16v = f32_to_f16_vec(&v);
        assert_eq!(f16v.len(), 2);
    }

    #[test]
    fn vector_index_add_and_search() {
        let mut manifest = EmbeddingManifest::qwen3_embedding_0_6b();
        manifest.dimensions = 2;
        let mut index = VectorIndex::new(manifest);

        // Add two vectors
        let mut v1 = vec![1.0, 0.0];
        normalize_f32(&mut v1);
        let f16v1 = f32_to_f16_vec(&v1);
        index.add_vector(OpId::new(NodeId(1), 0, 1), 0, &f16v1, "message", None, 1);

        let mut v2 = vec![0.0, 1.0];
        normalize_f32(&mut v2);
        let f16v2 = f32_to_f16_vec(&v2);
        index.add_vector(OpId::new(NodeId(1), 0, 2), 0, &f16v2, "message", None, 1);

        // Search with query similar to v1
        let mut q = vec![0.9, 0.1];
        normalize_f32(&mut q);
        let f16q = f32_to_f16_vec(&q);

        let results = index.search(&f16q, &SearchFilters::default(), 5);
        assert_eq!(results.len(), 2);
        // First result should be v1 (higher dot product)
        assert_eq!(results[0].op_id.seq, 1);
    }

}
