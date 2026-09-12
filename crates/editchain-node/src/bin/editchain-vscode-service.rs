//! The `EditChain` VS Code service binary.
//!
//! Reads length-prefixed JSON frames from stdin (4-byte little-endian length
//! followed by UTF-8 JSON), dispatches each against a stateful `Server`, and
//! writes framed responses to stdout. This mirrors the TS `StdioClient`.

#![expect(
    clippy::arithmetic_side_effects,
    clippy::as_conversions,
    clippy::indexing_slicing,
    clippy::print_stderr,
    reason = "Binary stdio loop; frame offsets and lengths are bounded by the input buffer"
)]

use clap as _;
use ctrlc as _;
use dirs as _;
use editchain_import as _;
use std::io::{self, Read, Write};
#[cfg(test)]
use tempfile as _;

// Crate-level dependency markers (used by Cargo for feature resolution).
use blake3 as _;
use editchain_core as _;
use editchain_git as _;
use editchain_index as _;
use editchain_project as _;
use editchain_store as _;
use serde as _;
use tantivy as _;

use editchain_node::Server;
use editchain_protocol::{Request, Response, ServiceError, MAX_REQUEST_FRAME_BYTES};

/// Read a single length-prefixed frame from a reader.
///
/// Returns `Ok(None)` on clean EOF (no bytes read).
fn read_frame(reader: &mut impl Read) -> io::Result<Option<Vec<u8>>> {
    let mut len_buf = [0u8; 4];
    let mut filled = 0usize;
    while filled < 4 {
        let n = reader.read(&mut len_buf[filled..])?;
        if n == 0 {
            if filled == 0 {
                return Ok(None); // clean EOF
            }
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "partial length prefix",
            ));
        }
        filled += n;
    }
    let len = u32::from_le_bytes(len_buf) as usize;
    if len > MAX_REQUEST_FRAME_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "request frame exceeds 8 MiB",
        ));
    }
    let mut payload = vec![0u8; len];
    reader.read_exact(&mut payload)?;
    Ok(Some(payload))
}

/// Write a length-prefixed frame to a writer.
fn write_frame(writer: &mut impl Write, payload: &[u8]) -> io::Result<()> {
    let len = u32::try_from(payload.len())
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
    writer.write_all(&len.to_le_bytes())?;
    writer.write_all(payload)?;
    writer.flush()
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let stdin = io::stdin();
    let mut reader = stdin.lock();
    let stdout = io::stdout();
    let mut writer = stdout.lock();

    let mut server = Server::new();

    loop {
        let frame = match read_frame(&mut reader) {
            Ok(Some(frame)) => frame,
            Ok(None) => break, // clean EOF
            Err(e) => {
                eprintln!("read error: {e}");
                break;
            }
        };

        let request: Request = match serde_json::from_slice(&frame) {
            Ok(req) => req,
            Err(e) => {
                eprintln!("parse error: {e}");
                continue;
            }
        };

        let payload = match server.handle_encoded(&request) {
            Ok(payload) => payload,
            Err(e) => serde_json::to_vec(&Response {
                id: request.id,
                body: editchain_protocol::ResponseBody::Error(ServiceError::from_error(e.as_ref())),
            })?,
        };
        write_frame(&mut writer, &payload)?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_reader_checks_the_advertised_limit_before_reading_payload() {
        let oversized = u32::try_from(MAX_REQUEST_FRAME_BYTES.saturating_add(1)).unwrap();
        let bytes = oversized.to_le_bytes();
        assert_eq!(
            read_frame(&mut bytes.as_slice()).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
        assert!(read_frame(&mut [].as_slice()).unwrap().is_none());
        assert_eq!(
            read_frame(&mut [1, 0].as_slice()).unwrap_err().kind(),
            io::ErrorKind::UnexpectedEof
        );
        assert_eq!(
            read_frame(&mut [1, 0, 0, 0].as_slice()).unwrap_err().kind(),
            io::ErrorKind::UnexpectedEof
        );
        assert_eq!(
            read_frame(&mut [1, 0, 0, 0, b'x'].as_slice()).unwrap(),
            Some(vec![b'x'])
        );
    }
}
