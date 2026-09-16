//! Durable admission of versioned VS Code observations.

#[cfg(test)]
mod conflict_tests;
mod context;
mod encoding;
#[cfg(test)]
mod encoding_tests;
mod identity;
mod normalize;
mod order;
mod projection;
mod remote;
pub(crate) use context::observe_context;
pub(crate) use encoding::Encoding;

use std::collections::BTreeMap;
use std::io;
use std::path::PathBuf;

use editchain_core::{
    ActorId, Admission, BlobRef, Clock, ContentId, NodeId, Op, OpId, OpKind, ParentSet, Payload,
    ScopeRef, Tags,
};
use editchain_protocol::editor::{EditorEvent, EditorEventKind, RecordEditorEvents};
use editchain_store::{
    format::{encode_op, Page},
    BlobStore, SegmentStore,
};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

pub(crate) fn record(
    request: &RecordEditorEvents,
    encoding: &mut Encoding,
) -> Result<serde_json::Value> {
    editchain_index::boundary(|| record_inner(request, encoding))?
}

fn record_inner(
    request: &RecordEditorEvents,
    encoding: &mut Encoding,
) -> Result<serde_json::Value> {
    let started = std::time::Instant::now();
    let root = PathBuf::from(&request.workspace_path).join(&request.chain_dir);
    // Capture processes share a durable, incrementally paged index. Release
    // its ownership after every batch so another editor window can record.
    let checkpoint = editchain_index::Storage::open(&root.join("editor-v1"))?;
    let mut projection = projection::Projection::open(&root, &checkpoint)?;
    // Serialize with live imports. Re-read the tail *after* taking the lock.
    // Cold indexing happens before this lock; contention never destroys it.
    let mut store = SegmentStore::open_wait(&root, std::time::Duration::from_secs(2))?;
    projection.refresh()?;
    let mut blobs = BlobStore::new(root.join("blobs"))?;
    identity::repair(&request.events, projection.tail.chain(), &root, &mut blobs)?;
    projection.synchronize(&mut store, &mut blobs, request, &BTreeMap::new())?;
    let prepared_ms = started.elapsed().as_millis();
    let mut staged = editchain_core::OpSet::new();
    let mut sources = BTreeMap::new();
    let mut fresh = BTreeMap::new();
    let mut page = Page::new(0);
    let mut accepted = 0_u64;
    let mut replayed = 0_u64;
    for event in &request.events {
        let raw = encoding.encode(event)?;
        let op = projection.source(event, &raw, &sources)?;
        let encoded = encode_op(&op)?;
        let admission = match projection.tail.chain().classify(op.id, &encoded)? {
            Admission::Accepted => staged.insert(op.id, encoded.clone()),
            other @ (Admission::Duplicate | Admission::Conflict) => other,
        };
        match admission {
            Admission::Duplicate => {
                // A retry can repair a missing payload. A corrupt existing
                // blob must fail visibly rather than receive a durable ack.
                blobs.write(&raw)?;
                replayed = replayed.saturating_add(1);
            }
            Admission::Conflict => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "editor identity reused with different content",
                )
                .into())
            }
            Admission::Accepted => {
                identity::validate_sequence(event, &op, projection.tail.chain(), &root, &sources)?;
                projection.admitted(event, op.id);
                drop(sources.insert(op.id, op.clone()));
                blobs.write(&raw)?;
                let _previous = fresh.insert(op.id, event);
                // A live reader can stop at any complete record. Admit the
                // source classification first so a raw buffer observation
                // never temporarily becomes a primary activity row.
                let marker = normalize::observation(event, op.id);
                let marker_bytes = encode_op(&marker)?;
                let marker_admission =
                    match projection.tail.chain().classify(marker.id, &marker_bytes)? {
                        Admission::Accepted => staged.insert(marker.id, marker_bytes.clone()),
                        other @ (Admission::Duplicate | Admission::Conflict) => other,
                    };
                match marker_admission {
                    Admission::Accepted | Admission::Conflict => page.add_record(0, marker_bytes),
                    Admission::Duplicate => {}
                }
                page.add_record(0, encoded);
                accepted = accepted.saturating_add(1);
            }
        }
    }
    if accepted > 0 {
        store.append_page(&page)?;
    }
    let stored_ms = started.elapsed().as_millis();
    // Raw retries repair missing payloads before any derived replay reads them.
    projection.synchronize(&mut store, &mut blobs, request, &fresh)?;
    drop(store);
    projection.checkpoint(&checkpoint)?;
    // An acknowledgement is returned only after blob and segment fsync.
    Ok(
        serde_json::json!({"schema":1, "accepted":accepted, "replayed":replayed,
            "work":{"bootstrap":projection.bootstrapped,"records_decoded":projection.records_decoded,
                "prepare_ms":prepared_ms,"store_ms":stored_ms.saturating_sub(prepared_ms),
                "project_ms":started.elapsed().as_millis().saturating_sub(stored_ms)},
            "ack":request.events.iter().map(|event| (&event.session, event.sequence)).collect::<Vec<_>>() }),
    )
}
fn event_id(event: &EditorEvent) -> io::Result<OpId> {
    let digest = blake3::derive_key(
        "editchain.vscode.editor.session.v1",
        event.session.as_bytes(),
    );
    let mut node = [0_u8; 8];
    node.copy_from_slice(
        digest
            .get(..8)
            .ok_or_else(|| io::Error::other("invalid hash"))?,
    );
    let mut boot = [0_u8; 4];
    boot.copy_from_slice(
        digest
            .get(8..12)
            .ok_or_else(|| io::Error::other("invalid hash"))?,
    );
    Ok(OpId {
        node: NodeId(u64::from_le_bytes(node)),
        boot: u32::from_le_bytes(boot),
        seq: event.sequence,
    })
}

fn event_op(event: &EditorEvent, raw: &[u8]) -> io::Result<Op> {
    let id = event_id(event)?;
    let hash = *blake3::hash(raw).as_bytes();
    Ok(Op {
        id,
        parents: if event.sequence == 1 {
            ParentSet::None
        } else {
            ParentSet::One(OpId {
                seq: event.sequence.saturating_sub(1),
                ..id
            })
        },
        actor: event
            .identity
            .as_ref()
            .map_or(ActorId(id.node.0), identity::actor),
        clock: Clock::UnixMs(event.time_ms),
        scope: event
            .identity
            .as_ref()
            .map_or(ScopeRef::None, identity::scope),
        tags: Tags::IMPORT
            | Tags::HUMAN
            | if matches!(
                event.event,
                EditorEventKind::HumanEdit { .. } | EditorEventKind::HumanEditBatch { .. }
            ) {
                Tags::INFERRED
            } else {
                Tags::NONE
            },
        kind: OpKind::Import(editchain_core::op::ImportOp {
            raw_ref: Payload::Blob(BlobRef {
                id: ContentId::Hash256(hash),
                len: u32::try_from(raw.len()).map_err(io::Error::other)?,
            }),
            raw_hash: Some(hash),
        }),
    })
}
