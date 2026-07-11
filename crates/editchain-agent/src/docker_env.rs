use std::collections::HashMap;
use std::process::Command;

use anyhow::{bail, Result};
use tracing::{debug, info};

use crate::config::EnvConfig;

/// Result of executing a command in the environment.
#[derive(Debug, Clone)]
pub struct ExecOutput {
    pub output: String,
    pub returncode: i32,
    pub exception_info: String,
}

/// Docker-based execution environment.
pub struct DockerEnvironment {
    config: EnvConfig,
    container_id: Option<String>,
}

impl DockerEnvironment {
    pub fn new(config: EnvConfig) -> Self {
        Self {
            config,
            container_id: None,
        }
    }

    /// Start the Docker container.
    pub fn start(&mut self) -> Result<()> {
        let container_name = format!("editchain-{}", &uuid::Uuid::new_v4().simple().to_string()[..8]);

        let mut cmd = Command::new("docker");
        cmd.args(["run", "-d", "--name", &container_name]);
        cmd.args(["-w", &self.config.cwd]);

        // Forward env vars
        for key in &self.config.forward_env {
            if let Ok(val) = std::env::var(key) {
                cmd.args(["-e", &format!("{}={}", key, val)]);
            }
        }
        for (key, val) in &self.config.env {
            cmd.args(["-e", &format!("{}={}", key, val)]);
        }

        cmd.arg("--rm");
        cmd.args([&self.config.image, "sleep", &self.config.container_timeout]);

        debug!("Starting container: {:?}", cmd);
        let output = cmd.output()?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            bail!("Failed to start container: {}", stderr);
        }

        let cid = String::from_utf8_lossy(&output.stdout).trim().to_string();
        info!("Started container: {}", cid);
        self.container_id = Some(cid);
        Ok(())
    }

    /// Execute a command in the container.
    pub fn execute(&self, action: &HashMap<String, String>) -> ExecOutput {
        let command = action.get("command").map(|s| s.as_str()).unwrap_or("");
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
        cmd.args(["exec", "-w", &self.config.cwd]);

        for key in &self.config.forward_env {
            if let Ok(val) = std::env::var(key) {
                cmd.args(["-e", &format!("{}={}", key, val)]);
            }
        }
        for (key, val) in &self.config.env {
            cmd.args(["-e", &format!("{}={}", key, val)]);
        }

        cmd.args([cid.as_str(), "bash", "-c", command]);

        debug!("Executing: {:?}", cmd);

        self.run_cmd(&mut cmd)
    }

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
                exception_info: format!("Error executing command: {}", e),
            },
        }
    }

    /// Stop and remove the container.
    pub fn cleanup(&mut self) {
        if let Some(cid) = self.container_id.take() {
            info!("Cleaning up container: {}", cid);
            let _ = Command::new("docker")
                .args(["stop", &cid])
                .output();
            let _ = Command::new("docker")
                .args(["rm", "-f", &cid])
                .output();
        }
    }
}

impl Drop for DockerEnvironment {
    fn drop(&mut self) {
        self.cleanup();
    }
}