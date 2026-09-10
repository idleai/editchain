//! State tests for `OpSet`, `CausalKey`, and reducers.

// Referenced by library derive macros; suppress unused-crate-dependencies lint.
use postcard as _;
use proptest as _;
use serde as _;

use editchain_core::{
    clock::Clock, op::*, parents::ParentSet, payload::Payload, scope::ScopeRef, tags::Tags,
    ActorId, Admission, CausalKey, ContentId, FileReducer, MessageReducer, NodeId, OpId, OpSet,
    PathId, Reducer,
};

#[test]
fn opset_insert_accepts_new() {
    let mut set = OpSet::new();
    let id = OpId::new(NodeId(1), 0, 1);
    assert_eq!(set.insert(id, vec![1, 2, 3]), Admission::Accepted);
    assert!(set.contains(&id));
    assert_eq!(set.len(), 1);
}

#[test]
fn opset_insert_duplicate() {
    let mut set = OpSet::new();
    let id = OpId::new(NodeId(1), 0, 1);
    let _: Admission = set.insert(id, vec![1, 2, 3]);
    assert_eq!(set.insert(id, vec![1, 2, 3]), Admission::Duplicate);
}

#[test]
fn opset_insert_quarantine() {
    let mut set = OpSet::new();
    let id = OpId::new(NodeId(1), 0, 1);
    let _: Admission = set.insert(id, vec![1, 2, 3]);
    let result = set.insert(id, vec![4, 5, 6]);
    assert_eq!(result, Admission::Conflict);
    assert!(!set.contains(&id));
    assert!(set.is_empty());
    assert_eq!(set.evidence().count(), 2);
    assert_eq!(set.conflicts().count(), 1);
}

#[test]
fn opset_merge_counts() {
    let mut a = OpSet::new();
    let mut b = OpSet::new();

    let _: Admission = a.insert(OpId::new(NodeId(1), 0, 1), vec![1]);
    let _: Admission = a.insert(OpId::new(NodeId(1), 0, 2), vec![2]);
    let _: Admission = b.insert(OpId::new(NodeId(1), 0, 2), vec![2]); // duplicate
    let _: Admission = b.insert(OpId::new(NodeId(2), 0, 1), vec![3]); // new

    let (accepted, duplicates, quarantined) = a.merge(&b);
    assert_eq!(accepted, 1);
    assert_eq!(duplicates, 1);
    assert_eq!(quarantined, 0);
}

#[test]
fn conflict_evidence_survives_permutations_empty_merges_and_replays() {
    let id = OpId::new(NodeId(1), 0, 1);
    let permutations = [
        [1, 2, 3],
        [1, 3, 2],
        [2, 1, 3],
        [2, 3, 1],
        [3, 1, 2],
        [3, 2, 1],
    ];
    for versions in permutations {
        let mut source = OpSet::new();
        for version in versions {
            let _: Admission = source.insert(id, vec![version]);
        }
        let mut replica = OpSet::new();
        let _: (usize, usize, usize) = replica.merge(&source);
        assert_eq!(replica, source);
        assert!(replica.is_empty());
        assert_eq!(
            replica
                .evidence()
                .map(|(_, bytes)| bytes)
                .collect::<Vec<_>>(),
            vec![&[1][..], &[2][..], &[3][..]]
        );
        for version in versions {
            assert_eq!(replica.insert(id, vec![version]), Admission::Duplicate);
        }
        assert_eq!(replica, source);
    }
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
