//! Incremental reads are checked against canonical full replay.

use blake3 as _;
use crc as _;
use postcard as _;
use proptest as _;
use serde as _;

use std::io::{self, Write as _};

use editchain_core::{
    ActorId, Clock, MessageOp, NodeId, Op, OpId, OpKind, ParentSet, Payload, ScopeRef, Tags,
};
use editchain_store::format::{encode_op, encode_page, Page};
use editchain_store::{CanonicalChain, CanonicalTail, SegmentStore};

fn check(condition: bool, message: &str) -> io::Result<()> {
    if condition {
        Ok(())
    } else {
        Err(io::Error::other(message))
    }
}

fn equal<T: std::fmt::Debug + PartialEq + Copy>(
    actual: T,
    expected: T,
    message: &str,
) -> io::Result<()> {
    if actual == expected {
        Ok(())
    } else {
        Err(io::Error::other(format!(
            "{message}: expected {expected:?}, got {actual:?}"
        )))
    }
}

fn message(seq: u64, text: &str) -> Op {
    Op {
        id: OpId::new(NodeId(1), 0, seq),
        parents: ParentSet::None,
        actor: ActorId(1),
        clock: Clock::UnixMs(seq),
        scope: ScopeRef::None,
        tags: Tags::MESSAGE,
        kind: OpKind::Message(MessageOp {
            content: Payload::Inline(text.as_bytes().to_vec()),
            content_type: Payload::Empty,
        }),
    }
}

fn page(ops: &[Op]) -> io::Result<Page> {
    let mut page = Page::new(0);
    for op in ops {
        page.add_record(0, encode_op(op).map_err(io::Error::other)?);
    }
    Ok(page)
}

#[test]
fn one_append_reads_only_one_record_at_different_history_sizes() -> io::Result<()> {
    let mut work = Vec::new();
    for size in [1, 10_000] {
        let dir = tempfile::tempdir()?;
        let mut store = SegmentStore::open(dir.path())?;
        store.append_page(&page(
            &(0..size).map(|seq| message(seq, "old")).collect::<Vec<_>>(),
        )?)?;
        let mut tail = CanonicalTail::open(dir.path())?;
        let op = message(20_000, "one new operation");
        store.append_page(&page(std::slice::from_ref(&op))?)?;
        let delta = tail.poll()?;
        equal(delta.added.len(), 1, "exactly one operation is added")?;
        equal(
            delta.added.get(&op.id).map(|(op, _)| op),
            Some(&op),
            "the appended value is retained",
        )?;
        equal(
            delta.work.records_decoded,
            1,
            "sealed history is never decoded again",
        )?;
        work.push(delta.work.bytes_read);
        equal(
            tail.poll()?.work.bytes_read,
            0,
            "idle polls read no payload bytes",
        )?;
    }
    equal(
        work.first(),
        work.last(),
        "per-append IO is independent of retained history size",
    )?;
    Ok(())
}

#[test]
fn partial_records_replays_conflicts_and_new_segments_match_full_replay() -> io::Result<()> {
    let dir = tempfile::tempdir()?;
    let one = message(1, "first");
    let bytes = encode_page(&page(std::slice::from_ref(&one))?).map_err(io::Error::other)?;
    let boundary = bytes.len().saturating_sub(1);
    let path = dir.path().join("000000.eclog");
    std::fs::write(
        &path,
        bytes
            .get(..boundary)
            .ok_or_else(|| io::Error::other("prefix"))?,
    )?;
    let mut tail = CanonicalTail::open(dir.path())?;
    check(
        tail.chain().get(one.id).is_none(),
        "an incomplete record is not admitted",
    )?;
    equal(
        tail.chain().stats().incomplete_tails,
        1,
        "pending tail is diagnosed once",
    )?;
    std::fs::OpenOptions::new()
        .append(true)
        .open(&path)?
        .write_all(
            bytes
                .get(boundary..)
                .ok_or_else(|| io::Error::other("suffix"))?,
        )?;
    equal(
        tail.poll()?.added.len(),
        1,
        "completion admits the record once",
    )?;
    equal(
        tail.chain().stats().incomplete_tails,
        0,
        "completion clears the pending diagnostic",
    )?;
    let mut store = SegmentStore::open(dir.path())?;
    let conflicting = message(1, "conflict");
    store.append_page(&page(&[
        one.clone(),
        conflicting.clone(),
        conflicting,
        message(2, "stable"),
    ])?)?;
    let delta = tail.poll()?;
    check(
        delta.removed.contains(&one.id),
        "conflict retracts the earlier identity",
    )?;
    equal(
        delta.added.len(),
        1,
        "only the unconflicted identity is added",
    )?;
    let full = CanonicalChain::read(dir.path())?;
    equal(
        tail.chain().evidence(),
        full.evidence(),
        "retained evidence matches complete replay",
    )?;
    equal(
        tail.chain().stats().quarantined,
        full.stats().quarantined,
        "quarantine counts match replay",
    )?;
    equal(
        tail.chain().stats().duplicates,
        full.stats().duplicates,
        "duplicates remain idempotent",
    )?;
    std::fs::write(dir.path().join("000001.eclog"), b"EC02")?;
    check(
        tail.poll().is_err(),
        "truncation requires explicit recovery",
    )?;
    Ok(())
}
