use std::io::{BufRead, BufReader, Read};
use std::path::Path;

use crate::error::ImportError;
use crate::ids::hash_raw;
use crate::sink::CursorValue;

/// Read a session file incrementally, returning complete lines and their hashes.
///
/// Uses streaming `BufRead::read_until` to avoid loading the entire file into
/// memory. Skips partial final lines (power-loss tolerance).
/// Returns (`lines_with_hashes`, `bytes_read`, `new_cursor_value`).
///
/// # Errors
///
/// Returns [`ImportError::Io`] if the file cannot be read or seeked.
/// Returns [`ImportError::SourceGenerationChanged`] if the file was truncated
/// since the last read.
#[expect(
    clippy::arithmetic_side_effects,
    reason = "byte counts and offsets are bounded by file size"
)]
#[expect(clippy::as_conversions, reason = "usize to u64 is safe for file sizes")]
pub fn read_session_file(
    path: &Path,
    cursor: Option<&CursorValue>,
) -> Result<(Vec<LineWithHash>, u64, CursorValue), ImportError> {
    let metadata = std::fs::metadata(path).map_err(ImportError::Io)?;
    let file_size = metadata.len();

    let offset = match cursor {
        Some(c) => {
            if file_size < c.file_size {
                return Err(ImportError::SourceGenerationChanged {
                    path: path.to_path_buf(),
                    expected_size: c.file_size,
                    actual_size: file_size,
                });
            }
            c.byte_offset
        }
        None => 0,
    };

    let mut file = std::fs::File::open(path).map_err(ImportError::Io)?;
    let mut hasher = blake3::Hasher::new();
    // Re-hash the accepted prefix directly. Persisting a digest as if it were
    // resumable hasher state made the old cursor hash depend on append batch
    // boundaries and could not prove continuity after source relocation.
    let hashed_bytes = {
        let mut prefix = (&mut file).take(offset);
        std::io::copy(&mut prefix, &mut hasher).map_err(ImportError::Io)?
    };
    if hashed_bytes != offset {
        return Err(ImportError::SourceGenerationChanged {
            path: path.to_path_buf(),
            expected_size: offset,
            actual_size: file_size,
        });
    }
    let mut reader = BufReader::new(file);

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
            let _: &mut blake3::Hasher = hasher.update(&line_buf);
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
        ops_emitted: cursor.map_or(0, |c| c.ops_emitted) + lines.len() as u64,
        content_hash,
        content_hash_version: 1,
        source_node: cursor.and_then(|c| c.source_node),
        normalization_version: cursor.map_or(0, |c| c.normalization_version),
        session_title_hash: cursor.and_then(|c| c.session_title_hash),
    };

    Ok((lines, bytes_read, new_cursor))
}

/// A single JSONL line with its Blake3 hash.
#[derive(Debug, Clone)]
pub struct LineWithHash {
    /// The raw JSONL line bytes (including newline).
    pub data: Vec<u8>,
    /// BLAKE3 hash of the line bytes.
    pub hash: [u8; 32],
}
