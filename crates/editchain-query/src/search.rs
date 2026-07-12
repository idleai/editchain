use editchain_core::{ActorId, OpId, SessionId};

/// Search mode — which index(es) to query.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchMode {
    /// BM25 only.
    Lexical,
    /// Dense vector only.
    Vector,
    /// Fused BM25 + vector via RRF.
    Hybrid,
}

/// Consistency mode for search results.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConsistencyMode {
    /// Current lexical snapshot; vector lag is reported but not waited for.
    Lexical,
    /// Wait for full hybrid coverage (vector must be ready).
    Hybrid,
}

/// Graph expansion parameters for context retrieval.
#[derive(Debug, Clone)]
pub struct GraphExpansion {
    /// Number of ancestor generations to include.
    pub ancestors: u32,
    /// Number of descendant generations to include.
    pub descendants: u32,
    /// Maximum nodes to traverse per seed hit.
    pub max_nodes_per_seed: u32,
    /// Maximum total nodes across all seeds.
    pub max_total: u32,
}

impl Default for GraphExpansion {
    fn default() -> Self {
        Self {
            ancestors: 2,
            descendants: 1,
            max_nodes_per_seed: 128,
            max_total: 512,
        }
    }
}

/// Tag-based filter for operation kinds.
#[derive(Debug, Clone)]
pub enum TagFilter {
    Message,
    Tool,
    Command,
    File,
    Reflection,
    Import,
    Error,
}

/// Filters applied to a search query.
#[derive(Debug, Clone, Default)]
pub struct SearchFilters {
    /// Only include these operation kinds.
    pub kinds: Option<Vec<TagFilter>>,
    /// Only include these sessions.
    pub sessions: Option<Vec<SessionId>>,
    /// Only include these actors.
    pub actors: Option<Vec<ActorId>>,
    /// Glob patterns for file paths.
    pub paths: Option<Vec<String>>,
    /// Earliest timestamp (Unix ms).
    pub after: Option<u64>,
    /// Latest timestamp (Unix ms).
    pub before: Option<u64>,
    /// Include raw import records in results.
    pub include_raw: bool,
    /// Include private/thinking content.
    pub include_private: bool,
}

/// A search request.
#[derive(Debug, Clone)]
pub struct SearchRequest {
    /// The query string (used for both BM25 and vector embedding).
    pub query: String,
    /// Which index(es) to search.
    pub mode: SearchMode,
    /// Number of final results to return.
    pub top_k: usize,
    /// Optional filters.
    pub filters: SearchFilters,
    /// Graph expansion parameters.
    pub graph_expansion: GraphExpansion,
    /// Consistency mode.
    pub consistency: ConsistencyMode,
    /// Minimum generation for read-your-writes consistency.
    pub min_generation: Option<u64>,
    /// Maximum time to wait for consistency (milliseconds).
    pub wait_ms: Option<u64>,
}

impl Default for SearchRequest {
    fn default() -> Self {
        Self {
            query: String::new(),
            mode: SearchMode::Hybrid,
            top_k: 20,
            filters: SearchFilters::default(),
            graph_expansion: GraphExpansion::default(),
            consistency: ConsistencyMode::Lexical,
            min_generation: None,
            wait_ms: None,
        }
    }
}

/// A chunk identifier — unique within a chain.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ChunkId {
    pub op_id: OpId,
    pub chunk_ordinal: u32,
}

impl core::fmt::Display for ChunkId {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{}:{}", self.op_id, self.chunk_ordinal)
    }
}

/// Metadata attached to a scored chunk in search results.
#[derive(Debug, Clone)]
pub struct ChunkMetadata {
    pub op_id: OpId,
    pub chunk_id: ChunkId,
    pub session_id: Option<SessionId>,
    pub actor_id: ActorId,
    pub kind_tags: u64,
    pub timestamp_ms: u64,
    pub generation: u64,
}

/// A scored search result chunk.
#[derive(Debug, Clone)]
pub struct ScoredChunk {
    pub chunk_id: ChunkId,
    pub op_id: OpId,
    /// Fused relevance score (higher = more relevant).
    pub score: f64,
    /// The text content of this chunk.
    pub text: String,
    pub metadata: ChunkMetadata,
}

/// Watermarks showing how current each projection is.
#[derive(Debug, Clone, Copy)]
pub struct ProjectionWatermarks {
    pub log: u64,
    pub hydrated: u64,
    pub graph: u64,
    pub lexical: u64,
    pub vector: u64,
}

/// A search response.
#[derive(Debug, Clone)]
pub struct SearchResult {
    pub results: Vec<ScoredChunk>,
    pub watermarks: ProjectionWatermarks,
}

/// A retrieval request for a specific operation or chunk.
#[derive(Debug, Clone)]
pub struct RetrieveRequest {
    /// Retrieve by operation ID.
    pub op_id: Option<OpId>,
    /// Retrieve by chunk ID.
    pub chunk_id: Option<ChunkId>,
    /// Include the raw JSON of the operation.
    pub raw: bool,
    /// Number of ancestor operations to include as context.
    pub ancestors: u32,
    /// Number of descendant operations to include as context.
    pub descendants: u32,
}

/// A summarization request (extractive only).
#[derive(Debug, Clone)]
pub struct SummarizeRequest {
    /// The query/topic to summarize around.
    pub query: String,
    /// Maximum tokens in the summary output.
    pub budget_tokens: usize,
    /// Summarization strategy.
    pub strategy: SummarizeStrategy,
}

/// Extractive summarization strategy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SummarizeStrategy {
    /// Top distinct evidence snippets.
    Extractive,
    /// Causal/time-ordered excerpts.
    Timeline,
    /// Stable spine + branch-diverse evidence + recent unresolved work.
    ContextPack,
}