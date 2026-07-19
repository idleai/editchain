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
    pub instance_id: String,
    pub problem_statement: String,
    pub repo: Option<String>,
    pub base_commit: Option<String>,
    pub hint: Option<String>,
    pub created_at: Option<String>,
    pub fail_to_pass: Option<Vec<String>>,
    pub pass_to_pass: Option<Vec<String>>,
    pub image_name: Option<String>,
    pub docker_image: Option<String>,
}

/// Run a single SWE-bench instance.
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
        .map(|s| s.to_string())
        .unwrap_or_else(|| {
            // Build docker-compatible image name
            let id_docker = instance.instance_id.replace("__", "_1776_");
            format!("docker.io/swebench/sweb.eval.x86_64.{}:latest", id_docker).to_lowercase()
        });
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