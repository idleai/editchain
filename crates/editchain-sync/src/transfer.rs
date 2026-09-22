//! One bounded object in flight in each direction.

use std::collections::BTreeSet;
use std::io;

use crate::{
    invalid, CheckProgress, DownloadProgress, Message, RecordKey, Replica, Snapshot, CHUNK_BYTES,
    INVENTORY_PAGE, MAX_OBJECT_BYTES,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct Object {
    pub record: RecordKey,
    pub blob: Option<[u8; 32]>,
}

impl Object {
    pub(crate) fn need(self, offset: usize) -> io::Result<Message> {
        Ok(Message::Need {
            record: self.record,
            blob: self.blob,
            offset: u32::try_from(offset).map_err(io::Error::other)?,
        })
    }

    pub(crate) fn ack(self) -> Message {
        Message::Ack {
            record: self.record,
            blob: self.blob,
        }
    }
}

#[derive(Debug)]
pub(crate) struct Download {
    pub object: Object,
    pub bytes: Vec<u8>,
    total: Option<usize>,
}

impl Download {
    pub(crate) fn progress(&self) -> io::Result<DownloadProgress> {
        Ok(DownloadProgress {
            content: self.object.blob.is_some(),
            received_bytes: u64::try_from(self.bytes.len()).map_err(io::Error::other)?,
            total_bytes: self
                .total
                .map(u64::try_from)
                .transpose()
                .map_err(io::Error::other)?,
        })
    }

    pub(crate) fn new(object: Object) -> Self {
        Self {
            object,
            bytes: Vec::new(),
            total: None,
        }
    }

    pub(crate) fn append(
        &mut self,
        object: Object,
        offset: u32,
        total: u32,
        bytes: &[u8],
    ) -> io::Result<bool> {
        let offset = usize::try_from(offset).map_err(io::Error::other)?;
        let total = usize::try_from(total).map_err(io::Error::other)?;
        let end = offset
            .checked_add(bytes.len())
            .ok_or_else(|| invalid("chunk length overflow"))?;
        if object != self.object
            || offset != self.bytes.len()
            || total > MAX_OBJECT_BYTES
            || self.total.is_some_and(|previous| previous != total)
            || end > total
            || bytes.len() > CHUNK_BYTES
            || (bytes.is_empty() && end != total)
        {
            return Err(invalid("unexpected object, offset or chunk size"));
        }
        self.total = Some(total);
        self.bytes.extend_from_slice(bytes);
        Ok(end == total)
    }
}

#[derive(Debug, Default)]
pub(crate) struct Source {
    pub progress: CheckProgress,
    snapshot: Option<Snapshot>,
    order: Vec<RecordKey>,
    next: usize,
    cursor: Option<RecordKey>,
    more: bool,
    active: Option<(Object, Vec<u8>, usize)>,
    unacked: BTreeSet<Object>,
    awaiting_check: bool,
}

impl Source {
    pub(crate) fn inventory(
        &mut self,
        replica: &Replica,
        after: Option<RecordKey>,
    ) -> io::Result<Message> {
        if self.active.is_some() || !self.unacked.is_empty() || self.awaiting_check {
            return Err(invalid(
                "inventory before pending transfers were acknowledged",
            ));
        }
        if after.is_none() {
            let snapshot = replica.snapshot()?;
            self.order = snapshot.ordered_keys()?;
            self.progress = CheckProgress {
                pass: self.progress.pass.saturating_add(1),
                total_records: Some(u64::try_from(self.order.len()).map_err(io::Error::other)?),
                ..CheckProgress::default()
            };
            self.snapshot = Some(snapshot);
            self.next = 0;
        } else if after != self.cursor || !self.more {
            return Err(invalid("invalid inventory continuation"));
        }
        let offset = u64::try_from(self.next).map_err(io::Error::other)?;
        let records: Vec<_> = self
            .order
            .iter()
            .skip(self.next)
            .take(INVENTORY_PAGE)
            .copied()
            .collect();
        self.next = self.next.saturating_add(records.len());
        let more = self.next < self.order.len();
        self.cursor = records.last().copied();
        self.more = more;
        self.awaiting_check = true;
        Ok(Message::Page {
            offset,
            total: u64::try_from(self.order.len()).map_err(io::Error::other)?,
            records,
            more,
        })
    }

    pub(crate) fn need(
        &mut self,
        replica: &Replica,
        object: Object,
        offset: u32,
    ) -> io::Result<Message> {
        if !self.awaiting_check {
            return Err(invalid("object request outside an active inventory page"));
        }
        let offset = usize::try_from(offset).map_err(io::Error::other)?;
        if offset == 0 {
            if self.active.is_some()
                || self.unacked.len() >= INVENTORY_PAGE
                || self.unacked.contains(&object)
            {
                return Err(invalid("too many in-flight objects"));
            }
            let snapshot = self
                .snapshot
                .as_ref()
                .ok_or_else(|| invalid("need before inventory"))?;
            let encoded = snapshot
                .record(object.record)
                .ok_or_else(|| invalid("record outside offered inventory"))?;
            let bytes = if let Some(hash) = object.blob {
                replica.read_blob(snapshot, object.record, hash)?
            } else {
                Some(encoded.to_vec())
            };
            let Some(bytes) = bytes else {
                self.progress.unavailable = self.progress.unavailable.saturating_add(1);
                return Ok(Message::Missing {
                    record: object.record,
                    blob: object.blob,
                });
            };
            self.active = Some((object, bytes, 0));
        }
        let (active, bytes, next) = self
            .active
            .as_mut()
            .ok_or_else(|| invalid("chunk without an active transfer"))?;
        if *active != object || *next != offset {
            return Err(invalid("out-of-order object request"));
        }
        let total = u32::try_from(bytes.len()).map_err(io::Error::other)?;
        let chunk: Vec<u8> = bytes
            .get(offset..)
            .ok_or_else(|| invalid("offset past object"))?
            .iter()
            .take(CHUNK_BYTES)
            .copied()
            .collect();
        *next = next.saturating_add(chunk.len());
        if *next == bytes.len() {
            self.active = None;
            let _: bool = self.unacked.insert(object);
        }
        Ok(Message::Chunk {
            record: object.record,
            blob: object.blob,
            offset: u32::try_from(offset).map_err(io::Error::other)?,
            total,
            bytes: chunk,
        })
    }

    pub(crate) fn ack(&mut self, object: Object) -> io::Result<()> {
        if !self.unacked.remove(&object) {
            return Err(invalid("acknowledgment for an unsent object"));
        }
        Ok(())
    }

    pub(crate) fn checked(&mut self, end: u64) -> io::Result<()> {
        if !self.awaiting_check
            || self.active.is_some()
            || !self.unacked.is_empty()
            || end != u64::try_from(self.next).map_err(io::Error::other)?
        {
            return Err(invalid("unexpected inventory check confirmation"));
        }
        self.awaiting_check = false;
        self.progress.checked_records = end;
        self.progress.complete = !self.more;
        Ok(())
    }
}
