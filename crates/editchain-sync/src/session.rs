//! Pull-based reconciliation; only durable store contents establish progress.

use std::collections::{BTreeSet, VecDeque};
use std::io;

use serde::Serialize;

use crate::transfer::{Download, Object, Source};
use crate::{
    invalid, Message, RecordKey, Replica, Snapshot, INVENTORY_PAGE, MAX_OBJECT_BYTES, PEER_VERSION,
};

const BATCH_BYTES: usize = 4 * 1024 * 1024;

/// Credential-free durable progress in both directions of this connection.
#[derive(Debug, Default, Clone, Serialize)]
pub struct Progress {
    /// Space/version negotiation has succeeded over an authenticated channel.
    pub accepted: bool,
    /// A reconciliation round is in progress.
    pub synchronizing: bool,
    /// Completed inventory rounds (not a claim about future edits).
    pub rounds: u64,
    /// Exact records durably received on this connection.
    pub records: u64,
    /// Content blobs durably received on this connection.
    pub blobs: u64,
    /// Sent records acknowledged as durable by the authenticated remote peer.
    pub sent_records: u64,
    /// Sent blobs acknowledged as durable by the authenticated remote peer.
    pub sent_blobs: u64,
    /// Missing content responses in the current or most recently completed round.
    pub unavailable: u64,
}

/// Replication state for one already authenticated, authorized peer.
/// A failed call invalidates the session; reconnect reconstructs from disk.
#[derive(Debug)]
pub struct Session {
    replica: Replica,
    source: Source,
    local: Snapshot,
    pull: Pull,
    progress: Progress,
}

#[derive(Debug, Default)]
struct Pull {
    waiting_page: bool,
    position: u64,
    after: Option<RecordKey>,
    more: bool,
    keys: Vec<RecordKey>,
    records: VecDeque<RecordKey>,
    blobs: VecDeque<Object>,
    tried: BTreeSet<Object>,
    staged: Vec<(RecordKey, Vec<u8>)>,
    staged_bytes: usize,
    download: Option<Download>,
}

impl Session {
    /// Construct without reading or exposing inventory before negotiation.
    #[must_use]
    pub fn new(replica: Replica) -> Self {
        Self {
            replica,
            source: Source::default(),
            local: Snapshot::default(),
            pull: Pull::default(),
            progress: Progress::default(),
        }
    }

    /// First message, sent only after the secure channel authenticates the peer.
    #[must_use]
    pub fn hello(&self) -> Message {
        Message::Hello {
            version: PEER_VERSION,
            encoding: 1,
            space: self.replica.space().to_owned(),
        }
    }

    /// Current durable progress; this never includes staged or partial bytes.
    #[must_use]
    pub fn progress(&self) -> &Progress {
        &self.progress
    }

    /// Start another inventory round to repair interrupted or newly appended work.
    /// No-op while an existing round is in flight.
    ///
    /// # Errors
    /// Returns local storage contention or failure; callers reconnect and retry.
    pub fn tick(&mut self) -> io::Result<Vec<Message>> {
        if !self.progress.accepted || self.progress.synchronizing {
            return Ok(Vec::new());
        }
        self.local = self.replica.snapshot()?;
        self.pull = Pull {
            waiting_page: true,
            ..Pull::default()
        };
        self.progress.synchronizing = true;
        self.progress.unavailable = 0;
        Ok(vec![Message::Inventory { after: None }])
    }

    /// Validate and process one authenticated peer message.
    ///
    /// # Errors
    /// Rejects mismatched negotiation, unsolicited or oversized messages, invalid
    /// object bytes, and any failed durable write. No failed write is acknowledged.
    pub fn receive(&mut self, message: Message) -> io::Result<Vec<Message>> {
        if !self.progress.accepted {
            if matches!(&message, Message::Hello { version, encoding, .. } if *version != PEER_VERSION || *encoding != 1)
            {
                return Err(io::Error::new(
                    io::ErrorKind::Unsupported,
                    "incompatible peer protocol",
                ));
            }
            if message != self.hello() {
                return Err(invalid("peer version, encoding or space mismatch"));
            }
            self.progress.accepted = true;
            return self.tick();
        }
        let mut replies = Vec::new();
        match message {
            Message::Hello { .. } => return Err(invalid("duplicate peer hello")),
            Message::Inventory { after } => {
                replies.push(self.source.inventory(&self.replica, after)?);
            }
            Message::Need {
                record,
                blob,
                offset,
            } => replies.push(
                self.source
                    .need(&self.replica, Object { record, blob }, offset)?,
            ),
            Message::Ack { record, blob } => self.acknowledge(Object { record, blob })?,
            Message::Page {
                offset,
                records,
                more,
            } => {
                self.page(offset, records, more)?;
                self.advance(&mut replies)?;
            }
            Message::Chunk {
                record,
                blob,
                offset,
                total,
                bytes,
            } => {
                let download = self
                    .pull
                    .download
                    .as_mut()
                    .ok_or_else(|| invalid("unsolicited object chunk"))?;
                if download.append(Object { record, blob }, offset, total, &bytes)? {
                    self.complete(&mut replies)?;
                    self.advance(&mut replies)?;
                } else {
                    replies.push(download.object.need(download.bytes.len())?);
                }
            }
            Message::Missing { record, blob } => {
                let download = self
                    .pull
                    .download
                    .take()
                    .ok_or_else(|| invalid("unsolicited missing response"))?;
                if download.object != (Object { record, blob })
                    || blob.is_none()
                    || !download.bytes.is_empty()
                {
                    return Err(invalid(
                        "missing response for a different or partial object",
                    ));
                }
                self.progress.unavailable = self.progress.unavailable.saturating_add(1);
                self.advance(&mut replies)?;
            }
        }
        Ok(replies)
    }

    fn page(&mut self, offset: u64, records: Vec<RecordKey>, more: bool) -> io::Result<()> {
        if !self.pull.waiting_page || records.len() > INVENTORY_PAGE || (more && records.is_empty())
        {
            return Err(invalid("unsolicited or oversized inventory page"));
        }
        if offset != self.pull.position
            || records.iter().collect::<BTreeSet<_>>().len() != records.len()
        {
            return Err(invalid("invalid inventory position or duplicate keys"));
        }
        self.pull.position = offset
            .checked_add(u64::try_from(records.len()).map_err(io::Error::other)?)
            .ok_or_else(|| invalid("inventory position overflow"))?;
        self.pull.after = records.last().copied();
        self.pull.waiting_page = false;
        self.pull.more = more;
        self.pull.records = records
            .iter()
            .filter(|key| !self.local.contains(**key))
            .copied()
            .collect();
        self.pull.keys = records;
        self.pull.tried.clear();
        Ok(())
    }

    fn acknowledge(&mut self, object: Object) -> io::Result<()> {
        self.source.ack(object)?;
        if object.blob.is_some() {
            self.progress.sent_blobs = self.progress.sent_blobs.saturating_add(1);
        } else {
            self.progress.sent_records = self.progress.sent_records.saturating_add(1);
        }
        Ok(())
    }

    fn complete(&mut self, replies: &mut Vec<Message>) -> io::Result<()> {
        let download = self
            .pull
            .download
            .take()
            .ok_or_else(|| invalid("missing completed transfer"))?;
        if let Some(hash) = download.object.blob {
            self.replica
                .ingest_blob(download.object.record, hash, &download.bytes)?;
            self.local.received_blob(hash);
            self.progress.blobs = self.progress.blobs.saturating_add(1);
            replies.push(download.object.ack());
        } else {
            if RecordKey::from_encoded(&download.bytes)? != download.object.record {
                return Err(invalid("received record digest or identity mismatch"));
            }
            if self.pull.staged_bytes.saturating_add(download.bytes.len()) > MAX_OBJECT_BYTES {
                self.flush(replies)?;
            }
            self.pull.staged_bytes = self.pull.staged_bytes.saturating_add(download.bytes.len());
            self.pull
                .staged
                .push((download.object.record, download.bytes));
            if self.pull.staged_bytes >= BATCH_BYTES {
                self.flush(replies)?;
            }
        }
        Ok(())
    }

    fn flush(&mut self, replies: &mut Vec<Message>) -> io::Result<()> {
        if self.pull.staged.is_empty() {
            return Ok(());
        }
        let acks = self
            .replica
            .ingest_into(&self.pull.staged, Some(&mut self.local))?;
        for record in acks {
            replies.push(Object { record, blob: None }.ack());
            self.progress.records = self.progress.records.saturating_add(1);
        }
        self.pull.staged.clear();
        self.pull.staged_bytes = 0;
        Ok(())
    }

    fn advance(&mut self, replies: &mut Vec<Message>) -> io::Result<()> {
        if let Some(record) = self.pull.records.pop_front() {
            return self.download(Object { record, blob: None }, replies);
        }
        self.flush(replies)?;
        if self.pull.blobs.is_empty() {
            self.find_blobs()?;
        }
        if let Some(object) = self.pull.blobs.pop_front() {
            return self.download(object, replies);
        }
        if self.pull.more {
            self.pull.waiting_page = true;
            replies.push(Message::Inventory {
                after: self.pull.after,
            });
        } else {
            self.progress.synchronizing = false;
            self.progress.rounds = self.progress.rounds.saturating_add(1);
        }
        Ok(())
    }

    fn find_blobs(&mut self) -> io::Result<()> {
        for key in &self.pull.keys {
            let hashes = self.replica.blob_hashes(&self.local, *key)?;
            if hashes.len() > 4096 {
                return Err(invalid("record content-reference limit exceeded"));
            }
            for hash in hashes {
                let object = Object {
                    record: *key,
                    blob: Some(hash),
                };
                if self.pull.tried.insert(object)
                    && self.replica.read_blob(&self.local, *key, hash)?.is_none()
                {
                    self.pull.blobs.push_back(object);
                }
            }
        }
        Ok(())
    }

    fn download(&mut self, object: Object, replies: &mut Vec<Message>) -> io::Result<()> {
        replies.push(object.need(0)?);
        self.pull.download = Some(Download::new(object));
        Ok(())
    }
}
