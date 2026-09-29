use super::*;
use editchain_core::activity::Operation;
use editchain_core::{ActorId, Clock, MessageOp, OpKind, ParentSet, Payload, ScopeRef, Tags};
use editchain_import::batch::DurableOpSink as _;
use editchain_protocol::rank::Measure;

fn message(sequence: u64) -> Operation {
    Operation::upgrade(&Op {
        source: None,
        id: OpId::new(NodeId(1), 0, sequence),
        parents: ParentSet::None,
        actor: ActorId(1),
        clock: Clock::UnixMs(sequence),
        scope: ScopeRef::None,
        tags: Tags::MESSAGE,
        kind: OpKind::Message(MessageOp {
            content: Payload::Inline(b"message".to_vec()),
            content_type: Payload::Empty,
        }),
    })
    .unwrap()
}

#[test]
fn version_fourteen_refreshes_cached_activity_parents_without_changing_the_chain() {
    let root = tempfile::tempdir().unwrap();
    let parents: Vec<_> = (1..=3).map(|n| message(n).into_op().unwrap()).collect();
    let mut child = message(4);
    child.parents = parents.iter().map(|op| op.id).collect();
    let child = child.into_op().unwrap();
    let mut ops = parents;
    ops.push(child.clone());
    let mut log = editchain_store::LogStore::new(
        editchain_store::SegmentStore::open(root.path().join(".editchain")).unwrap(),
    );
    let _admission = log.append_durable(&ops).unwrap();
    drop(log);
    let canonical = human_edits::canonical(root.path());
    let mut workspace = LiveWorkspace::open_paged(&request(root.path())).unwrap();
    let key = format!("op:{}", child.id);
    let order = workspace.orders.get(&key).unwrap().clone();
    let mut block = workspace.blocks.get(&order).unwrap().clone();
    let expected = block.meta.parents.clone();
    assert_eq!(expected.len(), 3);
    block.meta.parents.truncate(2);
    drop(workspace.blocks.insert(
        order,
        block.clone(),
        Measure {
            expanded: block.meta.row_count,
            visible: block.meta.row_count,
        },
    ));
    workspace.rows.flush().unwrap();
    let mut saved = workspace.saved();
    saved.version = 14;
    drop(
        workspace
            .checkpoint_store
            .commit::<_, Saved>(&saved)
            .unwrap(),
    );
    drop(workspace);
    for _ in 0..2 {
        let reopened = LiveWorkspace::open_paged(&request(root.path())).unwrap();
        assert!(reopened.reused_checkpoint);
        let order = reopened.orders.get(&key).unwrap();
        assert_eq!(reopened.blocks.get(order).unwrap().meta.parents, expected);
        assert_eq!(
            load(&reopened.checkpoint_store).unwrap().unwrap().version,
            VERSION
        );
        assert_eq!(human_edits::canonical(root.path()), canonical);
    }
}
