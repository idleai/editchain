//! Replication order is observable by an already open native History view.

use super::{drain, seed, session, start, EncodedRecord};
use crate::tests::operation;
use crate::{RecordKey, Replica};
use editchain_core::{
    CommandOp, CommandStage, ImportOp, NodeId, Op, OpId, OpKind, ParentSet, Payload, ScopeRef,
    SessionId, Tags, TurnId,
};
use editchain_store::format::encode_op;
use std::io;

fn fixture(count: u64) -> io::Result<Vec<EncodedRecord>> {
    let mut records = Vec::new();
    for seq in 1..=count {
        let mut raw = operation(seq << 16, Payload::Empty);
        raw.source.as_mut().unwrap().node = NodeId(9000);
        raw.id = raw.source.unwrap().id();
        raw.scope = ScopeRef::Session(SessionId(73));
        raw.parents = if seq == 1 {
            ParentSet::None
        } else {
            ParentSet::One(OpId::new(
                NodeId(9000),
                raw.source.unwrap().boot,
                seq.saturating_sub(1) << 16,
            ))
        };
        raw.kind = OpKind::Import(ImportOp {
            raw_ref: Payload::Inline(br#"{"type":"assistant"}"#.to_vec()),
            raw_hash: None,
        });
        raw.tags = Tags::IMPORT;
        let result = Op {
            source: Some(editchain_core::SourceId::new(
                NodeId(1),
                raw.source.unwrap().boot,
                raw.source.unwrap().seq.saturating_add(1),
            )),
            id: OpId::new(
                NodeId(1),
                raw.source.unwrap().boot,
                raw.source.unwrap().seq.saturating_add(1),
            ),
            parents: ParentSet::One(raw.id),
            scope: ScopeRef::Turn(TurnId(15)),
            tags: Tags::AGENT | Tags::COMMAND,
            kind: OpKind::Command(CommandOp {
                command_id: Payload::Inline(format!("command-{seq}").into_bytes()),
                content: Payload::Inline(format!("cargo test fixture-{seq}").into_bytes()),
                stage: CommandStage::Finish,
            }),
            ..raw.clone()
        };
        for op in [raw, result] {
            let bytes = encode_op(&op).map_err(io::Error::other)?;
            records.push((RecordKey::from_encoded(&bytes)?, bytes));
        }
    }
    Ok(records)
}

#[test]
fn ordering_never_pulls_an_excluded_ancestor_into_new_history_sharing() -> io::Result<()> {
    let tmp = tempfile::tempdir()?;
    let ar = tmp.path().join("a");
    let br = tmp.path().join("b");
    let old = fixture(2)?;
    seed(&ar, &old)?;
    let _scope = Replica::open(&ar, "space-1", false)?;
    let records = fixture(3)?;
    seed(
        &ar,
        records
            .get(4..)
            .ok_or_else(|| io::Error::other("fixture record count"))?,
    )?;
    let mut a = session(&ar)?;
    let mut b = session(&br)?;
    let mut queue = start(&a, &b);
    drain(&mut a, &mut b, &mut queue)?;
    let received = Replica::open(&br, "space-1", true)?.snapshot()?;
    check_eq!(
        received.len(),
        2,
        "only the new occurrence and its result may be shared"
    );
    check!(
        old.iter().all(|(key, _)| !received.contains(*key)),
        "ancestor ordering widened sharing consent"
    );
    Ok(())
}
