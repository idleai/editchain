use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use process_wrap::tokio::{ChildWrapper, CommandWrap, KillOnDrop};
use tokio::io::{AsyncRead, AsyncReadExt};

use crate::cancellation::ImportCancellation;
use crate::error::ImportError;

/// Independent limits on one helper invocation, including output draining.
#[derive(Debug, Clone, Copy)]
pub struct HelperLimits {
    /// Maximum captured stdout bytes.
    pub stdout_bytes: usize,
    /// Maximum captured stderr bytes, including unsuccessful invocations.
    pub stderr_bytes: usize,
    /// Maximum execution time, including waiting for inherited output pipes.
    pub timeout: Duration,
}

impl Default for HelperLimits {
    fn default() -> Self {
        Self {
            stdout_bytes: 256 * 1024 * 1024,
            stderr_bytes: 1024 * 1024,
            timeout: Duration::from_mins(2),
        }
    }
}

/// A configurable helper process that turns a Codex rollout file into the
/// `editchain-v1` NDJSON projection on stdout.
///
/// The helper is a program plus fixed prefix arguments; the rollout path is
/// appended as the final argument and the process is spawned directly (no
/// shell). This supports both a standalone exporter binary and a future
/// `codex rollout-export --format editchain-v1` command (e.g. program `codex`,
/// args `["rollout-export", "--format", "editchain-v1"]`).
#[derive(Debug, Clone)]
pub struct HelperCommand {
    /// Helper program to execute.
    pub program: String,
    /// Fixed prefix arguments passed before the rollout path.
    pub args: Vec<String>,
}

impl HelperCommand {
    /// Create a new helper command from a program and prefix arguments.
    #[must_use]
    pub fn new(program: impl Into<String>, args: Vec<String>) -> Self {
        Self {
            program: program.into(),
            args,
        }
    }

    /// Run with default limits, returning stdout after successful completion.
    ///
    /// # Errors
    ///
    /// Returns spawn, output limit, deadline, IO, or unsuccessful-exit errors.
    pub fn run(&self, rollout_path: &Path) -> Result<Vec<u8>, ImportError> {
        self.run_with_control(
            rollout_path,
            HelperLimits::default(),
            &ImportCancellation::default(),
        )
    }

    /// Run with explicit output/deadline bounds and cooperative cancellation.
    ///
    /// # Errors
    ///
    /// Returns spawn, limit, cancellation, IO, or unsuccessful-exit errors.
    pub fn run_with_control(
        &self,
        rollout_path: &Path,
        limits: HelperLimits,
        cancellation: &ImportCancellation,
    ) -> Result<Vec<u8>, ImportError> {
        self.run_captured(rollout_path, rollout_path, limits, cancellation)
    }

    /// Project a captured copy while retaining the original path in diagnostics.
    pub(crate) fn run_captured(
        &self,
        captured_path: &Path,
        source_path: &Path,
        limits: HelperLimits,
        cancellation: &ImportCancellation,
    ) -> Result<Vec<u8>, ImportError> {
        cancellation.check(source_path)?;
        if limits.timeout.is_zero() {
            return Err(limit_error(source_path, "helper elapsed milliseconds", 0));
        }
        if std::time::Instant::now()
            .checked_add(limits.timeout)
            .is_none()
        {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "helper deadline is outside the monotonic clock range",
            )
            .into());
        }
        // Keep this synchronous API usable from callers with their own runtime.
        // Pipe IO is asynchronous within this one joined worker, so cancellation
        // never leaves blocked reader threads holding inherited pipe handles.
        std::thread::scope(|scope| {
            std::thread::Builder::new()
                .name("editchain-helper".into())
                .spawn_scoped(scope, || {
                    let runtime = tokio::runtime::Builder::new_current_thread()
                        .enable_io()
                        .enable_time()
                        .build()?;
                    runtime.block_on(self.run_bounded(
                        captured_path,
                        source_path,
                        limits,
                        cancellation,
                    ))
                })?
                .join()
                .map_err(|_panic| std::io::Error::other("helper worker panicked"))?
        })
    }

    async fn run_bounded(
        &self,
        captured_path: &Path,
        source_path: &Path,
        limits: HelperLimits,
        cancellation: &ImportCancellation,
    ) -> Result<Vec<u8>, ImportError> {
        cancellation.check(source_path)?;
        let mut command = CommandWrap::with_new(&self.program, |command| {
            let _: &mut tokio::process::Command = command
                .args(&self.args)
                .arg(captured_path)
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped());
        });
        let _: &mut CommandWrap = command.wrap(KillOnDrop);
        #[cfg(unix)]
        let _: &mut CommandWrap = command.wrap(process_wrap::tokio::ProcessGroup::leader());
        #[cfg(windows)]
        let _: &mut CommandWrap = command.wrap(process_wrap::tokio::JobObject);
        let mut child = command.spawn().map_err(|source| ImportError::HelperSpawn {
            program: self.program.clone(),
            source,
        })?;

        let outcome = self
            .collect(child.as_mut(), source_path, limits, cancellation)
            .await;
        if outcome.is_err() {
            // Signal the owned group/job before reaping. Dropping asynchronous
            // reads closes our pipes even if a Unix descendant left its group.
            if let Err(error) = child.start_kill() {
                if child.try_wait()?.is_none() {
                    return Err(error.into());
                }
            }
            let _status = child.wait().await?;
        }
        outcome
    }

    async fn collect(
        &self,
        child: &mut dyn ChildWrapper,
        source: &Path,
        limits: HelperLimits,
        cancellation: &ImportCancellation,
    ) -> Result<Vec<u8>, ImportError> {
        let stdout = child
            .stdout()
            .take()
            .ok_or_else(|| std::io::Error::other("helper stdout pipe missing"))?;
        let stderr = child
            .stderr()
            .take()
            .ok_or_else(|| std::io::Error::other("helper stderr pipe missing"))?;
        let capture = async {
            let (stdout, stderr) = tokio::try_join!(
                read_bounded(stdout, source, "helper stdout bytes", limits.stdout_bytes),
                read_bounded(stderr, source, "helper stderr bytes", limits.stderr_bytes),
            )?;
            let status = child.wait().await?;
            Ok::<_, ImportError>((status, stdout, stderr))
        };
        let (status, stdout, stderr) = tokio::select! {
            result = capture => result?,
            () = tokio::time::sleep(limits.timeout) => {
                return Err(limit_error(source, "helper elapsed milliseconds",
                    u64::try_from(limits.timeout.as_millis()).unwrap_or(u64::MAX)));
            },
            error = cancelled(cancellation, source) => return Err(error),
        };
        cancellation.check(source)?;
        if status.success() {
            Ok(stdout)
        } else {
            Err(ImportError::HelperFailed {
                path: source.to_path_buf(),
                program: self.program.clone(),
                exit_code: status.code(),
                stderr: String::from_utf8_lossy(&stderr).into_owned(),
            })
        }
    }
}

async fn cancelled(cancellation: &ImportCancellation, source: &Path) -> ImportError {
    loop {
        if let Err(error) = cancellation.check(source) {
            return error;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

async fn read_bounded(
    mut pipe: impl AsyncRead + Unpin,
    source: &Path,
    resource: &'static str,
    limit: usize,
) -> Result<Vec<u8>, ImportError> {
    let mut output = Vec::new();
    let mut buffer = vec![0; 8192].into_boxed_slice();
    loop {
        let count = pipe.read(&mut buffer).await?;
        if count == 0 {
            return Ok(output);
        }
        if count > limit.saturating_sub(output.len()) {
            return Err(limit_error(
                source,
                resource,
                u64::try_from(limit).unwrap_or(u64::MAX),
            ));
        }
        if let Some(bytes) = buffer.get(..count) {
            output.extend_from_slice(bytes);
        }
    }
}

fn limit_error(path: &Path, resource: &'static str, limit: u64) -> ImportError {
    ImportError::ResourceLimit {
        path: path.to_path_buf(),
        resource,
        limit,
    }
}
