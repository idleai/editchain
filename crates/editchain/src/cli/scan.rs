//! Bounded payload previews and exact sizes from a sequential schema-three scan.

use std::{collections::HashMap, io, path::Path};

use editchain_engine::{OpId, OpKind, Payload};
use editchain_import::cancellation::ImportCancellation;
use editchain_store::format;
use serde_json::json;

use super::{
    error::{Failure, Result},
    output::Output,
    require_chain,
};

#[derive(Debug, clap::Args)]
pub(super) struct Args {
    /// Maximum inline preview bytes per payload field; exact lengths remain in the summary.
    #[arg(long, default_value = "16000", value_parser = clap::value_parser!(u32).range(0..=1_048_576))]
    preview_bytes: u32,
    /// Maximum inline preview bytes for Original source records.
    #[arg(long, default_value = "0", value_parser = clap::value_parser!(u32).range(0..=1_048_576))]
    original_preview_bytes: u32,
}

#[derive(Debug, Default)]
struct Scan {
    identities: HashMap<OpId, [u8; 32]>,
    duplicates: usize,
    binary_bytes: u64,
}

pub(super) fn run(
    chain: &Path,
    args: &Args,
    cancellation: &ImportCancellation,
    output: &mut Output,
) -> Result<()> {
    require_chain(chain)?;
    output.begin_stream()?;
    let mut scan = Scan::default();
    let mut failure = None;
    let visited = editchain_store::visit_records(chain, &mut |_flags, encoded| {
        let result = cancellation
            .check(chain)
            .map_err(Failure::from)
            .and_then(|()| scan.record(encoded, args, output));
        result.map_err(|error| {
            failure = Some(error);
            io::Error::other("scan stopped")
        })
    });
    if let Some(error) = failure {
        return Err(error);
    }
    if visited?.incomplete_tails != 0 {
        return Err(Failure::new(4, "Incomplete log tail; scan is incomplete"));
    }
    output.emit(&json!({
        "type": "ready", "stats": {
            "accepted": scan.identities.len(),
            "records": scan.identities.len().saturating_add(scan.duplicates),
            "duplicates": scan.duplicates, "quarantined": 0,
            "undecodable": 0, "incomplete_tails": 0
        },
        "binary_bytes": scan.binary_bytes,
        "preview_bytes": args.preview_bytes,
        "original_preview_bytes": args.original_preview_bytes
    }))
}

impl Scan {
    fn record(&mut self, encoded: &[u8], args: &Args, output: &mut Output) -> Result<()> {
        let mut operation =
            format::decode_op(encoded).map_err(|error| Failure::new(4, error.to_string()))?;
        let hash = *blake3::hash(encoded).as_bytes();
        if let Some(previous) = self.identities.insert(operation.id, hash) {
            if previous != hash {
                return Err(Failure::new(
                    4,
                    "Conflicting operation ID; scan is incomplete",
                ));
            }
            self.duplicates = self.duplicates.saturating_add(1);
            return Ok(());
        }
        let (fields, truncated) = match &mut operation.kind {
            OpKind::Activity(record) => preview(record, args)?,
            OpKind::ChainStart(start) => {
                let fields = vec![("ChainStart.Name".into(), "Inline", byte_length(&start.name))];
                let limit = usize::try_from(args.preview_bytes)
                    .map_err(|error| Failure::input(error.to_string()))?;
                (fields, truncate(&mut start.name, limit))
            }
            OpKind::Actor(_)
            | OpKind::Session(_)
            | OpKind::Message(_)
            | OpKind::Tool(_)
            | OpKind::Command(_)
            | OpKind::File(_)
            | OpKind::Reflection(_)
            | OpKind::Import(_)
            | OpKind::Note(_)
            | OpKind::Error(_)
            | OpKind::GitCommit(_)
            | OpKind::GitLink(_)
            | OpKind::Unknown(_) => return Err(Failure::input(
                "Scan requires schema-three activities (ChainStart is supported); use migrate --schema3",
            )),
        };
        self.binary_bytes = self
            .binary_bytes
            .saturating_add(u64::try_from(encoded.len()).unwrap_or(u64::MAX));
        output.emit(&json!({
            "type": "record", "entry": {
                "operation": operation,
                "record_ref": {"operation": operation.id, "record_hash": hash},
                "payload_summary": fields, "payloads_truncated": truncated,
                "binary_bytes": encoded.len()
            }
        }))
    }
}

type PayloadSummary = Vec<(String, &'static str, u64)>;

fn byte_length(bytes: &[u8]) -> u64 {
    u64::try_from(bytes.len()).unwrap_or(u64::MAX)
}

fn truncate(bytes: &mut Vec<u8>, limit: usize) -> bool {
    let mut length = limit.min(bytes.len());
    if let Ok(text) = std::str::from_utf8(bytes) {
        while !text.is_char_boundary(length) {
            length = length.saturating_sub(1);
        }
    }
    let truncated = bytes.len() > length;
    bytes.truncate(length);
    truncated
}

fn preview(
    record: &mut editchain_engine::activity::Operation,
    args: &Args,
) -> Result<(PayloadSummary, bool)> {
    let kind = format!("{:?}", record.kind.name());
    let fields = record
        .kind
        .fields()
        .into_iter()
        .map(|(field, payload)| {
            let (storage, length) = match payload {
                Payload::Inline(bytes) => ("Inline", byte_length(bytes)),
                Payload::Blob(reference) => ("Blob", u64::from(reference.len)),
                Payload::Empty => ("Empty", 0),
            };
            (format!("{kind}.{field:?}"), storage, length)
        })
        .collect();
    let limit = usize::try_from(if kind == "Original" {
        args.original_preview_bytes
    } else {
        args.preview_bytes
    })
    .map_err(|error| Failure::input(error.to_string()))?;
    let mut truncated = false;
    for (_, payload) in record.kind.fields_mut() {
        if let Payload::Inline(bytes) = payload {
            truncated |= truncate(bytes, limit);
        }
    }
    Ok((fields, truncated))
}
