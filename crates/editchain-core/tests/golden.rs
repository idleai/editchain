//! Golden tests for editchain-core.
//!
//! These tests verify the core CRDT invariants:
//! - Round-trip encoding/decoding for all operation kinds
//! - Set-union merge with duplicate detection and quarantine
//! - Immutable file facts across lifecycle stages
//! - Concurrent operation merge determinism

#![expect(
    unused_crate_dependencies,
    reason = "Test file; dependencies used by library macros"
)]

use editchain_core::*;

fn encode(op: &Op) -> Vec<u8> {
    #[expect(
        clippy::expect_used,
        reason = "Test helper; panic on failure is acceptable"
    )]
    postcard::to_stdvec(op).expect("encode failed")
}

fn decode(bytes: &[u8]) -> Op {
    #[expect(
        clippy::expect_used,
        reason = "Test helper; panic on failure is acceptable"
    )]
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

#[expect(
    clippy::too_many_arguments,
    reason = "Test helper constructing a full Op; all fields are needed"
)]
const fn file_op(
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
        assert_eq!(*op, decoded, "round-trip failed for {name}");
    }
}

// ---------------------------------------------------------------------------
// Concurrent merge
// ---------------------------------------------------------------------------

#[test]
fn golden_concurrent_merge() {
    let mut state_a = OpSet::new();
    let mut state_b = OpSet::new();

    let a1 = msg_op(1, 0, 1, 100, b"from A");
    let a2 = msg_op(1, 0, 2, 200, b"also from A");
    let b1 = msg_op(2, 0, 1, 150, b"from B");

    let _: Admission = state_a.insert(a1.id, encode(&a1));
    let _: Admission = state_a.insert(a2.id, encode(&a2));
    let _: Admission = state_b.insert(b1.id, encode(&b1));

    let (accepted, duplicates, quarantined) = state_a.merge(&state_b);
    assert_eq!(accepted, 1);
    assert_eq!(duplicates, 0);
    assert_eq!(quarantined, 0);
    assert_eq!(state_a.len(), 3);
}

// ---------------------------------------------------------------------------
// Immutable file facts
// ---------------------------------------------------------------------------

#[test]
fn golden_file_facts_retain_each_stage_and_revision() {
    let path = PathId(42);
    let mut accepted = OpSet::new();
    let mut originals = Vec::new();
    for (wire_tag, stage) in [
        (0, FileStage::Observed),
        (1, FileStage::Proposed),
        (2, FileStage::Applied),
        (3, FileStage::Saved),
        (4, FileStage::Deleted),
    ] {
        assert_eq!(postcard::to_stdvec(&stage).unwrap(), vec![wire_tag]);
        let seq = u64::from(wire_tag).saturating_add(1);
        let mut op = file_op(
            1,
            0,
            seq,
            seq,
            path,
            Some(ContentId::Hash128([wire_tag; 16])),
        );
        if let OpKind::File(file) = &mut op.kind {
            file.stage = stage;
        }
        assert_eq!(accepted.insert(op.id, encode(&op)), Admission::Accepted);
        originals.push(op);
    }
    let decoded: Vec<_> = accepted.iter().map(|(_, bytes)| decode(bytes)).collect();
    assert_eq!(decoded, originals);
}

// ---------------------------------------------------------------------------
// Duplicate detection
// ---------------------------------------------------------------------------

#[test]
fn golden_duplicate_detection() {
    let mut opset = OpSet::new();
    let id = OpId::new(NodeId(1), 0, 1);

    assert_eq!(opset.insert(id, vec![1, 2, 3]), Admission::Accepted);
    assert_eq!(opset.insert(id, vec![1, 2, 3]), Admission::Duplicate);
    assert_eq!(opset.insert(id, vec![4, 5, 6]), Admission::Conflict);
    assert_eq!(opset.conflicts().count(), 1);
}
