# Q1: Claude Code History Import — Implementation Plan

## Overview

Build a Claude Code history importer in `crates/editchain-import/` that converts all 5 existing CC session files into a single editchain workspace chain. The importer is deterministic, idempotent, append-friendly, and safe for concurrent agents/humans.

## Phase 1: Reference & Profiling

### 1a. Add reference submodule
Add `delexw/claude-code-trace` at commit `aacd7f0c5dd33ac41f7d17e259b22a723e783c37` as `externals/claude-code-trace`. Study its Rust parser for JSONL envelope parsing, partial-line handling, parent graph, forks/rewinds, and subagent discovery. Do NOT depend on it as a crate — reference only.

### 1b. Build session profiler
A small binary/tool that profiles the corpus of 5 session files:
- Record type distribution (assistant, user, attachment, system, etc.)
- Maximum line size, malformed lines
- Session IDs, UUID overlap across files
- Parent UUID resolution (parentUuid chains)
- Tool-result matching (tool_use_id correlation)
- Subagent indicators (sessionKind: "bg", agent-name records)
- File-history-snapshot coverage

This informs normalization decisions and validates assumptions.

## Phase 2: Lossless Raw Import

Build the byte-oriented JSONL reader and raw import pipeline.

### Files to create:

```
crates/editchain-import/src/
├── lib.rs              # Public API + re-exports
├── error.rs            # ImportError types
├── ids.rs              # Deterministic ID derivation from hashes
├── model.rs            # ImportOp, DiscoveryRequest, ImportOptions, ImportReport
├── sink.rs             # OpSink, BlobSink, CursorStore traits
├── cursor.rs           # File-level cursor for resume/idempotency
└── claude_code/
    ├── mod.rs          # pub mod + re-exports
    ├── discover.rs     # Session file discovery (main + subagents)
    ├── reader.rs       # JSONL line reader with partial-line deferral
    ├── envelope.rs     # Loose envelope parsing (type dispatch)
    └── normalize.rs    # Normalization: CC records → editchain ops
```

### Key behaviors:

**Reader (`reader.rs`):**
- Read JSONL line-by-line from a session file
- Defer partial final lines (power-loss tolerance)
- Hash each complete line (Blake3) for stable identity
- Track byte offset as cursor position

**Raw import (`model.rs` → `ImportOp`):**
- Every complete JSONL line → exactly one `ImportOp` containing:
  - `raw_ref`: inline bytes or blob reference for large lines
  - `raw_hash`: Blake3 hash of the raw line
- Unknown/malformed records still archived as raw ImportOps

**Deterministic IDs (`ids.rs`):**
- Derive all IDs from versioned hashes of stable source data:
  - `NodeId` → hash of workspace path + "node" salt
  - `ActorId` → hash of actor identifier + "actor" salt  
  - `SessionId` → hash of session UUID + "session" salt
  - `OpId` → node + boot + monotonic seq per source stream
  - `PathId` → hash of normalized path + "path" salt
- NEVER use current time, random values, file mtime, or directory enumeration order

**Sinks (`sink.rs`):**
- `OpSink`: accepts encoded operations (postcard bytes)
- `BlobSink`: accepts large payloads for external storage
- `CursorStore`: persists per-file read cursors for idempotency

**Cursor (`cursor.rs`):**
- Track (file_path, mtime_or_generation, byte_offset) per source file
- Re-importing unchanged files → no new ops (exact duplicates)
- Appending to a source → only new records emitted
- Truncation/rewrite → new source generation, prior IDs not reused

## Phase 3: Normalize Main Conversation

Add loose envelope parsing and normalization of the primary conversation flow.

### Envelope parsing (`envelope.rs`):
Parse each JSONL line's top-level fields:
```rust
pub struct CcEnvelope {
    pub record_type: String,       // "user", "assistant", "attachment", etc.
    pub uuid: Option<String>,
    pub parent_uuid: Option<String>,
    pub timestamp: Option<String>,
    pub session_id: Option<String>,
    pub message: Option<CcMessage>,
}

pub struct CcMessage {
    pub role: String,
    pub content: Vec<CcContentBlock>,
}

pub enum CcContentBlock {
    Text { text: String },
    ToolUse { id: String, name: String, input: serde_json::Value },
    ToolResult { tool_use_id: String, content: String },
    Thinking { thinking: String },
}
```

### Normalization mapping (`normalize.rs`):

| CC Record | Editchain Op | Tags |
|---|---|---|
| `user` message | `MessageOp`, human actor | HUMAN \| MESSAGE |
| `assistant` text | `MessageOp`, agent actor | AGENT \| MESSAGE |
| `assistant` tool_use | `ToolOp::Start` | AGENT \| TOOL |
| Matching `user` tool_result | `ToolOp::Finish` | HUMAN \| TOOL |
| Bash tool_use + result | `ToolOp` + `CommandOp` lifecycle | TOOL \| COMMAND |
| Summary/compact boundary | `ReflectionOp` | REFLECTION |
| File attachment / proven write | `FileOp` + path manifest | FILE |
| Hook/denial/source error | `NoteOp` or `ErrorOp` | NOTE \| ERROR |
| Metadata/progress noise/unknown | raw `ImportOp` only | IMPORT |

### Session & Turn IDs:
- Map CC session UUID → editchain `SessionId`
- Group messages by parentUuid chains into turns
- Each turn gets a deterministic `TurnId`

### Actor derivation:
- Human actor from user messages (origin.kind == "human")
- Agent actor from assistant messages (model name or agent-name record)
- Background agents get separate ActorIds

## Phase 4: Agent & File Relationships

### Subagent discovery (`discover.rs`):
- Scan for nested subagent files in CC session directories
- Parse `.meta.json` for parent-child relationships  
- Background sessions (sessionKind: "bg") link to parent sessions
- Forks/rewinds detected via parentUuid graph analysis

### File operations:
- Parse `file-history-snapshot` records for tracked file state
- Extract file attachments (attachment.type == "file")
- Emit `FileOp::Observed` for files seen in snapshots
- Emit `FileOp::Applied/Saved` when tool results prove file writes

### Path manifest:
- Store path-to-content mappings so PathId resolves back to text
- Conservative: only emit materialized FileOp when source proves resulting bytes

## Phase 5: Integration & CLI

### Wire into editchain-node CLI:
```
editchain import claude \
  --workspace <repo> \
  --claude-home ~/.claude \
  --chain .editchain/<chain> \
  [--dry-run]
```

### Cursor atomicity:
Cursor updates must be atomic and occur only after ops and blobs are durably written.

### Batch writer safety:
Before bulk import, ensure the segment writer handles concatenated pages safely and uses exclusive chain locks.

## Required Tests

1. **Raw import**: Every complete source line creates exactly one raw `ImportOp`
2. **Malformed tolerance**: Unknown/malformed lines don't stop the file
3. **Idempotency**: Two imports of unchanged input produce no new ops and no quarantines
4. **Append**: Appending lines preserves every existing op byte-for-byte
5. **Order stability**: Source enumeration order doesn't change accepted op set or logical view
6. **Copied prefixes**: Visible as occurrences but appear once in logical conversation
7. **UUID collision**: Same external UUID with different content is reported, never silently merged
8. **Tool matching**: Tool results link to starts when IDs exist; dangling results preserved
9. **Subagents**: Background sessions remain separately attributable
10. **Crash safety**: Crash before cursor update only causes harmless duplicate replay

## Completion Criteria

The quest is complete when:
1. All 5 linked CC session files import into one workspace chain
2. Sessions, actors, turns, tools, commands are queryable via dump/export
3. Every physical source line is recoverable via raw ImportOps
4. A second import is a no-op (no new ops generated)
5. An appended source adds only new operations
6. The reference submodule is pinned and documented

## Files to Modify

| File | Action |
|---|---|
| `.gitmodules` | Add claude-code-trace submodule |
| `crates/editchain-import/Cargo.toml` | Add dependencies (blake3, serde_json) |
| `crates/editchain-import/src/lib.rs` | Rewrite with full implementation |
| `crates/editchain-import/src/error.rs` | Create |
| `crates/editchain-import/src/ids.rs` | Create |
| `crates/editchain-import/src/model.rs` | Create |
| `crates/editchain-import/src/sink.rs` | Create |
| `crates/editchain-import/src/cursor.rs` | Create |
| `crates/editchain-import/src/claude_code/mod.rs` | Create |
| `crates/editchain-import/src/claude_code/discover.rs` | Create |
| `crates/editchain-import/src/claude_code/reader.rs` | Create |
| `crates/editchain-import/src/claude_code/envelope.rs` | Create |
| `crates/editchain-import/src/claude_code/normalize.rs` | Create |

## Implementation Order

1. Add submodule + build profiler → understand corpus shape
2. Implement raw import pipeline (reader, ids, sink, cursor)
3. Implement envelope parsing + main conversation normalization
4. Implement subagent discovery + file relationship tracking
5. Wire CLI integration + test against all 5 sessions