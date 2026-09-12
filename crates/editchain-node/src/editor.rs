//! Durable admission of versioned VS Code observations.

mod context;
mod normalize;
mod projection;
pub(crate) use context::observe_context;

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

#[derive(Debug, Default)]
pub(crate) struct EditorStore {
    projections: BTreeMap<PathBuf, projection::Projection>,
}

impl EditorStore {
    pub(crate) fn record(
        &mut self,
        request: &RecordEditorEvents,
    ) -> Result<serde_json::Value, Box<dyn std::error::Error>> {
        let result = self.record_inner(request);
        if result.is_err() {
            let root = PathBuf::from(&request.workspace_path).join(&request.chain_dir);
            drop(self.projections.remove(&root));
        }
        result
    }

    fn record_inner(
        &mut self,
        request: &RecordEditorEvents,
    ) -> Result<serde_json::Value, Box<dyn std::error::Error>> {
        let root = PathBuf::from(&request.workspace_path).join(&request.chain_dir);
        // Serialize with live imports. Re-read the tail *after* taking the lock.
        let mut store = SegmentStore::open(&root)?;
        if !self.projections.contains_key(&root) {
            drop(
                self.projections
                    .insert(root.clone(), projection::Projection::open(&root)?),
            );
        }
        let projection = self
            .projections
            .get_mut(&root)
            .ok_or("editor projection unavailable")?;
        projection.refresh()?;
        let mut blobs = BlobStore::new(root.join("blobs"))?;
        let mut staged = editchain_core::OpSet::new();
        let mut page = Page::new(0);
        let mut accepted = 0_u64;
        let mut replayed = 0_u64;
        for event in &request.events {
            let raw =
                serde_json::to_vec(&serde_json::json!({"source":"vscode.editor", "event":event}))?;
            let op = event_op(event, &raw)?;
            let encoded = encode_op(&op)?;
            let admission = match projection.tail.chain().evidence().classify(op.id, &encoded) {
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
                    if let Some(parent) = op.parents.iter().next() {
                        if !projection.tail.chain().evidence().contains(parent)
                            && !staged.contains(parent)
                        {
                            return Err(io::Error::new(
                                io::ErrorKind::InvalidInput,
                                "editor stream has a sequence gap; replay the pending outbox first",
                            )
                            .into());
                        }
                    }
                    blobs.write(&raw)?;
                    // A live reader can stop at any complete record. Admit the
                    // source classification first so a raw buffer observation
                    // never temporarily becomes a primary activity row.
                    let marker = normalize::observation(event, op.id);
                    let marker_bytes = encode_op(&marker)?;
                    let marker_admission = match projection
                        .tail
                        .chain()
                        .evidence()
                        .classify(marker.id, &marker_bytes)
                    {
                        Admission::Accepted => staged.insert(marker.id, marker_bytes.clone()),
                        other @ (Admission::Duplicate | Admission::Conflict) => other,
                    };
                    match marker_admission {
                        Admission::Accepted => page.add_record(0, marker_bytes),
                        Admission::Duplicate => {}
                        Admission::Conflict => {
                            return Err(
                                "editor observation marker conflicts with retained evidence".into(),
                            )
                        }
                    }
                    page.add_record(0, encoded);
                    accepted = accepted.saturating_add(1);
                }
            }
        }
        if accepted > 0 {
            store.append_page(&page)?;
        }
        // Raw retries repair missing payloads before any derived replay reads them.
        projection.synchronize(&mut store, &mut blobs, request)?;
        // An acknowledgement is returned only after blob and segment fsync.
        Ok(
            serde_json::json!({"schema":1, "accepted":accepted, "replayed":replayed,
            "ack":request.events.iter().map(|event| (&event.session, event.sequence)).collect::<Vec<_>>() }),
        )
    }
}

fn event_op(event: &EditorEvent, raw: &[u8]) -> io::Result<Op> {
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
    let id = OpId {
        node: NodeId(u64::from_le_bytes(node)),
        boot: u32::from_le_bytes(boot),
        seq: event.sequence,
    };
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
        actor: ActorId(id.node.0),
        clock: Clock::UnixMs(event.time_ms),
        scope: ScopeRef::None,
        tags: Tags::IMPORT
            | Tags::HUMAN
            | if matches!(event.event, EditorEventKind::HumanEdit { .. }) {
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
