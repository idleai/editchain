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

use std::io::{self, Read, Write};

// Crate-level dependency markers (used by Cargo for feature resolution).
use editchain_codec as _;
use editchain_core as _;
use editchain_git as _;
use editchain_index as _;
use editchain_node as _;
use editchain_project as _;
use editchain_query as _;
use gix as _;

#[cfg(test)]
use tempfile as _;

use editchain_protocol::{Request, Response};
use editchain_vscode_service::Server;

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
    let mut payload = vec![0u8; len];
    reader.read_exact(&mut payload)?;
    Ok(Some(payload))
}

/// Write a length-prefixed frame to a writer.
fn write_frame(writer: &mut impl Write, payload: &[u8]) -> io::Result<()> {
    let len = u32::try_from(payload.len()).unwrap_or(u32::MAX);
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

        let response: Response = match server.handle(&request) {
            Ok(resp) => resp,
            Err(e) => Response {
                id: request.id,
                body: editchain_protocol::ResponseBody::Error(e.to_string()),
            },
        };

        let payload = serde_json::to_vec(&response)?;
        write_frame(&mut writer, &payload)?;
    }

    Ok(())
}
