# codex-session-exporter

Isolated **Codex rollout projection bridge** for EditChain R2.

Reads one or more Codex rollout JSONL files and streams a typed, versioned,
deterministic NDJSON projection (`editchain-v1`) to stdout that an EditChain
adapter can map to neutral Message/Tool/Command/File/Reflection/Note ops —
without EditChain ever linking a Codex crate.

- Raw rollout JSONL remains canonical; this bridge is a semantic projection.
- Per physical line, one `line` record carries decode status/diagnostics and
  the `ThreadHistoryBuilder` change set (upsert identity + turn metadata).
- `response_item` messages/reasoning are projected as stable, sourcePath-scoped
  semantic items with full text using Codex's typed structures; legacy
  `event_msg`/`response_item` echoes fold to one logical item.
- Optional `--final` emits a per-file deduplicated session snapshot (including
  response-derived items) with stable references back into the line records.
- Unknown or future Codex item kinds degrade to `kind: "opaque"`; malformed or
  unknown line shapes produce `decode.status: "error"` with the raw `type`
  discriminant preserved — schema evolution is never fatal.

## Build and run contract (local-only)

This is a standalone Cargo mini-workspace (`[workspace]` with no members). It
deliberately links the sibling Codex checkout by relative path:

```text
tools/codex-session-exporter
  -> ../../../codex/codex-rs/rollout              (codex_rollout::decode_rollout_line)
  -> ../../../codex/codex-rs/app-server-protocol  (ThreadHistoryBuilder + v2 ThreadItem/Turn)
  -> ../../../codex/codex-rs/protocol             (ResponseItem/TurnItem typed structures)
```

```sh
# Requires rustc/cargo 1.97 (matches the Codex checkout's toolchain).
cd tools/codex-session-exporter
cargo build --release
./target/release/codex-session-exporter --help
```

Usage:

```text
codex-session-exporter [OPTIONS] <ROLLOUT_JSONL>...
  --final     emit a per-file final-session snapshot record after each file
  --schema    print the editchain-v1 schema summary as JSON and exit
  --stream    retain source reducers; read editchain-stream-v1 batches on stdin
```

The sibling checkout must be present at `../../../codex` relative to this
directory. `Cargo.lock` is committed; the manifest also mirrors the sibling
workspace's `[patch.crates-io]` forks and pins `rama-*` to the same
`0.3.0-alpha.4` prereleases its lockfile resolves (the stable `0.3.0` crates
are not source-compatible with `rama-core` used transitively by
codex-network-proxy).

For realtime collection, `--stream` accepts one JSON batch per physical stdin
line and flushes one reply per batch. It does not open source files:

```json
{"schema":"editchain-stream-v1","source":"rollout-id","generation":0,"after":0,"reset":true,"lines":["{}\n"]}
```

`after` is the number of complete physical source lines already accepted by
this reducer; subsequent requests advance it by `lines.length`. Every string
includes exactly one terminating newline. A null entry denotes one complete
invalid-UTF-8 line whose exact bytes the collector retains separately. Blank
lines advance the cursor without producing a projection record.

Replies contain `schema`, `through`, `records`, `recordsProjected` and `error`.
`records` uses the same `editchain-v1` occurrence schema as offline export.
An exact retry of the last batch returns the same records with zero new
projection work. A missing reducer, generation mismatch or ordinal gap requires
an explicit reset from ordinal zero. The process retains at most 64 source
reducers using LRU eviction. The native collector, rather than this helper,
owns durable admission and checkpoint ordering.

Plain `.jsonl` and gzip-compressed rollouts (magic `1f 8b`) are accepted. A
known `flate2`/`BufRead` interaction surfaces clean stream end as
`UnexpectedEof`; it is treated as EOF. Multi-gzip members are supported.

## Wire schema: editchain-v1

Every record: `schemaVersion`, `recordType`, `sourcePath`, `sourceOrdinal`
(1-based physical line; for `final` records it is the EOF anchor =
`physicalLineCount`), `decode` (status, diagnostic?, kind, eventType?,
rolloutOrdinal?, timestamp?).

### `line` records (one per non-blank physical line)

```jsonc
{
  "schemaVersion": "editchain-v1",
  "recordType": "line",
  "sourcePath": "sessions/rollout-....jsonl",
  "sourceOrdinal": 7,                 // 1-based physical line
  "decode": {
    "status": "ok" | "error",
    "diagnostic": "..." | null,       // decode error detail
    "kind": "eventMsg" | "sessionMeta" | "responseItem" | "compacted" |
            "interAgentCommunication" | "interAgentCommunicationMetadata" |
            "turnContext" | "worldState" | "securityRiskScore" | "unknownJson",
    "eventType": "agent_message" | ...,   // payload.type / legacy payload.event_type
    "rolloutOrdinal": 42 | null,          // optional Codex ordinal (absent in legacy rolls)
    "timestamp": "..." | null
  },
  "projection": {
    "changedItems": [{ "turnId": "turn-1", "item": { "kind": "userMessage",
                       "id": "msg_...", "text": "...", "contentHash": "...", ... },
                       "startedAtMs": ..., "completedAtMs": ... }],
    "changedTurns": [{ "turnId": "turn-1", "status": "completed", "errorMessage": null,
                        "startedAt": ..., "completedAt": ..., "durationMs": ... }],
    "removedTurnIds": ["turn-2"],           // rollback
    "sessionMeta": {
      "threadId": "...", "cwd": "/workspace",
      "git": {                              // one snapshot at session start
        "commitHash": "012345...",
        "branch": "r4",
        "repositoryUrl": "https://github.com/..."
      }
    } | null,
    "interAgent": { ... } | null,           // subagent message content + metadata
    "compacted": { "message": "...", "replacementCount": 1 } | null
  }
}
```

Decode semantics:

- `decode.status` is `ok` for decoded lines and `error` for JSON/UTF-8
  failures and unknown variants. On error, `decode.kind` preserves the raw
  wire `type` discriminant (e.g. `event_msg`, `future_gadget`) so unknown
  future shapes remain observable; `sourceOrdinal` is always present, so
  ordinal alignment is never lost.
- Blank/whitespace-only lines (including the trailing newline) emit no record;
  `sourceOrdinal` values therefore map 1:1 to physical line numbers of the
  emitted records (gaps in the sequence are blank lines).
- Echo lines produce `changedItems` that upsert the same logical item id
  (never a new item): `response_item` message lines mirroring an `event_msg`
  user/agent message, `item_completed` markers for message/reasoning kinds
  that fold onto an already-projected item, and tool-call output echoes.

Typed item projection (closed set, `kind` tag):

| kind | payload (camelCase) |
| --- | --- |
| `userMessage` | id, text, contentHash, attachments[] |
| `agentMessage` | id, text, contentHash, phase |
| `reasoning` | id, summary[], content[] (raw chain-of-thought when exposed) |
| `commandExecution` | id, command, cwd, source, status, exitCode, durationMs, aggregatedOutput |
| `fileChange` | id, status, changes[{path, kind, diff}] |
| `toolCall` | id, tool, server/namespace, pluginId, status, durationMs, arguments, result, errorMessage |
| `collabToolCall` | id, tool, senderThreadId, receiverThreadIds, model, status, prompt, agentsStates (per-child status map) |
| `subAgentActivity` | id, activityKind, agentThreadId, agentPath |
| `plan` | id, text |
| `contextCompaction`, `hookPrompt`, `reviewMode`, `imageView` | identity + counts/review text |
| `opaque` | id, typeName (diagnostic Rust type name), note — **non-fatal fallback for any future/unrecognized item kind** |

`agentsStates` is an additive optional per-child status map (`{childThreadId:
{status, message?}}`) and the only per-child completion signal: a collab tool
call whose own `status` is `Completed` (or a `CloseAgent`/`SendInput`/`Wait`
tool) does not complete children by itself.

Unknown *fields* inside known kinds are intentionally dropped; the raw JSONL
stays canonical.

### `final` records (`--final`)

```jsonc
{
  "schemaVersion": "editchain-v1",
  "recordType": "final",
  "sourcePath": "sessions/rollout-....jsonl",
  "sourceOrdinal": 95,                 // EOF anchor = last physical line
  "decode": { "status": "ok", "kind": "sessionSummary" },
  "physicalLineCount": 95, "decodedLines": 93, "failedLines": 2,
  "failedOrdinals": [7, 42],
  "threadId": "22222222-2222-7222-8222-222222222222",   // owning thread identity
  "sessionMeta": { ... },
  "turns": [{
    "turnId": "turn-1", "status": "completed", "errorMessage": null,
    "startedAt": ..., "completedAt": ..., "durationMs": ...,
    "itemCount": 36,
    "items": [{ "itemId": "item-9", "kind": "commandExecution",
                "firstSeenOrdinal": 12, "lastSeenOrdinal": 17,
                "seenLineCount": 2 },
              { "itemId": "msg_...", "kind": "agentMessage",
                "firstSeenOrdinal": 24, "lastSeenOrdinal": 24,
                "seenLineCount": 1 }]   // response-derived items are merged in
  }],
  "interAgentMessages": 3, "responseItemMessages": 41,
  "responseDerivedItems": 2, "compactions": 1
}
```

Final records are synthetic per-file summaries (flat shape, no `projection`),
emitted after EOF in deterministic order. Response-derived items whose turn id
was never opened by the builder appear in deterministic synthetic `turns`
entries appended after the builder turns.

## Response-item projection and echo folding

Paginated rollouts persist user/agent messages and reasoning only as
`response_item` lines (the builder materializes tool items from
`item_completed`, but not messages/reasoning). The bridge projects them
directly from the typed `ResponseItem` structures:

- `response_item message` (roles `user`/`developer` → `userMessage`,
  `assistant` → `agentMessage`) carries the newline-joined `InputText`/
  `OutputText` content, the Codex `msg_...` id (or a deterministic
  `response-<sourceOrdinal>` fallback), and the passthrough `turn_id` when
  present.
- `response_item reasoning` carries `summary` sections and `content`
  (chain-of-thought) text with the `rs_...` id.
- `response_item function_call` / `custom_tool_call` and their `*_output`
  counterparts project as one `toolCall` item per call, folded by `call_id`,
  carrying parsed arguments and the output result.
- `response_item agent_message` and legacy `inter_agent_communication` lines
  project inter-agent text via `projection.interAgent`.

Echo folding rules (legacy `event_msg` ↔ `response_item`, and `item_completed`
markers):

- Same typed id wins (e.g. the `rs_...`/`msg_...` id on both a marker and its
  response item).
- Otherwise, the most recent un-echoed materialized item of the same kind in
  the same turn with identical text folds; empty reasoning lifecycle markers
  fold to the most recent un-echoed reasoning in the turn.
- `item_completed` message/reasoning markers never create items — they only
  fold onto already-projected items; unmatched markers are ignored.
- Repetition of legitimate identical text (e.g. "same text" twice) always
  produces two logical items: folding requires typed ids, lifecycle position,
  and turn context — `contentHash` is a correlation signal, never the sole
  dedup key.

`item_completed` `FileChange` markers carry real diffs that the builder does
not materialize; the bridge projects them as `fileChange` items keyed by the
marker's item id.

## Dedup and upsert guidance for adapters (corpus-verified)

- **Lifecycle/upsert identity**: `changedItems` entries carry a deterministic
  item id. Key items by `(sourcePath, turnId, itemId)` and upsert — the same
  id is re-emitted with its latest snapshot when an item changes (e.g.
  `commandExecution` begin→end, `toolCall` call→output). Ids are stable
  across runs: builder ids are counter-based (`item-1`, ...), response ids are
  the typed `msg_`/`rs_`/`fc_`/`fco_` values (or `response-<ordinal>`), and
  turn ids come from rollout payloads or the builder's deterministic rollout
  index.
- **Echo correlation**: echo lines upsert the same item id with full text, so
  adapters emit exactly one viewer row per logical item and can ignore nothing
  — the seen-line accounting in the `--final` snapshot reflects every
  projected line.
- **Final reconciliation**: the per-file `--final` record is a deduplicated
  per-turn snapshot keyed by item id with `firstSeenOrdinal` /
  `lastSeenOrdinal` / `seenLineCount`, including response-derived items, so a
  session store can reconcile authoritative state with stable references into
  the line records.

## Session and subagent identity

- `sessionMeta.threadId` is **`session_meta.payload.id`** — the physical rollout
  thread that owns the file (also exposed as `final.threadId`). Use it as the
  owning thread identity for the projection.
- `sessionMeta.sessionId` is often the **parent** id for subagent sessions;
  `parentThreadId`, `forkedFromId`, `agentPath`, `agentNickname`, `agentRole`,
  `threadSource`, and the raw `source` passthrough (e.g.
  `{"subagent":{"thread_spawn":{...}}}`) are exposed separately.
- `sessionMeta.git` is the optional Git snapshot Codex captured when the
  session started (`commitHash`, `branch`, and `repositoryUrl`). It is
  session-level provenance; the bridge never synthesizes per-turn Git state.
- Each physical file is projected independently (fresh builder, fresh id
  space). Item ids and raw lines may repeat across files; always scope by
  `sourcePath`.

## Content policy

This is a local semantic projection over a corpus that is intentionally stored
in Git; there is no separate redaction policy. The projection carries the typed
content EditChain needs for neutral Message/Tool/Command/File/Reflection/Note
ops:

- message text (user/agent/developer), reasoning summaries and raw content,
  command output, file diffs, tool arguments/results/errors, plan text,
  review-mode text, and inter-agent plaintext content;
- command strings come from Codex's own client projection, which is already
  secret-redacted by `codex_secrets::redact_secrets` in the item builders.

Only genuinely non-text payloads stay length/presence-only: encrypted content
(`encryptedContentLength`), image/audio data URIs (presence flags on
attachments), and session base-instruction values. The raw JSONL remains
canonical for anything not projected.

## Testing and validation

```sh
cargo test
```

Unit tests cover: closed JSON shape, content-carrying serialization, legacy
(no `ordinal`) and paginated (`ordinal`) lines, malformed/unknown lines with
ordinal preservation, paginated response-item message/reasoning projection
with full text, legacy echo folding to one logical item, repeated-identical-
text non-dedup, function-call/result folding by `call_id`, `item_completed`
FileChange diffs, inter-agent text, compaction text, lifecycle upsert + final
deduplicated snapshot with response-derived items, per-file determinism
(byte-identical across runs), blank-line ordinal semantics, and gzip input.

## Known caveats / API notes

- The builder drops turn-scoped items whose `turn_id` was never opened by a
  `turn_started` event (a `warn!` in Codex). Real rollouts open turns first, so
  this only affects synthetic/partial inputs.
- `ThreadHistoryChangeSet`/`ThreadHistoryItemChange` are public-field structs
  that do not implement `Serialize`; the bridge maps them into `editchain-v1`
  types. Raw JSONL remains canonical for content-level reconstruction.
- `decode.kind` values use normalized camelCase labels on ok lines and raw wire
  `type` strings on error lines (documented above).
- Echo folding is correlation-based: it requires typed ids, lifecycle position,
  and turn context plus content equality. Identical text in different turns or
  different lifecycle positions is never merged. A synthetic or reordered
  response-item echo that precedes its corresponding event can remain a second
  logical item; raw order remains canonical.
- Discovery in EditChain currently selects uncompressed `rollout-*.jsonl`
  files. Codex's archived `.jsonl.zst` files are not yet imported; gzip support
  here applies only when an explicitly supplied rollout has gzip magic.
- This local bridge intentionally preserves typed tool arguments and results
  from the Git-backed raw corpus. Only command strings produced through Codex's
  item builders carry Codex's built-in secret redaction; this bridge does not
  add a second redaction policy for response-item tool arguments.
- This nested workspace is deliberately outside the main EditChain workspace
  and its CI/`cargo deny` gate. It depends on the explicitly local sibling Codex
  checkout and must be tested separately when that checkout changes.
- If the sibling checkout is updated, re-run `cargo update` and keep the
  `rama-*` pins and `[patch.crates-io]` mirrors in sync with its `Cargo.lock`.
