//! Durable row pages addressed by the checkpoint; only requested content is read.

#[cfg(test)]
mod tests;

use super::Result;
use editchain_protocol::{HistoryRow, LiveBlock, LiveBlockMeta};
use std::{
    cell::{Cell, RefCell},
    collections::BTreeMap,
    fs::File,
    io::{BufWriter, Read as _, Seek as _, SeekFrom, Write as _},
};

#[derive(Debug, Clone, Copy, serde::Serialize, serde::Deserialize)]
struct Page {
    offset: u64,
    length: u64,
    capacity: u64,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub(super) struct StoredBlock {
    pub(super) meta: LiveBlockMeta,
    pub(super) group: String,
    page: Page,
    digest: [u8; 32],
}

impl StoredBlock {
    pub(super) fn matches(&self, block: &LiveBlock) -> Result<bool> {
        Ok(self.meta == block.meta
            && &self.digest == blake3::hash(&serde_json::to_vec(&block.rows)?).as_bytes())
    }
}

#[derive(Debug)]
pub(super) struct RowStore {
    file: RefCell<BufWriter<File>>,
    free: BTreeMap<u64, Vec<u64>>,
    end: u64,
    position: Cell<Option<u64>>,
    persistent: bool,
}

impl RowStore {
    #[cfg(test)]
    pub(super) fn new() -> Result<Self> {
        Ok(Self {
            file: RefCell::new(BufWriter::with_capacity(256 * 1024, tempfile::tempfile()?)),
            free: BTreeMap::new(),
            end: 0,
            position: Cell::new(Some(0)),
            persistent: false,
        })
    }

    pub(super) fn open(path: &std::path::Path) -> Result<Self> {
        let file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(path)?;
        let end = file.metadata()?.len();
        Ok(Self {
            file: RefCell::new(BufWriter::with_capacity(256 * 1024, file)),
            free: BTreeMap::new(),
            end,
            position: Cell::new(None),
            persistent: true,
        })
    }

    pub(super) fn flush(&self) -> Result<()> {
        self.file.borrow_mut().flush()?;
        self.file.borrow().get_ref().sync_data()?;
        Ok(())
    }

    pub(super) fn put(&mut self, block: LiveBlock) -> Result<StoredBlock> {
        let bytes = serde_json::to_vec(&block.rows)?;
        let length = u64::try_from(bytes.len())?;
        let reusable = self.free.range(length..).next().map(|(size, _)| *size);
        let (offset, capacity) = if let Some(size) = reusable {
            let offsets = self.free.get_mut(&size).ok_or("missing free row page")?;
            let offset = offsets.pop().ok_or("empty free row page")?;
            if offsets.is_empty() {
                drop(self.free.remove(&size));
            }
            (offset, size)
        } else {
            let offset = self.end;
            self.end = self.end.checked_add(length).ok_or("row store exhausted")?;
            (offset, length)
        };
        let file = self.file.get_mut();
        // Seeking within BufWriter flushes it. Appends need no seek, so a cold
        // bootstrap writes large batches instead of one syscall per row.
        if self.position.replace(None) != Some(offset) {
            let _position = file.seek(SeekFrom::Start(offset))?;
        }
        file.write_all(&bytes)?;
        self.position.set(Some(
            offset.checked_add(length).ok_or("row position exhausted")?,
        ));
        Ok(StoredBlock {
            meta: block.meta,
            group: block
                .rows
                .first()
                .map(|row| row.group.clone())
                .unwrap_or_default(),
            page: Page {
                offset,
                length,
                capacity,
            },
            digest: *blake3::hash(&bytes).as_bytes(),
        })
    }

    pub(super) fn remove(&mut self, block: &StoredBlock) {
        // Old roots must remain recoverable until the next root is durable.
        if self.persistent {
            return;
        }
        self.free
            .entry(block.page.capacity)
            .or_default()
            .push(block.page.offset);
    }

    pub(super) fn rows(&self, block: &StoredBlock) -> Result<Vec<HistoryRow>> {
        let mut file = self.file.borrow_mut();
        self.position.set(None);
        let _position = file.seek(SeekFrom::Start(block.page.offset))?;
        let mut bytes = vec![0; usize::try_from(block.page.length)?];
        file.get_mut().read_exact(&mut bytes)?;
        if blake3::hash(&bytes).as_bytes() != &block.digest {
            return Err("row page checksum mismatch".into());
        }
        self.position.set(Some(
            block
                .page
                .offset
                .checked_add(block.page.length)
                .ok_or("row position exhausted")?,
        ));
        Ok(serde_json::from_slice(&bytes)?)
    }
}
