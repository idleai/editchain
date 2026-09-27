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
