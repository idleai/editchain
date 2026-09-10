//! Canonical admission and durable append for a complete captured import batch.

use editchain_codec::frame::encode_op;
use editchain_codec::page::Page;
use editchain_core::{Admission, Op};
use editchain_import::batch::{DurableAdmission, DurableOpSink};
use editchain_import::ImportError;
use editchain_store::{CanonicalChain, SegmentStore};

/// Target page size; a larger legal record occupies its own page.
const PAGE_BYTES: usize = 4 * 1024 * 1024;

pub(super) struct ImportWriter<'a> {
    pub(super) store: &'a mut SegmentStore,
}

impl DurableOpSink for ImportWriter<'_> {
    fn append_durable(&mut self, operations: &[Op]) -> Result<DurableAdmission, ImportError> {
        append(self.store, operations).map_err(|error| ImportError::OpSink(error.to_string()))
    }
}

fn append(store: &mut SegmentStore, operations: &[Op]) -> std::io::Result<DurableAdmission> {
    let mut corpus = CanonicalChain::read(store.chain_dir())?;
    let mut admission = DurableAdmission::default();
    let mut page = Page::new(0);
    let mut page_bytes = 0usize;
    for op in operations {
        match corpus.insert(op.clone())? {
            Admission::Duplicate => {
                admission.duplicates = admission.duplicates.saturating_add(1);
                continue;
            }
            Admission::Conflict => admission.conflicts = admission.conflicts.saturating_add(1),
            Admission::Accepted => {}
        }
        let encoded = encode_op(op).map_err(std::io::Error::other)?;
        let record_bytes = encoded.len().saturating_add(5);
        if !page.records.is_empty() && page_bytes.saturating_add(record_bytes) > PAGE_BYTES {
            store.append_page(&page)?;
            page = Page::new(page.page_seq.checked_add(1).ok_or_else(|| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "import page sequence exhausted",
                )
            })?);
            page_bytes = 0;
        }
        page_bytes = page_bytes.saturating_add(record_bytes);
        page.add_record(0, encoded);
        admission.written = admission.written.saturating_add(1);
    }
    if !page.records.is_empty() {
        store.append_page(&page)?;
    }
    Ok(admission)
}

#[cfg(test)]
mod tests {
    use super::*;
    use editchain_core::{
        ActorId, Clock, MessageOp, NodeId, OpId, OpKind, ParentSet, Payload, ScopeRef, Tags,
    };

    fn message(sequence: u64, bytes: Vec<u8>) -> Op {
        Op {
            id: OpId::new(NodeId(1), 0, sequence),
            parents: ParentSet::None,
            actor: ActorId(0),
            clock: Clock::UnixMs(1),
            scope: ScopeRef::None,
            tags: Tags::MESSAGE,
            kind: OpKind::Message(MessageOp {
                content: Payload::Inline(bytes),
                content_type: Payload::Empty,
            }),
        }
    }

    #[test]
    fn durable_admission_retains_conflicts_and_skips_exact_replays() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = SegmentStore::open(dir.path()).unwrap();
        let one = message(1, b"one".to_vec());
        let two = message(1, b"two".to_vec());
        let stable = message(2, b"stable".to_vec());
        let first = append(&mut store, &[one.clone(), one.clone(), stable.clone()]).unwrap();
        assert_eq!(
            first,
            DurableAdmission {
                written: 2,
                duplicates: 1,
                conflicts: 0
            }
        );
        let conflict = append(&mut store, &[two.clone(), one.clone(), two.clone()]).unwrap();
        assert_eq!(
            conflict,
            DurableAdmission {
                written: 1,
                duplicates: 2,
                conflicts: 1
            }
        );
        let size = std::fs::metadata(dir.path().join("000000.eclog"))
            .unwrap()
            .len();
        let replay = append(&mut store, &[one, two, stable.clone()]).unwrap();
        assert_eq!(
            replay,
            DurableAdmission {
                written: 0,
                duplicates: 3,
                conflicts: 0
            }
        );
        assert_eq!(
            std::fs::metadata(dir.path().join("000000.eclog"))
                .unwrap()
                .len(),
            size
        );
        let corpus = CanonicalChain::read(dir.path()).unwrap();
        assert_eq!(corpus.stats().quarantined, 2);
        assert_eq!(
            corpus
                .into_located_ops()
                .map(|(op, _)| op)
                .collect::<Vec<_>>(),
            vec![stable]
        );
    }

    #[test]
    fn page_batches_preserve_every_operation() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = SegmentStore::open(dir.path()).unwrap();
        let operations: Vec<_> = (1..=3)
            .map(|seq| message(seq, vec![b'x'; PAGE_BYTES / 2]))
            .collect();
        let result = append(&mut store, &operations).unwrap();
        assert_eq!(result.written, 3);
        let pages = store.read_all().unwrap();
        assert_eq!(pages.len(), 3);
        assert_eq!(
            CanonicalChain::read(dir.path())
                .unwrap()
                .into_located_ops()
                .map(|(op, _)| op)
                .collect::<Vec<_>>(),
            operations
        );
    }

    #[test]
    fn source_changes_after_a_failed_append_cannot_reuse_reserved_operation_ids() {
        use editchain_import::batch::ImportBatch;
        use editchain_import::import::import_claude_code;
        use editchain_import::{
            CursorStore, DiscoveryRequest, FsBlobSink, FsCursorStore, ImportOptions,
        };
        struct LostAcknowledgment<'a>(&'a mut SegmentStore);
        impl DurableOpSink for LostAcknowledgment<'_> {
            fn append_durable(&mut self, ops: &[Op]) -> Result<DurableAdmission, ImportError> {
                let _written = append(self.0, ops)?;
                Err(ImportError::OpSink(
                    "acknowledgment lost after append".into(),
                ))
            }
        }
        let event = |id: &str, text: &str| {
            serde_json::json!({
                "type": "user", "uuid": id, "sessionId": "session-1",
                "timestamp": "2026-09-09T00:00:00Z",
                "message": { "role": "user", "content": text },
            })
            .to_string()
                + "\n"
        };
        for has_accepted_prefix in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let sessions = dir.path().join("sessions");
            std::fs::create_dir(&sessions).unwrap();
            let source = sessions.join("session-1.jsonl");
            let chain = dir.path().join("chain");
            let mut store = SegmentStore::open(&chain).unwrap();
            let mut cursors = FsCursorStore::new(chain.join("cursors")).unwrap();
            let mut blobs = FsBlobSink::new(chain.join("blobs")).unwrap();
            let request = DiscoveryRequest {
                workspace_path: dir.path().into(),
                sessions_dir: sessions.clone(),
                chain_dir: chain.clone(),
            };
            let key =
                editchain_import::canonical_source_key("claude-code", &sessions, &source).unwrap();
            let mut capture = |cursors: &FsCursorStore| {
                ImportBatch::capture(cursors, |ops, pending| {
                    import_claude_code(
                        &request,
                        &ImportOptions::default(),
                        ops,
                        &mut blobs,
                        pending,
                    )
                })
                .unwrap()
            };
            let prefix = if has_accepted_prefix {
                let prefix = event("first", "accepted");
                std::fs::write(&source, &prefix).unwrap();
                let _outcome = capture(&cursors)
                    .persist(&mut ImportWriter { store: &mut store }, &mut cursors)
                    .unwrap();
                prefix
            } else {
                String::new()
            };
            let accepted = cursors.get_cursor(&key).unwrap();
            std::fs::write(&source, prefix.clone() + &event("second", "before")).unwrap();
            let attempted = capture(&cursors);
            let attempted_ops = attempted.operations().to_vec();
            let attempted_ids: Vec<_> = attempted_ops.iter().map(|op| op.id).collect();
            assert!(attempted
                .persist(&mut LostAcknowledgment(&mut store), &mut cursors)
                .is_err());
            assert_eq!(cursors.get_cursor(&key).unwrap(), accepted);
            assert!(cursors.get_reservation(&key).unwrap().is_some());
            assert_eq!(capture(&cursors).operations(), attempted_ops);
            drop(cursors);
            std::fs::write(&source, prefix + &event("second", "after_")).unwrap();
            let mut reopened = FsCursorStore::new(chain.join("cursors")).unwrap();
            let changed = capture(&reopened);
            assert!(!changed.operations().is_empty());
            assert!(changed
                .operations()
                .iter()
                .all(|op| !attempted_ids.contains(&op.id)));
            let _outcome = changed
                .persist(&mut ImportWriter { store: &mut store }, &mut reopened)
                .unwrap();
            assert_eq!(
                reopened
                    .get_cursor(&key)
                    .unwrap()
                    .unwrap()
                    .accepted_generation,
                Some(1)
            );
            assert!(reopened.get_reservation(&key).unwrap().is_none());
            assert_eq!(CanonicalChain::read(&chain).unwrap().stats().quarantined, 0);
        }
    }
}
