//! Bounded file inputs and cancellable consumers of chunked stdin.

use super::error::{Failure, Result};
use std::{
    fs::File,
    io::{self, Read},
    path::Path,
    sync::mpsc,
};

/// Shared maximum size of one CLI object (64 MiB).
pub const MAX_INPUT_BYTES: usize = 64 * 1024 * 1024;

/// Open a file, or stdin for the path `-`.
/// # Errors
/// Returns file-open failures.
pub fn reader(path: &Path) -> Result<Box<dyn Read>> {
    if path == Path::new("-") {
        Ok(Box::new(io::stdin()))
    } else {
        Ok(Box::new(File::open(path)?))
    }
}

/// Read one object with the shared size limit.
/// # Errors
/// Returns input or read failures.
pub fn bytes(path: &Path) -> Result<Vec<u8>> {
    let maximum =
        u64::try_from(MAX_INPUT_BYTES).map_err(|error| Failure::input(error.to_string()))?;
    let mut bytes = Vec::new();
    let _count = reader(path)?
        .take(maximum.saturating_add(1))
        .read_to_end(&mut bytes)?;
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > maximum {
        return Err(Failure::input("input exceeds the 64 MiB object limit"));
    }
    Ok(bytes)
}

/// Read stdin on a worker with two bounded chunks of backpressure.
#[must_use]
pub fn stdin_chunks() -> mpsc::Receiver<io::Result<Vec<u8>>> {
    let (sender, receiver) = mpsc::sync_channel(2);
    let _reader = std::thread::spawn(move || {
        let mut input = io::stdin().lock();
        loop {
            let mut bytes = vec![0; 64 * 1024];
            let result = input.read(&mut bytes).map(|count| {
                bytes.truncate(count);
                bytes
            });
            let ended = !result.as_ref().is_ok_and(|bytes| !bytes.is_empty());
            if sender.send(result).is_err() || ended {
                break;
            }
        }
    });
    receiver
}
