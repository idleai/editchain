use editchain_engine::{
    encode_op,
    queries::{ContentField, ContentQuery, IndexKey, Lookup, PageRequest},
    ActorId, Admission, Clock, Engine, OpKind, ParentSet, Payload,
};

use crate::{append, found, id, message};

#[test]
fn paging_refresh_and_conflicts_preserve_encoded_records() {
    let directory = tempfile::tempdir().unwrap();
    let engine = Engine::open(directory.path()).unwrap();
    let records = [
        message(40, Payload::Empty),
        message(10, Payload::Inline(b"unknown time".to_vec())),
        message(30, Payload::Empty),
    ];
    append(&engine, &records).unwrap();
    let mut queries = engine.queries().unwrap();
    let first = queries
        .history(
            None,
            PageRequest {
                after: None,
                limit: 2,
            },
        )
        .unwrap();
    assert_eq!(
        first
            .items
            .iter()
            .map(|entry| entry.operation.id)
            .collect::<Vec<_>>(),
        vec![id(10), id(30)]
    );
    assert_eq!(first.next_after, Some(id(30)));
    assert_eq!(first.items.first().unwrap().operation.clock, Clock::None);
    assert_eq!(
        first.items.first().unwrap().operation.observed_unix_ms(),
        None
    );
    let next = queries
        .history(
            None,
            PageRequest {
                after: first.next_after,
                limit: 2,
            },
        )
        .unwrap();
    assert_eq!(next.items.len(), 1);
    assert_eq!(next.items.first().unwrap().operation.id, id(40));
    assert_eq!(next.next_after, None);
    assert!(
        queries
            .history(Some(IndexKey::Actor(ActorId(99))), PageRequest::default())
            .unwrap()
            .items
            .is_empty(),
        "actor filtering uses recorded identities"
    );

    let encoded_record = queries.record_variants(id(10)).unwrap().pop().unwrap();
    assert_eq!(
        encoded_record.encoded,
        encode_op(records.get(1).unwrap()).unwrap()
    );
    assert_eq!(
        encoded_record.reference.record_hash,
        *blake3::hash(&encoded_record.encoded).as_bytes()
    );
    assert_eq!(
        found(queries.operation(id(10)).unwrap())
            .unwrap()
            .record_ref,
        encoded_record.reference
    );
    assert_eq!(
        engine.append(records.first().unwrap()).unwrap(),
        Admission::Duplicate
    );

    let late = message(5, Payload::Empty);
    assert_eq!(engine.append(&late).unwrap(), Admission::Accepted);
    let conflict = message(10, Payload::Inline(b"conflict".to_vec()));
    assert_eq!(engine.append(&conflict).unwrap(), Admission::Conflict);
    assert_eq!(queries.operation(id(5)).unwrap(), Lookup::Missing);
    assert!(
        matches!(queries.operation(id(10)).unwrap(), Lookup::Found(_)),
        "queries keep their refresh boundary"
    );
    let changes = queries.refresh().unwrap();
    assert_eq!(changes.added.into_iter().collect::<Vec<_>>(), vec![id(5)]);
    assert_eq!(
        changes.removed.into_iter().collect::<Vec<_>>(),
        vec![id(10)]
    );
    let conflict_lookup = queries.operation(id(10)).unwrap();
    assert!(
        matches!(conflict_lookup, Lookup::Conflicted(ref variants) if variants.len() == 2 && variants.contains(&encoded_record.reference)),
        "all conflicting record variants are retained"
    );
    assert!(
        matches!(
            queries
                .content(ContentQuery {
                    operation: id(10),
                    field: ContentField::MessageContent
                })
                .unwrap(),
            Lookup::Conflicted(_)
        ),
        "content cannot silently choose a variant"
    );
    assert!(
        matches!(queries.diff(id(10)).unwrap(), Lookup::Conflicted(_)),
        "conflict precedes revision interpretation"
    );
    assert!(
        matches!(queries.provenance(id(10)).unwrap(), Lookup::Conflicted(_)),
        "provenance cannot invent accepted authorship"
    );
    assert_eq!(
        queries
            .history(None, PageRequest::default())
            .unwrap()
            .items
            .iter()
            .map(|entry| entry.operation.id)
            .collect::<Vec<_>>(),
        vec![id(5), id(30), id(40)]
    );
    assert_eq!(queries.index().stats().quarantined, 2);
}

#[test]
fn literal_search_reports_field_ranges_and_pages_without_hits() {
    let directory = tempfile::tempdir().unwrap();
    let engine = Engine::open(directory.path()).unwrap();
    let mut binary = message(3, Payload::Inline(b"\xffneedle\0needle".to_vec()));
    binary.parents = ParentSet::One(id(2));
    append(
        &engine,
        &[
            message(1, Payload::Empty),
            message(
                2,
                Payload::Inline("é needle needle\r\n".as_bytes().to_vec()),
            ),
            binary,
        ],
    )
    .unwrap();
    let queries = engine.queries().unwrap();
    let empty = queries
        .search(
            "needle",
            None,
            PageRequest {
                after: None,
                limit: 1,
            },
        )
        .unwrap();
    assert!(empty.hits.is_empty(), "a scan page can have no hits");
    assert_eq!(empty.scanned, 1);
    assert_eq!(empty.next_after, Some(id(1)));
    let matches = queries
        .search(
            "needle",
            None,
            PageRequest {
                after: empty.next_after,
                limit: 2,
            },
        )
        .unwrap();
    assert_eq!(matches.hits.len(), 2);
    assert_eq!(matches.next_after, None);
    let first = matches.hits.first().unwrap().fields.first().unwrap();
    assert_eq!(first.field, ContentField::MessageContent);
    assert_eq!(
        first.range,
        editchain_engine::ByteRange { start: 3, end: 9 }
    );
    assert_eq!(
        matches
            .hits
            .last()
            .unwrap()
            .fields
            .first()
            .unwrap()
            .range
            .start,
        1
    );
    assert!(
        queries
            .search("Needle", None, PageRequest::default())
            .unwrap()
            .hits
            .is_empty(),
        "search does not case-fold recorded text"
    );
    assert!(
        queries.search("", None, PageRequest::default()).is_err(),
        "empty searches are rejected"
    );
    assert!(
        queries
            .search(&"x".repeat(16_385), None, PageRequest::default())
            .is_err(),
        "search input is bounded"
    );
    for limit in [0, 1001] {
        assert_eq!(
            queries
                .history(None, PageRequest { after: None, limit })
                .unwrap_err()
                .kind(),
            std::io::ErrorKind::InvalidInput
        );
    }
    assert_eq!(
        queries.diff(id(2)).unwrap_err().kind(),
        std::io::ErrorKind::InvalidInput
    );
    assert_eq!(queries.operation(id(99)).unwrap(), Lookup::Missing);
    assert!(
        matches!(
            found(queries.operation(id(2)).unwrap())
                .unwrap()
                .operation
                .kind,
            OpKind::Message(_)
        ),
        "the exact message remains accessible"
    );
}
