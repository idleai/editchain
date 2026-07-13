use std::time::Instant;
use editchain_embed::{Embedder, EmbeddingManifest};
use editchain_embed::http::HttpEmbedder;

#[test]
fn test_single_embed() {
    let manifest = EmbeddingManifest::qwen3_embedding_0_6b();
    let embedder = HttpEmbedder::new(manifest, "http://localhost:8001".to_string());

    let t0 = Instant::now();
    let result = embedder.embed(&["hello world".to_string()]);
    assert!(result.is_ok(), "single embed failed: {:?}", result.err());
    let vecs = result.unwrap();
    assert_eq!(vecs.len(), 1);
    assert_eq!(vecs[0].len(), 1024);
    println!("single embed OK ({}ms)", t0.elapsed().as_millis());
}

#[test]
fn test_batch_embed() {
    let manifest = EmbeddingManifest::qwen3_embedding_0_6b();
    let embedder = HttpEmbedder::new(manifest, "http://localhost:8001".to_string());

    let texts: Vec<String> = (0..32).map(|i| format!("test text {}", i)).collect();
    let t0 = Instant::now();
    let result = embedder.embed(&texts);
    assert!(result.is_ok(), "batch embed failed: {:?}", result.err());
    let vecs = result.unwrap();
    assert_eq!(vecs.len(), 32);
    println!("batch 32 embed OK ({}ms)", t0.elapsed().as_millis());
}

#[test]
fn test_concurrent_batches() {
    let manifest = EmbeddingManifest::qwen3_embedding_0_6b();
    let embedder = HttpEmbedder::new(manifest, "http://localhost:8001".to_string());

    let batches: Vec<Vec<String>> = (0..10).map(|b| {
        (0..32).map(|i| format!("batch {} text {}", b, i)).collect()
    }).collect();

    let t0 = Instant::now();
    let result = embedder.embed_batches(&batches);
    assert!(result.is_ok(), "concurrent embed failed: {:?}", result.err());
    let all = result.unwrap();
    assert_eq!(all.len(), 10);
    for v in &all {
        assert_eq!(v.len(), 32);
    }
    println!("10 concurrent batches OK ({}ms)", t0.elapsed().as_millis());
}