use std::collections::HashMap;
use std::time::Instant;

use anyhow::{bail, Result};
use minijinja::Environment as JEnv;
use serde_json::Value;
use tracing::{info, warn};

use crate::config::{AgentConfig, Config};
use crate::docker_env::{DockerEnvironment, ExecOutput};
use crate::model::{Model, ModelResponse};
use crate::trajectory::TrajectoryRecorder;

/// The agent loop — parallels mini-swe-agent's `DefaultAgent`.
#[derive(Debug)]
pub struct Agent {
    config: AgentConfig,
    model: Model,
    env: DockerEnvironment,
    messages: Vec<Value>,
    trajectory: TrajectoryRecorder,
    n_calls: usize,
    n_consecutive_format_errors: usize,
    cost: f64,
    start_time: Instant,
}

impl Agent {
    /// Create a new agent with the given configuration and environment.
    #[must_use]
    pub fn new(config: Config, env: DockerEnvironment) -> Self {
        let agent_config = config.agent;
        let model = Model::new(config.model);
        Self {
            config: agent_config,
            model,
            env,
            messages: Vec::new(),
            trajectory: TrajectoryRecorder::new(),
            n_calls: 0,
            n_consecutive_format_errors: 0,
            cost: 0.0,
            start_time: Instant::now(),
        }
    }

    /// Run the agent on a task. Returns (`exit_status`, submission).
    #[expect(
        clippy::arithmetic_side_effects,
        reason = "n_consecutive_format_errors is a bounded counter reset on success"
    )]
    pub async fn run(&mut self, task: &str) -> (String, String) {
        info!("Starting agent run for task");

        // Render and add system + user messages
        let system_msg = self.render_template(
            self.config
                .system_template
                .as_deref()
                .unwrap_or("You are a helpful assistant."),
            task,
        );
        let instance_msg = self.render_template(
            self.config
                .instance_template
                .as_deref()
                .unwrap_or("{{task}}"),
            task,
        );

        self.add_message("system", &system_msg);
        self.add_message("user", &instance_msg);

        let result = loop {
            match self.step().await {
                Ok(()) => {
                    self.n_consecutive_format_errors = 0;
                }
                Err(e) => {
                    // Check if it's an exit condition
                    if let Some(exit) = e.downcast_ref::<ExitCondition>() {
                        info!("Agent exiting: {}", exit.status);
                        self.trajectory.record_exit(&exit.status, &exit.submission);
                        break (exit.status.clone(), exit.submission.clone());
                    }
                    // Format error handling
                    if let Some(fe) = e.downcast_ref::<FormatError>() {
                        self.n_consecutive_format_errors += 1;
                        if self.config.max_consecutive_format_errors > 0
                            && self.n_consecutive_format_errors
                                >= self.config.max_consecutive_format_errors
                        {
                            warn!(
                                "Repeated format errors ({}), exiting",
                                self.n_consecutive_format_errors
                            );
                            self.trajectory.record_exit("RepeatedFormatError", "");
                            break ("RepeatedFormatError".into(), String::new());
                        }
                        self.add_message("user", &fe.message);
                        continue;
                    }
                    // Limits exceeded
                    if let Some(le) = e.downcast_ref::<LimitsExceeded>() {
                        self.trajectory.record_exit(&le.status, &le.submission);
                        break (le.status.clone(), le.submission.clone());
                    }
                    // Other error
                    warn!("Unhandled error in agent loop: {:?}", e);
                    self.trajectory.record_exit("Error", &format!("{e:?}"));
                    break ("Error".into(), format!("{e:?}"));
                }
            }

            // Check limits
            if let Some((status, submission)) = self.check_limits() {
                self.trajectory.record_exit(&status, &submission);
                break (status, submission);
            }

            // Save trajectory after each step
            if let Some(path) = &self.config.output_path {
                let path = path.clone();
                if let Err(e) = self.trajectory.save(&path) {
                    tracing::error!("Failed to save trajectory: {:?}", e);
                }
            }
        };

        // Final save before returning
        if let Some(path) = &self.config.output_path {
            if let Err(e) = self.trajectory.save(path) {
                tracing::error!("Failed to save trajectory: {:?}", e);
            }
        }

        result
    }

    async fn step(&mut self) -> Result<()> {
        let response = self.query().await?;
        self.execute_actions(&response)?;
        Ok(())
    }

    #[expect(
        clippy::arithmetic_side_effects,
        clippy::indexing_slicing,
        let_underscore_drop,
        reason = "n_calls is a bounded counter; msg indexing targets known keys in freshly created json; let _ discards insert return value"
    )]
    async fn query(&mut self) -> Result<ModelResponse> {
        if self.config.step_limit > 0 && self.n_calls >= self.config.step_limit {
            bail!(LimitsExceeded {
                status: "LimitsExceeded".into(),
                submission: String::new(),
            });
        }
        if self.config.cost_limit > 0.0 && self.cost >= self.config.cost_limit {
            bail!(LimitsExceeded {
                status: "CostLimitExceeded".into(),
                submission: String::new(),
            });
        }

        self.n_calls += 1;
        info!("Model call #{}", self.n_calls);

        // Prepare messages for API (strip extra fields)
        let api_messages: Vec<Value> = self
            .messages
            .iter()
            .map(|m| {
                let mut clean = serde_json::Map::new();
                if let Some(role) = m["role"].as_str() {
                    let _: Option<Value> = clean.insert("role".into(), Value::String(role.into()));
                }
                if let Some(content) = m["content"].as_str() {
                    let _: Option<Value> =
                        clean.insert("content".into(), Value::String(content.into()));
                }
                // Include tool_calls if present
                if let Some(tcs) = m.get("tool_calls") {
                    let _: Option<Value> = clean.insert("tool_calls".into(), tcs.clone());
                }
                // Include tool_call_id if present
                if let Some(tcid) = m.get("tool_call_id") {
                    let _: Option<Value> = clean.insert("tool_call_id".into(), tcid.clone());
                }
                Value::Object(clean)
            })
            .collect();

        // Debug: log message count and last message role
        tracing::debug!(
            "Sending {} messages, last role: {:?}, last has tool_calls: {}",
            api_messages.len(),
            api_messages
                .last()
                .and_then(|m| m["role"].as_str().map(ToString::to_string)),
            api_messages
                .last()
                .is_some_and(|m| m.get("tool_calls").is_some())
        );

        let response = match self.model.query(&api_messages).await {
            Ok(r) => r,
            Err(e) => {
                tracing::error!("Model query failed: {:?}", e);
                bail!(FormatError {
                    message: format!("Model query failed: {e}"),
                });
            }
        };

        self.cost += response.cost;

        // Record assistant message with tool calls
        let mut msg = serde_json::json!({
            "role": "assistant",
        });
        if let Some(content) = &response.content {
            msg["content"] = Value::String(content.clone());
        }
        if !response.tool_calls.is_empty() {
            let tcs: Vec<Value> = response
                .tool_calls
                .iter()
                .map(|tc| {
                    serde_json::json!({
                        "id": tc.id,
                        "type": "function",
                        "function": {
                            "name": tc.name,
                            "arguments": tc.arguments.to_string(),
                        }
                    })
                })
                .collect();
            msg["tool_calls"] = Value::Array(tcs);
        }
        self.messages.push(msg);

        // Record tool calls in trajectory
        for tc in &response.tool_calls {
            self.trajectory
                .record_tool_call(&tc.id, &tc.name, &tc.arguments);
        }

        // Validate tool calls
        if response.tool_calls.is_empty() {
            let finish_reason = &response.finish_reason;
            let error_msg = if finish_reason == "length" || finish_reason == "tool_calls" {
                format!(
                    "Your previous response reached the output token limit (finish_reason={finish_reason}) before you produced a tool call, so it was cut off. Respond more concisely and finish with exactly one bash tool call."
                )
            } else {
                format!(
                    "No tool calls found in the response (finish_reason={finish_reason}). Every response MUST include at least one bash tool call.\n\nHere is general guidance on how to submit correct toolcalls:\n\nEvery response needs to use the 'bash' tool at least once to execute commands.\n\nCall the bash tool with your command as the argument:\n- Tool: bash\n- Arguments: {{\"command\": \"your_command_here\"}}\n\nIf you have completed your assignment, please consult the first message about how to submit your solution (you will not be able to continue working on this task after that)."
                )
            };
            tracing::warn!(
                "FormatError: finish_reason={}, content_len={}",
                finish_reason,
                response.content.as_ref().map_or(0, String::len)
            );
            bail!(FormatError { message: error_msg });
        }

        Ok(response)
    }

    #[expect(
        clippy::indexing_slicing,
        let_underscore_drop,
        reason = "tc.arguments is a serde_json::Value Map; 'command' key is expected; let _ discards insert return value"
    )]
    fn execute_actions(&mut self, response: &ModelResponse) -> Result<()> {
        for tc in &response.tool_calls {
            if tc.name != "bash" {
                bail!(FormatError {
                    message: format!("Unknown tool '{}'. Only 'bash' is supported.", tc.name),
                });
            }

            let command = tc.arguments["command"].as_str().unwrap_or("");
            let mut action = HashMap::new();
            let _: Option<String> = action.insert("command".to_string(), command.to_string());

            info!("Executing bash command (tool_call_id={})", tc.id);

            let output = self.env.execute(&action);

            // Check for submission marker
            if output
                .output
                .starts_with("COMPLETE_TASK_AND_SUBMIT_FINAL_OUTPUT")
                && output.returncode == 0
            {
                let submission = output
                    .output
                    .trim_start()
                    .strip_prefix("COMPLETE_TASK_AND_SUBMIT_FINAL_OUTPUT")
                    .unwrap_or("")
                    .trim()
                    .to_string();
                self.trajectory.record_exit("Submitted", &submission);
                bail!(ExitCondition {
                    status: "Submitted".into(),
                    submission,
                });
            }

            // Format observation message
            let obs = Self::format_observation(&output);
            self.add_tool_result(&tc.id, &obs);
        }
        Ok(())
    }

    #[expect(
        clippy::format_push_string,
        clippy::string_slice,
        reason = "format_push_string is acceptable in agent harness; slicing uses saturating_sub so it never panics"
    )]
    fn format_observation(output: &ExecOutput) -> String {
        let mut result = String::new();

        if !output.exception_info.is_empty() {
            result.push_str(&format!(
                "<exception>{}</exception>\n",
                output.exception_info
            ));
        }
        result.push_str(&format!("<returncode>{}</returncode>\n", output.returncode));

        if output.output.len() < 10000 {
            result.push_str(&format!("<output>\n{}</output>", output.output));
        } else {
            result
                .push_str("<warning>\nThe output of your last command was too long.\n</warning>\n");
            result.push_str(&format!(
                "<output_head>\n{}</output_head>\n",
                &output.output[..5000]
            ));
            result.push_str(&format!(
                "<output_tail>\n{}</output_tail>",
                &output.output[output.output.len().saturating_sub(5000)..]
            ));
        }

        result
    }

    fn add_message(&mut self, role: &str, content: &str) {
        self.messages.push(serde_json::json!({
            "role": role,
            "content": content,
        }));
        self.trajectory.record_message(role, content);
    }

    fn add_tool_result(&mut self, tool_call_id: &str, content: &str) {
        self.messages.push(serde_json::json!({
            "role": "tool",
            "tool_call_id": tool_call_id,
            "content": content,
        }));
        self.trajectory
            .record_command_output(tool_call_id, content, 0);
    }

    #[expect(
        clippy::unwrap_used,
        reason = "template string is hardcoded and always valid"
    )]
    fn render_template(&self, template_str: &str, task: &str) -> String {
        let mut env = JEnv::new();
        env.set_undefined_behavior(minijinja::UndefinedBehavior::Strict);
        env.add_template("tpl", template_str).unwrap();
        let tmpl = env.get_template("tpl").unwrap();
        tmpl.render(minijinja::context! {
            task => task,
            n_model_calls => self.n_calls,
            model_cost => self.cost,
            elapsed_seconds => self.start_time.elapsed().as_secs(),
        })
        .unwrap_or_else(|e| format!("Template error: {e}"))
    }

    fn check_limits(&self) -> Option<(String, String)> {
        if self.config.wall_time_limit_seconds > 0
            && self.start_time.elapsed().as_secs() >= self.config.wall_time_limit_seconds
        {
            Some(("TimeExceeded".into(), String::new()))
        } else {
            None
        }
    }
}

// ---------------------------------------------------------------------------
// Error types for agent flow control
// ---------------------------------------------------------------------------

/// Exit condition used to signal successful task completion.
#[derive(Debug)]
pub struct ExitCondition {
    /// Human-readable exit status label.
    pub status: String,
    /// Final submission output from the agent.
    pub submission: String,
}

impl std::fmt::Display for ExitCondition {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "ExitCondition({})", self.status)
    }
}

impl std::error::Error for ExitCondition {}

/// Error indicating the model produced a malformed response.
#[derive(Debug)]
pub struct FormatError {
    /// Human-readable error message to feed back to the model.
    pub message: String,
}

impl std::fmt::Display for FormatError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "FormatError: {}", self.message)
    }
}

impl std::error::Error for FormatError {}

/// Error indicating the agent has exceeded one of its configured limits.
#[derive(Debug)]
pub struct LimitsExceeded {
    /// Human-readable limit-exceeded status label.
    pub status: String,
    /// Partial submission, if any, collected before the limit was hit.
    pub submission: String,
}

impl std::fmt::Display for LimitsExceeded {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "LimitsExceeded({})", self.status)
    }
}

impl std::error::Error for LimitsExceeded {}
