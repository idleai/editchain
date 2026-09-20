//! Full native workers exchange only opaque TLS bytes over their real stdio IPC.

use std::collections::VecDeque;
use std::io::{self, Read as _, Write as _};
use std::path::Path;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

use base64::{engine::general_purpose::STANDARD, Engine as _};
use editchain_core::{
    ActorId, BlobRef, Clock, ContentId, MessageOp, NodeId, Op, OpId, OpKind, ParentSet, Payload,
    ScopeRef, Tags,
};
use editchain_store::format::{encode_op, Page};
use editchain_store::{BlobStore, CanonicalChain, SegmentStore};
use editchain_sync::{MAX_BRIDGE_BYTES, MAX_CONTROL_BYTES};
use serde_json::{json, Value};

use postcard as _;
use rcgen as _;
use rustls as _;
use serde as _;
// Dev-only dependencies of the crate's tests; unused in this target.
use editchain_node as _;
use editchain_protocol as _;

fn require(condition: bool, message: &'static str) -> io::Result<()> {
    if condition {
        Ok(())
    } else {
        Err(io::Error::other(message))
    }
}

struct Worker {
    child: Child,
    input: ChildStdin,
    output: ChildStdout,
}

impl Worker {
    fn spawn() -> io::Result<Self> {
        let mut child = Command::new(env!("CARGO_BIN_EXE_editchain-peer"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()?;
        let input = child
            .stdin
            .take()
            .ok_or_else(|| io::Error::other("no worker input"))?;
        let output = child
            .stdout
            .take()
            .ok_or_else(|| io::Error::other("no worker output"))?;
        Ok(Self {
            child,
            input,
            output,
        })
    }

    fn call(&mut self, request: &Value) -> io::Result<Value> {
        let bytes = serde_json::to_vec(request).map_err(io::Error::other)?;
        let length = u32::try_from(bytes.len()).map_err(io::Error::other)?;
        self.input.write_all(&length.to_le_bytes())?;
        self.input.write_all(&bytes)?;
        self.input.flush()?;
        let mut length = [0; 4];
        self.output.read_exact(&mut length)?;
        let length = usize::try_from(u32::from_le_bytes(length)).map_err(io::Error::other)?;
        require(length <= MAX_CONTROL_BYTES, "native output exceeded bound")?;
        let mut bytes = vec![0; length];
        self.output.read_exact(&mut bytes)?;
        serde_json::from_slice(&bytes).map_err(io::Error::other)
    }

    fn ok(&mut self, request: &Value) -> io::Result<Value> {
        let response = self.call(request)?;
        require(
            response.get("ok") == Some(&Value::Bool(true)),
            "worker rejected an expected valid request",
        )?;
        response
            .get("result")
            .cloned()
            .ok_or_else(|| io::Error::other("missing worker result"))
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        drop(self.child.kill());
        drop(self.child.wait());
    }
}

fn seed(root: &Path, seq: u64, content: &[u8]) -> io::Result<()> {
    let mut store = SegmentStore::open(root)?;
    BlobStore::new(root.join("blobs"))?.write(content)?;
    let op = Op {
        id: OpId::new(NodeId(42), 1, seq),
        parents: ParentSet::None,
        actor: ActorId(123),
        clock: Clock::UnixMs(456),
        scope: ScopeRef::None,
        tags: Tags::MESSAGE,
        kind: OpKind::Message(MessageOp {
            content: Payload::Blob(BlobRef {
                id: ContentId::Hash256(*blake3::hash(content).as_bytes()),
                len: u32::try_from(content.len()).map_err(io::Error::other)?,
            }),
            content_type: Payload::Empty,
        }),
    };
    let mut page = Page::new(0);
    page.add_record(0, encode_op(&op).map_err(io::Error::other)?);
    store.append_page(&page)
}

fn opaque(response: &Value) -> io::Result<Vec<u8>> {
    STANDARD
        .decode(
            response
                .get("bytes")
                .and_then(Value::as_str)
                .ok_or_else(|| io::Error::other("missing opaque bytes"))?,
        )
        .map_err(io::Error::other)
}

fn connect(a: &mut Worker, b: &mut Worker, initial: Vec<u8>) -> io::Result<()> {
    let mut queue: VecDeque<_> = [(true, initial)].into();
    for _ in 0..20_000 {
        let Some((to_a, bytes)) = queue.pop_front() else {
            return Ok(());
        };
        let worker = if to_a { &mut *a } else { &mut *b };
        for part in bytes.chunks(MAX_BRIDGE_BYTES.min(997)) {
            let result = worker
                .ok(&json!({ "type": "turn", "bytes": STANDARD.encode(part), "tick": false }))?;
            let output = opaque(&result)?;
            if !output.is_empty() {
                queue.push_back((!to_a, output));
            }
        }
    }
    Err(io::Error::other("native workers did not converge"))
}

fn open(
    worker: &mut Worker,
    root: &Path,
    device: &Path,
    remote: Option<&str>,
) -> io::Result<Vec<u8>> {
    opaque(&worker.ok(&json!({ "type": "open", "chain_dir": root, "device_dir": device, "space": "process-space", "remote": remote }))?)
}

#[test]
fn separate_native_processes_replicate_restart_repair_and_revoke() -> io::Result<()> {
    let dir = tempfile::tempdir()?;
    let ar = dir.path().join("a");
    let br = dir.path().join("b");
    let ad = dir.path().join("a-device");
    let bd = dir.path().join("b-device");
    seed(&ar, 1, &vec![77; 190_000])?;
    seed(&br, 2, b"bob's distinct content")?;
    let mut a = Worker::spawn()?;
    let mut b = Worker::spawn()?;
    require(a.child.id() != b.child.id(), "independent native processes")?;
    let ai = a.ok(&json!({ "type": "identity", "device_dir": ad }))?;
    let bi = b.ok(&json!({ "type": "identity", "device_dir": bd }))?;
    let ac = ai
        .get("certificate")
        .and_then(Value::as_str)
        .ok_or_else(|| io::Error::other("no public device"))?;
    let bc = bi
        .get("certificate")
        .and_then(Value::as_str)
        .ok_or_else(|| io::Error::other("no public device"))?;
    for (worker, root, certificate) in [(&mut a, &ar, bc), (&mut b, &br, ac)] {
        let _result = worker.ok(&json!({ "type": "configure", "chain_dir": root, "space": "process-space", "backfill": true }))?;
        let _result = worker.ok(&json!({ "type": "approve", "chain_dir": root, "space": "process-space", "certificate": certificate }))?;
    }
    let _bytes = open(&mut a, &ar, &ad, None)?;
    let initial = open(&mut b, &br, &bd, Some(ac))?;
    require(
        !initial
            .windows(b"bob's distinct content".len())
            .any(|w| w == b"bob's distinct content"),
        "bridge contains TLS, no application plaintext",
    )?;
    connect(&mut a, &mut b, initial)?;
    require(
        CanonicalChain::read(&ar)?.stats().accepted == 2
            && CanonicalChain::read(&br)?.stats().accepted == 2,
        "both processes persisted exact union",
    )?;
    let status = b.ok(&json!({ "type": "turn", "bytes": "", "tick": false }))?;
    require(
        status.pointer("/progress/blobs").and_then(Value::as_u64) == Some(1),
        "chunked content traversed TLS and durable store",
    )?;
    drop((a, b));
    seed(&ar, 3, b"captured while peers were stopped")?;
    let mut a = Worker::spawn()?;
    let mut b = Worker::spawn()?;
    require(
        a.ok(&json!({ "type": "identity", "device_dir": ad }))? == ai,
        "same durable device after process restart",
    )?;
    let _bytes = open(&mut a, &ar, &ad, None)?;
    let initial = open(&mut b, &br, &bd, Some(ac))?;
    connect(&mut a, &mut b, initial)?;
    require(
        CanonicalChain::read(&br)?.stats().accepted == 3,
        "offline history repaired after process restart",
    )?;
    require(
        CanonicalChain::read(&br)?.stats().duplicates == 0,
        "durable reconciliation is physically idempotent",
    )?;
    let mut control = Worker::spawn()?;
    let _result = control.ok(&json!({ "type": "revoke", "chain_dir": ar, "space": "process-space", "fingerprint": bi.get("fingerprint") }))?;
    let rejected = a.call(&json!({ "type": "turn", "bytes": "", "tick": true }))?;
    require(
        rejected.get("error").and_then(Value::as_str) == Some("authentication_failed"),
        "another process's revocation closes a live edge",
    )?;
    Ok(())
}
