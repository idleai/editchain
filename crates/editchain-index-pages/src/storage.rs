//! Append-only checksummed pages and atomically replaced roots.

use serde::{de::DeserializeOwned, Deserialize, Serialize};
use std::{
    cell::RefCell,
    fs::{File, OpenOptions},
    io::{self, BufWriter, Read as _, Seek as _, SeekFrom, Write as _},
    path::{Path, PathBuf},
    rc::Rc,
};

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub(crate) struct Address {
    offset: u64,
    length: u64,
    digest: [u8; 32],
}

thread_local! { static ACTIVE: RefCell<Option<Rc<Storage>>> = const { RefCell::new(None) }; }

pub(crate) fn active() -> io::Result<Rc<Storage>> {
    ACTIVE
        .with(|active| active.borrow().clone())
        .ok_or_else(|| io::Error::other("index serialization requires a storage context"))
}

struct Restore(Option<Rc<Storage>>);
impl Drop for Restore {
    fn drop(&mut self) {
        ACTIVE.with(|active| *active.borrow_mut() = self.0.take());
    }
}

pub(crate) fn with_storage<T>(storage: &Rc<Storage>, work: impl FnOnce() -> T) -> T {
    let _restore = Restore(ACTIVE.with(|active| active.replace(Some(Rc::clone(storage)))));
    work()
}

/// One exclusive writer for a derived checkpoint directory. Immutable page
/// addresses remain valid across commits and interrupted writes.
#[derive(Debug)]
pub struct Storage {
    root: PathBuf,
    writer: RefCell<BufWriter<File>>,
    reader: RefCell<File>,
    end: RefCell<u64>,
    // Declared last: buffered page writes finish before ownership is released.
    _lock: Lock,
}

impl Storage {
    /// Open/create an exclusively owned checkpoint.
    /// # Errors
    /// Rejects another writer or inaccessible files.
    pub fn open(root: &Path) -> io::Result<Rc<Self>> {
        std::fs::create_dir_all(root)?;
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(root.join("lock"))?;
        lock.try_lock().map_err(io::Error::other)?;
        let mut file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(root.join("pages"))?;
        let end = file.seek(SeekFrom::End(0))?;
        let reader = File::open(root.join("pages"))?;
        Ok(Rc::new(Self {
            root: root.into(),
            writer: RefCell::new(BufWriter::with_capacity(256 * 1024, file)),
            reader: RefCell::new(reader),
            end: RefCell::new(end),
            _lock: Lock(lock),
        }))
    }

    pub(crate) fn write<T: Serialize + ?Sized>(&self, value: &T) -> io::Result<Address> {
        let mut bytes = Vec::new();
        ciborium::into_writer(value, &mut bytes).map_err(io::Error::other)?;
        let length = u64::try_from(bytes.len()).map_err(io::Error::other)?;
        let mut end = self.end.borrow_mut();
        let address = Address {
            offset: *end,
            length,
            digest: *blake3::hash(&bytes).as_bytes(),
        };
        self.writer.borrow_mut().write_all(&bytes)?;
        *end = end
            .checked_add(length)
            .ok_or_else(|| io::Error::other("index address exhausted"))?;
        Ok(address)
    }

    pub(crate) fn read<T: DeserializeOwned>(&self, address: Address) -> io::Result<T> {
        let mut reader = self.reader.borrow_mut();
        if address.length > 256 * 1024 * 1024
            || address
                .offset
                .checked_add(address.length)
                .is_none_or(|end| end > *self.end.borrow())
        {
            return Err(io::Error::other("invalid index page extent"));
        }
        let mut bytes = vec![0; usize::try_from(address.length).map_err(io::Error::other)?];
        let _offset = reader.seek(SeekFrom::Start(address.offset))?;
        reader.read_exact(&mut bytes)?;
        drop(reader);
        if blake3::hash(&bytes).as_bytes() != &address.digest {
            return Err(io::Error::other("index page checksum mismatch"));
        }
        ciborium::from_reader(bytes.as_slice()).map_err(io::Error::other)
    }

    /// Load the small root; referenced pages remain cold.
    /// # Errors
    /// Returns a missing, corrupt or incompatible root error.
    pub fn load<T: DeserializeOwned>(self: &Rc<Self>) -> io::Result<T> {
        let bytes = std::fs::read(self.root.join("root"))?;
        let address: Address = ciborium::from_reader(bytes.as_slice()).map_err(io::Error::other)?;
        with_storage(self, || self.read(address))
    }

    /// Durably publish changed pages, then return an unloaded copy of the state.
    /// Callers must make dependent row data durable before publishing this root.
    /// # Errors
    /// Returns IO/encoding errors; the previously published root stays readable.
    pub fn commit<T: Serialize, U: DeserializeOwned>(self: &Rc<Self>, value: &T) -> io::Result<U> {
        let address = with_storage(self, || self.write(value))?;
        self.writer.borrow_mut().flush()?;
        self.writer.borrow().get_ref().sync_data()?;
        let mut file = File::create(self.root.join("root.next"))?;
        ciborium::into_writer(&address, &mut file).map_err(io::Error::other)?;
        file.sync_all()?;
        std::fs::rename(self.root.join("root.next"), self.root.join("root"))?;
        #[cfg(unix)]
        File::open(&self.root)?.sync_all()?;
        with_storage(self, || self.read(address))
    }

    /// Drop decoded caches without publishing a root. Any newly addressed pages
    /// become readable, but the last durable checkpoint remains authoritative.
    /// # Errors
    /// Returns encoding or page access errors.
    pub fn unload<T: Serialize, U: DeserializeOwned>(self: &Rc<Self>, value: &T) -> io::Result<U> {
        with_storage(self, || {
            let mut bytes = Vec::new();
            ciborium::into_writer(value, &mut bytes).map_err(io::Error::other)?;
            self.writer.borrow_mut().flush()?;
            ciborium::from_reader(bytes.as_slice()).map_err(io::Error::other)
        })
    }
}

#[derive(Debug)]
struct Lock(File);

impl Drop for Lock {
    fn drop(&mut self) {
        // A concurrent fork can inherit the open file description until exec.
        // Explicit unlock ends ownership even while that duplicate still lives.
        let _released = self.0.unlock();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn releasing_ownership_unlocks_even_with_an_inherited_file_description() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("lock");
        let file = File::create(&path).unwrap();
        file.try_lock().unwrap();
        let inherited = file.try_clone().unwrap();
        let owner = Lock(file);
        let contender = OpenOptions::new().write(true).open(&path).unwrap();
        assert!(contender.try_lock().is_err());
        drop(owner);
        contender.try_lock().unwrap();
        drop(inherited);
        let other = OpenOptions::new().write(true).open(&path).unwrap();
        assert!(other.try_lock().is_err(), "the new owner keeps its lock");
    }
}
