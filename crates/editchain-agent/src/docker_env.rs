use std::collections::HashMap;
use std::process::Command;

use anyhow::{bail, Result};
use tracing::{debug, info};

use crate::config::EnvConfig;

/// Result of executing a command in the environment.
#[derive(Debug, Clone)]
pub struct ExecOutput {
    /// Combined stdout/stderr output from the command.
    pub output: String,
    /// Exit code of the command (-1 if execution failed).
    pub returncode: i32,
    /// Exception or error information if the command could not be run.
    pub exception_info: String,
}

/// Docker-based execution environment.
#[derive(Debug)]
pub struct DockerEnvironment {
    config: EnvConfig,
    container_id: Option<String>,
}

impl DockerEnvironment {
    /// Create a new Docker environment from the given configuration.
    #[must_use]
    pub const fn new(config: EnvConfig) -> Self {
        Self {
            config,
            container_id: None,
        }
    }

    /// Start the Docker container.
    ///
    /// # Errors
    ///
    /// Returns an error if the Docker command fails or the container does not start.
    #[expect(
        clippy::let_underscore_untyped,
        clippy::string_slice,
        reason = "let _ discards Command result; UUID hex string is always ASCII"
    )]
    pub fn start(&mut self) -> Result<()> {
        let container_name = format!(
            "editchain-{}",
            &uuid::Uuid::new_v4().simple().to_string()[..8]
        );

        let mut cmd = Command::new("docker");
        let _ = cmd.args(["run", "-d", "--name", &container_name]);
        let _ = cmd.args(["-w", &self.config.cwd]);

        // Forward env vars
        for key in &self.config.forward_env {
            if let Ok(val) = std::env::var(key) {
                let _ = cmd.args(["-e", &format!("{key}={val}")]);
            }
        }
        for (key, val) in &self.config.env {
            let _ = cmd.args(["-e", &format!("{key}={val}")]);
        }

        let _ = cmd.arg("--rm");
        let _ = cmd.args([&self.config.image, "sleep", &self.config.container_timeout]);

        debug!("Starting container: {:?}", cmd);
        let output = cmd.output()?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            bail!("Failed to start container: {stderr}");
        }

        let cid = String::from_utf8_lossy(&output.stdout).trim().to_string();
        info!("Started container: {}", cid);
        self.container_id = Some(cid);
        Ok(())
    }

    /// Execute a command in the container.
    #[expect(
        clippy::let_underscore_untyped,
        clippy::manual_let_else,
        reason = "let _ discards Command result; manual let-else is clearer for early return"
    )]
    pub fn execute(&self, action: &HashMap<String, String>) -> ExecOutput {
        let command = action.get("command").map_or("", String::as_str);
        let cid = match &self.container_id {
            Some(id) => id,
            None => {
                return ExecOutput {
                    output: String::new(),
                    returncode: -1,
                    exception_info: "Container not started".into(),
                };
            }
        };

        let mut cmd = Command::new("docker");
        let _ = cmd.args(["exec", "-w", &self.config.cwd]);

        for key in &self.config.forward_env {
            if let Ok(val) = std::env::var(key) {
                let _ = cmd.args(["-e", &format!("{key}={val}")]);
            }
        }
        for (key, val) in &self.config.env {
            let _ = cmd.args(["-e", &format!("{key}={val}")]);
        }

        let _ = cmd.args([cid.as_str(), "bash", "-c", command]);

        debug!("Executing: {:?}", cmd);

        self.run_cmd(&mut cmd)
    }

    #[expect(
        clippy::unused_self,
        reason = "self is used by callers expecting a method; refactoring would break consistency"
    )]
    fn run_cmd(&self, cmd: &mut Command) -> ExecOutput {
        match cmd.output() {
            Ok(output) => {
                let stdout = String::from_utf8_lossy(&output.stdout).to_string();
                ExecOutput {
                    output: stdout,
                    returncode: output.status.code().unwrap_or(-1),
                    exception_info: String::new(),
                }
            }
            Err(e) => ExecOutput {
                output: String::new(),
                returncode: -1,
                exception_info: format!("Error executing command: {e}"),
            },
        }
    }

    /// Stop and remove the container.
    pub fn cleanup(&mut self) {
        if let Some(cid) = self.container_id.take() {
            info!("Cleaning up container: {}", cid);
            drop(Command::new("docker").args(["stop", &cid]).output());
            drop(Command::new("docker").args(["rm", "-f", &cid]).output());
        }
    }
}

impl Drop for DockerEnvironment {
    fn drop(&mut self) {
        self.cleanup();
    }
}
