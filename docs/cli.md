# EditChain CLI

The `editchain` package and binary live in `crates/editchain` and use the shared
engine APIs independently of the legacy viewer and host.

```sh
cargo install --locked --path crates/editchain
editchain --chain /tmp/history init
editchain --chain /tmp/history import --provider claude --input /archive/claude
editchain --chain /tmp/history history --output jsonl
```

Use `editchain <command> --help` for all options.

## Inputs and output

Every command accepts `--chain PATH` (default: `.editchain` in the current
directory) and `--output human|json|jsonl` (default: `human`; alias: `--format`).
Flags work before or after the command. JSONL writes one result, page or event
per line; streaming JSON writes an array. Diagnostics go to stderr.

Operation input accepts one JSON object, an array, or JSONL using the shared
[record schema](engine-api.md). Supply stable canonical IDs: JSON uses full
64-digit hexadecimal strings; human query output uses unique prefixes of at least 12
digits. ID arguments and `--after` accept unambiguous prefixes of at least four
digits, full IDs, or legacy `node:boot:seq` addresses. Structured JSON references
require complete IDs. JSON bytes are integer arrays; other numeric IDs still
require exact 64-bit integers. See [EC03 and IDs](ec03.md).

`--input -` reads stdin and is the default for `append`, `annotate`, `reflect`
and `store-blob`. Binary input and staged provider stdin are limited to 64 MiB.
`content --raw` and `blob --raw` write exact bytes without a newline.
`replicate --stdio` reserves stdin/stdout for binary protocol frames and sends
its report to stderr. Only `init` and provider imports create a missing chain.

## Commands

| Command | Purpose |
| --- | --- |
| `init [--input FILE]` | Create storage; optionally append `ChainStart` records |
| `append [--input FILE]` | Append operations; `--encoded` accepts one exact binary record |
| `annotate`, `reflect` | Append complete `Note` or `Reflection` operation envelopes |
| `store-blob [--input FILE]` | Store raw bytes and return their content reference |
| `import --provider claude\|codex\|human --input PATH` | Capture provider history |
| `import-state` | Inspect selected derivations, logical Codex items, exact copies and source gaps |
| `export`, `append --archive` | Export or replay an chain archive |
| `scan [--preview-bytes N] [--original-preview-bytes N]` | Stream schema-three records with bounded payload previews and exact sizes |
| `history`, `annotations`, `reflections` | Page accepted records |
| `operation ID`, `variants ID` | Look up an operation or all its exact variants |
| `search TEXT` | Search literal, case-sensitive text |
| `content ID --field FIELD`, `blob JSON` | Resolve a recorded field or content ID |
| `diff ID`, `compare --before JSON --after JSON` | Compare file revisions or two `ContentQuery` fields |
| `meta ID` | Read an operation's metadata: actor/session records, direct parents and relationships |
| `ancestors ID` | Walk recorded causal parents |
| `relationships [--entity JSON]`, `git --query JSON` | Query recorded relationships and Git records |
| `follow` (`subscribe`) | Stream snapshots and changes |
| `migrate --destination PATH` | Convert a chain into a verified, resumable EC03 copy |
| `integrity` (`check`), `rebuild` | Check stored records or rebuild the derived index |
| `replicate --peer PATH` or `replicate --stdio` | Exchange operations and blobs |

Paged queries accept `--after ID` and `--limit 1..1000`. Follow `next_after` even
when a filtered page is empty. History, search, annotations and reflections also
accept `--key '{"Actor":9}'` or `--key '{"Session":12}'`. Content fields include
`MessageContent`, `FileAfter` and `ImportRaw`. See the [query guide](engine-queries.md)
for schemas and result semantics. `rebuild` recovers damaged checkpoints without
rewriting records or blobs.

## Compact statistics scans

`editchain --chain PATH --output jsonl scan` reads schema-three operations in
physical log order without building the query index. It emits one `record` event
per distinct operation and a final `ready` event with counts. Each entry contains
the recorded envelope, original record hash, encoded byte length, and a
`payload_summary` of field name, storage type, and exact original byte length.
Inline payloads are previews: at most 16,000 bytes per field by default, and zero
bytes for Original source payloads. `payloads_truncated` marks shortened entries.
Use `--preview-bytes` and `--original-preview-bytes` to choose limits from zero to
1 MiB. These preview records are for analysis; use `export` for exact replay.

Duplicates are counted once. Conflicting identities, unsupported operations,
corrupt frames, or incomplete tails fail the command without a `ready` event.
Consumers must require both the final summary and successful exit before using
the scan as complete. Frames and record encodings are validated, but referenced
blobs are not loaded or audited. Run against a fixed capture and check that the
source did not change during the scan; `scan` does not lock out appenders.

## Migrating existing chains

EC02 chains remain readable; new writes require migration. The source stays in
place, and the destination includes the exact original segments and import cursors.

```sh
editchain --chain /archive/old-chain migrate --destination /archive/ec03-chain
editchain --chain /archive/ec03-chain --output json integrity
```

Rerun the same migration command to resume an interruption. Use the destination
for later imports and queries. See the [migration contract](ec03.md#migration-and-recovery)
for validation, storage requirements, and replication compatibility.

## Imports and archives

Provider `--input` accepts a file, directory or stdin. Give stdin
a stable `.jsonl` filename for resumable imports; Codex names start with `rollout-`.

```sh
cat session.jsonl | editchain import --provider human --input - --source-name session.jsonl
editchain import --provider codex --input /archive/codex \
  --workspace /original/repository --codex-helper codex-session-exporter
```

`--workspace` supplies source discovery context; `--recorded-root` filters human
archives by their recorded root. `--dry-run` previews a full capture without
changing storage or cursors, `--raw-only` skips normalization, and
`--include-thinking` includes private reasoning. Cursors advance after durable
admission; incomplete final lines wait for the next import. Human capture retains
original bytes and identity links; editor replay and viewer checkpoints remain in
`editchain-legacy`.

For large directories, add a quoted `--glob` (repeatable) or `--bulk` to capture
and commit one file at a time while keeping one writer open:

```sh
editchain --chain /tmp/history import --provider codex \
  --input /archive/codex --glob '**/*.jsonl' \
  --workspace /original/repository --codex-helper codex-session-exporter --progress
```

Globs filter the provider's normal discovery results. Relative globs are relative
to `--input`; absolute globs are also accepted. Overlapping globs select a file
once. Keep the same input root across retries so provider-relative source IDs
remain stable. No matches are an input error before storage is opened.

Use `--manifest sources.json` instead of `--input` and `--provider` to import
multiple providers or recorded workspaces in one process:

```json
{
  "schema": 1,
  "sources": [
    {"provider": "claude", "input": "cc", "glob": ["**/*.jsonl"]},
    {"provider": "codex", "input": "codex", "glob": ["**/*.jsonl"],
     "workspace": "/original/repository"},
    {"provider": "human", "input": "human", "glob": ["**/*.jsonl"],
     "recorded_root": "/original/repository"}
  ]
}
```

```sh
editchain --chain /tmp/history import --manifest sources.json \
  --codex-helper codex-session-exporter --progress
```

Manifest inputs are directories, resolved relative to the manifest's directory.
Each source can additionally specify `paths`, an exact list relative to its input
root; globs then filter that list. `workspace` defaults to the CLI's `--workspace`.
Provider identity stays explicit, including when a manifest mixes providers.

Bulk runs commit after each file. A failure can leave earlier files committed;
rerun the same command to resume. Existing source and capture limits apply per
file (by default 1,000,000 captured operation variants and 512 MiB encoded bytes),
so an individual oversized file still fails. Memory used for the current capture
is bounded by those limits; the writer also retains admission state for the chain.
Bulk `--dry-run` streams one capture record per file followed by a summary, as a
JSON array or JSONL, without creating the destination. Capture reports describe
each file independently; the summary counts exact duplicates and conflicts
across all captured files and manifest sources, retaining admission state in
memory. Preview uses fresh capture state and does not read destination cursors
or operations. Its summary reports zero writes and uses the same exit codes
for malformed input and conflicts as durable import. Successful durable runs
emit one aggregate report with per-source counts and phase timings. `--progress`
writes completed-file progress to stderr; malformed input still returns exit
3 after processing all selected files, while conflicts return exit 4.

Imports keep payloads up to 16 MiB inline and store larger payloads as blobs.
Segments roll over at a 32 MiB target. When recapturing history imported with the
old 4 KiB cutoff, use a fresh destination; see [import compatibility](import-api.md#compatibility-and-checks).

```sh
editchain --chain /tmp/history export --output jsonl > chain.jsonl
editchain --chain /tmp/copy init
editchain --chain /tmp/copy append --archive --input chain.jsonl
```

Archives preserve exact record variants, conflicts and referenced blobs. They
omit unreferenced blobs, cursors and indexes. Missing content returns exit 3;
omitted damaged or unsupported records return exit 4—retain the original segment
files in that case. An interrupted archive can be replayed safely.

## Follow and replicate

```sh
editchain --chain /tmp/history follow --output jsonl
editchain --chain /tmp/left replicate --peer /tmp/right --namespace my-chain --share-all
```

`follow` emits an initial snapshot, `ready`, then additions, conflict retractions
and content-availability changes, including late records with older IDs. Use
`--once` for a snapshot or `--no-initial` for changes only. Restart from a fresh
snapshot: there is no durable subscription cursor. While following, other index
users can receive exit 5 (busy).

Local replication requires two existing, different chain directories. Both ends
must agree on `--namespace` and explicitly choose `--share-all` or `--scope FILE`.
A scope contains `records` (`RecordKey` objects with `id` and encoded-byte BLAKE3
`digest`) and `blobs` (32-byte hash arrays), authorized independently.

For `--stdio`, connect each process's stdout to the other's stdin through a
caller-authenticated, authorized transport. Replication preserves conflicts and
resumes from stored records after interruption or late blobs. The default
timeout is 30 seconds; use `--timeout-ms` to change it.

## Exit codes

| Code | Meaning |
| --- | --- |
| 0 | Success or exact duplicate; also a closed query/subscription output pipe |
| 1 | Execution/I/O/helper failure, including a closed result pipe during writes |
| 2 | Invalid arguments or input |
| 3 | Missing content/operation or incomplete results |
| 4 | Conflicting records or integrity failure |
| 5 | Busy writer or index; retry |
| 130 | Interrupted by Ctrl-C/SIGTERM |

Nonzero exits can accompany useful results and durable writes. Batches are not
atomic: replay the same immutable input after failure. Conflicting records is
retained; replaying a duplicate does not restore a quarantined identity.

## Operation schema 3

New imports use the ten types and direct IDs described in [operations.md](operations.md).
Use `--legacy` for the older capture contract. `migrate --schema3 --destination PATH`
creates a converted chain with old-address lookups and retained source segments.
History, search, annotations and reflections accept `--kind`, `--item`, `--session`
and `--turn`; named filters combine with AND. They cannot be combined with `--key`.
Paging cursors still advance over inspected candidates, so a filtered page can be empty.
Modern content fields accept names such as `Arguments`, `Output`, `Path`, `Content`,
or indexed JSON such as `'{"MessageBlock":0}'` and `'{"TextEdit":0}'`.
