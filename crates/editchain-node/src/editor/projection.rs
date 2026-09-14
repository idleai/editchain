//! Append missing derivations on bootstrap and process only new observations thereafter.

use editchain_core::{Admission, Op, OpId, OpKind, OpSet, ParentSet, Payload, Tags};
use editchain_index::Storage;
use editchain_protocol::editor::EditorEvent;
use editchain_store::{
    format::{encode_op, Page},
    BlobReader, BlobStore, IndexedTail, SegmentStore,
};
use std::borrow::Cow;
use std::collections::BTreeMap;
use std::path::Path;
use std::rc::Rc;

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub(super) struct Projection {
    #[serde(skip)]
    pub(super) bootstrapped: bool,
    #[serde(skip)]
    pub(super) records_decoded: u64,
    pub(super) tail: IndexedTail,
    normalizer: super::normalize::Normalizer,
    pending: Vec<Op>,
}

impl Projection {
    pub(super) fn source(
        &self,
        event: &EditorEvent,
        raw: &[u8],
        staged: &BTreeMap<OpId, Op>,
    ) -> Result<Op> {
        let mut op = super::event_op(event, raw)?;
        if let Some(identity) = &event.identity {
            op.parents = self
                .tail
                .chain()
                .get(op.id)
                .or_else(|| staged.get(&op.id))
                .map_or_else(
                    || {
                        self.normalizer
                            .frontier(identity)
                            .map_or(ParentSet::None, ParentSet::One)
                    },
                    |retained| retained.parents.clone(),
                );
        }
        Ok(op)
    }

    pub(super) fn admitted(&mut self, event: &EditorEvent, source: OpId) {
        self.normalizer.admitted(event, source);
    }

    pub(super) fn open(chain: &Path, storage: &Rc<Storage>) -> Result<Self> {
        match storage.load::<Self>() {
            Ok(saved) if saved.tail.resume(chain).is_ok() => return Ok(saved),
            // A moved or repaired source invalidates only this derived index.
            // Rebuild admission from the authoritative records in that case.
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        let tail = IndexedTail::open(chain)?;
        let pending = tail
            .chain()
            .shared_ops()
            .filter(|op| op.tags.matches_all(Tags::IMPORT | Tags::HUMAN))
            .map(|op| op.as_ref().clone())
            .collect();
        let projection = Self {
            bootstrapped: true,
            records_decoded: u64::try_from(tail.chain().stats().records)?,
            tail,
            normalizer: super::normalize::Normalizer::default(),
            pending,
        };
        // Preserve a completed cold scan even if an external writer is busy.
        projection.checkpoint(storage)?;
        Ok(projection)
    }

    pub(super) fn checkpoint(&self, storage: &Rc<Storage>) -> Result<()> {
        let saved: Self = storage.commit(self)?;
        drop(saved);
        Ok(())
    }

    pub(super) fn refresh(&mut self) -> Result<()> {
        let delta = self.tail.drain()?;
        self.records_decoded = self
            .records_decoded
            .saturating_add(delta.work.records_decoded);
        if !delta.removed.is_empty() {
            return Err("editor history contains newly conflicting source evidence".into());
        }
        self.pending
            .extend(delta.added.into_values().map(|(op, _)| op.as_ref().clone()));
        self.pending
            .retain(|op| op.tags.matches_all(Tags::IMPORT | Tags::HUMAN));
        Ok(())
    }

    pub(super) fn synchronize(
        &mut self,
        store: &mut SegmentStore,
        blobs: &mut BlobStore,
        request: &editchain_protocol::editor::RecordEditorEvents,
        fresh: &BTreeMap<OpId, &EditorEvent>,
    ) -> Result<()> {
        self.refresh()?;
        let chain = Path::new(&request.workspace_path).join(&request.chain_dir);
        let mut sources = std::mem::take(&mut self.pending);
        sources.retain(|op| op.tags.matches_all(Tags::IMPORT | Tags::HUMAN));
        let sources = super::order::sources(sources)?;
        let reader = BlobReader::open(&chain)?;
        let mut staged = OpSet::new();
        let mut page = Page::new(0);
        let mut count = 0_usize;
        for source in &sources {
            // Newly persisted observations already have validated, exact text
            // in this request. Historical replay still verifies stored blobs.
            let event = if let Some(event) = fresh.get(&source.id) {
                Cow::Borrowed(*event)
            } else {
                let Some(event) = event(source, &reader)? else {
                    continue;
                };
                Cow::Owned(event)
            };
            for op in self.normalizer.observe(&event, source.id, blobs)? {
                let encoded = encode_op(&op)?;
                let admission = match self.tail.chain().classify(op.id, &encoded)? {
                    Admission::Accepted => staged.insert(op.id, encoded.clone()),
                    other @ (Admission::Duplicate | Admission::Conflict) => other,
                };
                match admission {
                    Admission::Accepted => {
                        page.add_record(0, encoded);
                        count = count.saturating_add(1);
                    }
                    Admission::Duplicate => {}
                    Admission::Conflict => {
                        return Err("human work derivation conflicts with retained evidence".into())
                    }
                }
                if count >= 128 {
                    store.append_page(&page)?;
                    page = Page::new(0);
                    count = 0;
                }
            }
        }
        if count > 0 {
            store.append_page(&page)?;
        }
        self.refresh()
    }
}

fn event(op: &Op, reader: &BlobReader) -> Result<Option<EditorEvent>> {
    let OpKind::Import(import) = &op.kind else {
        return Ok(None);
    };
    let bytes = match &import.raw_ref {
        Payload::Inline(bytes) => bytes.clone(),
        Payload::Blob(blob) => reader
            .resolve_content(blob.id)
            .ok_or("editor source blob unavailable; replay its durable outbox")?,
        Payload::Empty => return Ok(None),
    };
    let Ok(mut raw) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
        return Ok(None);
    };
    if raw.get("source").and_then(serde_json::Value::as_str) != Some("vscode.editor") {
        return Ok(None);
    }
    Ok(Some(serde_json::from_value(
        raw.get_mut("event")
            .ok_or("editor source event missing")?
            .take(),
    )?))
}
