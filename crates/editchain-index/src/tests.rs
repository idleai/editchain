mod incremental;
mod integrity;
mod rebuild;

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

use editchain_core::{
    ActorId, BlobRef, Clock, ContentId, MessageOp, NodeId, Op, OpId, OpKind, ParentSet, Payload,
    ScopeRef, SessionId, Tags,
};
use editchain_store::{
    format::{encode_op, Page},
    BlobStorage as _, BlobStore, SegmentStore,
};

fn message(sequence: u64, content: Payload) -> Op {
    Op {
        source: Some(editchain_core::SourceId::new(NodeId(1), 0, sequence)),
        id: OpId::new(NodeId(1), 0, sequence),
        parents: ParentSet::None,
        actor: ActorId(17),
        clock: Clock::UnixMs(sequence),
        scope: ScopeRef::Session(SessionId(23)),
        tags: Tags::MESSAGE,
        kind: OpKind::Message(MessageOp {
            content,
            content_type: Payload::Empty,
        }),
    }
}

fn reference(bytes: &[u8]) -> BlobRef {
    BlobRef {
        id: ContentId::Hash256(*blake3::hash(bytes).as_bytes()),
        len: u32::try_from(bytes.len()).unwrap(),
    }
}

fn append(root: &Path, ops: &[Op]) {
    let mut page = Page::new(0);
    for op in ops {
        page.add_record(0, encode_op(op).unwrap());
    }
    SegmentStore::open(root)
        .unwrap()
        .append_page(&page)
        .unwrap();
}

fn put(root: &Path, bytes: &[u8]) -> BlobRef {
    BlobStore::new(root.join("blobs"))
        .unwrap()
        .put(bytes)
        .unwrap()
}

fn source_bytes(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    let mut paths: Vec<_> = std::fs::read_dir(root)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| {
            path.extension()
                .is_some_and(|extension| extension == "eclog")
        })
        .collect();
    if root.join("blobs").exists() {
        paths.extend(
            std::fs::read_dir(root.join("blobs"))
                .unwrap()
                .map(|entry| entry.unwrap().path()),
        );
    }
    paths
        .into_iter()
        .map(|path| {
            let bytes = std::fs::read(&path).unwrap();
            (path, bytes)
        })
        .collect()
}

#[test]
fn prefixes_include_quarantined_identities_and_survive_index_rebuilds() {
    use crate::ChainIndex;
    use editchain_core::IdQuery;
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    let a = OpId::from_display_str(&format!("abcdef0123450{}", "0".repeat(51))).unwrap();
    let b = OpId::from_display_str(&format!("abcdef0123451{}", "0".repeat(51))).unwrap();
    let mut first = message(1, Payload::Empty);
    first.id = a;
    first.source = None;
    append(root, &[first.clone()]);
    let mut index = ChainIndex::open(root).unwrap();
    assert_eq!(index.short_id(a).unwrap(), "abcdef012345");
    let second = Op {
        id: b,
        ..first.clone()
    };
    let mut conflicting = second.clone();
    conflicting.actor = ActorId(999);
    append(root, &[second, conflicting]);
    let _delta = index.refresh().unwrap();
    let check = |index: &ChainIndex| {
        assert_eq!(
            index
                .id_candidates(&IdQuery::parse("ABCDEF012345").unwrap())
                .unwrap(),
            [a, b]
        );
        assert_eq!(index.short_id(a).unwrap(), "abcdef0123450");
        assert_eq!(index.short_id(b).unwrap(), "abcdef0123451");
        assert_eq!(index.get(b).unwrap(), None);
        assert_eq!(index.record_variants(b).unwrap().len(), 2);
        let missing = OpId::new(NodeId(99), 7, 999);
        assert_eq!(index.short_id(missing).unwrap(), missing.to_string());
        assert!(index
            .id_candidates(&IdQuery::parse("ffff").unwrap())
            .unwrap()
            .is_empty());
    };
    check(&index);
    let _stats = index.rebuild().unwrap();
    check(&index);
    drop(index);
    std::fs::remove_dir_all(root.join("index-v3")).unwrap();
    check(&ChainIndex::open(root).unwrap());
}
