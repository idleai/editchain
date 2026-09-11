//! Atomic file publication primitives shared by durable storage and checkpoints.

use std::fs;
use std::io::{self, Write as _};
use std::path::Path;

/// Atomically write `data` to `path` via a same-directory temp file + rename.
///
/// The rename makes the final name appear only with complete contents; a crash
/// mid-write leaves at worst a stale temp file. After the rename the parent
/// directory is synced, so the new directory entry is durable before this
/// returns — otherwise a crash could lose the rename even though the file
/// bytes themselves were synced.
///
/// # Errors
///
/// Returns an IO error if the temp file cannot be written or renamed, or the
/// parent directory cannot be synced.
pub fn atomic_write(path: &Path, data: &[u8]) -> io::Result<()> {
    let tmp = path.with_extension(format!("tmp.{}", std::process::id()));
    {
        let mut file = fs::OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(&tmp)?;
        file.write_all(data)?;
        file.sync_all()?;
    }
    fs::rename(&tmp, path)?;
    sync_parent_dir(path)
}

/// Fsync `path`'s parent directory so a rename/create inside it survives a
/// crash (directory entries are metadata and are not covered by the file's own
/// `sync_all`).
///
/// On Unix the directory is opened read-only and fsynced. On Windows opening a
/// directory requires `FILE_FLAG_BACKUP_SEMANTICS`. On other platforms
/// directory fsync is not available portably and the call degrades to a no-op
/// (best-effort durability).
///
/// # Errors
///
/// Returns an IO error if the parent directory cannot be opened or synced.
#[cfg(unix)]
pub fn sync_parent_dir(path: &Path) -> io::Result<()> {
    let parent = path.parent().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("cannot sync parent of {}", path.display()),
        )
    })?;
    fs::File::open(parent)?.sync_all()
}

/// Windows variant of [`sync_parent_dir`]: directories open with
/// `FILE_FLAG_BACKUP_SEMANTICS` (0x02000000) and can then be fsynced.
#[cfg(windows)]
pub fn sync_parent_dir(path: &Path) -> io::Result<()> {
    use std::os::windows::fs::OpenOptionsExt;
    let parent = path.parent().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("cannot sync parent of {}", path.display()),
        )
    })?;
    fs::OpenOptions::new()
        .read(true)
        .custom_flags(0x0200_0000)
        .open(parent)?
        .sync_all()
}

/// Fallback for platforms without directory fsync: best-effort no-op.
#[cfg(not(any(unix, windows)))]
pub fn sync_parent_dir(_path: &Path) -> io::Result<()> {
    Ok(())
}
