//! Streaming contracts are independent of delivery order and display IDs.

use blake3 as _;
use postcard as _;
use proptest as _;
use serde as _;

use editchain_core::activity::{
    ContentUpdate, ItemId, Kind, Message, MessageKind, Operation, ReplayError, Stage, StreamState,
    UpdateMode,
};
use editchain_core::{OpId, Payload};

#[test]
fn json_content_is_text_with_lossless_binary_and_legacy_input() {
    for bytes in [
        b"Now fix the `from_str_radix` in loader.rs:".as_slice(),
        "é🦀\n\"\\\0".as_bytes(),
        b"",
        b"\xff\0\xfe",
    ] {
        let payload = Payload::Inline(bytes.to_vec());
        let json = serde_json::to_value(&payload).unwrap();
        match std::str::from_utf8(bytes) {
            Ok(text) => assert_eq!(json, serde_json::json!({"Inline": text})),
            Err(_) => assert_eq!(json, serde_json::json!({"Inline": bytes})),
        }
        assert_eq!(serde_json::from_value::<Payload>(json).unwrap(), payload);
        assert_eq!(
            serde_json::from_value::<Payload>(serde_json::json!({"Inline": bytes})).unwrap(),
            payload
        );
        let mut original_encoding = vec![1, u8::try_from(bytes.len()).unwrap()];
        original_encoding.extend_from_slice(bytes);
        assert_eq!(postcard::to_allocvec(&payload).unwrap(), original_encoding);
        assert_eq!(
            postcard::from_bytes::<Payload>(&original_encoding).unwrap(),
            payload
        );
    }
    assert_eq!(serde_json::to_value(Payload::Empty).unwrap(), "Empty");
    assert!(serde_json::from_value::<Payload>(serde_json::json!({"Inline": [256]})).is_err());
    assert!(serde_json::from_value::<Payload>(serde_json::json!({"Inline": [-1]})).is_err());
}

fn message(id: u8, previous: Option<u8>, mode: UpdateMode, text: &[u8]) -> Operation {
    Operation::new(
        OpId::from_bytes([id; 32]),
        ItemId::derive("message", b"one"),
        ItemId::derive("recorder", b"one"),
        Kind::Message(Message {
            category: MessageKind::Text,
            stage: Stage::Updated,
            audience: Vec::new(),
            coverage: None,
            outcome: None,
            blocks: vec![ContentUpdate {
                block: ItemId::derive("block", b"one"),
                position: Some(0),
                mode,
                previous: previous.map(|id| OpId::from_bytes([id; 32])),
                media_type: Payload::Inline(b"text/plain".to_vec()),
                content: Payload::Inline(text.to_vec()),
            }],
        }),
    )
}

fn inline(payload: &Payload) -> Option<Vec<u8>> {
    match payload {
        Payload::Inline(bytes) => Some(bytes.clone()),
        Payload::Empty => Some(Vec::new()),
        Payload::Blob(_) => None,
    }
}

#[test]
#[expect(
    clippy::panic_in_result_fn,
    reason = "Test assertions report contract failures; Result propagates setup errors"
)]
fn chunks_replay_after_reordered_and_duplicate_delivery() -> Result<(), Box<dyn std::error::Error>>
{
    let first = message(1, None, UpdateMode::Replace, b"hello");
    let second = message(2, Some(1), UpdateMode::Append, b" world");
    let mut state = StreamState::default();
    assert!(
        state.insert(second.clone())?,
        "first delivery inserts an event"
    );
    let partial = state
        .content(first.item, ItemId::derive("block", b"one"), None, inline)?
        .ok_or("missing partial content")?;
    assert!(!partial.complete, "a missing prefix must stay partial");
    assert_eq!(
        partial.bytes, b" world",
        "available suffix is retained exactly"
    );
    assert!(
        state.insert(first.clone())?,
        "late prefix inserts independently"
    );
    assert!(!state.insert(second)?, "identical retries are idempotent");
    let complete = state
        .content(first.item, ItemId::derive("block", b"one"), None, inline)?
        .ok_or("missing content")?;
    assert_eq!(
        complete.bytes, b"hello world",
        "source links order the content"
    );
    assert!(
        complete.complete,
        "replacement establishes a complete prefix"
    );
    Ok(())
}

#[test]
#[expect(
    clippy::panic_in_result_fn,
    reason = "Test assertions report contract failures; Result propagates setup errors"
)]
fn replacement_is_not_appended_and_branches_are_not_guessed(
) -> Result<(), Box<dyn std::error::Error>> {
    let first = message(1, None, UpdateMode::Replace, b"old");
    let second = message(2, Some(1), UpdateMode::Replace, b"new");
    let mut state = StreamState::default();
    let _inserted = state.insert(first.clone())?;
    let _inserted = state.insert(second)?;
    let content = state
        .content(first.item, ItemId::derive("block", b"one"), None, inline)?
        .ok_or("missing replacement")?;
    assert_eq!(
        content.bytes, b"new",
        "a whole snapshot replaces the earlier value"
    );
    let _inserted = state.insert(message(3, Some(1), UpdateMode::Append, b"branch"))?;
    assert!(
        matches!(
            state.content(first.item, ItemId::derive("block", b"one"), None, inline),
            Err(ReplayError::Ambiguous(_))
        ),
        "divergent heads need an explicit resolution"
    );
    Ok(())
}

#[test]
#[expect(
    clippy::panic_in_result_fn,
    reason = "Test assertions report contract failures; Result propagates setup errors"
)]
fn conflicting_delivery_and_cross_item_predecessors_fail() -> Result<(), Box<dyn std::error::Error>>
{
    let first = message(1, None, UpdateMode::Replace, b"original");
    let mut state = StreamState::default();
    let _inserted = state.insert(first.clone())?;
    assert!(
        matches!(
            state.insert(message(1, None, UpdateMode::Replace, b"changed")),
            Err(ReplayError::Conflict(_))
        ),
        "one event ID cannot change its content"
    );
    assert!(matches!(
        state.content(first.item, ItemId::derive("block", b"one"), None, inline),
        Err(ReplayError::Conflict(_))
    ));
    let mut state = StreamState::default();
    let _inserted = state.insert(first)?;
    let mut other = message(2, Some(1), UpdateMode::Append, b"suffix");
    other.item = ItemId::derive("message", b"other");
    let other_item = other.item;
    let _inserted = state.insert(other)?;
    assert!(
        matches!(
            state.content(other_item, ItemId::derive("block", b"one"), None, inline),
            Err(ReplayError::WrongPredecessor(_))
        ),
        "another message cannot supply this message's prefix"
    );
    Ok(())
}

#[test]
#[expect(
    clippy::panic_in_result_fn,
    reason = "Test assertions report contract failures; Result propagates setup errors"
)]
fn new_json_envelope_round_trips_without_legacy_wrapper() -> Result<(), Box<dyn std::error::Error>>
{
    let mut record = message(9, None, UpdateMode::Replace, b"snapshot");
    record.session = Some(ItemId::derive("session", b"one"));
    record.turn = Some(ItemId::derive("turn", b"one"));
    let op = record.clone().into_op()?;
    let value = serde_json::to_value(&op)?;
    assert!(
        value.get("item").is_some(),
        "machine output includes the logical identity"
    );
    assert!(
        value.get("source").is_none(),
        "modern JSON uses its own envelope"
    );
    let restored: editchain_core::Op = serde_json::from_value(value)?;
    assert_eq!(restored, op, "all direct references survive JSON input");
    assert_ne!(
        record.id, record.item.0,
        "event identity is separate from message identity"
    );
    Ok(())
}

#[test]
fn snapshots_use_one_recorders_explicit_sequence_and_never_timestamp_order() {
    let mut first = message(9, None, UpdateMode::Replace, b"old");
    first.sequence = Some(1);
    first.time_ms = Some(999);
    let mut last = message(2, None, UpdateMode::Replace, b"new");
    last.sequence = Some(2);
    last.time_ms = Some(1);
    let mut state = StreamState::default();
    let _inserted = state.insert(last.clone()).unwrap();
    let _inserted = state.insert(first).unwrap();
    let result = state
        .content(last.item, ItemId::derive("block", b"one"), None, inline)
        .unwrap()
        .unwrap();
    assert_eq!(result.bytes, b"new");
    last.id = OpId::from_bytes([7; 32]);
    last.recorder = ItemId::derive("recorder", b"other");
    let _inserted = state.insert(last.clone()).unwrap();
    assert!(matches!(
        state.content(last.item, ItemId::derive("block", b"one"), None, inline),
        Err(ReplayError::Ambiguous(_))
    ));
}

#[test]
fn applied_file_with_unavailable_patch_remains_an_explicit_observation() {
    use editchain_core::{
        ActorId, Clock, FileEdit, FileOp, FileStage, Op, OpKind, ParentSet, PathId, ScopeRef, Tags,
    };
    let old = Op {
        id: OpId::from_bytes([13; 32]),
        source: None,
        parents: ParentSet::None,
        actor: ActorId(1),
        clock: Clock::None,
        scope: ScopeRef::None,
        tags: Tags::FILE,
        kind: OpKind::File(FileOp {
            path: PathId(4),
            stage: FileStage::Applied,
            base: None,
            after: None,
            edit: FileEdit::None,
        }),
    };
    let current = Operation::upgrade(&old).unwrap();
    let _valid = current.clone().into_op().unwrap();
    assert!(matches!(current.kind, Kind::File(_)));
    let Kind::File(file) = current.kind else {
        return;
    };
    assert_eq!(
        file.change,
        Some(editchain_core::activity::ChangeState::Applied)
    );
    assert_eq!(file.edit, FileEdit::None);
    assert_eq!(file.after, None);
}

#[test]
fn outputless_completion_and_retries_keep_attempts_separate() {
    use editchain_core::activity::{Completion, OutputChannel, Status, Tool};
    let first = message(1, None, UpdateMode::Replace, b"output");
    assert!(matches!(first.kind, Kind::Message(_)));
    let Kind::Message(message) = first.kind else {
        return;
    };
    let attempt = ItemId::derive("attempt", b"first");
    let tool = Tool {
        native_call: Payload::Empty,
        name: Payload::Empty,
        stage: Stage::Updated,
        attempt,
        parent_call: None,
        arguments: Payload::Empty,
        channel: OutputChannel::Stdout,
        output: message.blocks.first().cloned(),
        terminal: None,
        outcome: None,
    };
    let mut record = Operation::new(
        first.id,
        first.item,
        first.recorder,
        Kind::Tool(tool.clone()),
    );
    let mut state = StreamState::default();
    let _inserted = state.insert(record.clone()).unwrap();
    let mut completion = tool.clone();
    completion.stage = Stage::Finished;
    completion.output = None;
    completion.outcome = Some(Completion {
        status: Status::Cancelled,
        detail: Payload::Empty,
    });
    record.id = OpId::from_bytes([2; 32]);
    record.kind = Kind::Tool(completion);
    let _inserted = state.insert(record.clone()).unwrap();
    let block = ItemId::derive("block", b"one");
    assert!(
        state
            .content(first.item, block, Some(attempt), inline)
            .unwrap()
            .unwrap()
            .finished
    );
    let mut retry = tool;
    retry.attempt = ItemId::derive("attempt", b"retry");
    let retry_attempt = retry.attempt;
    record.id = OpId::from_bytes([3; 32]);
    record.kind = Kind::Tool(retry);
    let _inserted = state.insert(record).unwrap();
    assert!(
        !state
            .content(first.item, block, Some(retry_attempt), inline)
            .unwrap()
            .unwrap()
            .finished
    );
}

#[test]
fn absent_payload_is_not_a_known_empty_replacement() {
    let mut record = message(1, None, UpdateMode::Replace, b"");
    let item = record.item;
    let mut state = StreamState::default();
    let _inserted = state.insert(record.clone()).unwrap();
    assert!(
        state
            .content(item, ItemId::derive("block", b"one"), None, inline)
            .unwrap()
            .unwrap()
            .complete
    );
    if let Kind::Message(message) = &mut record.kind {
        if let Some(block) = message.blocks.first_mut() {
            block.content = Payload::Empty;
        }
    }
    let mut missing = StreamState::default();
    let _inserted = missing.insert(record).unwrap();
    assert!(matches!(
        missing.content(item, ItemId::derive("block", b"one"), None, inline),
        Err(ReplayError::Unavailable(_))
    ));
}

#[test]
fn conflicting_streams_quarantine_both_orders_and_never_revive_on_retry() {
    let first = message(1, None, UpdateMode::Replace, b"first");
    let changed = message(1, None, UpdateMode::Replace, b"changed");
    let block = ItemId::derive("block", b"one");
    for variants in [
        [first.clone(), changed.clone()],
        [changed.clone(), first.clone()],
    ] {
        let mut state = StreamState::default();
        assert!(state.insert(variants.first().unwrap().clone()).unwrap());
        assert_eq!(
            state.insert(variants.last().unwrap().clone()),
            Err(ReplayError::Conflict(first.id))
        );
        for retry in [&first, &changed] {
            assert_eq!(
                state.insert(retry.clone()),
                Err(ReplayError::Conflict(first.id))
            );
            assert_eq!(
                state.content(first.item, block, None, inline),
                Err(ReplayError::Conflict(first.id))
            );
        }
        let mut other = message(2, None, UpdateMode::Replace, b"independent");
        other.item = ItemId::derive("message", b"other");
        assert!(state.insert(other.clone()).unwrap());
        assert_eq!(
            state
                .content(other.item, block, None, inline)
                .unwrap()
                .unwrap()
                .bytes,
            b"independent"
        );
        other.id = first.id;
        assert_eq!(
            state.insert(other.clone()),
            Err(ReplayError::Conflict(first.id))
        );
        assert_eq!(
            state.content(other.item, block, None, inline),
            Err(ReplayError::Conflict(first.id))
        );
    }
}

#[test]
fn replacements_and_appends_validate_late_cross_item_and_cross_block_predecessors() {
    for mode in [UpdateMode::Replace, UpdateMode::Append] {
        for cross_item in [false, true] {
            let mut first = message(1, None, UpdateMode::Replace, b"first");
            let second = message(2, Some(1), mode, b"second");
            if cross_item {
                first.item = ItemId::derive("message", b"other");
            } else if let Kind::Message(message) = &mut first.kind {
                message.blocks.first_mut().unwrap().block = ItemId::derive("block", b"other");
            }
            let mut state = StreamState::default();
            assert!(state.insert(second.clone()).unwrap());
            let block = ItemId::derive("block", b"one");
            let partial = state
                .content(second.item, block, None, inline)
                .unwrap()
                .unwrap();
            assert_eq!(partial.complete, mode == UpdateMode::Replace);
            assert!(state.insert(first.clone()).unwrap());
            assert_eq!(
                state.content(second.item, block, None, inline),
                Err(ReplayError::WrongPredecessor(first.id))
            );
        }
    }
}

fn tool_update(mut record: Operation, attempt: ItemId) -> Operation {
    use editchain_core::activity::{OutputChannel, Tool};
    if let Kind::Message(message) = record.kind {
        record.kind = Kind::Tool(Tool {
            native_call: Payload::Empty,
            name: Payload::Empty,
            stage: Stage::Updated,
            attempt,
            parent_call: None,
            arguments: Payload::Empty,
            channel: OutputChannel::Stdout,
            output: message.blocks.into_iter().next(),
            terminal: None,
            outcome: None,
        });
    }
    record
}

#[test]
fn tool_predecessors_and_conflicts_are_checked_per_attempt() {
    let attempt = ItemId::derive("attempt", b"one");
    let retry = ItemId::derive("attempt", b"retry");
    let first = tool_update(message(1, None, UpdateMode::Replace, b"first"), attempt);
    let block = ItemId::derive("block", b"one");
    for mode in [UpdateMode::Replace, UpdateMode::Append] {
        let other = tool_update(message(2, Some(1), mode, b"other"), retry);
        let mut state = StreamState::default();
        assert!(state.insert(first.clone()).unwrap());
        assert!(state.insert(other.clone()).unwrap());
        assert_eq!(
            state.content(other.item, block, Some(retry), inline),
            Err(ReplayError::WrongPredecessor(first.id))
        );
    }
    let mut state = StreamState::default();
    assert!(state.insert(first.clone()).unwrap());
    let conflict = tool_update(message(1, None, UpdateMode::Replace, b"changed"), attempt);
    assert_eq!(state.insert(conflict), Err(ReplayError::Conflict(first.id)));
    let other = tool_update(message(2, None, UpdateMode::Replace, b"retry"), retry);
    assert!(state.insert(other.clone()).unwrap());
    assert!(
        state
            .content(other.item, block, Some(retry), inline)
            .unwrap()
            .unwrap()
            .complete
    );
    assert_eq!(
        state.content(first.item, block, Some(attempt), inline),
        Err(ReplayError::Conflict(first.id))
    );
}

#[test]
fn replacements_check_cycles_without_resolving_superseded_payloads() {
    let mut first = message(1, None, UpdateMode::Replace, b"first");
    if let Kind::Message(message) = &mut first.kind {
        message.blocks.first_mut().unwrap().content = Payload::Empty;
    }
    let second = message(2, Some(1), UpdateMode::Replace, b"second");
    let mut state = StreamState::default();
    assert!(state.insert(first).unwrap());
    assert!(state.insert(second.clone()).unwrap());
    let block = ItemId::derive("block", b"one");
    assert_eq!(
        state
            .content(second.item, block, None, inline)
            .unwrap()
            .unwrap()
            .bytes,
        b"second"
    );
    let mut cycle = StreamState::default();
    for record in [
        message(1, Some(2), UpdateMode::Replace, b"first"),
        second.clone(),
        message(3, Some(1), UpdateMode::Replace, b"head"),
    ] {
        assert!(cycle.insert(record).unwrap());
    }
    assert!(matches!(
        cycle.content(second.item, block, None, inline),
        Err(ReplayError::Cycle(_))
    ));
}
