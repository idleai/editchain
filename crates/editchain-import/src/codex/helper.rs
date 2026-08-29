use std::path::Path;
use std::process::{Command, Stdio};

use crate::error::ImportError;

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

    /// Run the helper over `rollout_path`, returning its full stdout and stderr
    /// and validating that it exited successfully.
    ///
    /// # Errors
    ///
    /// Returns [`ImportError::HelperSpawn`] when the process cannot be spawned
    /// and [`ImportError::HelperFailed`] when it exits nonzero.
    pub fn run(&self, rollout_path: &Path) -> Result<Vec<u8>, ImportError> {
        let mut cmd = Command::new(&self.program);
        let _: &mut Command = cmd
            .args(&self.args)
            .arg(rollout_path)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let output = cmd.output().map_err(|source| ImportError::HelperSpawn {
            program: self.program.clone(),
            source,
        })?;
        if output.status.success() {
            Ok(output.stdout)
        } else {
            let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
            Err(ImportError::HelperFailed {
                path: rollout_path.to_path_buf(),
                program: self.program.clone(),
                exit_code: output.status.code(),
                stderr,
            })
        }
    }
}
