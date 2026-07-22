#![doc = "CLI entrypoint for the editchain SWE-bench agent."]

// Suppress unused-crate-dependencies warnings for deps used only by the library.
use chrono as _;
use editchain_codec as _;
use editchain_core as _;
use minijinja as _;
use reqwest as _;
use serde as _;
use serde_yaml as _;
use uuid as _;

use std::path::PathBuf;

use clap::{Parser, Subcommand};
use tracing_subscriber::EnvFilter;

use editchain_agent::config::Config;
use editchain_agent::swebench::{run_batch, run_instance, SwebenchInstance};

#[derive(Parser)]
#[command(name = "editchain-agent", about = "SWE-bench agent for editchain")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Run a single SWE-bench instance
    RunSingle {
        /// Path to config YAML
        #[arg(long)]
        config: PathBuf,
        /// Instance ID
        #[arg(long)]
        instance_id: String,
        /// Path to dataset JSONL
        #[arg(long)]
        dataset: PathBuf,
        /// Output directory
        #[arg(long, default_value = "output")]
        output: PathBuf,
    },
    /// Run a batch of SWE-bench instances
    Run {
        /// Path to config YAML
        #[arg(long)]
        config: PathBuf,
        /// Path to dataset JSONL
        #[arg(long)]
        dataset: PathBuf,
        /// Output directory
        #[arg(long, default_value = "output")]
        output: PathBuf,
        /// Number of parallel workers
        #[arg(long, default_value_t = 1)]
        workers: usize,
    },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    let cli = Cli::parse();

    match cli.command {
        Commands::RunSingle {
            config,
            instance_id,
            dataset,
            output,
        } => {
            let config = Config::from_yaml(&config)?;
            let instances = load_instances(&dataset)?;
            let instance = instances
                .iter()
                .find(|i| i.instance_id == instance_id)
                .ok_or_else(|| anyhow::anyhow!("Instance {instance_id} not found in dataset"))?;

            let (exit_status, submission) = run_instance(instance, &config, &output).await?;
            tracing::info!(
                "Instance {}: exit_status={}, submission_len={}",
                instance_id,
                exit_status,
                submission.len()
            );
        }
        Commands::Run {
            config,
            dataset,
            output,
            workers,
        } => {
            let config = Config::from_yaml(&config)?;
            let instances = load_instances(&dataset)?;
            tracing::info!(
                "Running {} instances with {} workers",
                instances.len(),
                workers
            );
            run_batch(&instances, &config, &output, workers).await?;
        }
    }

    Ok(())
}

fn load_instances(path: &PathBuf) -> anyhow::Result<Vec<SwebenchInstance>> {
    let contents = std::fs::read_to_string(path)?;
    let mut instances = Vec::new();
    for line in contents.lines() {
        if line.trim().is_empty() {
            continue;
        }
        let instance: SwebenchInstance = serde_json::from_str(line)?;
        instances.push(instance);
    }
    Ok(instances)
}
