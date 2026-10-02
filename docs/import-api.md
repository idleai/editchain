# History import API

`editchain-import` imports Claude, Codex and human archives into caller-selected
stores. It works independently of the viewer and node service.

## Usage

Call `capture_import` to get an inspectable `ImportBatch`, then call `persist`
to save it. This example imports human archives:

```rust
use std::path::Path;
use editchain_import::{capture_import, FsBlobSink, FsCursorStore, ImportOptions, ImportSource};
use editchain_import::human::HumanImportRequest;
use editchain_store::{LogStore, SegmentStore};

fn import_archive(source: &Path, chain: &Path) -> Result<(), Box<dyn std::error::Error>> {
    // Keep exclusive writer ownership through cursor loading, capture and commit.
    let mut writer = LogStore::new(SegmentStore::open(chain)?);
    let mut cursors = FsCursorStore::new(chain.join("cursors"))?;
    let mut blobs = FsBlobSink::new(chain.join("blobs"))?;
    let request = HumanImportRequest {
        source: source.to_path_buf(),
        recorded_root: None,
    };
    let batch = capture_import(
        ImportSource::Human(&request), &ImportOptions::default(),
        &mut blobs, &cursors,
    )?;
    let _durable = batch.persist(&mut writer, &mut cursors)?;
    Ok(())
}
```

For Claude, use `ImportSource::Claude(&DiscoveryRequest)`. For Codex, use
`ImportSource::Codex { request, helper }` with a `CodexDiscoveryRequest` and
`HelperCommand`. The sinks choose the destination; `DiscoveryRequest::chain_dir`
is retained for compatibility.

For collections too large for one `ImportBatch`, call `discover_import_files`
once, optionally filter the returned `ImportFile` values by `path()`, then call
`capture_import_file` and `persist` for each file using the same `LogStore`.
Keep the original `ImportSource` root throughout discovery and capture: this
preserves source identities, nested Claude parents, and spawn sidecar metadata.
Each file gets its own checkpoint overlay and the ordinary source/batch limits.
The writer retains admission state across files. Files committed before a later
failure remain durable; retry the original selection with the same cursors.
The [CLI's glob and manifest modes](cli.md#imports-and-archives) use these APIs.

## Cursors and retries

For large captures, `BufferedBlobSink::new(durable_blob_adapter)` groups payload
publication in bounded cohorts. Call `flush()` successfully before
`ImportBatch::persist`: capture-time references alone do not acknowledge durable
content. A failed flush retains pending bytes for retry. The CLI follows this
ordering automatically; callers using `FsBlobSink` directly retain its immediate
per-payload durability behavior.

- Capture exposes operations, counts and `proposed_cursors()` without advancing
  accepted cursors. Dropping the batch discards its checkpoints; blobs may already
  have been written.
- `persist` reserves source identities, saves operations durably, then commits
  cursors. Its result reports `written`, `duplicates` and `conflicts`.
- Retry failures with the same stores. Exact repeats add no history; conflicting
  variants remain stored and are excluded from accepted history.
- Raw JSONL bytes and line endings are preserved. Prefix hashes detect rewrites;
  an incomplete final line waits for the next import.

## Native identities

| Provider | Mapping API | Identity |
| --- | --- | --- |
| Claude | `native::claude_mapping(source, raw)` | Session and event UUID |
| Codex | `native::codex_mappings(&ProviderEvidence)` | Owning thread, turn, item and incarnation |
| Human | `human::human_mapping(source, raw)` | Recorder session and sequence |

Each mapping references its raw operation and BLAKE3 hash. Missing identifiers
stay unmapped. Copied history collapses only when the recorded evidence agrees;
conflicts and distinct revisions remain. Codex consumers must replay turn
removals as well as item updates.

Use `editchain_engine::imports::ImportState::from_ops(&accepted_ops)` or
`ChainQueries::import_state()` for shared derivation selection and logical replay.
The result includes current Codex items, exact copy equivalences and incomplete
source coverage. Copies coalesce only when complete source prefixes and their
derived operations agree after occurrence-ID rebinding. Divergent interpretations
remain separate. `from_partial_ops` accepts IDs of shortened records, which cannot
prove copy equivalence. Canonical operations and historical revisions stay intact.

`editchain import-state --output json` exposes the same result. Payload availability
is resolved through content queries using the returned operation IDs. Viewer row
hiding, message folding and display continuity remain in the presentation consumer.

Human capture accepts a version-one archive file or JSONL directory.
`recorded_root` optionally filters the exact recorded `workspace_path`, with
separate cursors per filter. Unrecognized records are retained and counted as
`malformed` when unfiltered, or skipped when they cannot match a filter.

Human archive IDs are stable across overlapping files and link to the existing
live IDs from `human::native_event_id`. Editor validation and work derivation
remain in the editor adapter and existing human CLI (f39).

## Compatibility and checks

The default payload cutoff is 16 MiB, with a 512 MiB encoded capture budget and
32 MiB segment rollover. Existing logs and blobs remain readable. For a complete
recapture of history imported with the old 4 KiB cutoff, use a fresh destination.
Inline/blob placement changes encoded operation bytes, so mixing representations
under the same operation IDs produces conflicts. Existing segments are not
rewritten automatically.

The CLI invokes the exporter in the Codex repository at
`tools/codex-session-exporter`. f10 owns completing
the exporter, switching callers and verifying native/import reconciliation.

[Recorded fixtures](../crates/editchain-import/tests/fixtures/README.md) pin raw
bytes and identities. Run the retry, overlap and conflict checks with:

```sh
cargo test -p editchain-import --test import_api --locked
```

Import-to-view regression tests live in web-ui's `history-geometry` package and
run from that repository with
`cargo test -p history-geometry --test legacy_imports --locked`. The importer has no
viewer dependency, including in its test graph.
