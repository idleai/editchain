//! One owned helper process multiplexes bounded, ordered source batches.

use process_wrap::tokio::{ChildWrapper, CommandWrap, KillOnDrop};
use serde::Deserialize;
use std::{io, path::Path, process::Stdio};
use tokio::io::{AsyncBufReadExt as _, AsyncReadExt as _, AsyncWriteExt as _, BufReader};

use super::{cancelled, HelperCommand, HelperLimits};
use crate::{cancellation::ImportCancellation, source_read::LineWithHash, ImportError};

const SCHEMA: &str = "editchain-stream-v1";

/// One ordered physical input batch for the persistent reducer.
#[derive(Debug)]
pub struct LiveHelperInput<'a> {
    /// Physical provider source, used only as an opaque identity by the helper.
    pub source: &'a Path,
    /// Accepted source generation.
    pub generation: u32,
    /// Number of preceding physical records; zero resets the reducer.
    pub after: u64,
    /// Complete physical records to consume.
    pub lines: &'a [LineWithHash],
}

/// Validated envelope from a retained Codex reducer.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LiveHelperReply {
    schema: String,
    through: u64,
    /// Occurrence deltas for the supplied nonblank physical lines only.
    pub records: Vec<serde_json::Value>,
    /// Reducer invocations, zero for an identical retried request.
    pub records_projected: u64,
    error: Option<String>,
}

/// Retained subprocess with bounded pipe IO and an independent runtime.
pub struct LiveHelper {
    child: Box<dyn ChildWrapper>,
    input: tokio::process::ChildStdin,
    output: BufReader<tokio::process::ChildStdout>,
    runtime: tokio::runtime::Runtime,
    failed: bool,
}

impl std::fmt::Debug for LiveHelper {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LiveHelper")
            .field("failed", &self.failed)
            .finish_non_exhaustive()
    }
}

impl LiveHelper {
    /// Launch an exporter supporting `--stream`; no shell is involved.
    ///
    /// # Errors
    /// Returns process or pipe setup errors.
    pub fn start(command: &HelperCommand) -> Result<Self, ImportError> {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_io()
            .enable_time()
            .build()?;
        let mut child = {
            let _entered = runtime.enter();
            let mut wrap = CommandWrap::with_new(&command.program, |cmd| {
                let _: &mut tokio::process::Command = cmd
                    .args(&command.args)
                    .arg("--stream")
                    .stdin(Stdio::piped())
                    .stdout(Stdio::piped())
                    .stderr(Stdio::inherit());
            });
            let _: &mut CommandWrap = wrap.wrap(KillOnDrop);
            #[cfg(unix)]
            let _: &mut CommandWrap = wrap.wrap(process_wrap::tokio::ProcessGroup::leader());
            #[cfg(windows)]
            let _: &mut CommandWrap = wrap.wrap(process_wrap::tokio::JobObject);
            wrap.spawn()?
        };
        let input = child
            .stdin()
            .take()
            .ok_or_else(|| io::Error::other("live helper stdin missing"))?;
        let output = BufReader::new(
            child
                .stdout()
                .take()
                .ok_or_else(|| io::Error::other("live helper stdout missing"))?,
        );
        Ok(Self {
            child,
            input,
            output,
            runtime,
            failed: false,
        })
    }

    /// Apply one batch. Ordinal zero also explicitly bootstraps the source.
    ///
    /// # Errors
    /// Returns errors on cancellation, timeout, invalid replies or a lost reducer.
    pub fn project(
        &mut self,
        batch: &LiveHelperInput<'_>,
        cancellation: &ImportCancellation,
    ) -> Result<LiveHelperReply, ImportError> {
        let LiveHelperInput {
            source,
            generation,
            after,
            lines,
        } = *batch;
        if self.failed {
            return Err(io::Error::other("live helper requires restart").into());
        }
        let through = after
            .checked_add(u64::try_from(lines.len()).map_err(io::Error::other)?)
            .ok_or_else(|| io::Error::other("live source ordinal exhausted"))?;
        let mut input = serde_json::to_vec(&serde_json::json!({
            "schema": SCHEMA, "source": source, "generation": generation,
            "after": after, "reset": after == 0,
            "lines": lines.iter().map(|line| std::str::from_utf8(&line.data).ok()).collect::<Vec<_>>()
        })).map_err(io::Error::other)?;
        input.push(b'\n');
        if input.len() > 64 * 1024 * 1024 {
            return Err(io::Error::other("live helper input batch exceeds 64 MiB").into());
        }
        let limits = HelperLimits::default();
        let result = self.runtime.block_on(async {
            let exchange = async {
                self.input.write_all(&input).await?;
                self.input.flush().await?;
                let mut output = Vec::new();
                let _count = (&mut self.output).take(u64::try_from(limits.stdout_bytes).unwrap_or(u64::MAX).saturating_add(1))
                    .read_until(b'\n', &mut output).await?;
                if output.len() > limits.stdout_bytes || output.last() != Some(&b'\n') {
                    return Err(io::Error::other("live helper reply incomplete or oversized").into());
                }
                let reply: LiveHelperReply = serde_json::from_slice(&output).map_err(io::Error::other)?;
                if reply.schema != SCHEMA || reply.through != through || reply.error.is_some() {
                    return Err(io::Error::other(format!("live helper requires bootstrap: {:?}", reply.error)).into());
                }
                Ok::<_, ImportError>(reply)
            };
            tokio::select! {
                reply = exchange => reply,
                error = cancelled(cancellation, source) => Err(error),
                () = tokio::time::sleep(limits.timeout) => Err(io::Error::new(io::ErrorKind::TimedOut, "live helper deadline").into()),
            }
        });
        if result.is_err() {
            self.failed = true;
            let _kill = self.child.start_kill();
        }
        result
    }
}

impl Drop for LiveHelper {
    fn drop(&mut self) {
        let _kill = self.child.start_kill();
        let _wait = self.runtime.block_on(async {
            tokio::time::timeout(std::time::Duration::from_secs(1), self.child.wait()).await
        });
    }
}
