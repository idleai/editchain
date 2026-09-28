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
[record schema](engine-api.md). Supply stable producer IDs; ID arguments use
decimal `node:boot:seq`. JSON bytes are integer arrays, and consumers must
preserve 64-bit integers.

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
| `export`, `append --archive` | Export or replay an evidence archive |
| `history`, `annotations`, `reflections` | Page accepted records |
| `operation ID`, `variants ID` | Look up an operation or all its exact variants |
| `search TEXT` | Search literal, case-sensitive text |
| `content ID --field FIELD`, `blob JSON` | Resolve a recorded field or content ID |
| `diff ID`, `compare --before JSON --after JSON` | Compare file revisions or two `ContentQuery` fields |
| `meta ID` | Read an operation's metadata: actor/session records, direct parents and relationships |
| `ancestors ID` | Walk recorded causal parents |
| `relationships [--entity JSON]`, `git --query JSON` | Query recorded relationships and Git evidence |
| `follow` (`subscribe`) | Stream snapshots and changes |
| `integrity` (`check`), `rebuild` | Audit evidence or rebuild the derived index |
| `replicate --peer PATH` or `replicate --stdio` | Exchange operations and blobs |

Paged queries accept `--after ID` and `--limit 1..1000`. Follow `next_after` even
when a filtered page is empty. History, search, annotations and reflections also
accept `--key '{"Actor":9}'` or `--key '{"Session":12}'`. Content fields include
`MessageContent`, `FileAfter` and `ImportRaw`. See the [query guide](engine-queries.md)
for schemas and result semantics. `rebuild` recovers damaged checkpoints without
rewriting records or blobs.

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
raw evidence and identity links; editor replay and viewer checkpoints remain in
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
file (by default 1,000,000 captured operation variants and 256 MiB encoded bytes),
so an individual oversized file still fails. Memory used for the current capture
is bounded by those limits; the writer also retains admission state for the chain.
Bulk `--dry-run` streams one capture record per file followed by a summary, as a
JSON array or JSONL, without creating the destination. Capture reports describe
each file independently; the summary counts exact duplicates and conflicts
across all captured files and manifest sources, retaining admission evidence in
memory. Preview uses fresh capture state and does not read destination cursors
or operations. Its summary reports zero writes and uses the same exit codes
for malformed evidence and conflicts as durable import. Successful durable runs
emit one aggregate report with per-source counts and phase timings. `--progress`
writes completed-file progress to stderr; malformed evidence still returns exit
3 after processing all selected files, while conflicts return exit 4.

```sh
editchain --chain /tmp/history export --output jsonl > evidence.jsonl
editchain --chain /tmp/copy init
editchain --chain /tmp/copy append --archive --input evidence.jsonl
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
resumes from durable evidence after interruption or late blobs. The default
timeout is 30 seconds; use `--timeout-ms` to change it.

## Exit codes

| Code | Meaning |
| --- | --- |
| 0 | Success or exact duplicate; also a closed query/subscription output pipe |
| 1 | Execution/I/O/helper failure, including a closed result pipe during writes |
| 2 | Invalid arguments or input |
| 3 | Missing content/operation or incomplete results |
| 4 | Conflicting evidence or integrity failure |
| 5 | Busy writer or index; retry |
| 130 | Interrupted by Ctrl-C/SIGTERM |

Nonzero exits can accompany useful results and durable writes. Batches are not
atomic: replay the same immutable input after failure. Conflicting evidence is
retained; replaying a duplicate does not restore a quarantined identity.
