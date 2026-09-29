//! Append, content storage, and integrity command adapters.

use std::path::Path;

use editchain_engine::{Admission, BlobResolution, ContentId, Engine, Op, OpKind};
use editchain_store::BlobSource as _;
use serde_json::json;

use super::{
    error::{Failure, Result},
    input,
    output::Output,
    require_chain,
};

#[derive(Debug, clap::Args)]
pub(super) struct Append {
    #[command(flatten)]
    input: input::Input,
    /// Input is the exact JSON/JSONL archive produced by export.
    #[arg(long, conflicts_with = "encoded")]
    archive: bool,
    /// Input is one exact binary operation encoding.
    #[arg(long)]
    encoded: bool,
}

#[derive(Clone, Copy)]
pub(super) enum Kind {
    Any,
    Chain,
    Note,
    Reflection,
}

pub(super) fn append(chain: &Path, args: &Append, output: &mut Output) -> Result<()> {
    if args.archive {
        return super::archive::restore(chain, &args.input.input, output);
    }
    if args.encoded {
        require_chain(chain)?;
        let bytes = input::bytes(&args.input.input)?;
        let operation = editchain_engine::decode_op(&bytes)
            .map_err(|error| Failure::input(error.to_string()))?;
        let admission = Engine::open(chain)?.append_encoded(&bytes)?;
        emit_admission(operation.id, admission, output)?;
        return conflict_result(admission == Admission::Conflict);
    }
    append_kind(chain, &args.input.input, Kind::Any, output)
}

pub(super) fn append_kind(
    chain: &Path,
    path: &Path,
    kind: Kind,
    output: &mut Output,
) -> Result<()> {
    if !matches!(kind, Kind::Chain) {
        require_chain(chain)?;
    }
    output.begin_stream()?;
    let mut conflict = false;
    let mut count = 0_usize;
    let mut writer: Option<editchain_engine::ChainWriter> = None;
    input::records::<Op>(path, |operation| {
        let valid = match kind {
            Kind::Any => true,
            Kind::Chain => matches!(operation.kind, OpKind::ChainStart(_)),
            Kind::Note => {
                matches!(&operation.kind, OpKind::Note(_))
                    || matches!(&operation.kind, OpKind::Activity(record) if matches!(record.kind, editchain_engine::activity::Kind::Note(_)))
            }
            Kind::Reflection => {
                matches!(&operation.kind, OpKind::Reflection(_))
                    || matches!(&operation.kind, OpKind::Activity(record) if matches!(&record.kind, editchain_engine::activity::Kind::Message(message) if message.category == editchain_engine::activity::MessageKind::Summary))
            }
        };
        if !valid {
            return Err(Failure::input("operation kind does not match this command"));
        }
        let admission = if let Some(writer) = &mut writer {
            writer.append(&operation)?
        } else {
            let mut owned = Engine::open(chain)?.writer()?;
            let admission = owned.append(&operation)?;
            writer = Some(owned);
            admission
        };
        conflict |= admission == Admission::Conflict;
        count = count.saturating_add(1);
        emit_admission(operation.id, admission, output)
    })?;
    if count == 0 {
        return Err(Failure::input("no operations in input"));
    }
    conflict_result(conflict)
}

pub(super) fn emit_admission(
    id: editchain_engine::OpId,
    admission: Admission,
    output: &mut Output,
) -> Result<()> {
    let status = match admission {
        Admission::Accepted => "accepted",
        Admission::Duplicate => "duplicate",
        Admission::Conflict => "conflict",
    };
    output.emit(&json!({"operation":id,"admission":status}))
}

pub(super) fn conflict_result(conflict: bool) -> Result<()> {
    if conflict {
        Err(Failure::new(
            4,
            "conflicting record variants were retained; the identity is quarantined",
        ))
    } else {
        Ok(())
    }
}

pub(super) fn blob(chain: &Path, id: ContentId, raw: bool, output: &mut Output) -> Result<()> {
    require_chain(chain)?;
    let resolved = editchain_store::BlobReader::open(chain)?.read_content(id)?;
    match resolved {
        BlobResolution::Found(bytes) => {
            if raw {
                output.raw(&bytes)
            } else {
                output.emit(&json!({"id":id,"status":"available","bytes":bytes}))
            }
        }
        BlobResolution::Missing => unavailable_blob(id, "missing", 3, raw, output),
        BlobResolution::Corrupt => unavailable_blob(id, "corrupt", 4, raw, output),
        BlobResolution::Unresolvable => unavailable_blob(id, "unresolvable", 3, raw, output),
    }
}

fn unavailable_blob(
    id: ContentId,
    status: &str,
    code: u8,
    raw: bool,
    output: &mut Output,
) -> Result<()> {
    if !raw {
        output.emit(&json!({"id":id,"status":status}))?;
    }
    Err(Failure::new(code, format!("content is {status}")))
}

pub(super) fn integrity(chain: &Path, output: &mut Output) -> Result<()> {
    require_chain(chain)?;
    let index = editchain_index::ChainIndex::open(chain)?;
    let report = index.verify_integrity()?;
    let issues: Vec<_> = report
        .content_issues
        .iter()
        .map(|issue| {
            json!({
                "operation":issue.operation, "location":issue.location,
                "reference":issue.reference, "state":issue.state,
            })
        })
        .collect();
    output.emit(&json!({
        "clean":report.is_clean(), "chain":report.chain, "index_matches":report.index_matches,
        "index_error":report.index_error, "content_issues":issues,
    }))?;
    if report.is_clean() {
        Ok(())
    } else {
        Err(Failure::new(
            4,
            "chain integrity check found gaps or conflicts",
        ))
    }
}
