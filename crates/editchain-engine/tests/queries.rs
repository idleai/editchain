//! Consumer-facing query contracts, independent of the viewer and live repositories.

mod query_cases;

use editchain_core as _;
use editchain_index as _;
use editchain_store as _;
use serde as _;

use editchain_engine::{
    queries::Lookup, ActorId, Admission, BlobRef, Clock, ContentId, Engine, MessageOp, Op, OpId,
    OpKind, ParentSet, Payload, ScopeRef, SessionId, Tags,
};

fn id(sequence: u64) -> OpId {
    let mut bytes = [0; 32];
    if let Some(suffix) = bytes.get_mut(24..) {
        suffix.copy_from_slice(&sequence.to_be_bytes());
    }
    OpId::from_bytes(bytes)
}

fn record(sequence: u64, kind: OpKind) -> Op {
    Op {
        source: None,
        id: id(sequence),
        parents: ParentSet::None,
        actor: ActorId(17),
        clock: Clock::None,
        scope: ScopeRef::Session(SessionId(23)),
        tags: Tags::SOURCE_TIME_UNKNOWN,
        kind,
    }
}

fn message(sequence: u64, content: Payload) -> Op {
    record(
        sequence,
        OpKind::Message(MessageOp {
            content,
            content_type: Payload::Empty,
        }),
    )
}

fn reference(bytes: &[u8]) -> std::io::Result<BlobRef> {
    Ok(BlobRef {
        id: ContentId::Hash256(*blake3::hash(bytes).as_bytes()),
        len: u32::try_from(bytes.len()).map_err(std::io::Error::other)?,
    })
}

fn append(engine: &Engine, records: &[Op]) -> std::io::Result<()> {
    for record in records {
        if engine.append(record)? != Admission::Accepted {
            return Err(std::io::Error::other("fixture operation was not accepted"));
        }
    }
    Ok(())
}

fn found<T>(lookup: Lookup<T>) -> std::io::Result<T> {
    match lookup {
        Lookup::Found(value) => Ok(value),
        Lookup::Missing | Lookup::Conflicted(_) => {
            Err(std::io::Error::other("expected an accepted query result"))
        }
    }
}
