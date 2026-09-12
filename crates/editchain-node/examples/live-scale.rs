//! Manual release benchmark: `cargo run --release -p editchain-node --example live-scale -- 1000000`.
use blake3 as _;
use clap as _;
use ctrlc as _;
use dirs as _;
use editchain_git as _;
use editchain_import as _;
use editchain_index as _;
use editchain_project as _;
use serde as _;
use tantivy as _;

use editchain_core::{
    ActorId, Clock, MessageOp, NodeId, Op, OpId, OpKind, ParentSet, Payload, ScopeRef, Tags,
};
use editchain_protocol::{Request, ResponseBody};
use editchain_store::{
    format::{encode_op, Page},
    SegmentStore,
};
use serde_json::{json, Value};
use std::{io::Write as _, time::Instant};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

fn operation(seq: u64) -> Op {
    Op {
        id: OpId::new(NodeId(1), 0, seq),
        parents: if seq > 1 {
            ParentSet::One(OpId::new(NodeId(1), 0, seq.saturating_sub(1)))
        } else {
            ParentSet::None
        },
        actor: ActorId(1),
        clock: Clock::UnixMs(seq),
        scope: ScopeRef::None,
        tags: Tags::MESSAGE,
        kind: OpKind::Message(MessageOp {
            content: Payload::Inline(b"scaling message".to_vec()),
            content_type: Payload::Empty,
        }),
    }
}

fn request(server: &mut editchain_node::Server, body: Value) -> Result<Value> {
    match server
        .handle(&Request {
            id: 1,
            body: serde_json::from_value(body)?,
        })?
        .body
    {
        ResponseBody::Ok(value) => Ok(value),
        ResponseBody::Error(error) => Err(error.into()),
    }
}

fn main() -> Result<()> {
    if std::env::args().nth(1).as_deref() == Some("--append") {
        return append_fixture();
    }
    let count = std::env::args()
        .nth(1)
        .map_or(Ok(1_000_000_u64), |value| value.parse())?;
    let directory = tempfile::tempdir()?;
    let chain = directory.path().join(".editchain");
    {
        let mut store = SegmentStore::open(&chain)?;
        let mut page = Page::new(0);
        for seq in 1..=count {
            page.add_record(0, encode_op(&operation(seq))?);
            if seq.rem_euclid(1000) == 0 {
                store.append_page(&page)?;
                page = Page::new(u32::try_from(seq)?);
            }
        }
        if count.rem_euclid(1000) != 0 {
            store.append_page(&page)?;
        }
    }
    let mut server = editchain_node::Server::new();
    let start = Instant::now();
    let opened = request(
        &mut server,
        json!({"OpenLive": {"workspace_path": directory.path(), "chain_dir": ".editchain"}}),
    )?;
    let bootstrap_ms = start.elapsed().as_millis();
    {
        let mut store = SegmentStore::open(&chain)?;
        let mut page = Page::new(u32::try_from(count)?);
        page.add_record(0, encode_op(&operation(count.saturating_add(1)))?);
        store.append_page(&page)?;
    }
    let start = Instant::now();
    let epoch = opened
        .get("live")
        .and_then(|live| live.get("epoch"))
        .ok_or("no live epoch")?;
    let update = request(
        &mut server,
        json!({"SyncLive": {"epoch": epoch, "after_revision": 0, "codex": null}}),
    )?;
    let delta_ms = start.elapsed().as_millis();
    let work = update.get("work").ok_or("no work counters")?;
    if work.get("chain_records").and_then(Value::as_u64) != Some(1)
        || work.get("presentation_ops").and_then(Value::as_u64) != Some(1)
    {
        return Err("an ordinary +1 update rebuilt history".into());
    }
    writeln!(
        std::io::stdout().lock(),
        "{}",
        json!({"operations_before": count, "rows_before": opened.get("nodes"), "bootstrap_ms": bootstrap_ms, "delta_ms": delta_ms,
        "delta_bytes": serde_json::to_vec(&update)?.len(), "work": work})
    )?;
    Ok(())
}

/// Append exactly one independent record to an explicitly owned scale fixture.
fn append_fixture() -> Result<()> {
    let chain = std::env::args().nth(2).ok_or("missing fixture chain")?;
    let seq = u64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_millis(),
    )?;
    let mut op = operation(seq);
    op.id = OpId::new(NodeId(0xed17_ca11), 0, seq);
    op.parents = ParentSet::None;
    let mut page = Page::new(0);
    page.add_record(0, encode_op(&op)?);
    SegmentStore::open(std::path::Path::new(&chain))?.append_page(&page)?;
    Ok(())
}
