//! Index refresh subscriptions include late lower IDs, conflicts and late blobs.

use std::{
    path::Path,
    time::{Duration, Instant},
};

use editchain_engine::queries::{ChainQueries, PageRequest};
use editchain_import::cancellation::ImportCancellation;
use serde_json::json;

use super::{
    error::{Failure, Result},
    output::Output,
    require_chain,
};

#[derive(Debug, clap::Args)]
pub(super) struct Args {
    /// Emit a complete initial snapshot and exit.
    #[arg(long, conflicts_with = "no_initial")]
    once: bool,
    /// Start with a ready event, then emit only changes observed after startup.
    #[arg(long)]
    no_initial: bool,
    /// Poll interval, in milliseconds (1..60000).
    #[arg(long, default_value = "250", value_parser = clap::value_parser!(u64).range(1..=60_000))]
    poll_ms: u64,
    /// Stop after this many change batches (initial snapshot is not counted).
    #[arg(long, value_parser = clap::value_parser!(u64).range(1..))]
    count: Option<u64>,
    /// Stop successfully after this many milliseconds.
    #[arg(long, value_parser = clap::value_parser!(u64).range(1..))]
    timeout_ms: Option<u64>,
}

pub(super) fn run(
    chain: &Path,
    args: &Args,
    cancellation: &ImportCancellation,
    output: &mut Output,
) -> Result<()> {
    require_chain(chain)?;
    let mut queries = ChainQueries::open(chain)?;
    let _changes = queries.refresh()?;
    output.begin_stream()?;
    if !args.no_initial {
        initial(&queries, output)?;
    }
    output.emit(&json!({"type":"ready","stats":queries.index().stats()}))?;
    if args.once {
        return Ok(());
    }
    let started = Instant::now();
    let mut count = 0_u64;
    loop {
        cancellation.check(chain)?;
        if args
            .timeout_ms
            .is_some_and(|ms| started.elapsed() >= Duration::from_millis(ms))
        {
            return Ok(());
        }
        let delta = queries.refresh()?;
        if !delta.added.is_empty() || !delta.removed.is_empty() || !delta.content_changed.is_empty()
        {
            let added = delta
                .added
                .iter()
                .map(|id| queries.operation(*id))
                .collect::<std::io::Result<Vec<_>>>()?;
            let changed = delta
                .content_changed
                .iter()
                .map(|id| queries.operation(*id))
                .collect::<std::io::Result<Vec<_>>>()?;
            let removed = delta
                .removed
                .iter()
                .map(|id| {
                    queries
                        .record_variants(*id)
                        .map(|variants| json!({"operation":id,"variants":variants}))
                })
                .collect::<std::io::Result<Vec<_>>>()?;
            output.emit(&json!({"type":"change","added":added,"removed":removed,"content_changed":changed,"stats":queries.index().stats()}))?;
            count = count.saturating_add(1);
            if args.count.is_some_and(|limit| count >= limit) {
                return Ok(());
            }
        }
        let poll = Duration::from_millis(args.poll_ms);
        let remaining = args.timeout_ms.map_or(poll, |ms| {
            Duration::from_millis(ms)
                .saturating_sub(started.elapsed())
                .min(poll)
        });
        wait(remaining, cancellation)?;
    }
}

fn initial(queries: &ChainQueries, output: &mut Output) -> Result<()> {
    let mut page = PageRequest {
        after: None,
        limit: 1000,
    };
    loop {
        let result = queries.history(None, page)?;
        output.emit(&json!({"type":"snapshot","page":result}))?;
        let Some(after) = result.next_after else {
            break;
        };
        page.after = Some(after);
    }
    Ok(())
}

fn wait(duration: Duration, cancellation: &ImportCancellation) -> Result<()> {
    let start = Instant::now();
    while let Some(remaining) = duration.checked_sub(start.elapsed()) {
        if cancellation.is_cancelled() {
            return Err(Failure::new(130, "subscription interrupted"));
        }
        std::thread::sleep(remaining.min(Duration::from_millis(50)));
    }
    Ok(())
}
