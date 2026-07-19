/// Pooling strategy for embedding models.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pooling {
    /// Use the [CLS] token embedding.
    Cls,
    /// Mean pool over all token embeddings.
    Mean,
}

/// An embedding model manifest — pinned configuration that determines
/// vector identity. Any manifest change forces a vector rebuild.
#[derive(Debug, Clone)]
pub struct EmbeddingManifest {
    /// HuggingFace model ID (e.g. "Qwen/Qwen3-Embedding-0.6B").
    pub model_id: String,
    /// Model revision (git commit hash or tag).
    pub revision: String,
    /// Output dimensionality.
    pub dimensions: u32,
    /// Maximum input token length.
    pub max_tokens: u32,
    /// Pooling strategy.
    pub pooling: Pooling,
    /// Whether to L2-normalize output vectors.
    pub normalize: bool,
    /// Optional query instruction prefix (e.g. "search_query:").
    pub query_instruction: String,
    /// Optional document prefix (e.g. "search_document:").
    pub document_prefix: String,
}

impl EmbeddingManifest {
    /// Create a manifest for Qwen3-Embedding-0.6B at 1024 dimensions.
    pub fn qwen3_embedding_0_6b() -> Self {
        Self {
            model_id: "Qwen/Qwen3-Embedding-0.6B".to_string(),
            revision: "main".to_string(),
            dimensions: 1024,
            max_tokens: 32768,
            pooling: Pooling::Mean,
            normalize: true,
            query_instruction: String::new(),
            document_prefix: String::new(),
        }
    }

    /// Create a manifest for Qwen3-Embedding-4B at 512 dimensions.
    pub fn qwen3_embedding_4b() -> Self {
        Self {
            model_id: "Qwen/Qwen3-Embedding-4B".to_string(),
            revision: "main".to_string(),
            dimensions: 512,
            max_tokens: 32768,
            pooling: Pooling::Mean,
            normalize: true,
            query_instruction: String::new(),
            document_prefix: String::new(),
        }
    }

    /// A stable identity string for this manifest (used to detect changes).
    pub fn identity(&self) -> String {
        format!(
            "{}|{}|{}|{}|{:?}|{}|{}|{}",
            self.model_id,
            self.revision,
            self.dimensions,
            self.max_tokens,
            self.pooling,
            self.normalize,
            self.query_instruction,
            self.document_prefix,
        )
    }
}

/// The embedder trait — implemented by inference backends.
pub trait Embedder: Send + Sync {
    /// Embed a batch of texts into normalized vectors.
    fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, EmbedError>;

    /// Embed multiple batches concurrently.
    fn embed_batches(&self, batches: &[Vec<String>]) -> Result<Vec<Vec<Vec<f32>>>, EmbedError> {
        // Default sequential fallback.
        let mut all = Vec::new();
        for batch in batches {
            let results = self.embed(batch)?;
            all.push(results);
        }
        Ok(all)
    }

    /// Embed a single query text (applies query instruction if configured).
    fn embed_query(&self, query: &str) -> Result<Vec<f32>, EmbedError> {
        let prefixed = if self.manifest().query_instruction.is_empty() {
            query.to_string()
        } else {
            format!("{} {}", self.manifest().query_instruction, query)
        };
        self.embed(&[prefixed])
            .map(|v| v.into_iter().next().unwrap_or_default())
    }

    /// Embed a single document text (applies document prefix if configured).
    fn embed_document(&self, text: &str) -> Result<Vec<f32>, EmbedError> {
        let prefixed = if self.manifest().document_prefix.is_empty() {
            text.to_string()
        } else {
            format!("{} {}", self.manifest().document_prefix, text)
        };
        self.embed(&[prefixed])
            .map(|v| v.into_iter().next().unwrap_or_default())
    }

    /// Return the embedding manifest.
    fn manifest(&self) -> &EmbeddingManifest;

    /// Return the vector dimensionality.
    fn dimensions(&self) -> u32 {
        self.manifest().dimensions
    }
}

/// Errors that can occur during embedding.
#[derive(Debug)]
pub enum EmbedError {
    /// Model not loaded or unavailable.
    ModelNotReady(String),
    /// Input too long for model's max_tokens.
    InputTooLong { actual: usize, max: usize },
    /// Backend-specific error.
    Backend(String),
}

impl std::fmt::Display for EmbedError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EmbedError::ModelNotReady(msg) => write!(f, "model not ready: {}", msg),
            EmbedError::InputTooLong { actual, max } => {
                write!(f, "input too long: {} tokens (max {})", actual, max)
            }
            EmbedError::Backend(msg) => write!(f, "embedding backend error: {}", msg),
        }
    }
}

impl std::error::Error for EmbedError {}

