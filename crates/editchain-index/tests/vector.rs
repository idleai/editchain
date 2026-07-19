use editchain_core::{NodeId, OpId};
use editchain_embed::EmbeddingManifest;
use editchain_index::vector::{f32_to_f16_vec, normalize_f32, VectorIndex};
use editchain_query::search::SearchFilters;

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