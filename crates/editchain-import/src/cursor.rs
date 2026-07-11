use std::path::Path;

use crate::error::ImportError;
use crate::ids::hash_raw;
use crate::sink::CursorValue;

/// Check whether a source file has changed since the last cursor was written.
///
/// Returns `Ok(true)` if the file is unchanged (same size), `Ok(false)` if it
/// has grown (append detected), and `Err` if it was truncated or rewritten
/// (generation changed).
pub fn check_file_generation(
    path: &Path,
    cursor: &CursorValue,
) -> Result<bool, ImportError> {
    let metadata = std::fs::metadata(path).map_err(ImportError::Io)?;
    let current_size = metadata.len();

    if current_size < cursor.file_size {
        // File was truncated or rewritten — generation changed.
        Err(ImportError::SourceGenerationChanged {
            path: path.to_path_buf(),
            expected_size: cursor.file_size,
            actual_size: current_size,
        })
    } else if current_size == cursor.file_size {
        // Unchanged.
        Ok(true)
    } else {
        // Appended.
        Ok(false)
    }
}

/// Read new bytes from a file starting at the given offset.
///
/// Returns the new bytes and their cumulative hash.
pub fn read_new_bytes(
    path: &Path,
    offset: u64,
) -> Result<(Vec<u8>, [u8; 32]), ImportError> {
    use std::io::Read;

    let mut file = std::fs::File::open(path).map_err(ImportError::Io)?;
    use std::io::SeekFrom;
    std::io::Seek::seek(&mut file, SeekFrom::Start(offset)).map_err(ImportError::Io)?;

    let mut new_bytes = Vec::new();
    file.read_to_end(&mut new_bytes).map_err(ImportError::Io)?;

    let hash = hash_raw(&new_bytes);
    Ok((new_bytes, hash))
}

/// Split raw bytes into complete JSONL lines, deferring a partial final line.
///
/// Returns (complete_lines, partial_line_bytes).
pub fn split_lines(data: &[u8]) -> (Vec<Vec<u8>>, Vec<u8>) {
    let mut lines = Vec::new();
    let mut start = 0;

    for i in 0..data.len() {
        if data[i] == b'\n' {
            lines.push(data[start..=i].to_vec());
            start = i + 1;
        }
    }

    let remainder = data[start..].to_vec();
    (lines, remainder)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_lines_complete() {
        let data = b"line1\nline2\nline3\n";
        let (lines, remainder) = split_lines(data);
        assert_eq!(lines.len(), 3);
        assert!(remainder.is_empty());
    }

    #[test]
    fn split_lines_partial_final() {
        let data = b"line1\nline2\npartial";
        let (lines, remainder) = split_lines(data);
        assert_eq!(lines.len(), 2);
        assert_eq!(remainder, b"partial");
    }

    #[test]
    fn split_lines_empty() {
        let data = b"";
        let (lines, remainder) = split_lines(data);
        assert!(lines.is_empty());
        assert!(remainder.is_empty());
    }
}