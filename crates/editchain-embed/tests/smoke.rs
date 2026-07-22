#![doc = "Smoke tests for HTTP embedding backend."]

use serde as _;
use serde_json as _;

use editchain_embed::http::HttpEmbedder;
use editchain_embed::{Embedder, EmbeddingManifest};

#[test]
#[expect(
    clippy::indexing_slicing,
    reason = "test assertion on known-valid index"
)]
#[ignore = "Requires running SGLang server on port 8001"]
fn test_single_embed() {
    let manifest = EmbeddingManifest::qwen3_embedding_0_6b();
    let embedder = HttpEmbedder::new(manifest, "http://localhost:8001".to_string());

    let result = embedder.embed(&["hello world".to_string()]);
    assert!(result.is_ok(), "single embed failed: {:?}", result.err());
    let vecs = result.unwrap();
    assert_eq!(vecs.len(), 1);
    assert_eq!(vecs[0].len(), 1024);
}

#[test]
#[ignore = "Requires running SGLang server on port 8001"]
fn test_batch_embed() {
    let manifest = EmbeddingManifest::qwen3_embedding_0_6b();
    let embedder = HttpEmbedder::new(manifest, "http://localhost:8001".to_string());

    let texts: Vec<String> = (0..32).map(|i| format!("test text {i}")).collect();
    let result = embedder.embed(&texts);
    assert!(result.is_ok(), "batch embed failed: {:?}", result.err());
    let vecs = result.unwrap();
    assert_eq!(vecs.len(), 32);
}

#[test]
#[ignore = "Requires running SGLang server on port 8001"]
fn test_concurrent_batches() {
    let manifest = EmbeddingManifest::qwen3_embedding_0_6b();
    let embedder = HttpEmbedder::new(manifest, "http://localhost:8001".to_string());

    let batches: Vec<Vec<String>> = (0..10)
        .map(|b| (0..32).map(|i| format!("batch {b} text {i}")).collect())
        .collect();

    let result = embedder.embed_batches(&batches);
    assert!(
        result.is_ok(),
        "concurrent embed failed: {:?}",
        result.err()
    );
    let all = result.unwrap();
    assert_eq!(all.len(), 10);
    for v in &all {
        assert_eq!(v.len(), 32);
    }
}
