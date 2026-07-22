use std::path::Path;

use anyhow::Result;
use serde::{Deserialize, Serialize};
use tracing::info;

use crate::agent::Agent;
use crate::config::Config;
use crate::docker_env::DockerEnvironment;

/// A single SWE-bench instance from the dataset.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SwebenchInstance {
    /// Unique identifier for this SWE-bench instance.
    pub instance_id: String,
    /// Problem statement / task description.
    pub problem_statement: String,
    /// Repository name (e.g. "django/django").
    pub repo: Option<String>,
    /// Base commit hash to checkout before applying the patch.
    pub base_commit: Option<String>,
    /// Optional hint for solving the task.
    pub hint: Option<String>,
    /// Timestamp when the instance was created.
    pub created_at: Option<String>,
    /// Tests that should fail before the patch and pass after.
    pub fail_to_pass: Option<Vec<String>>,
    /// Tests that should pass both before and after the patch.
    pub pass_to_pass: Option<Vec<String>>,
    /// Pre-built Docker image name for this instance.
    pub image_name: Option<String>,
    /// Alternative Docker image specification.
    pub docker_image: Option<String>,
}

/// Run a single SWE-bench instance.
///
/// # Errors
///
/// Returns an error if the environment setup, agent execution, or trajectory
/// saving fails.
pub async fn run_instance(
    instance: &SwebenchInstance,
    config: &Config,
    output_dir: &Path,
) -> Result<(String, String)> {
    let instance_dir = output_dir.join(&instance.instance_id);
    std::fs::create_dir_all(&instance_dir)?;

    let mut env_config = config.environment.clone();
    // Set the image from the instance
    let image_name = instance
        .image_name
        .as_deref()
        .or(instance.docker_image.as_deref())
        .map_or_else(
            || {
                // Build docker-compatible image name
                let id_docker = instance.instance_id.replace("__", "_1776_");
                format!("docker.io/swebench/sweb.eval.x86_64.{id_docker}:latest").to_lowercase()
            },
            ToString::to_string,
        );
    env_config.image = image_name;

    info!("Starting environment for {}", instance.instance_id);
    let mut env = DockerEnvironment::new(env_config);
    env.start()?;

    // Set output path for trajectory saving
    let traj_path = instance_dir.join(format!("{}.traj.json", instance.instance_id));
    let mut instance_config = config.clone();
    instance_config.agent.output_path = Some(traj_path.clone());

    let mut agent = Agent::new(instance_config, env);
    let (exit_status, submission) = agent.run(&instance.problem_statement).await;

    info!(
        "Instance {} finished: exit_status={}, submission_len={}",
        instance.instance_id,
        exit_status,
        submission.len()
    );

    Ok((exit_status, submission))
}

/// Run multiple instances in parallel.
///
/// # Errors
///
/// Returns an error if any instance fails to run.
pub async fn run_batch(
    instances: &[SwebenchInstance],
    config: &Config,
    output_dir: &Path,
    workers: usize,
) -> Result<()> {
    use tokio::sync::Semaphore;

    let semaphore = std::sync::Arc::new(Semaphore::new(workers));
    let mut handles = Vec::new();

    for instance in instances {
        let permit = semaphore.clone().acquire_owned().await?;
        let config = config.clone();
        let output_dir = output_dir.to_path_buf();
        let instance_data = instance.clone();

        let handle = tokio::spawn(async move {
            let _permit = permit;
            match run_instance(&instance_data, &config, &output_dir).await {
                Ok((exit_status, submission)) => {
                    // Update preds.json
                    update_preds(&output_dir, &instance_data.instance_id, &submission)?;
                    info!("{}: {}", instance_data.instance_id, exit_status);
                }
                Err(e) => {
                    tracing::error!("{} failed: {:?}", instance_data.instance_id, e);
                }
            }
            anyhow::Ok(())
        });
        handles.push(handle);
    }

    for handle in handles {
        handle.await??;
    }

    Ok(())
}

#[expect(
    clippy::indexing_slicing,
    reason = "preds is a serde_json::Value::Object; indexing by instance_id is intentional"
)]
fn update_preds(output_dir: &Path, instance_id: &str, submission: &str) -> Result<()> {
    let preds_path = output_dir.join("preds.json");
    let mut preds: serde_json::Value = if preds_path.exists() {
        serde_json::from_str(&std::fs::read_to_string(&preds_path)?)?
    } else {
        serde_json::json!({})
    };

    preds[instance_id] = serde_json::json!({
        "model_name_or_path": "editchain-agent",
        "instance_id": instance_id,
        "model_patch": submission,
    });

    std::fs::write(&preds_path, serde_json::to_string_pretty(&preds)?)?;
    Ok(())
}
