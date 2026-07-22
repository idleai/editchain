//! State tests for `OpSet`, `CausalKey`, and reducers.

// Referenced by library derive macros; suppress unused-crate-dependencies lint.
use postcard as _;
use proptest as _;
use serde as _;

use editchain_core::{
    clock::Clock, op::*, parents::ParentSet, payload::Payload, scope::ScopeRef, tags::Tags,
    ActorId, CausalKey, ContentId, FileReducer, MessageReducer, NodeId, OpId, OpSet, PathId,
    Reducer,
};

#[test]
fn opset_insert_accepts_new() {
    let mut set = OpSet::new();
    let id = OpId::new(NodeId(1), 0, 1);
    assert!(set.insert(id, vec![1, 2, 3]).unwrap());
    assert!(set.contains(&id));
    assert_eq!(set.len(), 1);
}

#[test]
fn opset_insert_duplicate() {
    let mut set = OpSet::new();
    let id = OpId::new(NodeId(1), 0, 1);
    let _: Option<bool> = set.insert(id, vec![1, 2, 3]).ok();
    assert!(!set.insert(id, vec![1, 2, 3]).unwrap());
}

#[test]
fn opset_insert_quarantine() {
    let mut set = OpSet::new();
    let id = OpId::new(NodeId(1), 0, 1);
    let _: Option<bool> = set.insert(id, vec![1, 2, 3]).ok();
    let result = set.insert(id, vec![4, 5, 6]);
    assert!(result.is_err());
    assert_eq!(set.quarantined().len(), 1);
}

#[test]
fn opset_merge_counts() {
    let mut a = OpSet::new();
    let mut b = OpSet::new();

    let _: Option<bool> = a.insert(OpId::new(NodeId(1), 0, 1), vec![1]).ok();
    let _: Option<bool> = a.insert(OpId::new(NodeId(1), 0, 2), vec![2]).ok();
    let _: Option<bool> = b.insert(OpId::new(NodeId(1), 0, 2), vec![2]).ok(); // duplicate
    let _: Option<bool> = b.insert(OpId::new(NodeId(2), 0, 1), vec![3]).ok(); // new

    let (accepted, duplicates, quarantined) = a.merge(&b);
    assert_eq!(accepted, 1);
    assert_eq!(duplicates, 1);
    assert_eq!(quarantined, 0);
}

#[test]
fn causal_key_ordering() {
    let a = CausalKey {
        clock_val: 100,
        clock_sub: 0,
        node: 1,
        boot: 0,
        seq: 1,
    };
    let b = CausalKey {
        clock_val: 200,
        clock_sub: 0,
        node: 1,
        boot: 0,
        seq: 1,
    };
    assert!(a < b);

    let c = CausalKey {
        clock_val: 100,
        clock_sub: 0,
        node: 2,
        boot: 0,
        seq: 1,
    };
    assert!(a < c); // same clock, lower node wins
}

#[expect(
    clippy::indexing_slicing,
    reason = "Test helper; indexing known-length vec is safe"
)]
#[test]
fn message_reducer_orders_by_causal_key() {
    let mut reducer = MessageReducer::new();

    let op_b = Op {
        id: OpId::new(NodeId(2), 0, 1),
        parents: ParentSet::None,
        actor: ActorId(0),
        clock: Clock::UnixMs(200),
        scope: ScopeRef::None,
        tags: Tags::MESSAGE,
        kind: OpKind::Message(MessageOp {
            content: Payload::Inline(b"later".to_vec()),
            content_type: Payload::Empty,
        }),
    };

    let op_a = Op {
        id: OpId::new(NodeId(1), 0, 1),
        parents: ParentSet::None,
        actor: ActorId(0),
        clock: Clock::UnixMs(100),
        scope: ScopeRef::None,
        tags: Tags::MESSAGE,
        kind: OpKind::Message(MessageOp {
            content: Payload::Inline(b"earlier".to_vec()),
            content_type: Payload::Empty,
        }),
    };

    let _: Option<()> = reducer.reduce(&op_a).ok();
    let _: Option<()> = reducer.reduce(&op_b).ok();

    let view = reducer.into_view();
    assert_eq!(view.len(), 2);
    assert_eq!(view[0], op_a.id); // earlier clock first
    assert_eq!(view[1], op_b.id);
}

#[test]
fn file_reducer_latest_wins() {
    let mut reducer = FileReducer::new();
    let path = PathId(42);

    let earlier = Op {
        id: OpId::new(NodeId(1), 0, 1),
        parents: ParentSet::None,
        actor: ActorId(0),
        clock: Clock::UnixMs(100),
        scope: ScopeRef::File(path),
        tags: Tags::FILE,
        kind: OpKind::File(FileOp {
            path,
            stage: FileStage::Applied,
            base: None,
            after: Some(ContentId::Hash128([0; 16])),
            edit: FileEdit::None,
        }),
    };

    let later = Op {
        id: OpId::new(NodeId(1), 0, 2),
        parents: ParentSet::None,
        actor: ActorId(0),
        clock: Clock::UnixMs(200),
        scope: ScopeRef::File(path),
        tags: Tags::FILE,
        kind: OpKind::File(FileOp {
            path,
            stage: FileStage::Applied,
            base: None,
            after: Some(ContentId::Hash128([1; 16])),
            edit: FileEdit::None,
        }),
    };

    let _: Option<()> = reducer.reduce(&earlier).ok();
    let _: Option<()> = reducer.reduce(&later).ok();

    let view = reducer.into_view();
    let rev = view.get(&path).unwrap();
    assert_eq!(rev.op_id, later.id); // later clock wins
}
