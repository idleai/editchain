//! Append missing derivations on bootstrap and process only new observations thereafter.

use editchain_core::{Admission, Op, OpKind, OpSet, Payload, Tags};
use editchain_protocol::editor::EditorEvent;
use editchain_store::{
    format::{encode_op, Page},
    BlobReader, BlobStore, CanonicalTail, SegmentStore,
};
use std::path::Path;

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

#[derive(Debug)]
pub(super) struct Projection {
    pub(super) tail: CanonicalTail,
    normalizer: super::normalize::Normalizer,
    pending: Vec<Op>,
}

impl Projection {
    pub(super) fn open(chain: &Path) -> Result<Self> {
        let tail = CanonicalTail::open(chain)?;
        let pending = tail
            .chain()
            .located_ops()
            .map(|(op, _)| op.clone())
            .collect();
        Ok(Self {
            tail,
            normalizer: super::normalize::Normalizer::default(),
            pending,
        })
    }

    pub(super) fn refresh(&mut self) -> Result<()> {
        let delta = self.tail.drain()?;
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
    ) -> Result<()> {
        self.refresh()?;
        let chain = Path::new(&request.workspace_path).join(&request.chain_dir);
        let mut sources = std::mem::take(&mut self.pending);
        sources.sort_by_key(|op| op.id);
        let reader = BlobReader::open(&chain)?;
        let mut staged = OpSet::new();
        let mut page = Page::new(0);
        let mut count = 0_usize;
        for source in &sources {
            let Some(event) = event(source, &reader)? else {
                continue;
            };
            for op in self.normalizer.observe(&event, source.id, blobs)? {
                let encoded = encode_op(&op)?;
                let admission = match self.tail.chain().evidence().classify(op.id, &encoded) {
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
