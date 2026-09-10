//! Codex (OpenAI) session import — capture, bridge, and materialize occurrences.
//!
//! The importer never parses Codex conversation semantics itself. A
//! configurable helper process (`tools/codex-session-exporter` today, or a
//! future `codex rollout-export --format editchain-v1` command) reads each raw
//! rollout JSONL file and writes a versioned `editchain-v1` NDJSON projection
//! to stdout. This crate validates and consumes only that projection; the raw
//! JSONL bytes remain canonical and are preserved byte-exact, one raw
//! `ImportOp` per complete physical line. Like the shared Claude Code reader,
//! a newline-unterminated EOF record remains pending until the source grows;
//! only complete physical records can advance capture or derivation checkpoints.
//!
//! # Wire contract (editchain-v1 projection)
//!
//! ## Invocation
//!
//! The helper is configured as a program plus fixed prefix arguments
//! ([`HelperCommand`]); the rollout path is appended as the final argument and
//! the helper is spawned directly with no shell involved:
//!
//! ```text
//! program [prefix args...] <rollout-path>
//! ```
//!
//! The helper writes the `editchain-v1` NDJSON projection for that file to
//! stdout and must exit `0`. Stdout is captured whole; stderr is captured for
//! diagnostics. A nonzero exit is a fatal [`ImportError::HelperFailed`] and the
//! cursor for that file is not advanced.
//!
//! ## Record schema (bridge envelope)
//!
//! Every record carries `schemaVersion` (must be `"editchain-v1"`),
//! `recordType`, and `sourceOrdinal` — the 1-based physical JSONL line ordinal
//! in the rollout file (the physical line, never the optional Codex item
//! ordinal that paginated files may carry in their payloads; legacy files carry
//! none). Blank/whitespace-only lines emit no record, so `sourceOrdinal` values
//! are strictly increasing with gaps; a single `final` record (when the helper
//! is run with `--final`) closes the stream with the EOF anchor ordinal.
//!
//! `line` records:
//!
//! ```json
//! {"schemaVersion":"editchain-v1","recordType":"line","sourcePath":"...",
//!  "sourceOrdinal":7,
//!  "decode":{"status":"ok","kind":"eventMsg","eventType":"agent_message","timestamp":"..."},
//!  "projection":{
//!    "changedItems":[{"turnId":"turn-1","item":{"kind":"agentMessage","id":"...","text":"..."}}],
//!    "changedTurns":[],"removedTurnIds":[],
//!    "sessionMeta":{"threadId":"<owning thread id>"},"interAgent":null,
//!    "responseItems":[],"compacted":null}}
//! ```
//!
//! `decode.status` is `"ok"` or `"error"` (error lines are non-fatal; they
//! increment [`crate::ImportReport::malformed`] and the raw lane preserves the
//! line). `projection.changedItems` is the upsert lane — each entry carries the
//! owning `turnId` and a typed item projection (`kind` tags: `userMessage`,
//! `agentMessage`, `toolCall`, `collabToolCall`, `commandExecution`,
//! `fileChange`, `imageView`, `reasoning`, `plan`, `contextCompaction`,
//! `subAgentActivity`, `opaque`, …). Unrecognized or future kind tags are
//! non-fatal and remain raw-only. `projection.removedTurnIds` is the remove
//! lane (turn rollback).
//!
//! The bridge correlates message echoes, completion repeats, and compaction
//! replays through stable logical item IDs. The importer preserves every
//! reported change at its witnessing physical occurrence.
//!
//! ## Immutable revisions and logical state
//!
//! The named `codex-occurrences-v1` materialization retains all item upserts,
//! turn metadata changes, and explicit turn removals. Operation namespaces
//! separate items and line-content roles from legacy numeric lanes, so cursor
//! boundaries and reasoning inclusion cannot shift a sibling operation's ID.
//! Tool results refer to the record that actually reported their result;
//! later changes never rewrite an earlier revision's content.
//!
//! Typed evidence binds each materialization to the raw record hash and lists
//! its complete outputs and logical changes. Projection replays those changes
//! to rebuild current logical items; removing a turn retires its active items
//! while preserving their history. Reuse after removal begins a new incarnation.
//! Older normalized operations remain stored and readable through a compatibility
//! view. A one-time derivation backfill supersedes their display content without
//! regenerating their IDs. Incomplete replacement evidence leaves logical state
//! unresolved and cannot revive stale legacy content.
//!
//! Semantic coverage is checkpointed separately from raw capture and metadata
//! upgrades. Enabling normalization or private reasoning later replays complete
//! captured occurrences. Disabling capture does not erase already stored data.
//! Repeated backfills retain the same IDs and operation bytes.
//!
//! ## Session scope
//!
//! A physical file's scope is the owning thread id, resolved in order from (1)
//! bridge metadata (`projection.sessionMeta.threadId`, or the `final` record's
//! `threadId` — both carry `session_meta.payload.id`), (2) raw
//! `session_meta.payload.id`, (3) a deterministic fallback to the rollout
//! filename stem (the file path is the source stream identity). Codex subagent
//! lines often carry a parent `payload.session_id` (and the bridge's
//! `sessionMeta.sessionId`), which is deliberately never used for scope; parent
//! relationships stay in the raw/bridge lane only.
//!
//! ## Project filtering
//!
//! `--provider codex` imports are scoped to the requested workspace. A
//! rollout is included when its projected `sessionMeta.cwd` is equal to or
//! nested within the workspace, and excluded only when cwd is explicitly
//! present and outside it; empty, relative, or otherwise unclassifiable cwd
//! values are included conservatively, so older bridge projections import
//! exactly as before. Filtering runs on the versioned projection before any
//! op or cursor is written, so excluded rollouts never advance cursors and
//! repeated runs stay deterministic.
//!
//! ## Content mapping
//!
//! The bridge carries full typed content for the closed item set, and the
//! mapping in [`super::normalize`] consumes it into source-neutral ops:
//! message text, reasoning summaries and raw chain-of-thought (private,
//! gated on `include_thinking`), plan text, command strings plus aggregated
//! output, file diffs, tool arguments/results/errors (and collab tool
//! prompts), subagent activity identity, per-line inter-agent content
//! (as `Note` ops), and per-line compaction summaries (as `Reflection` ops).
//! Large values spill to blob storage through [`crate::sink::payload_for`]
//! exactly like the raw lane. Unknown or future item kinds stay raw-only.
//!
//! ## Session Git base
//!
//! Codex's `session_meta.git.commit_hash` is the sole source of session-to-Git
//! anchoring. When that value is a full SHA-1/SHA-256 OID and the projected
//! `sessionMeta.cwd` resolves through the host-supplied repository catalog,
//! the importer emits one durable `GitLinkKind::BasedOn` relation
//! from the raw `session_meta` op to that exact commit. Missing/invalid metadata
//! yields no relation. Command text, operation timestamps, and later turns are
//! never inspected or matched to commits. A versioned cursor checkpoint runs
//! this as a metadata-only one-time backfill for already-imported rollouts,
//! without replaying their raw or conversational rows.
//! The CLI supplies its shared Git catalog; library callers explicitly supply
//! a [`RepositoryLookup`] (or `()` when no live repository is available).
//!
//! ## Structural topology
//!
//! Capture preserves typed source extents and lifecycle observations; project
//! resolves relationships from all admitted evidence, across import batches:
//!
//! - `sessionMeta.parentThreadId` yields a visible `SpawnedBy` edge only when
//!   exactly one matching current `collabToolCall.spawnAgent` occurrence (or
//!   older `subAgentActivity.started` occurrence) identifies that child;
//! - `collabToolCall.agentsStates` entries whose per-child status is
//!   `completed` (the exporter's additive `agentsStates` map) yield
//!   `ReconnectsTo` edges — the collab tool's own status or a
//!   `CloseAgent`/`SendInput` tool never completes a child, and the
//!   subAgentActivity kinds (`started`/`interacted`/`interrupted`) are never
//!   completion signals;
//! - legacy pre-R2 `collaboration.list_agents` tool results (a JSON-string
//!   `agents[{agent_name, agent_status:{completed:...}}]`) yield
//!   `ReconnectsTo` edges by mapping each completed `agent_name` to exactly
//!   one `started` marker's `agentPath` in the same thread;
//! - `sessionMeta.forkedFromId` stays in the typed source evidence, without
//!   supplying a timestamp-selected row-level `ForkOf` edge.
//!
//! Missing and ambiguous endpoints stay unlinked. The resolver never uses
//! timestamps, file order, content, names, or proximity as provenance.
//!
//! ## Error handling
//!
//! Schema-level violations — invalid JSON on stdout, missing or wrong
//! `schemaVersion`, missing/out-of-sequence/out-of-range `sourceOrdinal`,
//! unknown `recordType`/`decode.status`, a `final` record not at EOF (or a
//! second `final` record), and a line-record count mismatch on full imports —
//! are fatal [`ImportError::ProjectionProtocol`] errors and the cursor for
//! that file is not advanced. Item-level problems (missing `turnId`/`item`/
//! `id`) drop only the malformed `changedItems` entry, keep the line's record
//! count intact, and increment `ImportReport::malformed`; bridge decode-error
//! lines are non-fatal too.

/// Rollout file discovery in a raw Codex sessions root.
pub mod discover;
mod evidence;
/// Helper process bridge configuration and invocation.
pub mod helper;
/// Top-level import orchestrator for Codex rollouts.
pub mod import;
mod materialize;
/// Normalization of raw lines and projection items into editchain ops.
pub mod normalize;
/// Projection parsing, validation, and item folding.
pub mod projection;
/// Exact session-start Git anchoring from Codex metadata.
mod session_git;
mod title;

pub use discover::{discover_rollouts, RolloutFile};
pub use helper::HelperCommand;
pub use import::{import_codex, CodexDiscoveryRequest};
pub use normalize::{
    build_raw_op, completed_agent_paths_from_tool, inter_agent_summary,
    normalized_ops_for_compaction, normalized_ops_for_inter_agent, normalized_ops_for_item,
    normalized_ops_for_turn, subagent_activity_summary, ItemAnchor, NormalizeContext,
};
pub use projection::{
    parse_projection, CompactedLine, FinalItem, InterAgentLine, Projection, ProjectionError,
    ProjectionItem, ProjectionKind, SessionGitMeta, SessionMeta, TurnMeta,
};
pub use session_git::RepositoryLookup;

/// Cursor checkpoint for Codex normalized metadata. Version six captures typed
/// source/lifecycle evidence for resolution across imports. Version five retains
/// path-specific file changes; version four recognized current collab spawns,
/// version three added session titles, and version two added topology notes.
pub const CODEX_NORMALIZATION_VERSION: u32 = 6;
