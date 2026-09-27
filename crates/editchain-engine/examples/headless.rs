//! Record every shared record family without a viewer or host service.

use std::io::{self, Write as _};

use blake3 as _;
use editchain_core as _;
use editchain_index as _;
use editchain_store as _;
use serde as _;
use serde_json as _;
use tempfile as _;

use editchain_engine::records::{
    ActorRecord, AnnotationRecord, ChainRecord, OperationRecord, ReflectionRecord, RevisionRecord,
    SessionRecord,
};
use editchain_engine::{
    ActorId, ChainId, Clock, Engine, FileEdit, FileStage, Frontier, FrontierSet, NodeId,
    NoteRelationship, OpId, OpKind, ParentSet, PathId, Payload, ScopeRef, SessionId, Tags,
    WindowRef,
};

fn record(sequence: u64, scope: ScopeRef, kind: OpKind) -> OperationRecord {
    OperationRecord {
        id: OpId::new(NodeId(1), 0, sequence),
        parents: sequence.checked_sub(1).map_or(ParentSet::None, |parent| {
            ParentSet::One(OpId::new(NodeId(1), 0, parent))
        }),
        actor: ActorId(1),
        clock: Clock::None,
        scope,
        tags: Tags::NONE,
        kind,
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = std::env::args_os().nth(1).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "usage: headless <chain-directory>",
        )
    })?;
    let engine = Engine::open(std::path::PathBuf::from(path))?;
    let content = engine.store_blob(b"exact revision bytes\0\xff\r\n")?;
    let session = ScopeRef::Session(SessionId(1));
    let operations = [
        record(
            0,
            ScopeRef::Chain(ChainId(1)),
            OpKind::ChainStart(ChainRecord {
                name: b"headless example".to_vec(),
                version: 1,
            }),
        ),
        record(
            1,
            ScopeRef::Chain(ChainId(1)),
            OpKind::Actor(ActorRecord {
                label: Payload::Inline(b"producer".to_vec()),
                role: Payload::Inline(b"agent".to_vec()),
            }),
        ),
        record(
            2,
            session,
            OpKind::Session(SessionRecord {
                id: SessionId(1),
                parent: None,
                label: Payload::Inline(b"native session".to_vec()),
                metadata: Payload::Inline(b"opaque producer evidence".to_vec()),
            }),
        ),
        record(
            3,
            session,
            OpKind::File(RevisionRecord {
                path: PathId(1),
                stage: FileStage::Applied,
                base: None,
                after: Some(content.id),
                edit: FileEdit::Blob(content),
            }),
        ),
        // Identical contents retain a distinct saved revision occurrence.
        record(
            4,
            session,
            OpKind::File(RevisionRecord {
                path: PathId(1),
                stage: FileStage::Saved,
                base: Some(content.id),
                after: Some(content.id),
                edit: FileEdit::None,
            }),
        ),
        record(
            5,
            session,
            OpKind::Note(AnnotationRecord {
                target_ids: vec![OpId::new(NodeId(1), 0, 3)],
                relationship: NoteRelationship::Explains,
                content: Payload::Inline(b"consumer-defined annotation".to_vec()),
            }),
        ),
        record(
            6,
            session,
            OpKind::Reflection(ReflectionRecord {
                scope: session,
                covers: FrontierSet(vec![Frontier {
                    node: NodeId(1),
                    boot: 0,
                    max_seq: 5,
                }]),
                window: WindowRef {
                    start_seq: 0,
                    end_seq: 6,
                },
                summary: Payload::Inline(b"recorded summary".to_vec()),
                anchors: Payload::Inline(b"opaque evidence anchors".to_vec()),
            }),
        ),
    ];
    for operation in &operations {
        let _admission = engine.append(operation)?;
    }
    let snapshot = engine.snapshot()?;
    writeln!(
        io::stdout().lock(),
        "accepted={} conflicts={}",
        snapshot.stats().accepted,
        snapshot.evidence().conflicts().count()
    )?;
    Ok(())
}
