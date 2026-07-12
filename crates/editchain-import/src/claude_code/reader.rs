use std::io::{BufRead, BufReader, Seek, SeekFrom};
use std::path::Path;

use crate::error::ImportError;
use crate::ids::hash_raw;
use crate::sink::CursorValue;

/// Read a session file incrementally, returning complete lines and their hashes.
///
/// Uses streaming `BufRead::read_until` to avoid loading the entire file into
/// memory. Skips partial final lines (power-loss tolerance).
/// Returns (lines_with_hashes, bytes_read, new_cursor_value).
pub fn read_session_file(
    path: &Path,
    cursor: Option<&CursorValue>,
) -> Result<(Vec<LineWithHash>, u64, CursorValue), ImportError> {
    let metadata = std::fs::metadata(path).map_err(ImportError::Io)?;
    let file_size = metadata.len();

    let (offset, prior_hash) = match cursor {
        Some(c) => {
            if file_size < c.file_size {
                return Err(ImportError::SourceGenerationChanged {
                    path: path.to_path_buf(),
                    expected_size: c.file_size,
                    actual_size: file_size,
                });
            }
            (c.byte_offset, c.content_hash)
        }
        None => (0, [0u8; 32]),
    };

    let file = std::fs::File::open(path).map_err(ImportError::Io)?;
    let mut reader = BufReader::new(file);

    if offset > 0 {
        reader.seek(SeekFrom::Start(offset)).map_err(ImportError::Io)?;
    }

    let mut hasher = blake3::Hasher::new();
    if prior_hash != [0u8; 32] {
        hasher.update(&prior_hash);
    }

    let mut lines = Vec::new();
    let mut bytes_read: u64 = 0;
    let mut line_buf = Vec::new();

    loop {
        line_buf.clear();
        let n = reader
            .read_until(b'\n', &mut line_buf)
            .map_err(ImportError::Io)?;

        if n == 0 {
            // EOF — no partial line to retain.
            break;
        }

        // Check if this is a complete line (ends with newline).
        if line_buf.last() == Some(&b'\n') {
            hasher.update(&line_buf);
            let line_hash = hash_raw(&line_buf);
            lines.push(LineWithHash {
                data: line_buf.clone(),
                hash: line_hash,
            });
            bytes_read += n as u64;
        } else {
            // Partial final line — stop (power-loss tolerance).
            // Do NOT include it in the hash or byte count.
            break;
        }
    }

    let content_hash = *hasher.finalize().as_bytes();

    let new_cursor = CursorValue {
        file_size,
        byte_offset: offset + bytes_read,
        ops_emitted: cursor.map(|c| c.ops_emitted).unwrap_or(0) + lines.len() as u64,
        content_hash,
    };

    Ok((lines, bytes_read, new_cursor))
}

/// A single JSONL line with its Blake3 hash.
#[derive(Debug, Clone)]
pub struct LineWithHash {
    pub data: Vec<u8>,
    pub hash: [u8; 32],
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_empty_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("empty.jsonl");
        std::fs::write(&path, b"").unwrap();

        let (lines, bytes, cursor) = read_session_file(&path, None).unwrap();
        assert!(lines.is_empty());
        assert_eq!(bytes, 0);
        assert_eq!(cursor.byte_offset, 0);
    }

    #[test]
    fn read_single_line() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("single.jsonl");
        std::fs::write(&path, b"{\"type\":\"test\"}\n").unwrap();

        let (lines, bytes, _) = read_session_file(&path, None).unwrap();
        assert_eq!(lines.len(), 1);
        assert_eq!(bytes, 16);
    }

    #[test]
    fn read_partial_final_line() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("partial.jsonl");
        std::fs::write(&path, b"{\"type\":\"a\"}\n{\"type\":\"b").unwrap();

        let (lines, bytes, cursor) = read_session_file(&path, None).unwrap();
        assert_eq!(lines.len(), 1);
        assert_eq!(bytes, 13); // only the complete line
        assert_eq!(cursor.byte_offset, 13); // partial not counted
    }

    #[test]
    fn read_appended_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("append.jsonl");

        // Write first line.
        std::fs::write(&path, b"line1\n").unwrap();
        let (lines1, _, cursor1) = read_session_file(&path, None).unwrap();
        assert_eq!(lines1.len(), 1);

        // Append second line.
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap();
        use std::io::Write;
        writeln!(f, "line2").unwrap();
        drop(f);

        // Read from cursor.
        let (lines2, _, cursor2) = read_session_file(&path, Some(&cursor1)).unwrap();
        assert_eq!(lines2.len(), 1);
        assert_eq!(cursor2.byte_offset, 12); // 6 + 6 bytes
    }
}