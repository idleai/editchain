use editchain_embed::EmbeddingManifest;

#[test]
fn manifest_identity_stable() {
    let a = EmbeddingManifest::qwen3_embedding_0_6b();
    let b = EmbeddingManifest::qwen3_embedding_0_6b();
    assert_eq!(a.identity(), b.identity());
}

#[test]
fn manifest_identity_changes_with_dimensions() {
    let a = EmbeddingManifest::qwen3_embedding_0_6b();
    let mut b = EmbeddingManifest::qwen3_embedding_0_6b();
    b.dimensions = 256;
    assert_ne!(a.identity(), b.identity());
}