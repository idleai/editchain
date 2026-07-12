# Quest 2: CC History Retrieval — Implementation Plan

## Overview

Build a hybrid BM25 + vector search query plane over the edit chain DAG, exposed through a clean CLI interface with online consistency for parallel subagents.

**Embedding candidates:** Qwen3-Embedding-0.6B (512d) or Qwen3-Embedding-4B (preferred if inference budget allows)

---

## Phase 0: Fix Import Correctness (prerequisite)

Before building indexes on top of the chain, fix the known correctness issues in the importer.

### 0.1 — ID collision fix

The current `norm_seq = seq * 1000 + 1` overlaps with raw records at `seq = 1001`.

**Change:** Replace with checked `SourcePosition` packing:

```rust
pub struct SourcePosition {
    pub record_ordinal: u64,
    pub derived_ordinal: u16, // 0 = raw record
}
```

Shift left by 16 bits: `seq = (record_ordinal << 16) | derived_ordinal`. This gives 65,535 derived ops per source record and makes overflow explicit.

**Files:** `crates/editchain-import/src/ids.rs` (add `SourcePosition`), `crates/editchain-import/src/claude_code/normalize.rs` (use it)

### 0.2 — Blob spill for large records

Current code truncates at 4096 bytes. Replace with content-addressed spill using existing `Payload::Blob(BlobRef)`.

**Change:** Add `BlobSink::put(&mut self, bytes: &[u8]) -> Result<BlobRef>` that stores via BLAKE3 hash. Use inline threshold of 4 KiB; spill above that.

**Files:** `crates/editchain-import/src/sink.rs` (extend `BlobSink` trait), `crates/editchain-import/src/claude_code/normalize.rs` (use it)

### 0.3 — Deterministic source lanes

Current importer uses one global sequence across all files. Change to one deterministic lane per physical source stream (session file).

**Change:** Derive `SourceStream` from workspace identity + session file path hash. Use `boot` field for generation tracking on file rewrite.

**Files:** `crates/editchain-import/src/ids.rs`, `crates/editchain-import/src/import.rs`

### 0.4 — Streaming line reader

Current path reads entire file into memory. Replace with streaming `BufRead::read_until(b'\n')` that yields lines one at a time, retaining only a partial final line across reads.

**Files:** `crates/editchain-import/src/claude_code/reader.rs`

### 0.5 — Frame format with checksums

Add EC03 frame format with total length, record count, CRC32C header/payload checksums alongside existing EC02 read compatibility.

**Files:** `crates/editchain-codec/src/frame.rs`, `crates/editchain-codec/src/page.rs`

---

## Phase 1: New Crates & Core Types

### 1.1 — Create `editchain-query` crate

New crate for request/response types, rank fusion, and graph algorithms.

**Key types:**

```rust
// Request
pub struct SearchRequest {
    pub query: String,
    pub mode: SearchMode,           // Lexical | Vector | Hybrid
    pub top_k: usize,
    pub filters: SearchFilters,
    pub graph_expansion: GraphExpansion,
    pub consistency: ConsistencyMode,
    pub min_generation: Option<Generation>,
}

pub struct SearchFilters {
    pub kinds: Option<Vec<TagFilter>>,
    pub sessions: Option<Vec<SessionId>>,
    pub actors: Option<Vec<ActorId>>,
    pub paths: Option<Vec<String>>,
    pub after: Option<u64>,         // timestamp ms
    pub before: Option<u64>,
    pub include_raw: bool,
    pub include_private: bool,
}

// Response
pub struct SearchResult {
    pub results: Vec<ScoredChunk>,
    pub watermarks: ProjectionWatermarks,
}

pub struct ScoredChunk {
    pub chunk_id: ChunkId,
    pub op_id: OpId,
    pub score: f64,
    pub text: String,
    pub metadata: ChunkMetadata,
}

// Graph expansion
pub struct GraphExpansion {
    pub ancestors: u32,
    pub descendants: u32,
    pub max_nodes_per_seed: u32,
    pub max_total: u32,
}
```

**Files:** `crates/editchain-query/Cargo.toml`, `crates/editchain-query/src/lib.rs`, `crates/editchain-query/src/search.rs`, `crates/editchain-query/src/fusion.rs`, `crates/editchain-query/src/graph.rs`

### 1.2 — Create `editchain-index` crate

New crate for text projection (Tantivy BM25), metadata filters, and query snapshots.

**Key types:**

```rust
pub struct QuerySnapshot {
    pub hydrated_generation: Generation,
    pub graph_generation: Generation,
    pub lexical_generation: Generation,
    pub vector_generation: Generation,
    pub lexical: Arc<LexicalSnapshot>,
    pub vectors: Arc<dyn VectorSnapshot>,
    pub graph: Arc<GraphSnapshot>,
    pub metadata: Arc<MetadataSnapshot>,
}

pub struct LexicalSnapshot {
    pub reader: tantivy::IndexReader,
    pub searcher: tantivy::Searcher,
}

pub struct ChunkRecord {
    pub chunk_id: ChunkId,
    pub op_id: OpId,
    pub chunk_ordinal: u32,
    pub byte_start: u32,
    pub byte_end: u32,
    pub generation: Generation,
}
```

**Chunking policy:** Window 768 tokens, overlap 96 tokens. Split on message/tool/command/file boundaries first.

**Files:** `crates/editchain-index/Cargo.toml`, `crates/editchain-index/src/lib.rs`, `crates/editchain-index/src/chunker.rs`, `crates/editchain-index/src/lexical.rs`, `crates/editchain-index/src/snapshot.rs`

### 1.3 — Create `editchain-embed` crate

New crate for embedding model manifests and inference backends.

**Key types:**

```rust
pub trait Embedder: Send + Sync {
    fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>>;
    fn dimensions(&self) -> u32;
    fn max_tokens(&self) -> u32;
}

pub struct EmbeddingManifest {
    pub model_id: String,
    pub revision: String,
    pub dimensions: u32,
    pub max_tokens: u32,
    pub pooling: Pooling,
    pub normalize: bool,
}
```

Start with FastEmbed-rs for in-process Rust inference. Support Qwen3-Embedding-0.6B at 512d as default.

**Files:** `crates/editchain-embed/Cargo.toml`, `crates/editchain-embed/src/lib.rs`, `crates/editchain-embed/src/model.rs`, `crates/editchain-embed/src/fastembed.rs`

---

## Phase 2: Vector Index & Hybrid Fusion

### 2.1 — Exact flat vector index

Store row-major `f16` vectors in segments with RoaringBitmap filters:

```rust
pub struct FlatVectorSegment {
    pub generation_start: Generation,
    pub generation_end: Generation,
    pub dimensions: usize,
    pub doc_ordinals: Vec<DocOrdinal>,
    pub vectors: AlignedVec<f16>,
}
```

Memory estimate for ~150MB corpus (~100K chunks at 512d f16): ~98 MiB.

Maintain RoaringBitmap filters for kind, session, tags, path prefix.

### 2.2 — Reciprocal Rank Fusion

```rust
pub fn rrf_fuse(
    lexical_results: Vec<ScoredChunk>,
    vector_results: Vec<ScoredChunk>,
    k: f64,           // default 60
    top_k: usize,     // default 20
) -> Vec<ScoredChunk>;
```

Starting candidates: BM25=200, vector=200 → fused=20.

### 2.3 — Occurrence collapse

Group chunks by logical content hash / provider UUID. Default output shows one logical result + occurrence count + branch/session locations.

---

## Phase 3: Daemon & Online Consistency

### 3.1 — Append coordinator

Single append coordinator per chain directory ensures monotonic commit generations:

```rust
pub struct AppendCoordinator {
    chain_dir: PathBuf,
    next_generation: AtomicU64,
}
```

### 3.2 — Projector bus & watermarks

```rust
pub struct ProjectionWatermarks {
    pub log: Generation,
    pub hydrated: Generation,
    pub graph: Generation,
    pub lexical: Generation,
    pub vector: Generation,
}
```

Graph and lexical projection have priority (p99 < 250ms). Vector may lag (p99 < 1s). Never allow unbounded embedding queue.

### 3.3 — ArcSwap snapshot publication

```rust
use arc_swap::ArcSwap;

pub struct QueryPlane {
    snapshot: ArcSwap<QuerySnapshot>,
}
```

Readers load one immutable snapshot per query.

### 3.4 — Unix socket daemon

```bash
editchain serve --chain .editchain --watch \
  --lexical-commit-interval 100ms \
  --embedding-model Qwen/Qwen3-Embedding-0.6B \
  --embedding-dim 512
```

CLI commands connect over Unix-domain socket for read operations; direct file access for append.

---

## Phase 4: DAG-Aware Retrieval

### 4.1 — Frontier filtering

For frontier mapping `(node, boot) -> max_seq`:

```
visible(op, frontier) := op.seq <= frontier[op.node, op.boot]
```

Enables "what was known at checkpoint?" queries without full graph traversal.

### 4.2 — Causal cone expansion

For each hit, retrieve bounded context:

```
ancestors:  2 (prompt, tool start, command invocation)
descendants: 1 (results, file effects)
max per seed: 128 nodes
max total: 512 nodes
```

### 4.3 — Causal corridor (`retrieve --why`)

Shortest parent path from hit to selected tip/frontier. Return ordered OpIds + snippets connecting evidence to later work.

### 4.4 — Branch diversity MMR

Graph-aware Maximal Marginal Relevance penalty:

```
utility = relevance - λ_text * duplicate_similarity - λ_graph * graph_overlap
```

---

## Phase 5: CLI Surface

### Lifecycle commands

```bash
editchain serve --chain .editchain [--watch]
editchain index build --chain .editchain [--rebuild]
editchain index status --chain .editchain
```

### Search command

```bash
editchain search "<query>" \
  --mode hybrid|lexical|vector \
  --top 20 \
  --kind message,tool,command \
  --path 'flashinfer/**' \
  --session <uuid> \
  --actor <id> \
  --ancestors 2 --descendants 1 \
  --group-by logical \
  --consistency lexical \
  --min-generation <gen> --wait <ms>
```

### Retrieve command

```bash
editchain retrieve --op <op-id> [--raw]
editchain retrieve --chunk <chunk-id> [--context N:N]
editchain retrieve --op <op-id> --ancestors N --descendants M
editchain retrieve --op <op-id> --why-tip <tip-op-id>
```

### Summarize command (extractive only)

```bash
editchain summarize \
  --query "why does X happen" \
  --budget-tokens 6000 \
  --strategy context-pack|timeline|extractive \
  --format markdown
```

### Tail / read-your-writes

```bash
editchain tail --follow --since-generation <gen>
GEN=$(editchain append --json event.json --print-generation)
editchain search "new fact" --min-generation "$GEN" --wait 1s
```

---

## Phase 6 (Later): ANN & Optimizations

- HNSW via USearch or hnsw_rs when flat scan latency exceeds budget
- Dynamic reachability labels (DAGGER/O'Reach) if BFS becomes bottleneck on million-node DAGs
- MCP server on top of daemon socket
- Generative summarization (future)

---

## Implementation Order (PRs)

| PR | Focus | Crates | Est. Files |
|---|---|---|---|
| **A** | Import correctness fixes | editchain-import, editchain-codec | ~8 |
| **B** | Core query types + chunker | editchain-query (new), editchain-index (new) | ~10 |
| **C** | Tantivy BM25 projection | editchain-index | ~6 |
| **D** | Embedding + flat vector index | editchain-embed (new), editchain-index | ~8 |
| **E** | Hybrid fusion + CLI search | editchain-query, editchain-node | ~6 |
| **F** | Daemon + online consistency | editchain-node | ~5 |
| **G** | DAG-aware retrieval features | editchain-query, editchain-index | ~6 |
| **H** | Summarize + tail commands | editchain-node | ~4 |

---

## Key Design Decisions

1. **Tantivy in RamDirectory** — no fsync overhead; rebuild from chain on restart.
2. **Exact flat vector index first** — HNSW only after measured latency requires it.
3. **f16 vectors** — half precision halves memory vs f32 with negligible accuracy loss.
4. **RRF fusion** — avoids mixing incompatible BM25 and cosine scores.
5. **ArcSwap snapshots** — lock-free reader access; atomic publication.
6. **Unix socket daemon** — warm process keeps indexes + model ready; CLI stays clean.
7. **Qwen3-Embedding-0.6B at 512d** — good balance of quality and inference cost; upgrade to -4B if GPU available.