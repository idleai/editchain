//! Dedicated local peer control. Network bytes have only a TLS input route.

use std::io::{self, Read, Write};
use std::path::PathBuf;

use base64::{engine::general_purpose::STANDARD, Engine as _};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::{
    invalid, DeviceIdentity, Membership, PublicDevice, Replica, SecurePeer, MAX_BRIDGE_BYTES,
};

/// Limit checked before allocating a native peer control frame.
pub const MAX_CONTROL_BYTES: usize = 512 * 1024;

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
enum Request {
    Verify {
        certificate: String,
    },
    Identity {
        device_dir: PathBuf,
    },
    Scope {
        chain_dir: PathBuf,
    },
    Configure {
        chain_dir: PathBuf,
        space: String,
        backfill: bool,
    },
    Approve {
        chain_dir: PathBuf,
        space: String,
        certificate: String,
    },
    Revoke {
        chain_dir: PathBuf,
        space: String,
        fingerprint: String,
    },
    Devices {
        chain_dir: PathBuf,
        space: String,
    },
    Open {
        chain_dir: PathBuf,
        device_dir: PathBuf,
        space: String,
        remote: Option<String>,
    },
    Turn {
        bytes: String,
        tick: bool,
    },
    Close,
}

#[derive(Debug, Default)]
struct Worker {
    peer: Option<SecurePeer>,
}

impl Worker {
    fn handle(&mut self, request: Request) -> io::Result<Value> {
        match request {
            Request::Verify { certificate } => Ok(json!(PublicDevice::parse(&certificate)?)),
            Request::Identity { device_dir } => {
                Ok(json!(DeviceIdentity::load_or_create(&device_dir)?.public()))
            }
            Request::Scope { chain_dir } => {
                Ok(json!({ "space": Replica::bound_space(&chain_dir)? }))
            }
            Request::Configure {
                chain_dir,
                space,
                backfill,
            } => {
                let replica = Replica::open(&chain_dir, &space, backfill)?;
                if backfill {
                    replica.include_backfill()?;
                }
                Ok(json!({ "space": replica.space() }))
            }
            Request::Approve {
                chain_dir,
                space,
                certificate,
            } => Ok(json!(
                Membership::open(&chain_dir, &space)?.approve(&certificate)?
            )),
            Request::Revoke {
                chain_dir,
                space,
                fingerprint,
            } => {
                Membership::open(&chain_dir, &space)?.revoke(&fingerprint)?;
                Ok(json!({}))
            }
            Request::Devices { chain_dir, space } => {
                Ok(json!(Membership::open(&chain_dir, &space)?.devices()?))
            }
            Request::Open {
                chain_dir,
                device_dir,
                space,
                remote,
            } => {
                if self.peer.is_some() {
                    return Err(invalid("peer already open"));
                }
                let identity = DeviceIdentity::load_or_create(&device_dir)?;
                self.peer = Some(SecurePeer::open(
                    &chain_dir,
                    &space,
                    &identity,
                    remote.as_deref(),
                )?);
                self.turn(&[], false)
            }
            Request::Turn { bytes, tick } => {
                if bytes.len() > MAX_BRIDGE_BYTES.saturating_add(2).saturating_mul(4) / 3 {
                    return Err(invalid("opaque input exceeds limit"));
                }
                let bytes = STANDARD
                    .decode(bytes)
                    .map_err(|_error| invalid("invalid opaque input encoding"))?;
                self.turn(&bytes, tick)
            }
            Request::Close => {
                let closing = self.peer.take();
                let bytes = if let Some(mut peer) = closing {
                    peer.close()?
                } else {
                    Vec::new()
                };
                Ok(json!({ "bytes": STANDARD.encode(bytes) }))
            }
        }
    }

    fn turn(&mut self, bytes: &[u8], tick: bool) -> io::Result<Value> {
        let peer = self
            .peer
            .as_mut()
            .ok_or_else(|| invalid("peer is not open"))?;
        let output = peer.turn(bytes, tick)?;
        Ok(
            json!({ "bytes": STANDARD.encode(output), "device": peer.device(), "progress": peer.progress() }),
        )
    }
}

/// Run one dedicated worker using bounded local JSON control frames.
///
/// The peer protocol is intentionally absent from this dispatch table. Opaque
/// remote bytes enter only `SecurePeer::turn`, after local configuration. Errors
/// return fixed codes, never raw content, certificates, paths or key material.
///
/// # Errors
/// Returns truncated/oversized framing, EOF during a frame, or output failure.
pub fn run_worker(reader: &mut impl Read, writer: &mut impl Write) -> io::Result<()> {
    let mut worker = Worker::default();
    while let Some(frame) = read_frame(reader)? {
        let result = serde_json::from_slice::<Request>(&frame)
            .map_err(|_error| invalid("invalid peer control request"))
            .and_then(|request| worker.handle(request));
        let response = match result {
            Ok(value) => json!({ "ok": true, "result": value }),
            Err(error) => json!({ "ok": false, "error": error_code(&error) }),
        };
        write_frame(
            writer,
            &serde_json::to_vec(&response).map_err(io::Error::other)?,
        )?;
    }
    Ok(())
}

fn error_code(error: &io::Error) -> &'static str {
    let kind = error.kind();
    if kind == io::ErrorKind::PermissionDenied {
        "authentication_failed"
    } else if kind == io::ErrorKind::WouldBlock {
        "storage_busy"
    } else if matches!(
        kind,
        io::ErrorKind::InvalidData | io::ErrorKind::InvalidInput
    ) {
        "invalid_request_or_peer_data"
    } else if matches!(
        kind,
        io::ErrorKind::ConnectionAborted | io::ErrorKind::UnexpectedEof
    ) {
        "connection_closed"
    } else {
        "storage_or_transport_failure"
    }
}

fn read_frame(reader: &mut impl Read) -> io::Result<Option<Vec<u8>>> {
    let mut first = [0];
    if reader.read(&mut first)? == 0 {
        return Ok(None);
    }
    let mut tail = [0; 3];
    reader.read_exact(&mut tail)?;
    let [a] = first;
    let [b, c, d] = tail;
    let length = usize::try_from(u32::from_le_bytes([a, b, c, d])).map_err(io::Error::other)?;
    if length == 0 || length > MAX_CONTROL_BYTES {
        return Err(invalid("invalid peer control frame length"));
    }
    let mut bytes = vec![0; length];
    reader.read_exact(&mut bytes)?;
    Ok(Some(bytes))
}

fn write_frame(writer: &mut impl Write, bytes: &[u8]) -> io::Result<()> {
    if bytes.len() > MAX_CONTROL_BYTES {
        return Err(invalid("peer control output exceeds limit"));
    }
    writer.write_all(
        &u32::try_from(bytes.len())
            .map_err(io::Error::other)?
            .to_le_bytes(),
    )?;
    writer.write_all(bytes)?;
    writer.flush()
}
