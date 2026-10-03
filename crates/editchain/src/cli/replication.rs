//! Replication adapters for explicitly selected chains and caller-owned pipes.

use std::{
    collections::{BTreeSet, VecDeque},
    io::{self, Write},
    path::{Path, PathBuf},
    sync::mpsc,
    time::{Duration, Instant},
};

use super::cancellation::Cancellation;
use editchain_store::{BlobStore, SegmentStore};
use editchain_sync::{
    ExportScope, PeerConnection, Progress, RecordKey, Session, StoreReplica, Transport,
};
use serde::Deserialize;
use serde_json::json;

use super::{
    error::{Failure, Result},
    input,
    output::{diagnostic, Output},
    require_chain,
};

pub(super) type Storage = StoreReplica<SegmentStore, BlobStore, ExportScope>;

#[derive(Debug, clap::Args)]
#[command(group(clap::ArgGroup::new("transport").required(true).args(["peer", "stdio"])), group(clap::ArgGroup::new("sharing").required(true).args(["share_all", "scope"])))]
pub(super) struct Args {
    /// Existing local peer chain. Both peers exchange records and referenced blobs.
    #[arg(long)]
    peer: Option<PathBuf>,
    /// Exchange framed protocol bytes on stdin/stdout with an authorized transport (e.g. SSH).
    #[arg(long)]
    stdio: bool,
    /// Caller-selected logical chain/sharing namespace; must agree at both ends.
    #[arg(long)]
    namespace: String,
    /// Explicitly share the entire store, including received records.
    #[arg(long)]
    share_all: bool,
    /// JSON file with records (`RecordKey` array) and blobs (32-byte hash array).
    #[arg(long)]
    scope: Option<PathBuf>,
    /// Maximum catch-up duration in milliseconds; retries resume from durable evidence.
    #[arg(long, default_value = "30000", value_parser = clap::value_parser!(u64).range(1..))]
    timeout_ms: u64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Selection {
    records: BTreeSet<RecordKey>,
    blobs: BTreeSet<[u8; 32]>,
}

pub(super) fn storage(chain: &Path, scope: ExportScope) -> Result<Storage> {
    Ok(StoreReplica::new(
        SegmentStore::open(chain)?,
        BlobStore::new(chain.join("blobs"))?,
        scope,
    ))
}

pub(super) fn run(
    chain: &Path,
    args: &Args,
    cancellation: &Cancellation,
    output: &mut Output,
) -> Result<()> {
    require_chain(chain)?;
    let scope = if let Some(path) = &args.scope {
        if args.stdio && path == Path::new("-") {
            return Err(Failure::input(
                "stdio replication requires a scope file, not stdin",
            ));
        }
        let selection: Selection = serde_json::from_slice(&input::bytes(path)?)?;
        ExportScope::selected(&args.namespace, selection.records, selection.blobs)?
    } else if args.share_all {
        ExportScope::all(&args.namespace)?
    } else {
        return Err(Failure::input("select --share-all or --scope"));
    };
    if let Some(peer) = &args.peer {
        require_chain(peer)?;
        if std::fs::canonicalize(chain)? == std::fs::canonicalize(peer)? {
            return Err(Failure::input(
                "replication needs two different chain directories",
            ));
        }
        let (local, remote) = local(chain, peer, scope, args, cancellation)?;
        output.emit(&json!({"local":local,"peer":remote,
            "local_chain":editchain_engine::ChainSnapshot::read(chain)?.stats(),
            "peer_chain":editchain_engine::ChainSnapshot::read(peer)?.stats()}))?;
        complete(&local)?;
        complete(&remote)
    } else {
        let progress = stdio(chain, scope, args, cancellation)?;
        diagnostic(&serde_json::to_string(&json!({"progress":progress,
            "chain":editchain_engine::ChainSnapshot::read(chain)?.stats()}))?)?;
        complete(&progress)
    }
}

fn local(
    chain: &Path,
    peer: &Path,
    scope: ExportScope,
    args: &Args,
    cancellation: &Cancellation,
) -> Result<(Progress, Progress)> {
    let mut left = Session::new(storage(chain, scope.clone())?);
    let mut right = Session::new(storage(peer, scope)?);
    let mut queue = VecDeque::from([(false, left.hello()), (true, right.hello())]);
    let started = Instant::now();
    loop {
        check_deadline(started, args, cancellation)?;
        if let Some((to_left, message)) = queue.pop_front() {
            let replies = if to_left {
                left.receive(message)?
            } else {
                right.receive(message)?
            };
            queue.extend(replies.into_iter().map(|reply| (!to_left, reply)));
        } else if done(left.progress()) && done(right.progress()) {
            return Ok((left.progress().clone(), right.progress().clone()));
        } else {
            queue.extend(left.tick()?.into_iter().map(|message| (false, message)));
            queue.extend(right.tick()?.into_iter().map(|message| (true, message)));
            if queue.is_empty() {
                std::thread::sleep(Duration::from_millis(10));
            }
        }
    }
}

struct StdioTransport(io::BufWriter<io::Stdout>);

impl Transport for StdioTransport {
    fn send(&mut self, _peer: &str, frame: &[u8]) -> io::Result<()> {
        self.0.write_all(frame)?;
        self.0.flush()
    }
}

fn stdio(
    chain: &Path,
    scope: ExportScope,
    args: &Args,
    cancellation: &Cancellation,
) -> Result<Progress> {
    let mut connection = PeerConnection::new(
        "stdio",
        storage(chain, scope)?,
        StdioTransport(io::BufWriter::new(io::stdout())),
    );
    connection.start()?;
    let receiver = input::stdin_chunks();
    let started = Instant::now();
    loop {
        check_deadline(started, args, cancellation)?;
        match receiver.recv_timeout(Duration::from_millis(100)) {
            Ok(Ok(bytes)) if bytes.is_empty() => {
                connection.finish()?;
                return Ok(connection.progress().clone());
            }
            Ok(Ok(bytes)) => connection.receive("stdio", &bytes)?,
            Ok(Err(error)) => return Err(error.into()),
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                return Err(Failure::new(1, "replication stdin reader disconnected"))
            }
            // Hello starts the one-shot catch-up. Starting another round while
            // the outgoing check is unfinished can strand frames at shutdown.
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
        if done(connection.progress()) {
            connection.finish()?;
            return Ok(connection.progress().clone());
        }
    }
}

fn check_deadline(start: Instant, args: &Args, cancellation: &Cancellation) -> Result<()> {
    if cancellation.is_cancelled() {
        return Err(Failure::new(
            130,
            "replication interrupted; retry to resume",
        ));
    }
    if start.elapsed() >= Duration::from_millis(args.timeout_ms) {
        return Err(Failure::new(
            3,
            "replication timed out before both inventories completed; retry to resume",
        ));
    }
    Ok(())
}

fn done(progress: &Progress) -> bool {
    progress.accepted
        && progress.incoming.complete
        && progress.outgoing.complete
        && progress.pending_records == 0
        && progress.pending_blobs == 0
}

fn complete(progress: &Progress) -> Result<()> {
    if !done(progress) {
        return Err(Failure::new(
            3,
            "transport ended before replication completed",
        ));
    }
    if progress.incoming.unavailable > 0 || progress.outgoing.unavailable > 0 {
        return Err(Failure::new(
            3,
            "replication retained records with unavailable blobs; retry after content arrives",
        ));
    }
    Ok(())
}
