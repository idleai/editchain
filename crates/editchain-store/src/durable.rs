//! Atomic publication and directory synchronization for durable storage.

use std::fs;
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

/// Atomically replace metadata via an exclusive, same-directory temporary file.
///
/// File contents and the publication's directory ancestry are synchronized
/// before success. Concurrent writers use different temporary files. A crash
/// can leave an unreferenced temporary file; it is never treated as evidence.
/// Use content-addressed publication for immutable blobs, not this replacement
/// primitive.
///
/// # Errors
/// Returns file creation, write, sync, rename or directory sync errors.
pub fn atomic_write(path: &Path, data: &[u8]) -> io::Result<()> {
    let temporary = TemporaryFile::write(path, data)?;
    fs::rename(&temporary.path, path)?;
    sync_directory_tree(parent(path)?)
}

/// Publish immutable bytes only if the destination does not already exist.
/// The hard link atomically installs a complete, synced inode without replacing
/// another writer's evidence. `false` requires the caller to verify and sync the
/// winner before acknowledging a duplicate.
pub(crate) fn publish_new(path: &Path, data: &[u8]) -> io::Result<bool> {
    publish_new_with(path, data, sync_directory_tree)
}

/// Stage a bounded cohort before synchronizing and publishing its immutable files.
/// Existing destinations and publication races are checked by the caller.
pub(crate) fn publish_batch(
    entries: &[(PathBuf, &[u8])],
    verify: impl Fn(&Path, &[u8]) -> io::Result<bool>,
) -> io::Result<()> {
    publish_batch_with(entries, verify, sync_directory_tree)
}

fn publish_batch_with(
    entries: &[(PathBuf, &[u8])],
    verify: impl Fn(&Path, &[u8]) -> io::Result<bool>,
    sync: impl Fn(&Path) -> io::Result<()>,
) -> io::Result<()> {
    let mut pending = Vec::new();
    let mut unique = std::collections::BTreeMap::new();
    for (path, bytes) in entries {
        if let Some(previous) = unique.insert(path, *bytes) {
            if previous != *bytes {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "batch contains different bytes for one immutable destination",
                ));
            }
            continue;
        }
        if !verify(path, bytes)? {
            let (temporary, file) = TemporaryFile::prepare(path, bytes)?;
            pending.push((temporary, file, path, bytes));
        }
    }
    // Creating the cohort first lets file synchronization share the same
    // filesystem journal transaction. Nothing has been acknowledged yet.
    // Bound concurrent data fences so the filesystem can coalesce allocation
    // and journal work. All workers finish before any final name is published.
    std::thread::scope(|scope| -> io::Result<()> {
        let mut workers = Vec::new();
        for group in pending.chunks(pending.len().div_ceil(8).max(1)) {
            workers.push(std::thread::Builder::new().spawn_scoped(
                scope,
                move || -> io::Result<()> {
                    for (_, file, _, _) in group {
                        file.sync_all()?;
                    }
                    Ok(())
                },
            )?);
        }
        for worker in workers {
            worker.join().map_err(|_panic| {
                io::Error::other("blob data synchronization worker panicked")
            })??;
        }
        Ok(())
    })?;
    let mut directories = std::collections::BTreeSet::new();
    for (temporary, _, path, bytes) in &pending {
        match fs::hard_link(&temporary.path, path) {
            Ok(()) => {
                let _inserted = directories.insert(parent(path)?);
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                if !verify(path, bytes)? {
                    return Err(io::Error::new(
                        io::ErrorKind::NotFound,
                        "concurrent blob publication disappeared",
                    ));
                }
            }
            Err(error) => return Err(error),
        }
    }
    for directory in directories {
        sync(directory)?;
    }
    Ok(())
}

fn publish_new_with(
    path: &Path,
    data: &[u8],
    sync: impl FnOnce(&Path) -> io::Result<()>,
) -> io::Result<bool> {
    let temporary = TemporaryFile::write(path, data)?;
    match fs::hard_link(&temporary.path, path) {
        Ok(()) => {
            sync(parent(path)?)?;
            Ok(true)
        }
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => Ok(false),
        Err(error) => Err(error),
    }
}

/// Create a directory hierarchy and persist its directory entries.
///
/// Existing ancestors are synced too: a retry may follow a failed sync after
/// creation. Their existence alone does not establish durable publication.
///
/// # Errors
/// Returns creation, canonicalization or directory synchronization errors.
pub fn create_dir_all(path: &Path) -> io::Result<()> {
    fs::create_dir_all(path)?;
    sync_directory_tree(path)
}

fn sync_directory_tree(path: &Path) -> io::Result<()> {
    let absolute = fs::canonicalize(path)?;
    for directory in absolute.ancestors() {
        sync_dir(directory)?;
    }
    Ok(())
}

/// Fsync a file's parent directory to persist its publication metadata.
///
/// # Errors
/// Returns directory open/sync errors, or `Unsupported` on platforms without
/// directory sync. Such platforms must supply another durable adapter.
pub fn sync_parent_dir(path: &Path) -> io::Result<()> {
    sync_dir(parent(path)?)
}

fn parent(path: &Path) -> io::Result<&Path> {
    path.parent()
        .map(|parent| {
            if parent.as_os_str().is_empty() {
                Path::new(".")
            } else {
                parent
            }
        })
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "path has no parent"))
}

#[cfg(unix)]
pub(crate) fn sync_dir(path: &Path) -> io::Result<()> {
    fs::File::open(path)?.sync_all()
}

#[cfg(windows)]
pub(crate) fn sync_dir(path: &Path) -> io::Result<()> {
    use std::os::windows::fs::OpenOptionsExt as _;
    fs::OpenOptions::new()
        .read(true)
        .custom_flags(0x0200_0000)
        .open(path)?
        .sync_all()
}

#[cfg(not(any(unix, windows)))]
pub(crate) fn sync_dir(_path: &Path) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "directory sync unavailable",
    ))
}

struct TemporaryFile {
    path: PathBuf,
}

impl TemporaryFile {
    fn write(path: &Path, bytes: &[u8]) -> io::Result<Self> {
        let (temporary, file) = Self::prepare(path, bytes)?;
        file.sync_all()?;
        Ok(temporary)
    }

    fn prepare(path: &Path, bytes: &[u8]) -> io::Result<(Self, fs::File)> {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        loop {
            let sequence = NEXT
                .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |value| {
                    value.checked_add(1)
                })
                .map_err(|_value| io::Error::other("temporary sequence exhausted"))?;
            let name = path.file_name().ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidInput, "missing publication filename")
            })?;
            let mut name = name.to_os_string();
            name.push(format!(".tmp.{}.{sequence}", std::process::id()));
            let temporary_path = path.with_file_name(name);
            match fs::OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(&temporary_path)
            {
                Ok(mut file) => {
                    let temporary = Self {
                        path: temporary_path,
                    };
                    file.write_all(bytes)?;
                    return Ok((temporary, file));
                }
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                Err(error) => return Err(error),
            }
        }
    }
}

impl Drop for TemporaryFile {
    fn drop(&mut self) {
        // Cleanup is best effort: stale temporary files are never addressable.
        drop(fs::remove_file(&self.path));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn concurrent_metadata_replacement_never_shares_a_temporary_file() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("checkpoint");
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(8));
        let workers = (0u8..8)
            .map(|value| {
                let path = path.clone();
                let barrier = std::sync::Arc::clone(&barrier);
                std::thread::spawn(move || {
                    let _wait = barrier.wait();
                    atomic_write(&path, &vec![value; 16_384])
                })
            })
            .collect::<Vec<_>>();
        for worker in workers {
            worker.join().unwrap().unwrap();
        }
        let bytes = fs::read(path).unwrap();
        assert_eq!(bytes.len(), 16_384);
        assert!(
            bytes.iter().all(|byte| Some(byte) == bytes.first()),
            "only one complete publication is visible"
        );
    }

    #[test]
    fn exclusive_publication_never_replaces_a_conflicting_destination() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("blob");
        fs::write(&path, b"retained conflicting evidence").unwrap();
        assert!(
            !publish_new(&path, b"incoming bytes").unwrap(),
            "existing evidence wins publication"
        );
        assert_eq!(fs::read(&path).unwrap(), b"retained conflicting evidence");
    }
    #[test]
    fn publication_sync_failure_is_unacknowledged_and_blob_retry_preserves_bytes() {
        let directory = tempfile::tempdir().unwrap();
        let mut store = crate::BlobStore::new(directory.path()).unwrap();
        let bytes = b"published, but directory sync failed\0\xff";
        let path = store.path_for(blake3::hash(bytes).as_bytes());
        let result = publish_new_with(&path, bytes, |_parent| {
            Err(io::Error::other("injected directory sync failure"))
        });
        assert!(
            result.is_err(),
            "visible publication is not a durable acknowledgement"
        );
        assert_eq!(fs::read(&path).unwrap(), bytes);
        store.write(bytes).unwrap();
        assert_eq!(fs::read(path).unwrap(), bytes);
        assert_eq!(store.len().unwrap(), 1);
    }

    #[test]
    fn blob_cohort_shares_its_publication_fence_and_retries_uncertain_bytes() {
        use crate::BlobStorage as _;
        let directory = tempfile::tempdir().unwrap();
        let mut store = crate::BlobStore::new(directory.path()).unwrap();
        let values: Vec<_> = (0..32)
            .map(|n| format!("cohort {n}\0").into_bytes())
            .collect();
        let payloads: Vec<_> = values.iter().map(Vec::as_slice).collect();
        let entries: Vec<_> = payloads
            .iter()
            .map(|bytes| (store.path_for(blake3::hash(bytes).as_bytes()), *bytes))
            .collect();
        let fences = std::cell::Cell::new(0_usize);
        let result = publish_batch_with(
            &entries,
            |_path, _bytes| Ok(false),
            |_parent| {
                fences.set(fences.get().saturating_add(1));
                Err(io::Error::other("injected cohort directory sync failure"))
            },
        );
        assert!(
            result.is_err(),
            "readable bytes cannot acknowledge a failed cohort"
        );
        assert_eq!(
            fences.get(),
            1,
            "one publication fence covers the directory cohort"
        );
        for (path, bytes) in &entries {
            assert_eq!(fs::read(path).unwrap(), *bytes);
        }
        let references = store.put_batch(&payloads).unwrap();
        assert_eq!(references.len(), 32);
        assert_eq!(store.len().unwrap(), 32);
        for (reference, bytes) in references.iter().zip(&payloads) {
            assert_eq!(
                store.resolve(reference),
                crate::BlobResolution::Found(bytes.to_vec())
            );
        }
    }

    #[test]
    fn cohort_deduplicates_exact_payloads_before_staging_and_rejects_collisions() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("one-destination");
        let entries = vec![(path.clone(), b"same bytes".as_slice()); 32];
        let visits = std::cell::Cell::new(0_usize);
        publish_batch(&entries, |_path, _bytes| {
            visits.set(visits.get().saturating_add(1));
            Ok(false)
        })
        .unwrap();
        assert_eq!(
            visits.get(),
            1,
            "duplicates cannot produce extra temporary-file IO"
        );
        assert_eq!(fs::read(&path).unwrap(), b"same bytes");
        let collision = vec![
            (path.clone(), b"first".as_slice()),
            (path.clone(), b"different".as_slice()),
        ];
        assert!(publish_batch(&collision, |_path, _bytes| Ok(true)).is_err());
        assert_eq!(fs::read(path).unwrap(), b"same bytes");
    }
}
