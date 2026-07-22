//! Editchain embedding — model manifests and inference backends.
//!
//! This crate provides embedding model manifests (`EmbeddingManifest`) and an
//! HTTP-based inference backend (`HttpEmbedder`) that calls OpenAI-compatible
//! `/v1/embeddings` endpoints.

/// HTTP-based embedding backend module.
pub mod http;

/// Model manifest types and traits module.
pub mod model;

pub use model::*;
