use super::{live_request, write_page_sequence};
use editchain_core::{
    ActorId, Clock, CommandOp, CommandStage, ImportOp, NodeId, Op, OpId, OpKind, ParentSet,
    Payload, ScopeRef, SessionId, Tags, TurnId,
};
use editchain_node::Server;
use editchain_store::format::{encode_op, Page};
use serde_json::{json, Value};
use std::path::Path;

fn source(seq: u64) -> Op {
    Op {
        id: OpId::new(NodeId(90), 0, seq << 16),
        parents: if seq == 1 {
            ParentSet::None
        } else {
            ParentSet::One(OpId::new(NodeId(90), 0, (seq - 1) << 16))
        },
        actor: ActorId(1),
        clock: Clock::UnixMs(1000 * seq),
        scope: ScopeRef::Session(SessionId(73)),
        tags: Tags::IMPORT,
        kind: OpKind::Import(ImportOp {
            raw_ref: Payload::Inline(
                br#"{"type":"response_item","payload":{"type":"function_call_output"}}"#.to_vec(),
            ),
            raw_hash: None,
        }),
    }
}

fn command(source: &Op, node: u64) -> Op {
    Op {
        id: OpId::new(NodeId(node), 0, source.id.seq + 1),
        parents: ParentSet::One(source.id),
        scope: ScopeRef::Turn(TurnId(15)),
        tags: Tags::AGENT | Tags::COMMAND,
        kind: OpKind::Command(CommandOp {
            command_id: Payload::Inline(format!("command-{node}").into_bytes()),
            content: Payload::Inline(format!("cargo test --package fixture-{node}").into_bytes()),
            stage: CommandStage::Finish,
        }),
        ..source.clone()
    }
}

fn deliver(root: &Path, sequence: u32, ops: &[Op]) {
    let mut page = Page::new(0);
    for op in ops {
        page.add_record(0, encode_op(op).expect("valid peer context fixture"));
    }
    write_page_sequence(&root.join(".editchain"), sequence, &page);
}

#[test]
fn peer_command_results_rejoin_their_late_source_chain_in_an_open_view() {
    let tmp = tempfile::tempdir().expect("valid peer context fixture");
    let mut server = Server::new();
    let opened = live_request(
        &mut server,
        json!({"OpenLive": {
            "workspace_path": tmp.path(), "chain_dir": ".editchain"
        }}),
    );
    let sources = [source(1), source(2)];
    let commands = [command(&sources[0], 1), command(&sources[1], 2)];
    let sync = |server: &mut Server, revision: u64| {
        live_request(
            server,
            json!({"SyncLive": {
                "epoch": opened["live"]["epoch"], "after_revision": revision, "codex": null
            }}),
        )
    };
    deliver(tmp.path(), 0, &commands);
    let pending = sync(&mut server, 0);
    assert_eq!(pending["deltas"][0]["visible_total"], 2);
    assert!(pending["deltas"][0]["upserts"]
        .as_array()
        .expect("valid peer context fixture")
        .iter()
        .all(|block| block["rows"][0]["group"] == "ops"));
    deliver(tmp.path(), 1, &sources);
    let repaired = sync(
        &mut server,
        pending["revision"]
            .as_u64()
            .expect("valid peer context fixture"),
    );
    let blocks: Vec<&Value> = repaired["deltas"]
        .as_array()
        .expect("valid peer context fixture")
        .iter()
        .flat_map(|delta| {
            delta["upserts"]
                .as_array()
                .expect("valid peer context fixture")
        })
        .collect();
    assert!(
        blocks
            .iter()
            .any(|block| block["meta"]["parents"] == json!([format!("record:{}", sources[0].id)])),
        "{repaired}"
    );
    assert_eq!(
        repaired["deltas"][0]["removed"]
            .as_array()
            .expect("valid peer context fixture")
            .len(),
        2
    );
    assert!(blocks
        .iter()
        .all(|block| block["rows"][0]["group"] == "session:73"));
    drop(server);
    let reopened = live_request(
        &mut Server::new(),
        json!({"OpenLive": {
            "workspace_path": tmp.path(), "chain_dir": ".editchain"
        }}),
    );
    assert!(reopened["live"]["blocks"]
        .as_array()
        .expect("valid peer context fixture")
        .iter()
        .any(|block| block["parents"] == json!([format!("record:{}", sources[0].id)])));
}
