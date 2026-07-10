//! Golden tests for editchain-core.
//!
//! These tests verify the core CRDT invariants:
//! - Round-trip encoding/decoding for all operation kinds
//! - Set-union merge with duplicate detection and quarantine
//! - Deterministic causal ordering
//! - File revision register (latest materializing revision wins)
//! - Concurrent operation merge determinism

use editchain_core::*;

fn encode(op: &Op) -> Vec<u8> {
    postcard::to_stdvec(op).expect("encode failed")
}

fn decode(bytes: &[u8]) -> Op {
    postcard::from_bytes(bytes).expect("decode failed")
}

fn msg_op(node: u64, boot: u32, seq: u64, ms: u64, text: &[u8]) -> Op {
    Op {
        id: OpId::new(NodeId(node), boot, seq),
        parents: ParentSet::None,
        actor: ActorId(1),
        clock: Clock::UnixMs(ms),
        scope: ScopeRef::None,
        tags: Tags(1 << 3), // MESSAGE
        kind: OpKind::Message(MessageOp {
            content: Payload::Inline(text.to_vec()),
            content_type: Payload::Empty,
        }),
    }
}

fn file_op(
    node: u64,
    boot: u32,
    seq: u64,
    ms: u64,
    path: PathId,
    after: Option<ContentId>,
) -> Op {
    Op {
        id: OpId::new(NodeId(node), boot, seq),
        parents: ParentSet::None,
        actor: ActorId(0),
        clock: Clock::UnixMs(ms),
        scope: ScopeRef::File(path),
        tags: Tags(1 << 2), // FILE
        kind: OpKind::File(FileOp {
            path,
            stage: FileStage::Applied,
            base: None,
            after,
            edit: FileEdit::None,
        }),
    }
}

// ---------------------------------------------------------------------------
// Helper to build a CausalKey compactly
// ---------------------------------------------------------------------------

fn ck(clock_val: u64, clock_sub: u16, node: u64, boot: u32, seq: u64) -> CausalKey {
    CausalKey { clock_val, clock_sub, node, boot, seq }
}

// ---------------------------------------------------------------------------
// Round-trip all operation kinds
// ---------------------------------------------------------------------------

#[test]
fn golden_round_trip_all_kinds() {
    let ops = [
        (
            "ChainStart",
            Op {
                id: OpId::new(NodeId(0), 0, 0),
                parents: ParentSet::None,
                actor: ActorId(0),
                clock: Clock::None,
                scope: ScopeRef::None,
                tags: Tags(0),
                kind: OpKind::ChainStart(ChainStart {
                    name: b"test-chain".to_vec(),
                    version: 1,
                }),
            },
        ),
        (
            "Actor",
            Op {
                id: OpId::new(NodeId(1), 0, 1),
                parents: ParentSet::None,
                actor: ActorId(1),
                clock: Clock::Lamport(42),
                scope: ScopeRef::None,
                tags: Tags(1 << 0), // AGENT
                kind: OpKind::Actor(ActorOp {
                    label: Payload::Inline(b"test-agent".to_vec()),
                    role: Payload::Inline(b"assistant".to_vec()),
                }),
            },
        ),
        ("Message", msg_op(1, 0, 2, 1000, b"hello world")),
        (
            "Tool",
            Op {
                id: OpId::new(NodeId(1), 0, 3),
                parents: ParentSet::None,
                actor: ActorId(0),
                clock: Clock::UnixMs(2000),
                scope: ScopeRef::None,
                tags: Tags(1 << 4), // TOOL
                kind: OpKind::Tool(ToolOp {
                    tool_call_id: Payload::Inline(b"call_123".to_vec()),
                    tool_name: Payload::Inline(b"bash".to_vec()),
                    stage: ToolStage::Start,
                    content: Payload::Inline(b"ls -la".to_vec()),
                }),
            },
        ),
        (
            "Command",
            Op {
                id: OpId::new(NodeId(1), 0, 4),
                parents: ParentSet::None,
                actor: ActorId(0),
                clock: Clock::UnixMs(3000),
                scope: ScopeRef::None,
                tags: Tags(1 << 5), // COMMAND
                kind: OpKind::Command(CommandOp {
                    command_id: Payload::Inline(b"cmd_1".to_vec()),
                    content: Payload::Inline(b"echo hello".to_vec()),
                    stage: CommandStage::Start,
                }),
            },
        ),
        (
            "File",
            Op {
                id: OpId::new(NodeId(1), 0, 5),
                parents: ParentSet::None,
                actor: ActorId(0),
                clock: Clock::UnixMs(4000),
                scope: ScopeRef::File(PathId(42)),
                tags: Tags(1 << 2), // FILE
                kind: OpKind::File(FileOp {
                    path: PathId(42),
                    stage: FileStage::Applied,
                    base: None,
                    after: Some(ContentId::Hash128([0xAA; 16])),
                    edit: FileEdit::None,
                }),
            },
        ),
    ];

    for (name, op) in &ops {
        let encoded = encode(op);
        let decoded = decode(&encoded);
        assert_eq!(*op, decoded, "round-trip failed for {}", name);
    }
}

// ---------------------------------------------------------------------------
// Concurrent merge
// ---------------------------------------------------------------------------

#[test]
fn golden_concurrent_merge() {
    let mut state_a = ChainState::new();
    let mut state_b = ChainState::new();

    let a1 = msg_op(1, 0, 1, 100, b"from A");
    let a2 = msg_op(1, 0, 2, 200, b"also from A");
    let b1 = msg_op(2, 0, 1, 150, b"from B");

    state_a.ops.insert(a1.id, encode(&a1)).unwrap();
    state_a.ops.insert(a2.id, encode(&a2)).unwrap();
    state_b.ops.insert(b1.id, encode(&b1)).unwrap();

    let (accepted, duplicates, quarantined) = state_a.ops.merge(&state_b.ops);
    assert_eq!(accepted, 1);
    assert_eq!(duplicates, 0);
    assert_eq!(quarantined, 0);
    assert_eq!(state_a.ops.len(), 3);
}

// ---------------------------------------------------------------------------
// File revision register
// ---------------------------------------------------------------------------

#[test]
fn golden_file_revision_register() {
    let path = PathId(42);
    let cid_a = ContentId::Hash128([0xAA; 16]);
    let cid_b = ContentId::Hash128([0xBB; 16]);

    let earlier = file_op(1, 0, 1, 100, path, Some(cid_a));
    let later = file_op(1, 0, 2, 200, path, Some(cid_b));

    let mut reducer = FileReducer::new();
    reducer.reduce(&earlier).unwrap();
    reducer.reduce(&later).unwrap();

    let view = reducer.into_view();
    let rev = view.get(&path).unwrap();
    assert_eq!(rev.op_id.seq, 2);
}

// ---------------------------------------------------------------------------
// Duplicate detection
// ---------------------------------------------------------------------------

#[test]
fn golden_duplicate_detection() {
    let mut opset = OpSet::new();
    let id = OpId::new(NodeId(1), 0, 1);

    assert!(opset.insert(id, vec![1, 2, 3]).unwrap());
    assert!(!opset.insert(id, vec![1, 2, 3]).unwrap());
    assert!(opset.insert(id, vec![4, 5, 6]).is_err());
    assert_eq!(opset.quarantined().len(), 1);
}

// ---------------------------------------------------------------------------
// Causal ordering
// ---------------------------------------------------------------------------

#[test]
fn golden_causal_ordering() {
    assert!(ck(100, 0, 1, 0, 1) < ck(200, 0, 1, 0, 1));
    assert!(ck(100, 0, 1, 0, 1) < ck(100, 0, 2, 0, 1));
    assert!(ck(100, 0, 1, 0, 1) < ck(100, 0, 1, 0, 2));
}