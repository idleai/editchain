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

## Cursors and retries

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

Human capture accepts a version-one archive file or JSONL directory.
`recorded_root` optionally filters the exact recorded `workspace_path`, with
separate cursors per filter. Unrecognized records are retained and counted as
`malformed` when unfiltered, or skipped when they cannot match a filter.

Human archive IDs are stable across overlapping files and link to the existing
live IDs from `human::native_event_id`. Editor validation and work derivation
remain in the editor adapter and existing human CLI (f39).

## Compatibility and checks

The existing CLI and `tools/codex-session-exporter` remain usable. f10 owns moving
the exporter, switching callers and verifying native/import reconciliation.

[Recorded fixtures](../crates/editchain-import/tests/fixtures/README.md) pin raw
bytes and identities. Run the retry, overlap and conflict checks with:

```sh
cargo test -p editchain-import --test import_api --locked
```
