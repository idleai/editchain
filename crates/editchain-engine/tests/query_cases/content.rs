use editchain_engine::{
    queries::{ByteComparison, ContentField, ContentQuery, ContentValue, Lookup, PageRequest},
    ByteRange, ContentId, Engine, FileEdit, FileOp, FileStage, OpKind, PathId, Payload,
};

use crate::{append, found, id, message, record, reference};

#[test]
fn missing_late_invalid_and_unrecorded_content_are_distinct() {
    let directory = tempfile::tempdir().unwrap();
    let engine = Engine::open(directory.path()).unwrap();
    let bytes = b"late needle\0\xff\r\n";
    let blob = reference(bytes).unwrap();
    append(&engine, &[message(1, Payload::Blob(blob))]).unwrap();
    let mut queries = engine.queries().unwrap();
    let query = ContentQuery {
        operation: id(1),
        field: ContentField::MessageContent,
    };
    let missing = found(queries.content(query).unwrap()).unwrap();
    assert_eq!(missing.value, ContentValue::Missing);
    assert_eq!(missing.reference.unwrap().len, Some(blob.len));
    let search = queries
        .search("needle", None, PageRequest::default())
        .unwrap();
    assert!(
        search.hits.is_empty(),
        "unavailable bytes produce no guessed hit"
    );
    assert_eq!(search.unavailable, vec![missing.clone()]);
    assert_eq!(engine.store_blob(bytes).unwrap(), blob);
    assert_eq!(found(queries.content(query).unwrap()).unwrap(), missing);
    assert_eq!(
        queries
            .refresh()
            .unwrap()
            .content_changed
            .into_iter()
            .collect::<Vec<_>>(),
        vec![id(1)]
    );
    let available = found(queries.content(query).unwrap()).unwrap();
    assert_eq!(available.evidence, missing.evidence);
    assert_eq!(available.value, ContentValue::Available(bytes.to_vec()));
    let search = queries
        .search("needle", None, PageRequest::default())
        .unwrap();
    assert_eq!(search.hits.len(), 1);
    assert!(
        search.unavailable.is_empty(),
        "late bytes become searchable after refresh"
    );
    assert_eq!(
        found(
            queries
                .content(ContentQuery {
                    field: ContentField::FileAfter,
                    ..query
                })
                .unwrap()
        )
        .unwrap()
        .value,
        ContentValue::NotRecorded
    );
    assert_eq!(
        found(
            queries
                .content(ContentQuery {
                    field: ContentField::MessageContentType,
                    ..query
                })
                .unwrap()
        )
        .unwrap()
        .value,
        ContentValue::Available(Vec::new())
    );

    let wrong_length = editchain_engine::BlobRef {
        len: blob.len.saturating_add(1),
        ..blob
    };
    append(
        &engine,
        &[
            message(2, Payload::Blob(wrong_length)),
            message(
                3,
                Payload::Blob(editchain_engine::BlobRef {
                    id: ContentId::Hash128([4; 16]),
                    len: 10,
                }),
            ),
        ],
    )
    .unwrap();
    drop(queries.refresh().unwrap());
    assert_eq!(
        found(
            queries
                .content(ContentQuery {
                    operation: id(2),
                    ..query
                })
                .unwrap()
        )
        .unwrap()
        .value,
        ContentValue::Corrupt
    );
    assert_eq!(
        found(
            queries
                .content(ContentQuery {
                    operation: id(3),
                    ..query
                })
                .unwrap()
        )
        .unwrap()
        .value,
        ContentValue::Unresolvable
    );
    let stored = directory
        .path()
        .join("blobs")
        .join(blake3::hash(bytes).to_hex().as_str());
    std::fs::write(&stored, b"external damage").unwrap();
    assert_eq!(
        found(queries.content(query).unwrap()).unwrap().value,
        ContentValue::Corrupt
    );
    std::fs::remove_file(stored).unwrap();
    assert_eq!(
        found(queries.content(query).unwrap()).unwrap().value,
        ContentValue::Missing
    );
    assert_eq!(
        queries
            .content(ContentQuery {
                operation: id(99),
                ..query
            })
            .unwrap(),
        Lookup::Missing
    );
}

#[test]
fn diffs_preserve_binary_bytes_partial_edits_and_absent_deletion_sides() {
    let directory = tempfile::tempdir().unwrap();
    let engine = Engine::open(directory.path()).unwrap();
    let before = engine.store_blob(b"same\0old\xfftail\r\n").unwrap();
    let after = engine.store_blob(b"same\0NEWER\xfftail\r\n").unwrap();
    let file = FileOp {
        path: PathId(42),
        stage: FileStage::Applied,
        base: Some(before.id),
        after: Some(after.id),
        edit: FileEdit::None,
    };
    let partial = FileOp {
        base: None,
        after: None,
        stage: FileStage::Deleted,
        edit: FileEdit::UnifiedDiff(Payload::Inline(b"@@ -1 +1 @@\n-old\n+new\n".to_vec())),
        ..file.clone()
    };
    append(
        &engine,
        &[
            record(1, OpKind::File(file)),
            record(2, OpKind::File(partial)),
        ],
    )
    .unwrap();
    let queries = engine.queries().unwrap();
    let diff = found(queries.diff(id(1)).unwrap()).unwrap();
    assert_eq!(
        diff.comparison,
        ByteComparison::Changed {
            before: ByteRange { start: 5, end: 8 },
            after: ByteRange { start: 5, end: 10 }
        }
    );
    assert_eq!(diff.before.value.bytes().unwrap(), b"same\0old\xfftail\r\n");
    assert_eq!(
        diff.after.value.bytes().unwrap(),
        b"same\0NEWER\xfftail\r\n"
    );
    let diff = found(queries.diff(id(2)).unwrap()).unwrap();
    assert_eq!(diff.comparison, ByteComparison::Unavailable);
    assert_eq!(diff.before.value, ContentValue::NotRecorded);
    assert_eq!(diff.after.value, ContentValue::NotRecorded);
    assert_eq!(
        diff.edit.value.bytes().unwrap(),
        b"@@ -1 +1 @@\n-old\n+new\n"
    );
}

#[test]
fn byte_comparisons_reconstruct_insert_delete_empty_and_disjoint_changes() {
    let directory = tempfile::tempdir().unwrap();
    let engine = Engine::open(directory.path()).unwrap();
    let samples: &[&[u8]] = &[
        b"",
        b"a",
        b"aa",
        b"aba",
        b"aca",
        b"\0\xff\r\n",
        "é".as_bytes(),
    ];
    for (offset, sample) in samples.iter().enumerate() {
        append(
            &engine,
            &[message(
                u64::try_from(offset).unwrap(),
                Payload::Inline(sample.to_vec()),
            )],
        )
        .unwrap();
    }
    let queries = engine.queries().unwrap();
    for (before_id, before) in samples.iter().enumerate() {
        for (after_id, after) in samples.iter().enumerate() {
            let query = |index| ContentQuery {
                operation: id(u64::try_from(index).unwrap()),
                field: ContentField::MessageContent,
            };
            let diff = queries.compare(query(before_id), query(after_id)).unwrap();
            if let ByteComparison::Changed {
                before: left,
                after: right,
            } = diff.comparison
            {
                let start = usize::try_from(left.start).unwrap();
                let end = usize::try_from(left.end).unwrap();
                let replacement = after
                    .get(usize::try_from(right.start).unwrap()..usize::try_from(right.end).unwrap())
                    .unwrap();
                let reconstructed: Vec<_> = before
                    .get(..start)
                    .unwrap()
                    .iter()
                    .chain(replacement)
                    .chain(before.get(end..).unwrap())
                    .copied()
                    .collect();
                assert_eq!(&reconstructed, after);
            } else {
                assert_eq!(diff.comparison, ByteComparison::Identical);
                assert_eq!(before, after);
            }
        }
    }
    let query = ContentQuery {
        operation: id(0),
        field: ContentField::MessageContent,
    };
    let missing = queries
        .compare(
            query,
            ContentQuery {
                operation: id(99),
                ..query
            },
        )
        .unwrap();
    assert_eq!(missing.comparison, ByteComparison::Unavailable);
    assert_eq!(missing.after, Lookup::Missing);
}
