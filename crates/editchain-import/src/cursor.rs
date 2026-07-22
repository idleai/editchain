use std::io::{Read, SeekFrom};
use std::path::Path;

use crate::error::ImportError;
use crate::ids::hash_raw;
use crate::sink::CursorValue;

/// Check whether a source file has changed since the last cursor was written.
///
/// Returns `Ok(true)` if the file is unchanged (same size), `Ok(false)` if it
/// has grown (append detected), and `Err` if it was truncated or rewritten
/// (generation changed).
///
/// # Errors
///
/// Returns `ImportError::Io` if the file cannot be read, or
/// `ImportError::SourceGenerationChanged` if the file was truncated.
pub fn check_file_generation(path: &Path, cursor: &CursorValue) -> Result<bool, ImportError> {
    let metadata = std::fs::metadata(path).map_err(ImportError::Io)?;
    let current_size = metadata.len();

    match current_size.cmp(&cursor.file_size) {
        std::cmp::Ordering::Less => {
            // File was truncated or rewritten — generation changed.
            Err(ImportError::SourceGenerationChanged {
                path: path.to_path_buf(),
                expected_size: cursor.file_size,
                actual_size: current_size,
            })
        }
        std::cmp::Ordering::Equal => {
            // Unchanged.
            Ok(true)
        }
        std::cmp::Ordering::Greater => {
            // Appended.
            Ok(false)
        }
    }
}

/// Read new bytes from a file starting at the given offset.
///
/// Returns the new bytes and their cumulative hash.
///
/// # Errors
///
/// Returns `ImportError::Io` if the file cannot be opened or read.
pub fn read_new_bytes(path: &Path, offset: u64) -> Result<(Vec<u8>, [u8; 32]), ImportError> {
    let mut file = std::fs::File::open(path).map_err(ImportError::Io)?;
    let _: u64 =
        std::io::Seek::seek(&mut file, SeekFrom::Start(offset)).map_err(ImportError::Io)?;

    let mut new_bytes = Vec::new();
    let _: usize = file.read_to_end(&mut new_bytes).map_err(ImportError::Io)?;

    let hash = hash_raw(&new_bytes);
    Ok((new_bytes, hash))
}

/// Split raw bytes into complete JSONL lines, deferring a partial final line.
///
/// Returns (`complete_lines`, `partial_line_bytes`).
#[must_use]
#[expect(
    clippy::indexing_slicing,
    reason = "slice indices are bounded by data.len() via iteration"
)]
#[expect(
    clippy::arithmetic_side_effects,
    reason = "i + 1 is bounded by data.len() since we iterate over data"
)]
pub fn split_lines(data: &[u8]) -> (Vec<Vec<u8>>, Vec<u8>) {
    let mut lines = Vec::new();
    let mut start = 0;

    for (i, &byte) in data.iter().enumerate() {
        if byte == b'\n' {
            let end = i + 1;
            lines.push(data[start..end].to_vec());
            start = end;
        }
    }

    let remainder = data[start..].to_vec();
    (lines, remainder)
}
