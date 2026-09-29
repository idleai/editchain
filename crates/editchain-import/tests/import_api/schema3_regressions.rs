use super::*;
use editchain_core::activity::{Entity, Kind, Operation};
use editchain_core::{ChainStart, OpId, ParentSet};
use editchain_import::activity::{migrate, uses_migration_ids};
use editchain_import::{BufferedBlobSink, MemoryBlobSink};

fn saved(root: &Path, ops: &[Op]) -> Result {
    use editchain_import::batch::DurableOpSink as _;
    let mut log = LogStore::new(SegmentStore::open(root)?);
    let _admission = log.append_durable(ops)?;
    Ok(())
}

fn loaded(root: &Path) -> Result<Vec<Op>> {
    let mut ops = Vec::new();
    let _stats = editchain_store::visit_records(root, &mut |_flags, bytes| {
        ops.push(editchain_store::format::decode_op(bytes).map_err(io::Error::other)?);
        Ok(())
    })?;
    Ok(ops)
}

#[test]
fn buffered_capture_matches_preview_and_missing_reads_cannot_accept_cursors() {
    struct WriteOnly;
    impl BlobSink for WriteOnly {
        fn store_blob(
            &mut self,
            _data: &[u8],
        ) -> std::result::Result<(), editchain_import::ImportError> {
            Ok(())
        }
    }
    let temp = tempfile::tempdir().unwrap();
    let mut value: serde_json::Value =
        serde_json::from_slice(records(HUMAN).get(1).unwrap()).unwrap();
    drop(value.as_object_mut().unwrap().insert(
        "padding".into(),
        serde_json::Value::String("x".repeat(editchain_import::sink::INLINE_LIMIT)),
    ));
    std::fs::write(temp.path().join("session.jsonl"), format!("{value}\n")).unwrap();
    let cursors = MemoryCursorStore::new();
    let options = ImportOptions::default();
    let mut memory = MemoryBlobSink::default();
    let preview = Provider::Human
        .capture(temp.path(), &options, &mut memory, &cursors)
        .unwrap()
        .into_schema3(&mut memory)
        .unwrap();
    let mut buffered =
        BufferedBlobSink::new(editchain_store::BlobStore::new(temp.path().join("blobs")).unwrap());
    let durable = Provider::Human
        .capture(temp.path(), &options, &mut buffered, &cursors)
        .unwrap()
        .into_schema3(&mut buffered)
        .unwrap();
    assert_eq!(preview.operations(), durable.operations());
    assert!(durable.operations().iter().any(
        |op| matches!(&op.kind, OpKind::Activity(record) if matches!(record.kind, Kind::File(_)))
    ));
    let original = durable
        .operations()
        .iter()
        .find_map(|op| {
            if let OpKind::Activity(record) = &op.kind {
                if let Kind::Original(value) = &record.kind {
                    return Some(value);
                }
            }
            None
        })
        .unwrap();
    assert_eq!(original.provider, "human");
    assert!(matches!(original.bytes, Payload::Blob(_)));
    buffered.flush().unwrap();
    let mut write_only = WriteOnly;
    let failed = Provider::Human
        .capture(temp.path(), &options, &mut write_only, &cursors)
        .unwrap();
    let keys: Vec<_> = failed
        .proposed_cursors()
        .map(|(key, _)| key.to_owned())
        .collect();
    assert!(failed.into_schema3(&mut write_only).is_err());
    assert!(keys
        .iter()
        .all(|key| cursors.get_cursor(key).unwrap().is_none()));
}

#[test]
fn migration_preserves_chain_start_references_and_old_addresses() {
    let temp = tempfile::tempdir().unwrap();
    std::fs::write(temp.path().join("session.jsonl"), CLAUDE).unwrap();
    let mut blobs = MemoryBlobSink::default();
    let batch = Provider::Claude
        .capture(
            temp.path(),
            &ImportOptions::default(),
            &mut blobs,
            &MemoryCursorStore::new(),
        )
        .unwrap();
    let mut child = batch
        .operations()
        .iter()
        .find(|op| matches!(op.kind, OpKind::Message(_)))
        .unwrap()
        .clone();
    let mut root = child.clone();
    root.id = OpId::from_bytes([11; 32]);
    root.source = None;
    root.parents = ParentSet::None;
    root.kind = OpKind::ChainStart(ChainStart {
        name: b"root".to_vec(),
        version: 3,
    });
    child.parents = ParentSet::One(root.id);
    let old = temp.path().join("old");
    let new = temp.path().join("new");
    saved(&old, &[root.clone(), child.clone()]).unwrap();
    let _report = migrate(&old, &new, || false).unwrap();
    let ops = loaded(&new).unwrap();
    assert!(ops.contains(&root));
    let child_new = ops.iter().find(|op| op.id != root.id).unwrap();
    assert_eq!(child_new.causal_parents(), vec![root.id]);
    let query = editchain_engine::queries::ChainQueries::open(&new).unwrap();
    for id in [root.id, child.id] {
        assert!(matches!(
            query.resolve_id(&id.to_string().parse().unwrap()).unwrap(),
            editchain_engine::queries::IdResolution::Found(_)
        ));
    }
}

#[test]
fn claude_parent_links_remain_distinct_and_session_scoped_after_migration() {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("source");
    std::fs::create_dir(&source).unwrap();
    let records = [
        serde_json::json!({"type":"user", "uuid":"child", "parentUuid":"parent", "logicalParentUuid":"parent", "sessionId":"session-a", "cwd":"/workspace", "message":{"role":"user","content":"child"}}),
        serde_json::json!({"type":"user", "uuid":"parent", "sessionId":"session-a", "cwd":"/workspace", "message":{"role":"user","content":"parent"}}),
    ];
    let mut bytes = Vec::new();
    for record in records {
        serde_json::to_writer(&mut bytes, &record).unwrap();
        bytes.push(b'\n');
    }
    std::fs::write(source.join("session.jsonl"), bytes).unwrap();
    let mut blobs = MemoryBlobSink::default();
    let capture = || {
        Provider::Claude
            .capture(
                &source,
                &ImportOptions::default(),
                &mut MemoryBlobSink::default(),
                &MemoryCursorStore::new(),
            )
            .unwrap()
    };
    let fresh = capture().into_schema3(&mut blobs).unwrap();
    let old = temp.path().join("old");
    let new = temp.path().join("new");
    saved(&old, capture().operations()).unwrap();
    let _report = migrate(&old, &new, || false).unwrap();
    for ops in [fresh.operations().to_vec(), loaded(&new).unwrap()] {
        let activities: Vec<_> = ops.iter().filter_map(Operation::view).collect();
        let find = |uuid: &str| {
            activities.iter().find(|record| matches!(&record.kind, Kind::Original(raw) if matches!(&raw.bytes, Payload::Inline(bytes) if serde_json::from_slice::<serde_json::Value>(bytes).unwrap().get("uuid").and_then(serde_json::Value::as_str) == Some(uuid)))).unwrap()
        };
        let parent = find("parent");
        let child = find("child");
        let links: Vec<_> = activities
            .iter()
            .filter_map(|record| {
                if let Kind::Link(link) = &record.kind {
                    if ["ProviderParent", "LogicalParent"].contains(&link.relation.as_str()) {
                        return Some(link);
                    }
                }
                None
            })
            .collect();
        assert_eq!(links.len(), 2);
        for link in links {
            assert_eq!(link.from, Entity::Operation(child.id));
            assert_eq!(link.to, vec![Entity::Item(parent.item)]);
        }
    }
}

#[test]
fn converter_version_is_checked_with_and_without_a_migration_marker() {
    let temp = tempfile::tempdir().unwrap();
    std::fs::write(temp.path().join("session.jsonl"), CLAUDE).unwrap();
    let batch = Provider::Claude
        .capture(
            temp.path(),
            &ImportOptions::default(),
            &mut MemoryBlobSink::default(),
            &MemoryCursorStore::new(),
        )
        .unwrap();
    let old = batch
        .operations()
        .iter()
        .find(|op| matches!(op.kind, OpKind::Message(_)))
        .unwrap();
    let mut record = Operation::upgrade(old).unwrap();
    let current = temp.path().join("current");
    saved(&current, &[record.clone().into_op().unwrap()]).unwrap();
    assert!(!uses_migration_ids(&current).unwrap());
    record.id = OpId::from_bytes(blake3::derive_key(
        "editchain.operation-schema3.v1",
        old.id.as_bytes(),
    ));
    let previous = temp.path().join("previous");
    saved(&previous, &[record.into_op().unwrap()]).unwrap();
    assert!(uses_migration_ids(&previous).is_err());
    std::fs::write(
        previous.join("schema3-migration.json"),
        br#"{"converter":"activity-schema3-v1"}"#,
    )
    .unwrap();
    assert!(uses_migration_ids(&previous).is_err());
    assert_eq!(
        loaded(&previous).unwrap().len(),
        1,
        "old chains remain readable"
    );
}
