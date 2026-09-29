//! Recorded-provider contracts, restart/retry safety and exact conflict retention.
#![cfg(unix)]

use blake3 as _;
use process_wrap as _;
use proptest as _;
use serde as _;
use sha2 as _;
use time as _;
use tokio as _;

use std::io;
use std::path::{Path, PathBuf};

use editchain_core::provider::ProviderEvidence;
use editchain_core::{NoteRelationship, Op, OpKind, Payload};
use editchain_engine::imports::ImportState;
use editchain_import::batch::ImportBatch;
use editchain_import::codex::{CodexDiscoveryRequest, HelperCommand};
use editchain_import::human::{human_mapping, native_event_id, HumanImportRequest};
use editchain_import::native::{claude_mapping, codex_mappings, NativeIdentity, NativeMapping};
use editchain_import::{
    canonical_source_key, capture_import, hash_raw, BlobSink, CursorStore, DiscoveryRequest,
    FsBlobSink, FsCursorStore, ImportOptions, ImportSource, MemoryCursorStore,
};
use editchain_store::{AppendLog, CanonicalChain, LogReadStats, LogStore, SegmentStore};

#[path = "import_api/schema3_regressions.rs"]
mod schema3_regressions;

type Result<T = ()> = std::result::Result<T, Box<dyn std::error::Error>>;

// Fallible assertions keep fixture IO and validation failures in the same
// Result path, without panicking or suppressing the repository's lint policy.
macro_rules! verify {
    ($condition:expr_2021, $message:expr_2021 $(,)?) => {
        if !$condition {
            return Err($message.into());
        }
    };
}
macro_rules! verify_eq {
    ($left:expr_2021, $right:expr_2021, $message:expr_2021 $(,)?) => {{
        let (left, right) = (&$left, &$right);
        if left != right {
            return Err(format!("{}: left={left:?}, right={right:?}", $message).into());
        }
    }};
}
macro_rules! verify_ne {
    ($left:expr_2021, $right:expr_2021, $message:expr_2021 $(,)?) => {{
        let (left, right) = (&$left, &$right);
        if left == right {
            return Err(format!("{}: both={left:?}", $message).into());
        }
    }};
}

const HUMAN: &[u8] = include_bytes!("fixtures/human/session.jsonl");
const CLAUDE: &[u8] = include_bytes!("fixtures/claude/session.jsonl");
const CODEX: &[u8] = include_bytes!("fixtures/codex/rollout-contract.jsonl");

#[derive(Clone, Copy)]
enum Provider {
    Claude,
    Codex,
    Human,
}

impl Provider {
    fn name(self) -> &'static str {
        match self {
            Self::Claude => "claude-code",
            Self::Codex => "codex",
            Self::Human => "human",
        }
    }

    fn filename(self) -> &'static str {
        match self {
            Self::Claude | Self::Human => "session.jsonl",
            Self::Codex => "rollout-contract.jsonl",
        }
    }

    fn bytes(self) -> &'static [u8] {
        match self {
            Self::Claude => CLAUDE,
            Self::Codex => CODEX,
            Self::Human => HUMAN,
        }
    }

    fn capture(
        self,
        root: &Path,
        options: &ImportOptions,
        blobs: &mut dyn BlobSink,
        cursors: &dyn CursorStore,
    ) -> Result<ImportBatch> {
        let batch = match self {
            Self::Claude => capture_import(
                ImportSource::Claude(&DiscoveryRequest {
                    workspace_path: "/workspace".into(),
                    sessions_dir: root.into(),
                    chain_dir: PathBuf::new(),
                }),
                options,
                blobs,
                cursors,
            )?,
            Self::Human => capture_import(
                ImportSource::Human(&HumanImportRequest {
                    source: root.into(),
                    recorded_root: None,
                }),
                options,
                blobs,
                cursors,
            )?,
            Self::Codex => capture_import(
                ImportSource::Codex {
                    request: &CodexDiscoveryRequest {
                        workspace_path: "/workspace".into(),
                        raw_root: root.into(),
                        selected_paths: Vec::new(),
                        repositories: &(),
                    },
                    helper: &recorded_helper(),
                },
                options,
                blobs,
                cursors,
            )?,
        };
        Ok(batch)
    }
}

fn recorded_helper() -> HelperCommand {
    let fixture =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/codex/projection.ndjson");
    // Replay the recorded exporter output through the number of captured lines.
    // The production helper itself is exercised separately in its own workspace.
    HelperCommand::new(
        "sh",
        vec![
            "-c".into(),
            "awk 'NR == FNR {n++; next} FNR <= n' \"$2\" \"$1\"".into(),
            "recorded-exporter".into(),
            fixture.to_string_lossy().into_owned(),
        ],
    )
}

fn raw_bytes(op: &Op, blobs: &FsBlobSink) -> Result<Option<Vec<u8>>> {
    let OpKind::Import(import) = &op.kind else {
        return Ok(None);
    };
    let bytes = match &import.raw_ref {
        Payload::Inline(bytes) => bytes.clone(),
        Payload::Blob(reference) => {
            let editchain_core::ContentId::Hash256(hash) = reference.id else {
                return Err("unexpected content identifier".into());
            };
            blobs.get(&hash)?.ok_or("missing raw blob")?
        }
        Payload::Empty => return Err("raw bytes must be retained".into()),
    };
    verify_eq!(
        import.raw_hash,
        Some(hash_raw(&bytes)),
        "hash must bind the exact raw record"
    );
    Ok(Some(bytes))
}

fn records(bytes: &[u8]) -> Vec<&[u8]> {
    bytes.split_inclusive(|byte| *byte == b'\n').collect()
}

fn accepted(chain: &CanonicalChain) -> Vec<Op> {
    chain.located_ops().map(|(op, _)| op.clone()).collect()
}

#[test]
fn recorded_fixtures_preserve_bytes_native_mappings_and_relocated_retry() -> Result {
    for provider in [Provider::Claude, Provider::Codex, Provider::Human] {
        let temp = tempfile::tempdir()?;
        let source = temp.path().join("source");
        std::fs::create_dir(&source)?;
        std::fs::write(source.join(provider.filename()), provider.bytes())?;
        let chain = temp.path().join("chain");
        let mut blobs = FsBlobSink::new(chain.join("blobs"))?;
        let mut cursors = FsCursorStore::new(chain.join("cursors"))?;
        let options = ImportOptions::default();
        let batch = provider.capture(&source, &options, &mut blobs, &cursors)?;
        let original = batch.operations().to_vec();
        let key =
            canonical_source_key(provider.name(), &source, &source.join(provider.filename()))?;
        verify!(
            cursors.get_cursor(&key)?.is_none(),
            "capture cannot accept a checkpoint"
        );
        let raw = original
            .iter()
            .map(|op| raw_bytes(op, &blobs))
            .collect::<Result<Vec<_>>>()?
            .into_iter()
            .flatten()
            .collect::<Vec<_>>();
        verify_eq!(
            raw,
            records(provider.bytes()),
            "every complete physical record must roundtrip"
        );
        assert_native_mappings(provider, &original, &blobs)?;
        let mut writer = LogStore::new(SegmentStore::open(&chain)?);
        let result = batch.persist(&mut writer, &mut cursors)?;
        verify_eq!(
            result.admission.written,
            original.len(),
            "first admission retains every distinct operation"
        );
        verify_eq!(
            result.admission.conflicts,
            0,
            "recorded fixture is internally consistent"
        );
        drop(writer);
        drop(cursors);
        let relocated = temp.path().join("relocated");
        std::fs::rename(&source, &relocated)?;
        let cursors = FsCursorStore::new(chain.join("cursors"))?;
        let unchanged = provider.capture(&relocated, &options, &mut blobs, &cursors)?;
        verify!(
            unchanged.operations().is_empty(),
            "portable cursor survives restart and source relocation"
        );
        let mut lost = MemoryCursorStore::new();
        let retry = provider.capture(&relocated, &options, &mut blobs, &lost)?;
        verify_eq!(
            retry.operations(),
            original,
            "losing a cursor cannot change immutable operation bytes"
        );
        let mut writer = LogStore::new(SegmentStore::open(&chain)?);
        let result = retry.persist(&mut writer, &mut lost)?;
        verify_eq!(
            result.admission.duplicates,
            original.len(),
            "every replay is acknowledged as an exact duplicate"
        );
        verify_eq!(
            result.admission.written,
            0,
            "retry does not append duplicate history"
        );
        verify_eq!(
            writer.snapshot()?.stats().records,
            original.len(),
            "durable log stays compact"
        );
    }
    Ok(())
}

fn native_mappings(
    provider: Provider,
    operations: &[Op],
    blobs: &FsBlobSink,
) -> Result<Vec<NativeMapping>> {
    let mut mappings = Vec::new();
    for op in operations {
        match provider {
            Provider::Claude => {
                if let Some(bytes) = raw_bytes(op, blobs)? {
                    mappings.extend(claude_mapping(
                        op.source.ok_or("source provenance")?,
                        &bytes,
                    ));
                }
            }
            Provider::Human => {
                if let Some(bytes) = raw_bytes(op, blobs)? {
                    mappings.extend(human_mapping(op.source.ok_or("source provenance")?, &bytes));
                }
            }
            Provider::Codex => {
                if let OpKind::Note(note) = &op.kind {
                    if note.relationship == NoteRelationship::ProviderEvidence {
                        let Payload::Inline(bytes) = &note.content else {
                            return Err("expected fixture evidence inline".into());
                        };
                        let evidence: ProviderEvidence = serde_json::from_slice(bytes)?;
                        mappings.extend(codex_mappings(&evidence));
                    }
                }
            }
        }
    }
    Ok(mappings)
}

fn assert_native_mappings(provider: Provider, operations: &[Op], blobs: &FsBlobSink) -> Result {
    let mappings = native_mappings(provider, operations, blobs)?;
    let recorded = match provider {
        Provider::Claude => include_str!("fixtures/claude/identities.json"),
        Provider::Codex => include_str!("fixtures/codex/identities.json"),
        Provider::Human => include_str!("fixtures/human/identities.json"),
    };
    let recorded: Vec<NativeMapping> = serde_json::from_str(recorded)?;
    verify_eq!(
        mappings,
        recorded,
        "recorded native identities, occurrence IDs and output IDs must stay stable"
    );
    let expected = match provider {
        Provider::Claude | Provider::Codex => 4,
        Provider::Human => 5,
    };
    verify_eq!(
        mappings.len(),
        expected,
        "fixture identities must remain mapped"
    );
    for mapping in &mappings {
        let raw = operations
            .iter()
            .find(|op| op.id == mapping.source.id())
            .ok_or("missing mapping source")?;
        verify_eq!(
            mapping.raw_hash,
            hash_raw(&raw_bytes(raw, blobs)?.ok_or("mapping source is not raw")?),
            "mapping binds retained evidence"
        );
        verify!(
            mapping
                .outputs
                .iter()
                .all(|id| operations.iter().any(|op| op.id == id.id())),
            "mapped outputs must exist"
        );
    }
    if matches!(provider, Provider::Codex) {
        let items = mappings
            .iter()
            .map(|mapping| &mapping.identity)
            .collect::<Vec<_>>();
        verify_eq!(
            items.get(1),
            items.get(2),
            "tool call and result retain the same logical native item"
        );
        verify_ne!(
            mappings.get(1).map(|mapping| mapping.source),
            mappings.get(2).map(|mapping| mapping.source),
            "tool revisions retain separate raw evidence"
        );
        for mapping in &mappings {
            verify!(
                matches!(&mapping.identity, NativeIdentity::Codex { thread, turn, .. }
                if thread == "22222222-2222-7222-8222-222222222222" && turn == "turn-1"),
                "owning thread must not be replaced by parent session"
            );
        }
    }
    Ok(())
}

#[test]
fn every_fixture_can_resume_with_overlapping_captures_and_pending_final_line() -> Result {
    for provider in [Provider::Claude, Provider::Codex, Provider::Human] {
        let temp = tempfile::tempdir()?;
        let source = temp.path().join("source");
        std::fs::create_dir(&source)?;
        let path = source.join(provider.filename());
        let parts = records(provider.bytes());
        let prefix = parts.iter().take(2).copied().collect::<Vec<_>>().concat();
        std::fs::write(&path, &prefix)?;
        let chain = temp.path().join("chain");
        let mut blobs = FsBlobSink::new(chain.join("blobs"))?;
        let mut cursors = FsCursorStore::new(chain.join("cursors"))?;
        let options = ImportOptions::default();
        let first = provider.capture(&source, &options, &mut blobs, &cursors)?;
        let overlapping = provider.capture(&source, &options, &mut blobs, &cursors)?;
        let mut writer = LogStore::new(SegmentStore::open(&chain)?);
        let result = first.persist(&mut writer, &mut cursors)?;
        let retry = overlapping.persist(&mut writer, &mut cursors)?;
        verify_eq!(
            retry.admission.duplicates,
            result.admission.written,
            "concurrent captures collapse on durable admission"
        );
        verify_eq!(
            retry.admission.written,
            0,
            "overlapping capture adds no history"
        );
        std::fs::write(
            &path,
            provider
                .bytes()
                .strip_suffix(b"\n")
                .ok_or("fixture must end in newline")?,
        )?;
        let tail = provider.capture(&source, &options, &mut blobs, &cursors)?;
        verify_eq!(
            tail.report().raw_ops,
            parts.len().saturating_sub(3),
            "unterminated last record is deferred"
        );
        let _result = tail.persist(&mut writer, &mut cursors)?;
        drop(cursors);
        std::fs::write(&path, provider.bytes())?;
        let mut cursors = FsCursorStore::new(chain.join("cursors"))?;
        let tail = provider.capture(&source, &options, &mut blobs, &cursors)?;
        verify_eq!(
            tail.report().raw_ops,
            1,
            "restart accepts only the completed tail"
        );
        let _result = tail.persist(&mut writer, &mut cursors)?;
        let incremental = writer.snapshot()?;
        let full = provider.capture(&source, &options, &mut blobs, &MemoryCursorStore::new())?;
        for op in full.operations() {
            verify_eq!(
                incremental.get(op.id),
                Some(op),
                "incremental import must retain each one-shot operation unchanged"
            );
        }
        verify_eq!(
            incremental.stats().duplicates,
            0,
            "no duplicates may be appended to the log"
        );
    }
    Ok(())
}

struct LostAcknowledgement {
    inner: SegmentStore,
}

impl AppendLog for LostAcknowledgement {
    fn visit_records(
        &self,
        visitor: &mut editchain_store::RecordVisitor<'_>,
    ) -> io::Result<LogReadStats> {
        self.inner.visit_records(visitor)
    }
    fn append_record(&mut self, flags: u8, bytes: &[u8]) -> io::Result<()> {
        self.inner.append_record(flags, bytes)?;
        Err(io::Error::other("injected lost append acknowledgement"))
    }
    fn sync(&self) -> io::Result<()> {
        self.inner.sync()
    }
}

#[test]
fn uncertain_append_reopens_without_advancing_any_provider_cursor() -> Result {
    for provider in [Provider::Claude, Provider::Codex, Provider::Human] {
        let temp = tempfile::tempdir()?;
        let source = temp.path().join("source");
        std::fs::create_dir(&source)?;
        std::fs::write(source.join(provider.filename()), provider.bytes())?;
        let chain = temp.path().join("chain");
        let mut blobs = FsBlobSink::new(chain.join("blobs"))?;
        let mut cursors = FsCursorStore::new(chain.join("cursors"))?;
        let batch = provider.capture(&source, &ImportOptions::default(), &mut blobs, &cursors)?;
        let count = batch.operations().len();
        let mut writer = LogStore::new(LostAcknowledgement {
            inner: SegmentStore::open(&chain)?,
        });
        verify!(
            batch.persist(&mut writer, &mut cursors).is_err(),
            "lost acknowledgement must fail visibly"
        );
        drop(writer);
        drop(cursors);
        let mut cursors = FsCursorStore::new(chain.join("cursors"))?;
        let key =
            canonical_source_key(provider.name(), &source, &source.join(provider.filename()))?;
        verify!(
            cursors.get_cursor(&key)?.is_none(),
            "unacknowledged append cannot advance accepted bytes"
        );
        verify!(
            cursors.get_reservation(&key)?.is_some(),
            "source identity must already be reserved"
        );
        let batch = provider.capture(&source, &ImportOptions::default(), &mut blobs, &cursors)?;
        let mut writer = LogStore::new(SegmentStore::open(&chain)?);
        let result = batch.persist(&mut writer, &mut cursors)?;
        verify_eq!(
            result.admission.duplicates,
            1,
            "uncertain complete record is recognized on retry"
        );
        verify_eq!(
            writer.snapshot()?.stats().records,
            count,
            "recovery retains one copy of every operation"
        );
    }
    Ok(())
}

#[test]
fn overlapping_human_archives_preserve_conflicts_without_duplicate_history() -> Result {
    let temp = tempfile::tempdir()?;
    let source = temp.path().join("source");
    std::fs::create_dir(&source)?;
    let path = source.join("session.jsonl");
    std::fs::write(&path, HUMAN)?;
    let chain = temp.path().join("chain");
    let mut blobs = FsBlobSink::new(chain.join("blobs"))?;
    let mut cursors = FsCursorStore::new(chain.join("cursors"))?;
    let options = ImportOptions::default();
    let first = Provider::Human.capture(&source, &options, &mut blobs, &cursors)?;
    let original = first.operations().to_vec();
    let mut writer = LogStore::new(SegmentStore::open(&chain)?);
    let _result = first.persist(&mut writer, &mut cursors)?;
    let parts = records(HUMAN);
    std::fs::write(
        source.join("overlap.jsonl"),
        parts.iter().skip(2).copied().collect::<Vec<_>>().concat(),
    )?;
    let overlap = Provider::Human.capture(&source, &options, &mut blobs, &cursors)?;
    let result = overlap.persist(&mut writer, &mut cursors)?;
    verify_eq!(
        result.admission.written,
        0,
        "filename and starting ordinal cannot duplicate native observations"
    );
    verify_eq!(
        result.admission.duplicates,
        6,
        "three observations and their mappings overlap"
    );
    let altered = std::str::from_utf8(HUMAN)?.replace("draft!", "other!");
    std::fs::write(&path, altered)?;
    let conflict = Provider::Human.capture(&source, &options, &mut blobs, &cursors)?;
    let result = conflict.persist(&mut writer, &mut cursors)?;
    verify_eq!(
        result.admission.conflicts,
        1,
        "rewriting native event content preserves its conflict"
    );
    let disputed = native_mappings(Provider::Human, &original, &blobs)?
        .into_iter()
        .find(|mapping| matches!(mapping.identity, NativeIdentity::Human { sequence: 3, .. }))
        .ok_or("missing disputed fixture record")?;
    let snapshot = writer.snapshot()?;
    verify!(
        snapshot.get(disputed.source.id()).is_none(),
        "conflicting identity is excluded from accepted history"
    );
    verify_eq!(
        snapshot.stats().quarantined,
        2,
        "both conflicting variants remain evidence"
    );
    verify_eq!(
        snapshot.stats().duplicates,
        0,
        "no exact variant is appended twice"
    );
    let before = snapshot.stats().records;
    let retry =
        Provider::Human.capture(&source, &options, &mut blobs, &MemoryCursorStore::new())?;
    let _result = retry.persist(&mut writer, &mut MemoryCursorStore::new())?;
    verify_eq!(
        writer.snapshot()?.stats().records,
        before,
        "replaying either side cannot grow or resolve the conflict"
    );
    Ok(())
}

#[test]
fn recorded_claude_copy_preserves_native_identity_and_source_occurrences() -> Result {
    let temp = tempfile::tempdir()?;
    std::fs::write(temp.path().join("session.jsonl"), CLAUDE)?;
    let mut blobs = FsBlobSink::new(temp.path().join("blobs"))?;
    let mut cursors = MemoryCursorStore::new();
    let first =
        Provider::Claude.capture(temp.path(), &ImportOptions::default(), &mut blobs, &cursors)?;
    let initial: std::collections::BTreeSet<_> =
        native_mappings(Provider::Claude, first.operations(), &blobs)?
            .into_iter()
            .map(|mapping| mapping.identity)
            .collect();
    let original_count = first.operations().len();
    let chain = temp.path().join("chain");
    let mut writer = LogStore::new(SegmentStore::open(&chain)?);
    let _result = first.persist(&mut writer, &mut cursors)?;
    std::fs::write(temp.path().join("copied.jsonl"), CLAUDE)?;
    let copied =
        Provider::Claude.capture(temp.path(), &ImportOptions::default(), &mut blobs, &cursors)?;
    let _result = copied.persist(&mut writer, &mut cursors)?;
    let retained = accepted(&writer.snapshot()?);
    let identities: std::collections::BTreeSet<_> =
        native_mappings(Provider::Claude, &retained, &blobs)?
            .into_iter()
            .map(|mapping| mapping.identity)
            .collect();
    verify_eq!(
        identities,
        initial,
        "copied Claude occurrences retain the same native identities"
    );
    verify!(
        retained.len() > original_count,
        "both physical sources remain evidence"
    );
    Ok(())
}

#[test]
fn human_native_id_matches_the_existing_recorder_hash_contract() -> Result {
    let session = "11111111-1111-4111-8111-111111111111";
    let id = editchain_import::human::native_event_source(session, 3)?;
    verify_eq!(
        id.id(),
        native_event_id(session, 3)?,
        "canonical recorder mapping"
    );
    // Pin the public 160-bit identity once, separately from archive IDs.
    verify_eq!(
        id.node.0,
        16_994_115_120_866_439_534,
        "recorder node must preserve the pre-import contract"
    );
    verify_eq!(
        id.boot,
        1_150_987_358,
        "recorder boot must preserve the pre-import contract"
    );
    verify_eq!(
        id.seq,
        3,
        "native sequence is not the physical JSONL ordinal"
    );
    verify_ne!(
        id.id(),
        native_event_id("22222222-2222-4222-8222-222222222222", 3)?,
        "recorder incarnations remain distinct"
    );
    Ok(())
}

#[test]
fn conflicting_variants_survive_retries_for_every_provider() -> Result {
    for provider in [Provider::Claude, Provider::Codex, Provider::Human] {
        let temp = tempfile::tempdir()?;
        let source = temp.path().join("source");
        std::fs::create_dir(&source)?;
        std::fs::write(source.join(provider.filename()), provider.bytes())?;
        let chain = temp.path().join("chain");
        let mut blobs = FsBlobSink::new(chain.join("blobs"))?;
        let mut cursors = MemoryCursorStore::new();
        let batch = provider.capture(&source, &ImportOptions::default(), &mut blobs, &cursors)?;
        let count = batch.operations().len();
        let mut disputed = batch.operations().first().ok_or("empty fixture")?.clone();
        disputed.tags |= editchain_core::Tags::ERROR;
        let mut writer = LogStore::new(SegmentStore::open(&chain)?);
        let encoded = editchain_store::format::encode_op(&disputed)?;
        let _admission = writer.append_encoded(&encoded)?;
        let result = batch.persist(&mut writer, &mut cursors)?;
        verify_eq!(
            result.admission.conflicts,
            1,
            "existing disagreement is preserved on import"
        );
        drop(writer);
        let retry = provider.capture(
            &source,
            &ImportOptions::default(),
            &mut blobs,
            &MemoryCursorStore::new(),
        )?;
        let mut writer = LogStore::new(SegmentStore::open(&chain)?);
        let result = retry.persist(&mut writer, &mut cursors)?;
        verify_eq!(
            result.admission.duplicates,
            count,
            "quarantined variants also deduplicate"
        );
        let snapshot = writer.snapshot()?;
        verify_eq!(
            snapshot.stats().records,
            count.saturating_add(1),
            "exact retries add no log records"
        );
        verify_eq!(
            snapshot.stats().quarantined,
            2,
            "both variants survive reopening"
        );
        verify!(
            snapshot.get(disputed.id).is_none(),
            "retries cannot resurrect disputed history"
        );
    }
    Ok(())
}

#[test]
fn copied_codex_rollout_preserves_revisions_without_duplicating_logical_items() -> Result {
    let temp = tempfile::tempdir()?;
    std::fs::write(temp.path().join("rollout-contract.jsonl"), CODEX)?;
    let chain = temp.path().join("chain");
    let mut blobs = FsBlobSink::new(chain.join("blobs"))?;
    let mut cursors = MemoryCursorStore::new();
    let first =
        Provider::Codex.capture(temp.path(), &ImportOptions::default(), &mut blobs, &cursors)?;
    let original_operations = first.operations().to_vec();
    let initial = ImportState::from_ops(&original_operations);
    verify_eq!(
        initial.codex_items.len(),
        3,
        "the fixture has three logical items and two tool revisions"
    );
    let mut writer = LogStore::new(SegmentStore::open(&chain)?);
    let _result = first.persist(&mut writer, &mut cursors)?;
    let mut queries = editchain_engine::queries::ChainQueries::open(&chain)?;
    verify_eq!(
        queries.import_state()?,
        initial,
        "indexed queries expose shared import state"
    );
    std::fs::write(temp.path().join("rollout-copy.jsonl"), CODEX)?;
    let copy =
        Provider::Codex.capture(temp.path(), &ImportOptions::default(), &mut blobs, &cursors)?;
    let copied_operations = copy.operations().to_vec();
    let _result = copy.persist(&mut writer, &mut cursors)?;
    let retained = accepted(&writer.snapshot()?);
    let projection = ImportState::from_ops(&retained);
    drop(queries.refresh()?);
    verify_eq!(
        queries.import_state()?,
        projection,
        "refresh observes newly imported copies"
    );
    let _rebuilt = queries.rebuild()?;
    verify_eq!(
        queries.import_state()?,
        projection,
        "rebuilding preserves logical reconciliation"
    );
    let serialized = serde_json::to_vec(&projection)?;
    verify_eq!(
        serde_json::from_slice::<ImportState>(&serialized)?,
        projection,
        "reconciliation facts round-trip through machine output"
    );
    verify_eq!(
        projection.codex_items.len(),
        initial.codex_items.len(),
        "copied source extents retain one logical history"
    );
    verify!(
        !projection.copies.is_empty(),
        "shared reconciliation records exact copy equivalences"
    );
    verify_eq!(
        retained.len(),
        original_operations
            .len()
            .saturating_add(copied_operations.len()),
        "logical reconciliation preserves every immutable occurrence"
    );
    let mut reordered = retained.clone();
    reordered.reverse();
    verify_eq!(
        ImportState::from_ops(&reordered),
        projection,
        "replay order cannot choose a different logical history"
    );
    let mut conflicting = copied_operations.clone();
    let output = conflicting
        .iter_mut()
        .find(|op| matches!(op.kind, OpKind::Message(_)))
        .ok_or("missing projected message")?;
    output.clock = editchain_core::Clock::None;
    let mut retained = original_operations.clone();
    retained.extend(conflicting);
    verify_eq!(
        ImportState::from_ops(&retained).codex_items.len(),
        6,
        "equal raw prefixes must not hide different derived interpretations"
    );
    let mut missing = original_operations.clone();
    let missing_id = missing
        .iter()
        .find(|op| matches!(op.kind, OpKind::Message(_)))
        .ok_or("missing derived fixture message")?
        .id;
    missing.retain(|op| op.id != missing_id);
    let incomplete = ImportState::from_ops(&missing);
    verify!(
        incomplete.codex_items.is_empty(),
        "missing derived operations cannot supply logical state"
    );
    verify!(
        !incomplete.incomplete_sources.is_empty(),
        "incomplete source coverage is explicit"
    );
    let mut previews = original_operations;
    let shortened = copied_operations
        .iter()
        .find(|op| matches!(op.kind, OpKind::Message(_)))
        .ok_or("missing projected message")?
        .id;
    previews.extend(copied_operations);
    verify_eq!(
        ImportState::from_partial_ops(&previews, &std::collections::HashSet::from([shortened]))
            .codex_items
            .len(),
        6,
        "shortened previews cannot prove exact copy equivalence"
    );
    Ok(())
}

#[test]
fn human_root_filters_have_independent_cursors_and_failures_do_not_accept_bytes() -> Result {
    let temp = tempfile::tempdir()?;
    let source = temp.path().join("session.jsonl");
    std::fs::write(&source, HUMAN)?;
    let mut blobs = FsBlobSink::new(temp.path().join("blobs"))?;
    let mut cursors = MemoryCursorStore::new();
    let options = ImportOptions::default();
    let mut request = HumanImportRequest {
        source,
        recorded_root: Some("/other".into()),
    };
    let skipped = capture_import(
        ImportSource::Human(&request),
        &options,
        &mut blobs,
        &cursors,
    )?;
    verify!(
        skipped.operations().is_empty(),
        "foreign recorded roots are excluded"
    );
    let mut writer = LogStore::new(SegmentStore::open(temp.path().join("chain"))?);
    let _result = skipped.persist(&mut writer, &mut cursors)?;
    request.recorded_root = Some("/workspace".into());
    let batch = capture_import(
        ImportSource::Human(&request),
        &options,
        &mut blobs,
        &cursors,
    )?;
    verify_eq!(
        batch.report().raw_ops,
        5,
        "a filter cannot inherit another root's skipped cursor"
    );
    let key = batch
        .proposed_cursors()
        .next()
        .ok_or("missing proposed cursor")?
        .0
        .to_owned();
    drop(batch);
    verify!(
        cursors.get_cursor(&key)?.is_none(),
        "discarding a preview leaves accepted cursors unchanged"
    );
    let mut bounded = ImportOptions::default();
    bounded.batch_limits.operations = 1;
    verify!(
        capture_import(
            ImportSource::Human(&request),
            &bounded,
            &mut blobs,
            &cursors
        )
        .is_err(),
        "aggregate limits fail before checkpoint acceptance"
    );
    verify!(
        cursors.get_cursor(&key)?.is_none(),
        "failed capture cannot advance a cursor"
    );
    options.cancellation.cancel();
    verify!(
        capture_import(
            ImportSource::Human(&request),
            &options,
            &mut blobs,
            &cursors
        )
        .is_err(),
        "cancellation is honored"
    );
    verify!(
        cursors.get_cursor(&key)?.is_none(),
        "cancelled capture cannot advance a cursor"
    );
    Ok(())
}

#[test]
fn human_raw_capture_retains_unknown_records_whitespace_and_spilled_evidence() -> Result {
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("session.jsonl");
    let first = *records(HUMAN).first().ok_or("empty fixture")?;
    let line = format!(
        " {}\r\n",
        std::str::from_utf8(first)?.trim_end().replace(
            "1.90.0",
            &"x".repeat(editchain_import::sink::INLINE_LIMIT.saturating_add(1))
        )
    );
    let bytes = format!("{line}\r\n{{broken\n");
    std::fs::write(&path, &bytes)?;
    let mut blobs = FsBlobSink::new(temp.path().join("blobs"))?;
    let batch = Provider::Human.capture(
        temp.path(),
        &ImportOptions::default(),
        &mut blobs,
        &MemoryCursorStore::new(),
    )?;
    verify_eq!(
        batch.report().raw_ops,
        3,
        "blank and malformed lines also survive capture"
    );
    verify_eq!(
        batch.report().malformed,
        2,
        "uninterpretable records are reported explicitly"
    );
    let raw = batch
        .operations()
        .iter()
        .map(|op| raw_bytes(op, &blobs))
        .collect::<Result<Vec<_>>>()?
        .into_iter()
        .flatten()
        .collect::<Vec<_>>();
    verify_eq!(
        raw,
        records(bytes.as_bytes()),
        "line endings, whitespace and opaque payloads stay byte-exact"
    );
    verify_eq!(blobs.len()?, 1, "large raw evidence is content-addressed");
    Ok(())
}

#[test]
fn shorter_codex_copies_reconcile_against_the_longest_complete_source() -> Result {
    let temp = tempfile::tempdir()?;
    std::fs::write(temp.path().join("rollout-full.jsonl"), CODEX)?;
    let prefix: Vec<u8> = CODEX
        .split_inclusive(|byte| *byte == b'\n')
        .take(4)
        .flatten()
        .copied()
        .collect();
    std::fs::write(temp.path().join("rollout-prefix.jsonl"), prefix)?;
    let mut blobs = FsBlobSink::new(temp.path().join("blobs"))?;
    let batch = Provider::Codex.capture(
        temp.path(),
        &ImportOptions::default(),
        &mut blobs,
        &MemoryCursorStore::new(),
    )?;
    let state = ImportState::from_ops(batch.operations());
    verify_eq!(
        state.codex_items.len(),
        3,
        "a shorter source prefix adds no duplicate logical items"
    );
    verify!(
        state.incomplete_sources.is_empty(),
        "both captured sources have complete derivations"
    );
    let copied_raw = state
        .copies
        .iter()
        .filter(|copy| {
            batch
                .operations()
                .iter()
                .any(|op| op.id == copy.operation && matches!(op.kind, OpKind::Import(_)))
        })
        .count();
    verify_eq!(
        copied_raw,
        4,
        "every raw occurrence in the shorter source maps to the complete source"
    );
    Ok(())
}

#[test]
fn schema_three_all_sources_retry_and_raw_backfill_are_stable() -> Result {
    use editchain_core::activity::Kind;
    for provider in [Provider::Claude, Provider::Codex, Provider::Human] {
        let temp = tempfile::tempdir()?;
        let source = temp.path().join("source");
        std::fs::create_dir(&source)?;
        std::fs::write(source.join(provider.filename()), provider.bytes())?;
        let mut blobs = FsBlobSink::new(temp.path().join("blobs"))?;
        let cursors = MemoryCursorStore::new();
        let options = ImportOptions {
            include_thinking: true,
            ..ImportOptions::default()
        };
        let legacy = provider.capture(&source, &options, &mut blobs, &cursors)?;
        let first = editchain_import::activity::convert(legacy.operations(), &mut blobs)?;
        let second = editchain_import::activity::convert(legacy.operations(), &mut blobs)?;
        verify_eq!(
            first,
            second,
            "identical capture has identical schema-three bytes"
        );
        let raw = provider
            .capture(
                &source,
                &ImportOptions {
                    normalize: false,
                    ..options
                },
                &mut blobs,
                &cursors,
            )?
            .into_schema3(&mut blobs)?;
        for op in raw.operations() {
            if matches!(&op.kind, OpKind::Activity(record) if matches!(record.kind, Kind::Original(_)))
            {
                verify!(
                    first.contains(op),
                    "normalization cannot change an Original under the same ID"
                );
            }
        }
        let mut originals = 0;
        let mut activities = 0;
        for op in &first {
            let OpKind::Activity(record) = &op.kind else {
                return Err("new captures must use schema three".into());
            };
            record.validate()?;
            verify_eq!(
                editchain_store::format::decode_op(&editchain_store::format::encode_op(op)?)?,
                *op,
                "binary roundtrip retains every field"
            );
            verify_eq!(
                serde_json::from_value::<Op>(serde_json::to_value(op)?)?,
                *op,
                "JSON roundtrip retains every field"
            );
            if matches!(record.kind, Kind::Original(_)) {
                originals += 1;
            } else {
                activities += 1;
            }
            if let Some(original) = &record.original {
                verify!(
                    first.iter().any(|op| op.id == original.operation),
                    "direct Original reference resolves"
                );
            }
        }
        verify!(
            originals > 0 && activities > 0,
            "each source supplies originals and typed activities"
        );
    }
    Ok(())
}

#[test]
fn schema_three_migration_retains_old_lookup_and_retries_after_marker_loss() -> Result {
    let temp = tempfile::tempdir()?;
    let source = temp.path().join("source");
    std::fs::create_dir(&source)?;
    std::fs::write(source.join(Provider::Claude.filename()), CLAUDE)?;
    let old = temp.path().join("old");
    let new = temp.path().join("new");
    let mut blobs = FsBlobSink::new(old.join("blobs"))?;
    let mut cursors = FsCursorStore::new(old.join("cursors"))?;
    let batch =
        Provider::Claude.capture(&source, &ImportOptions::default(), &mut blobs, &cursors)?;
    let previous = batch.operations().to_vec();
    let mut log = LogStore::new(SegmentStore::open(&old)?);
    let _report = batch.persist(&mut log, &mut cursors)?;
    drop(log);
    let report = editchain_import::activity::migrate(&old, &new, || false)?;
    verify_eq!(
        usize::try_from(report.records)?,
        previous.len(),
        "migration sees every source record"
    );
    verify!(
        report.output_records < report.records,
        "direct fields remove bookkeeping operations"
    );
    let queries = editchain_engine::queries::ChainQueries::open(&new)?;
    for op in &previous {
        verify!(
            matches!(
                queries.resolve_id(&op.id.to_string().parse()?)?,
                editchain_engine::queries::IdResolution::Found(_)
            ),
            "every old address resolves, including folded notes"
        );
    }
    drop(queries);
    let repeated = temp.path().join("repeated");
    let error = editchain_import::activity::migrate(&new, &repeated, || false)
        .err()
        .ok_or("reconverting schema three must preserve the import namespace")?;
    verify_eq!(
        error.kind(),
        io::ErrorKind::InvalidInput,
        "explicit schema guard"
    );
    verify!(
        !repeated.exists(),
        "a second conversion cannot be published"
    );
    std::fs::remove_file(new.join("schema3-migration.json"))?;
    verify!(
        editchain_import::activity::uses_migration_ids(&new)?,
        "namespace is recoverable from persisted records"
    );
    let mut new_blobs = FsBlobSink::new(new.join("blobs"))?;
    let new_cursors = FsCursorStore::new(new.join("cursors-v3"))?;
    let retry = Provider::Claude
        .capture(
            &source,
            &ImportOptions::default(),
            &mut new_blobs,
            &new_cursors,
        )?
        .into_migrated_schema3(&mut new_blobs)?;
    verify!(
        retry.operations().is_empty(),
        "migration retains accepted source offsets"
    );
    Ok(())
}

#[test]
fn schema_three_normalization_backfill_preserves_already_accepted_records() -> Result {
    for provider in [Provider::Claude, Provider::Codex] {
        let temp = tempfile::tempdir()?;
        let source = temp.path().join("source");
        std::fs::create_dir(&source)?;
        std::fs::write(source.join(provider.filename()), provider.bytes())?;
        let chain = temp.path().join("chain");
        let mut blobs = FsBlobSink::new(chain.join("blobs"))?;
        let mut cursors = MemoryCursorStore::new();
        let batch = provider
            .capture(&source, &ImportOptions::default(), &mut blobs, &cursors)?
            .into_schema3(&mut blobs)?;
        let previous = batch.operations().to_vec();
        let mut writer = LogStore::new(SegmentStore::open(&chain)?);
        let _report = batch.persist(&mut writer, &mut cursors)?;
        let batch = provider
            .capture(
                &source,
                &ImportOptions {
                    include_thinking: true,
                    ..ImportOptions::default()
                },
                &mut blobs,
                &cursors,
            )?
            .into_schema3(&mut blobs)?;
        for candidate in batch.operations() {
            if let Some(existing) = previous.iter().find(|op| op.id == candidate.id) {
                if existing != candidate {
                    let before = serde_json::to_value(existing)?;
                    let after = serde_json::to_value(candidate)?;
                    let differences: Vec<_> = before.as_object().ok_or("expected envelope")?.iter().filter(|(key, value)| after.get(*key) != Some(*value)).map(|(key, value)| serde_json::json!({"field": key, "before": value, "after": after.get(key)})).collect();
                    return Err(format!(
                        "{} backfill changed {}: {}",
                        provider.name(),
                        candidate.id,
                        serde_json::to_string(&differences)?
                    )
                    .into());
                }
            }
        }
        let report = batch.persist(&mut writer, &mut cursors)?;
        verify_eq!(
            report.admission.conflicts,
            0,
            "adding recorded reasoning must not change existing events"
        );
        let snapshot = writer.snapshot()?;
        for op in previous {
            verify_eq!(
                snapshot.get(op.id),
                Some(&op),
                "earlier event stays accepted and unchanged"
            );
        }
    }
    Ok(())
}

#[test]
fn schema_three_old_aliases_keep_conflicts_visible_after_index_rebuild() -> Result {
    use editchain_engine::queries::{ChainQueries, IdResolution, Lookup};
    let temp = tempfile::tempdir()?;
    let source = temp.path().join("source");
    std::fs::create_dir(&source)?;
    std::fs::write(source.join(Provider::Claude.filename()), CLAUDE)?;
    let old = temp.path().join("old");
    let new = temp.path().join("new");
    let mut blobs = FsBlobSink::new(old.join("blobs"))?;
    let mut cursors = MemoryCursorStore::new();
    let batch =
        Provider::Claude.capture(&source, &ImportOptions::default(), &mut blobs, &cursors)?;
    let old_id = batch.operations().first().ok_or("missing source")?.id;
    let mut writer = LogStore::new(SegmentStore::open(&old)?);
    let _report = batch.persist(&mut writer, &mut cursors)?;
    drop(writer);
    let _migration = editchain_import::activity::migrate(&old, &new, || false)?;
    let queries = ChainQueries::open(&new)?;
    let IdResolution::Found(converted) = queries.resolve_id(&old_id.to_string().parse()?)? else {
        return Err("old alias missing".into());
    };
    let Lookup::Found(entry) = queries.operation(converted)? else {
        return Err("converted record missing".into());
    };
    let OpKind::Activity(mut changed) = entry.operation.kind else {
        return Err("expected schema three".into());
    };
    changed.time_ms = Some(changed.time_ms.unwrap_or_default().saturating_add(1));
    drop(queries);
    let result = editchain_engine::Engine::open(&new)?.append(&changed.into_op()?)?;
    verify_eq!(
        result,
        editchain_core::Admission::Conflict,
        "a mapped ID still rejects changed bytes"
    );
    for rebuild in [false, true] {
        if rebuild {
            std::fs::remove_dir_all(new.join("index-v3"))?;
        }
        let queries = ChainQueries::open(&new)?;
        verify_eq!(
            queries.resolve_id(&old_id.to_string().parse()?)?,
            IdResolution::Found(converted),
            "old alias still selects the quarantined identity"
        );
        verify!(
            matches!(queries.operation(converted)?, Lookup::Conflicted(_)),
            "rebuilding cannot resurrect the converted record"
        );
    }
    Ok(())
}
