use std::sync::OnceLock;
use std::time::{Duration, Instant};

use anyhow::{bail, Result};
use serde_json::{json, Value};
use tracing::{debug, info};

use crate::config::ModelConfig;

/// The bash tool definition matching mini-swe-agent's BASH_TOOL.
pub fn bash_tool() -> &'static Value {
    static TOOL: OnceLock<Value> = OnceLock::new();
    TOOL.get_or_init(|| {
        json!({
            "type": "function",
            "function": {
                "name": "bash",
                "description": "Execute a bash command",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "command": {
                            "type": "string",
                            "description": "The bash command to execute"
                        }
                    },
                    "required": ["command"]
                }
            }
        })
    })
}

/// Response from a model query.
#[derive(Debug, Clone)]
pub struct ModelResponse {
    pub content: Option<String>,
    pub tool_calls: Vec<ToolCall>,
    pub finish_reason: String,
    pub cost: f64,
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
}

/// A parsed tool call from the model.
#[derive(Debug, Clone)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: Value,
}

/// OpenAI-compatible model client.
pub struct Model {
    config: ModelConfig,
    client: reqwest::Client,
}

impl Model {
    pub fn new(config: ModelConfig) -> Self {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(300))
            .build()
            .expect("Failed to build HTTP client");
        Self { config, client }
    }

    /// Query the model with a list of messages.
    /// Returns parsed response with tool calls.
    pub async fn query(&self, messages: &[Value]) -> Result<ModelResponse> {
        let url = format!("{}/chat/completions", self.config.api_base.trim_end_matches('/'));

        let body = json!({
            "model": self.config.model_name,
            "messages": messages,
            "tools": [bash_tool()],
            "temperature": self.config.temperature,
            "top_p": self.config.top_p,
            "max_tokens": self.config.max_tokens,
            "frequency_penalty": self.config.frequency_penalty,
            "parallel_tool_calls": self.config.parallel_tool_calls,
            "stream": false,
        });

        let start = Instant::now();
        debug!("Sending request to {} with {} messages", url, messages.len());
        let resp = self.client.post(&url).json(&body).send().await?;
        let elapsed = start.elapsed();
        debug!("Response received in {:.1}s, status={}", elapsed.as_secs_f64(), resp.status());

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            bail!("API error ({}): {}", status, text);
        }

        let data: Value = resp.json().await?;

        // Debug: log response structure
        tracing::debug!(
            "API response has_choices={}, choices_len={}",
            data.get("choices").is_some(),
            data["choices"].as_array().map(|a| a.len()).unwrap_or(0)
        );

        // Extract usage
        let usage = &data["usage"];
        let prompt_tokens = usage["prompt_tokens"].as_u64().unwrap_or(0) as u32;
        let completion_tokens = usage["completion_tokens"].as_u64().unwrap_or(0) as u32;

        // Extract choice
        let choice = &data["choices"][0];
        let finish_reason = choice["finish_reason"].as_str().unwrap_or("stop").to_string();
        let msg = &choice["message"];

        let content = msg["content"].as_str().map(|s| s.to_string());

        // Parse tool calls
        let tool_calls = msg["tool_calls"]
            .as_array()
            .map(|arr| {
                arr.iter()
                    .map(|tc| ToolCall {
                        id: tc["id"].as_str().unwrap_or("").to_string(),
                        name: tc["function"]["name"].as_str().unwrap_or("").to_string(),
                        arguments: tc["function"]["arguments"].as_str().and_then(|s| serde_json::from_str(s).ok()).unwrap_or(json!({})),
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();

        // Rough cost estimate (not critical for local models)
        let cost = 0.0;

        info!(
            "Model query: {} prompt + {} completion tokens in {:.1}s, finish_reason={}",
            prompt_tokens,
            completion_tokens,
            elapsed.as_secs_f64(),
            finish_reason,
        );

        Ok(ModelResponse {
            content,
            tool_calls,
            finish_reason,
            cost,
            prompt_tokens,
            completion_tokens,
        })
    }
}