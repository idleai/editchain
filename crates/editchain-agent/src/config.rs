use std::collections::HashMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Top-level configuration matching mini-swe-agent's YAML structure.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    /// Agent behaviour settings (step limits, templates, etc.).
    #[serde(default)]
    pub agent: AgentConfig,
    /// Model / API settings (model name, temperature, etc.).
    #[serde(default)]
    pub model: ModelConfig,
    /// Docker environment settings (image, working directory, etc.).
    #[serde(default)]
    pub environment: EnvConfig,
}

// ---------------------------------------------------------------------------
// AgentConfig
// ---------------------------------------------------------------------------

/// Agent behaviour settings: step limits, cost limits, templates, and output paths.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentConfig {
    /// Optional system prompt template (minijinja).
    pub system_template: Option<String>,
    /// Optional instance prompt template (minijinja).
    pub instance_template: Option<String>,
    /// Maximum number of agent steps before forced exit.
    #[serde(default = "default_step_limit")]
    pub step_limit: usize,
    /// Maximum total cost (in dollars) before forced exit.
    #[serde(default = "default_cost_limit")]
    pub cost_limit: f64,
    /// Wall-clock time limit in seconds (0 = no limit).
    #[serde(default = "default_wall_time")]
    pub wall_time_limit_seconds: u64,
    /// Maximum consecutive format errors before forced exit.
    #[serde(default = "default_max_format_errors")]
    pub max_consecutive_format_errors: usize,
    /// Optional path to write trajectory output.
    pub output_path: Option<PathBuf>,
}

const fn default_step_limit() -> usize {
    250
}
const fn default_cost_limit() -> f64 {
    3.0
}
const fn default_wall_time() -> u64 {
    0
}
const fn default_max_format_errors() -> usize {
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

/// Model / API configuration: model name, endpoint, sampling parameters.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelConfig {
    /// Model name (e.g. "hosted_vllm/deepseek-v4-flash").
    #[serde(default = "default_model_name")]
    pub model_name: String,
    /// Base URL for the OpenAI-compatible API.
    #[serde(default = "default_api_base")]
    pub api_base: String,
    /// API key for authentication.
    #[serde(default = "default_api_key")]
    pub api_key: String,
    /// Sampling temperature.
    #[serde(default = "default_temperature")]
    pub temperature: f64,
    /// Top-p nucleus sampling parameter.
    #[serde(default = "default_top_p")]
    pub top_p: f64,
    /// Maximum output tokens per request.
    #[serde(default = "default_max_tokens")]
    pub max_tokens: u32,
    /// Frequency penalty applied during sampling.
    #[serde(default = "default_frequency_penalty")]
    pub frequency_penalty: f64,
    /// Whether to allow parallel tool calls.
    #[serde(default = "default_parallel_tool_calls")]
    pub parallel_tool_calls: bool,
    /// Optional template for formatting observations.
    #[serde(default)]
    pub observation_template: Option<String>,
    /// Optional template for formatting format-error messages.
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
const fn default_temperature() -> f64 {
    0.2
}
const fn default_top_p() -> f64 {
    0.95
}
const fn default_max_tokens() -> u32 {
    65536
}
const fn default_frequency_penalty() -> f64 {
    0.1
}
const fn default_parallel_tool_calls() -> bool {
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

/// Docker environment configuration: image, working directory, timeouts, env vars.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnvConfig {
    /// Docker image name.
    #[serde(default)]
    pub image: String,
    /// Working directory inside the container.
    #[serde(default = "default_cwd")]
    pub cwd: String,
    /// Command execution timeout in seconds.
    #[serde(default = "default_timeout")]
    pub timeout: u64,
    /// Additional environment variables to set in the container.
    #[serde(default)]
    pub env: HashMap<String, String>,
    /// Host environment variable names to forward into the container.
    #[serde(default)]
    pub forward_env: Vec<String>,
    /// Container sleep timeout (e.g. "2h").
    #[serde(default = "default_container_timeout")]
    pub container_timeout: String,
}

fn default_cwd() -> String {
    "/testbed".into()
}
const fn default_timeout() -> u64 {
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
    ///
    /// # Errors
    ///
    /// Returns an error if the file cannot be read or the YAML is invalid.
    pub fn from_yaml(path: &std::path::Path) -> anyhow::Result<Self> {
        let contents = std::fs::read_to_string(path)?;
        let config: Self = serde_yaml::from_str(&contents)?;
        Ok(config)
    }

    /// Load config from a YAML string.
    ///
    /// # Errors
    ///
    /// Returns an error if the YAML string is invalid.
    pub fn from_yaml_str(s: &str) -> anyhow::Result<Self> {
        let config: Self = serde_yaml::from_str(s)?;
        Ok(config)
    }
}
