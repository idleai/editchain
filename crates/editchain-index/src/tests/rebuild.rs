use editchain_core::{
    ActorId, FileEdit, FileOp, FileStage, Op, OpId, OpKind, ParentSet, PathId, Payload, ScopeRef,
    SessionId, SessionOp,
};
use editchain_store::{
    format::{decode_op, encode_op, Page},
    CanonicalChain, ChainReadStats, SegmentStore,
};

use crate::{ChainIndex, ContentStatus, IndexKey, RecordVariant};

use super::{append, message, put, source_bytes};

#[derive(Debug, PartialEq, Eq)]
struct Results {
    operations: Vec<Op>,
    postings: Vec<Vec<OpId>>,
    content: Vec<Option<Vec<ContentStatus>>>,
    record_variants: Vec<Vec<RecordVariant>>,
    stats: ChainReadStats,
}

fn results(index: &ChainIndex, keys: &[IndexKey]) -> Results {
    let ids = index.operations(None, usize::MAX).unwrap();
    Results {
        operations: ids
            .iter()
            .map(|id| index.get(*id).unwrap().unwrap())
            .collect(),
        postings: keys
            .iter()
            .map(|key| index.lookup(*key, None, usize::MAX).unwrap())
            .collect(),
        content: ids.iter().map(|id| index.content(*id).unwrap()).collect(),
        record_variants: ids
            .iter()
            .map(|id| index.record_variants(*id).unwrap())
            .collect(),
        stats: index.stats(),
    }
}

#[test]
fn rebuild_reopen_and_deleted_checkpoints_preserve_queries_and_exact_record_bytes() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    let blob = put(root, b"persisted file and session content\0\xff");
    let first = message(3, Payload::Inline(b"inline".to_vec()));
    let mut file = message(1, Payload::Empty);
    file.parents = ParentSet::One(first.id);
    file.scope = ScopeRef::File(PathId(42));
    file.kind = OpKind::File(FileOp {
        path: PathId(42),
        stage: FileStage::Saved,
        base: None,
        after: Some(blob.id),
        edit: FileEdit::Blob(blob),
    });
    let mut session = message(2, Payload::Empty);
    session.kind = OpKind::Session(SessionOp {
        id: SessionId(23),
        parent: None,
        label: Payload::Empty,
        metadata: Payload::Blob(blob),
    });
    append(
        root,
        &[first.clone(), file.clone(), session.clone(), first.clone()],
    );

    let conflicted = message(
        4,
        Payload::Inline(b"same decoded record, different encoding".to_vec()),
    );
    let encoded = encode_op(&conflicted).unwrap();
    let legacy = editchain_core::legacy::LegacyOp {
        id: conflicted.source.unwrap(),
        parents: ParentSet::None,
        actor: conflicted.actor,
        clock: conflicted.clock,
        scope: conflicted.scope,
        tags: conflicted.tags,
        kind: OpKind::Message(editchain_core::MessageOp {
            content: Payload::Inline(b"same decoded record, different encoding".to_vec()),
            content_type: Payload::Empty,
        }),
    };
    let legacy_bytes = postcard::to_stdvec(&legacy).unwrap();
    let mut alternate = vec![0x81, 0];
    alternate.extend_from_slice(legacy_bytes.get(1..).unwrap());
    assert_eq!(decode_op(&alternate).unwrap(), conflicted);
    let mut page = Page::new(1);
    page.add_record(7, encoded.clone());
    page.add_record(9, alternate.clone());
    page.add_record(9, alternate.clone());
    SegmentStore::open(root)
        .unwrap()
        .append_page(&page)
        .unwrap();

    let keys = [
        IndexKey::Actor(ActorId(17)),
        IndexKey::Session(SessionId(23)),
        IndexKey::File(PathId(42)),
        IndexKey::Parent(first.id),
        IndexKey::Content(blob.id),
    ];
    let mut index = ChainIndex::open(root).unwrap();
    let expected = results(&index, &keys);
    assert_eq!(
        expected.operations,
        vec![first.clone(), file.clone(), session.clone()]
    );
    assert_eq!(expected.postings.get(2), Some(&vec![file.id]));
    assert_eq!(
        index.operations(Some(file.id), 1).unwrap(),
        vec![session.id]
    );
    assert_eq!(
        index
            .lookup(keys.first().copied().unwrap(), Some(first.id), 1)
            .unwrap(),
        vec![file.id]
    );
    assert!(
        index.operations(None, 0).unwrap().is_empty(),
        "zero-sized page"
    );
    assert!(
        index
            .lookup(IndexKey::Actor(ActorId(99)), None, 10)
            .unwrap()
            .is_empty(),
        "unknown key"
    );
    assert_eq!(index.get(conflicted.id).unwrap(), None);
    let variants = index.record_variants(conflicted.id).unwrap();
    assert_eq!(
        variants
            .iter()
            .map(|entry| &entry.encoded)
            .collect::<Vec<_>>(),
        vec![&encoded, &alternate]
    );
    assert_eq!(index.stats(), CanonicalChain::read(root).unwrap().stats());
    let original = source_bytes(root);

    let _stats = index.rebuild().unwrap();
    assert_eq!(results(&index, &keys), expected);
    assert_eq!(index.record_variants(conflicted.id).unwrap(), variants);
    assert!(
        index.verify_integrity().unwrap().index_matches,
        "rebuilt index matches canonical replay"
    );
    drop(index);
    let index = ChainIndex::open(root).unwrap();
    assert_eq!(results(&index, &keys), expected);
    drop(index);
    std::fs::remove_dir_all(root.join("index-v3")).unwrap();
    let index = ChainIndex::open(root).unwrap();
    assert_eq!(results(&index, &keys), expected);
    assert_eq!(index.record_variants(conflicted.id).unwrap(), variants);
    assert_eq!(
        source_bytes(root),
        original,
        "rebuilding must never rewrite record bytes or blobs"
    );
}

#[test]
fn index_can_start_empty_then_resume_appends_and_rotation() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    let index = ChainIndex::open(root).unwrap();
    assert!(index.verify_integrity().unwrap().is_clean(), "empty index");
    assert!(ChainIndex::open(root).is_err(), "one checkpoint owner");
    drop(index);
    let first = message(5, Payload::Empty);
    append(root, std::slice::from_ref(&first));
    let mut index = ChainIndex::open(root).unwrap();
    assert_eq!(index.get(first.id).unwrap(), Some(first));
    let mut writer = SegmentStore::open(root).unwrap();
    writer.rotate().unwrap();
    let late = message(1, Payload::Empty);
    let mut page = Page::new(0);
    page.add_record(0, encode_op(&late).unwrap());
    writer.append_page(&page).unwrap();
    assert_eq!(
        index
            .refresh()
            .unwrap()
            .added
            .into_iter()
            .collect::<Vec<_>>(),
        vec![late.id]
    );
    assert_eq!(index.get(late.id).unwrap(), Some(late));
    assert!(
        index.verify_integrity().unwrap().is_clean(),
        "rotation matches full replay"
    );
}
