//! Migration preserves exact original evidence, conflicts, cursors and retry identity.

use crc as _;
use editchain_core::{
    legacy::LegacyOp, ActorId, Clock, MessageOp, NodeId, OpKind, ParentSet, Payload, ScopeRef,
    SourceId, Tags,
};
use editchain_index_pages as _;
use editchain_store::format::{encode_op, encode_page, Page};
use editchain_store::{AppendLog as _, CanonicalChain, LogStore, SegmentStore};
use proptest as _;
use serde as _;
use std::{
    fs, io,
    io::{Seek as _, Write as _},
    path::Path,
};

fn legacy(sequence: u64) -> LegacyOp {
    LegacyOp {
        id: SourceId::new(NodeId(1), 0, sequence),
        parents: ParentSet::One(SourceId::new(NodeId(99), 7, 4)),
        actor: ActorId(2),
        clock: Clock::None,
        scope: ScopeRef::None,
        tags: Tags::MESSAGE,
        kind: OpKind::Message(MessageOp {
            content: Payload::Inline(b"evidence".to_vec()),
            content_type: Payload::Empty,
        }),
    }
}

fn segment(
    root: &Path,
    sequence: u32,
    records: Vec<(u8, Vec<u8>)>,
) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    fs::create_dir_all(root)?;
    let mut page = Page::new(sequence);
    for (flags, bytes) in records {
        page.add_record(flags, bytes);
    }
    let bytes = encode_page(&page)?;
    fs::write(root.join(format!("{sequence:06}.eclog")), &bytes)?;
    Ok(bytes)
}

#[test]
#[expect(
    clippy::panic_in_result_fn,
    reason = "test assertions report mismatches while fixture I/O errors propagate"
)]
fn migration_retains_conflicts_unknown_flags_and_original_bytes(
) -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    let source = temp.path().join("old");
    let destination = temp.path().join("new");
    let first = postcard::to_stdvec(&legacy(1))?;
    let old_received =
        serde_json::json!({"id": legacy(1).id, "digest": blake3::hash(&first).as_bytes()});
    let old_local = serde_json::json!({"id": legacy(2).id, "digest": blake3::hash(&postcard::to_stdvec(&legacy(2))?).as_bytes()});
    let mut variant = vec![0x81, 0];
    variant.extend_from_slice(first.get(1..).ok_or("missing legacy fixture")?);
    let original = segment(
        &source,
        0,
        vec![
            (0xa5, first),
            (0x5a, variant.clone()),
            (7, vec![255]),
            (3, postcard::to_stdvec(&legacy(2))?),
        ],
    )?;
    fs::create_dir_all(source.join("cursors"))?;
    fs::write(source.join("cursors/source.json"), b"{\"generation\":7}")?;
    assert_eq!(
        SegmentStore::open(&source).err().map(|error| error.kind()),
        Some(io::ErrorKind::Unsupported)
    );
    let interrupted = temp.path().join("old-empty-first");
    fs::create_dir_all(&interrupted)?;
    fs::write(interrupted.join("000000.eclog"), [])?;
    let _legacy = segment(&interrupted, 1, vec![(0, postcard::to_stdvec(&legacy(1))?)])?;
    assert_eq!(
        SegmentStore::open(&interrupted)
            .err()
            .map(|error| error.kind()),
        Some(io::ErrorKind::Unsupported)
    );
    fs::create_dir_all(source.join("multiplayer"))?;
    fs::write(
        source.join("multiplayer/scope-revision"),
        4_u64.to_le_bytes(),
    )?;
    fs::write(
        source.join("multiplayer/scope.json"),
        serde_json::to_vec(&serde_json::json!({
            "version": 1, "space": "migration-test", "excluded": [],
            "received": [old_received], "local": [old_local], "received_blobs": [], "revision": 4,
            "cutoff": {"first_segment": 1, "selected_at_ms": 123}
        }))?,
    )?;
    let unowned = temp.path().join("unowned.migrating");
    fs::create_dir_all(&unowned)?;
    fs::write(unowned.join("keep.txt"), b"existing unrelated work")?;
    assert_eq!(
        editchain_store::migration::migrate(&source, &temp.path().join("unowned"), || false)
            .err()
            .map(|error| error.kind()),
        Some(io::ErrorKind::InvalidData)
    );
    assert_eq!(
        fs::read(unowned.join("keep.txt"))?,
        b"existing unrelated work"
    );
    assert!(!unowned.join(".migration.json").exists());
    let report = editchain_store::migration::migrate(&source, &destination, || false)?;
    assert_eq!(report.records, 4);
    assert_eq!(report.unknown_records, 1);
    assert_eq!(report.semantic_digest.len(), 64);
    assert_eq!(fs::read(source.join("000000.eclog"))?, original);
    assert_eq!(
        fs::read(destination.join("migration-v1/original/000000.eclog"))?,
        original
    );
    assert_eq!(
        fs::read(destination.join("cursors/source.json"))?,
        b"{\"generation\":7}"
    );
    assert_eq!(
        fs::read(destination.join("000000.eclog"))?.get(..4),
        Some(b"EC03".as_slice())
    );
    let before = CanonicalChain::read(&source)?.stats();
    let after = CanonicalChain::read(&destination)?.stats();
    assert_eq!(before, after);
    assert_eq!(after.quarantined, 2);
    let manifest: serde_json::Value =
        serde_json::from_slice(&fs::read(destination.join("migration-v1/manifest.json"))?)?;
    assert_eq!(
        manifest.pointer("/originals/0").ok_or("original hash")?,
        &serde_json::Value::String(blake3::hash(&original).to_hex().to_string())
    );
    let chain = CanonicalChain::read(&destination)?;
    let accepted = legacy(2).into_canonical();
    assert_eq!(chain.get(accepted.id), Some(&accepted));
    let scope: serde_json::Value =
        serde_json::from_slice(&fs::read(destination.join("multiplayer/scope.json"))?)?;
    assert_eq!(scope.pointer("/cutoff"), Some(&serde_json::Value::Null));
    assert_eq!(
        scope
            .pointer("/excluded")
            .and_then(serde_json::Value::as_array)
            .map(Vec::len),
        Some(3)
    );
    let received = serde_json::json!({"id": legacy(1).id.id(), "digest": blake3::hash(&encode_op(&legacy(1).into_canonical())?).as_bytes()});
    let local = serde_json::json!({"id": accepted.id, "digest": blake3::hash(&encode_op(&accepted)?).as_bytes()});
    assert_eq!(scope.pointer("/received/0"), Some(&received));
    assert_eq!(scope.pointer("/local/0"), Some(&local));
    assert_eq!(
        fs::read(destination.join("multiplayer/scope-revision"))?,
        5_u64.to_le_bytes()
    );
    let mut writer = LogStore::new(SegmentStore::open(&destination)?);
    assert_eq!(
        writer.append_encoded(&encode_op(&accepted)?)?,
        editchain_core::Admission::Duplicate
    );
    let mut flags = Vec::new();
    let _stats = writer.into_inner().visit_records(&mut |flag, _| {
        flags.push(flag);
        Ok(())
    })?;
    assert_eq!(flags, [0xa5, 0x5a, 7, 3]);
    let location = chain
        .located_ops()
        .find(|(op, _)| op.id == accepted.id)
        .and_then(|(_, location)| location)
        .ok_or("accepted location")?;
    assert!(location.checksum.is_some());
    assert_eq!(
        editchain_store::read_encoded_at(&destination, location)?,
        encode_op(&accepted)?
    );
    let mut file = fs::OpenOptions::new()
        .write(true)
        .open(destination.join(format!("{:06}.eclog", location.segment_seq)))?;
    let _offset = file.seek(io::SeekFrom::Start(location.data_offset))?;
    file.write_all(&[0xfe])?;
    assert_eq!(
        editchain_store::read_encoded_at(&destination, location)
            .err()
            .map(|error| error.kind()),
        Some(io::ErrorKind::InvalidData)
    );

    Ok(())
}

#[test]
#[expect(
    clippy::panic_in_result_fn,
    reason = "test assertions report mismatches while fixture I/O errors propagate"
)]
fn interrupted_migration_resumes_without_duplicate_records(
) -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    let source = temp.path().join("old");
    let destination = temp.path().join("new");
    let staging = temp.path().join("new.migrating");
    for sequence in 0..3 {
        let _original = segment(
            &source,
            sequence,
            vec![(0, postcard::to_stdvec(&legacy(u64::from(sequence)))?)],
        )?;
    }
    let failure = editchain_store::migration::migrate(&source, &destination, || {
        fs::read(staging.join(".migration.json"))
            .ok()
            .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
            .is_some_and(|state| {
                state
                    .pointer("/report/source_segments")
                    .and_then(serde_json::Value::as_u64)
                    == Some(1)
            })
    });
    assert_eq!(
        failure.err().map(|error| error.kind()),
        Some(io::ErrorKind::Interrupted)
    );
    assert!(!destination.exists());
    assert!(SegmentStore::open(&staging).is_err());
    let retained = fs::read(source.join("000000.eclog"))?;
    fs::write(source.join("000000.eclog"), b"changed source")?;
    assert!(editchain_store::migration::migrate(&source, &destination, || false).is_err());
    fs::write(source.join("000000.eclog"), retained)?;
    // Simulate an unacknowledged destination suffix after the durable checkpoint.
    fs::OpenOptions::new()
        .append(true)
        .open(staging.join("000000.eclog"))?
        .write_all(b"EC03partial")?;
    let report = editchain_store::migration::migrate(&source, &destination, || false)?;
    assert_eq!(report.records, 3);
    assert_eq!(CanonicalChain::read(&destination)?.stats().duplicates, 0);
    assert!(!staging.exists());
    assert!(editchain_store::migration::migrate(&source, &destination, || false).is_err());
    let _copied = fs::copy(
        destination.join("migration-v1/manifest.json"),
        destination.join(".migration.json"),
    )?;
    assert!(SegmentStore::open(&destination).is_err());
    let finished = editchain_store::migration::migrate(&source, &destination, || false)?;
    assert_eq!(finished.records, 3);
    assert!(!destination.join(".migration.json").exists());
    Ok(())
}
