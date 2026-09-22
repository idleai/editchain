//! Retain exact encoded evidence across receipts without retaining decoded ops.

use std::cell::{Cell, Ref, RefCell};
use std::collections::BTreeMap;
use std::io;
use std::path::Path;
use std::sync::Arc;

use editchain_core::{Admission, Op};
use editchain_store::{OpRecordLocation, Tail, TailCorpus};

use crate::{invalid, RecordKey};

pub(crate) type Records = BTreeMap<RecordKey, Arc<[u8]>>;

#[derive(Debug)]
pub(crate) struct RetainedRecord {
    pub bytes: Arc<[u8]>,
    /// First occurrence, so appending a duplicate cannot bypass a cutoff.
    pub segment: u32,
}

#[derive(Debug, Default)]
pub(crate) struct Corpus {
    pub records: BTreeMap<RecordKey, RetainedRecord>,
    pub decoded: u64,
}

impl TailCorpus for Corpus {
    fn empty(_root: &Path) -> Self {
        Self::default()
    }

    fn admit_record(
        &mut self,
        op: Arc<Op>,
        encoded: &[u8],
        location: OpRecordLocation,
    ) -> io::Result<Admission> {
        self.decoded = self.decoded.saturating_add(1);
        let key = RecordKey {
            id: op.id,
            digest: *blake3::hash(encoded).as_bytes(),
        };
        if let Some(previous) = self.records.get(&key) {
            if previous.bytes.as_ref() != encoded {
                return Err(invalid("record digest collision"));
            }
            return Ok(Admission::Duplicate);
        }
        let first = RecordKey {
            id: op.id,
            digest: [0; 32],
        };
        let last = RecordKey {
            id: op.id,
            digest: [u8::MAX; 32],
        };
        let admission = if self.records.range(first..=last).next().is_some() {
            Admission::Conflict
        } else {
            Admission::Accepted
        };
        drop(self.records.insert(
            key,
            RetainedRecord {
                bytes: Arc::from(encoded),
                segment: location.segment_seq,
            },
        ));
        Ok(admission)
    }

    // Undecodable records are not exported. Framing and frontier continuity
    // remain enforced by Tail; there is no executable projection in this corpus.
    fn record_undecodable(&mut self) {}
    fn record_tail_change(&mut self, _incomplete: bool) {}
}

#[derive(Debug)]
pub(crate) struct Evidence {
    tail: RefCell<Tail<Corpus>>,
    loaded: Cell<bool>,
}

impl Evidence {
    pub(crate) fn new(root: &Path) -> Self {
        Self {
            tail: RefCell::new(Tail::empty(root)),
            loaded: Cell::new(false),
        }
    }

    /// The cold read can run before taking the writer lock. Call read again
    /// under that lock before relying on the bytes being durably published.
    pub(crate) fn warm(&self, root: &Path) -> io::Result<()> {
        if !self.loaded.get() {
            drop(self.read(root)?);
        }
        Ok(())
    }

    pub(crate) fn read(&self, root: &Path) -> io::Result<Ref<'_, Corpus>> {
        let mut tail = self.tail.borrow_mut();
        if self.loaded.get() {
            // Metadata validation preserves the full reader's rejection of
            // removed/replaced sealed segments without decoding them again.
            tail.resume(root)?;
            drop(tail.drain()?);
        } else {
            *tail = Tail::open(root)?;
            self.loaded.set(true);
        }
        drop(tail);
        Ok(Ref::map(self.tail.borrow(), Tail::chain))
    }

    pub(crate) fn decoded(&self) -> u64 {
        self.tail.borrow().chain().decoded
    }
}
