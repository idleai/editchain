use editchain_engine::{
    encode_op,
    queries::{ChainQueries, ContentField, ContentQuery, GitQuery, PageRequest},
    Admission, Engine, FileEdit, FileOp, FileStage, OpKind, ParentSet, PathId, Payload,
    RepositoryId,
};

use super::commit;
use crate::{append, found, id, message, record, reference};

fn answers(queries: &ChainQueries) -> std::io::Result<serde_json::Value> {
    Ok(serde_json::json!({
        "history": queries.history(None, PageRequest::default())?,
        "search": queries.search("needle", None, PageRequest::default())?,
        "content": queries.content(ContentQuery { operation: id(1), field: ContentField::MessageContent })?,
        "diff": queries.diff(id(4))?,
        "provenance": queries.provenance(id(4))?,
        "ancestors": queries.ancestors(id(4), 20)?,
        "relationships": queries.relationships(None, PageRequest::default())?,
        "git": queries.git(GitQuery { repository: RepositoryId(1), oid: None }, PageRequest::default())?,
        "conflict": queries.operation(id(3))?,
        "variants": queries.evidence(id(3))?,
    }))
}

#[test]
fn rebuild_reopen_and_reverse_replay_preserve_all_query_results_and_bytes() {
    let source = tempfile::tempdir().unwrap();
    let replica = tempfile::tempdir().unwrap();
    let engine = Engine::open(source.path()).unwrap();
    let target = Engine::open(replica.path()).unwrap();
    let bytes = b"retained needle\0\xff\r\n";
    let blob = engine.store_blob(bytes).unwrap();
    let canonical = encode_op(&message(1, Payload::Inline(b"needle".to_vec()))).unwrap();
    // Preserve an accepted overlong varint; evidence must not hash a re-encoding.
    let mut original = vec![0x81, 0];
    original.extend_from_slice(canonical.get(1..).unwrap());
    assert_eq!(
        engine.append_encoded(&original).unwrap(),
        Admission::Accepted
    );
    let mut revision = record(
        4,
        OpKind::File(FileOp {
            path: PathId(42),
            stage: FileStage::Applied,
            base: Some(blob.id),
            after: Some(reference(b"unavailable").unwrap().id),
            edit: FileEdit::Blob(blob),
        }),
    );
    revision.parents = ParentSet::Two(id(1), id(3));
    append(
        &engine,
        &[
            message(2, Payload::Blob(blob)),
            message(3, Payload::Inline(b"one".to_vec())),
            revision,
            commit(5, RepositoryId(1)),
        ],
    )
    .unwrap();
    assert_eq!(
        engine
            .append(&message(3, Payload::Inline(b"two".to_vec())))
            .unwrap(),
        Admission::Conflict
    );
    let mut queries = engine.queries().unwrap();
    assert_eq!(
        found(queries.operation(id(1)).unwrap())
            .unwrap()
            .evidence
            .record_hash,
        *blake3::hash(&original).as_bytes()
    );
    assert_ne!(
        *blake3::hash(&original).as_bytes(),
        *blake3::hash(&canonical).as_bytes()
    );
    let expected = answers(&queries).unwrap();
    let before = engine.snapshot().unwrap();
    let _stats = queries.rebuild().unwrap();
    assert_eq!(answers(&queries).unwrap(), expected);
    drop(queries);
    assert_eq!(answers(&engine.queries().unwrap()).unwrap(), expected);
    let encoded: Vec<_> = before
        .evidence()
        .evidence()
        .map(|(_, bytes)| bytes.to_vec())
        .collect();
    for bytes in encoded.iter().rev() {
        let _admission = target.append_encoded(bytes).unwrap();
        assert_eq!(target.append_encoded(bytes).unwrap(), Admission::Duplicate);
    }
    assert_eq!(target.store_blob(bytes).unwrap(), blob);
    assert_eq!(answers(&target.queries().unwrap()).unwrap(), expected);
    assert_eq!(engine.snapshot().unwrap().evidence(), before.evidence());
    let decoded: editchain_engine::queries::QueryPage<editchain_engine::queries::HistoryEntry> =
        serde_json::from_value(expected.get("history").unwrap().clone()).unwrap();
    assert_eq!(
        decoded,
        engine
            .queries()
            .unwrap()
            .history(None, PageRequest::default())
            .unwrap()
    );
}
