use std::collections::HashMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Top-level configuration matching mini-swe-agent's YAML structure.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub agent: AgentConfig,
    #[serde(default)]
    pub model: ModelConfig,
    #[serde(default)]
    pub environment: EnvConfig,
}

// ---------------------------------------------------------------------------
// AgentConfig
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentConfig {
    pub system_template: Option<String>,
    pub instance_template: Option<String>,
    #[serde(default = "default_step_limit")]
    pub step_limit: usize,
    #[serde(default = "default_cost_limit")]
    pub cost_limit: f64,
    #[serde(default = "default_wall_time")]
    pub wall_time_limit_seconds: u64,
    #[serde(default = "default_max_format_errors")]
    pub max_consecutive_format_errors: usize,
    pub output_path: Option<PathBuf>,
}

fn default_step_limit() -> usize {
    250
}
fn default_cost_limit() -> f64 {
    3.0
}
fn default_wall_time() -> u64 {
    0
}
fn default_max_format_errors() -> usize {
    10
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            system_template: None,
            instance_template: None,
            step_limit: default_step_limit(),
            cost_limit: default_cost_limit(),
            wall_time_limit_seconds: default_wall_time(),
            max_consecutive_format_errors: default_max_format_errors(),
            output_path: None,
        }
    }
}

// ---------------------------------------------------------------------------
// ModelConfig
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelConfig {
    #[serde(default = "default_model_name")]
    pub model_name: String,
    #[serde(default = "default_api_base")]
    pub api_base: String,
    #[serde(default = "default_api_key")]
    pub api_key: String,
    #[serde(default = "default_temperature")]
    pub temperature: f64,
    #[serde(default = "default_top_p")]
    pub top_p: f64,
    #[serde(default = "default_max_tokens")]
    pub max_tokens: u32,
    #[serde(default = "default_frequency_penalty")]
    pub frequency_penalty: f64,
    #[serde(default = "default_parallel_tool_calls")]
    pub parallel_tool_calls: bool,
    #[serde(default)]
    pub observation_template: Option<String>,
    #[serde(default)]
    pub format_error_template: Option<String>,
}

fn default_model_name() -> String {
    "hosted_vllm/deepseek-v4-flash".into()
}
fn default_api_base() -> String {
    "http://localhost:8000/v1".into()
}
fn default_api_key() -> String {
    "noop".into()
}
fn default_temperature() -> f64 {
    0.2
}
fn default_top_p() -> f64 {
    0.95
}
fn default_max_tokens() -> u32 {
    65536
}
fn default_frequency_penalty() -> f64 {
    0.1
}
fn default_parallel_tool_calls() -> bool {
    false
}

impl Default for ModelConfig {
    fn default() -> Self {
        Self {
            model_name: default_model_name(),
            api_base: default_api_base(),
            api_key: default_api_key(),
            temperature: default_temperature(),
            top_p: default_top_p(),
            max_tokens: default_max_tokens(),
            frequency_penalty: default_frequency_penalty(),
            parallel_tool_calls: default_parallel_tool_calls(),
            observation_template: None,
            format_error_template: None,
        }
    }
}

// ---------------------------------------------------------------------------
// EnvConfig
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnvConfig {
    #[serde(default)]
    pub image: String,
    #[serde(default = "default_cwd")]
    pub cwd: String,
    #[serde(default = "default_timeout")]
    pub timeout: u64,
    #[serde(default)]
    pub env: HashMap<String, String>,
    #[serde(default)]
    pub forward_env: Vec<String>,
    #[serde(default = "default_container_timeout")]
    pub container_timeout: String,
}

fn default_cwd() -> String {
    "/testbed".into()
}
fn default_timeout() -> u64 {
    60
}
fn default_container_timeout() -> String {
    "2h".into()
}

impl Default for EnvConfig {
    fn default() -> Self {
        Self {
            image: String::new(),
            cwd: default_cwd(),
            timeout: default_timeout(),
            env: HashMap::new(),
            forward_env: Vec::new(),
            container_timeout: default_container_timeout(),
        }
    }
}

// ---------------------------------------------------------------------------
// Load from YAML
// ---------------------------------------------------------------------------

impl Config {
    /// Load config from a YAML file path.
    pub fn from_yaml(path: &std::path::Path) -> anyhow::Result<Self> {
        let contents = std::fs::read_to_string(path)?;
        let config: Config = serde_yaml::from_str(&contents)?;
        Ok(config)
    }

    /// Load config from a YAML string.
    pub fn from_yaml_str(s: &str) -> anyhow::Result<Self> {
        let config: Config = serde_yaml::from_str(s)?;
        Ok(config)
    }
}