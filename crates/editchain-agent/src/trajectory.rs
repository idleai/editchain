use std::path::Path;

use anyhow::Result;
use editchain_core::*;
use serde_json::Value;

/// Records agent trajectory as editchain operations.
pub struct TrajectoryRecorder {
    ops: Vec<Op>,
}

impl TrajectoryRecorder {
    pub fn new() -> Self {
        Self { ops: Vec::new() }
    }

    /// Record a message as an editchain operation.
    pub fn record_message(&mut self, role: &str, content: &str) {
        let op = Op {
            id: self.next_id(),
            parents: parents::ParentSet::None,
            actor: ActorId(0),
            clock: clock::Clock::UnixMs(
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis() as u64,
            ),
            scope: scope::ScopeRef::None,
            tags: tags::Tags::MESSAGE,
            kind: op::OpKind::Message(op::MessageOp {
                content: payload::Payload::Inline(content.as_bytes().to_vec()),
                content_type: payload::Payload::Inline(role.as_bytes().to_vec()),
            }),
        };
        self.ops.push(op);
    }

    /// Record a tool call.
    pub fn record_tool_call(&mut self, tool_call_id: &str, tool_name: &str, arguments: &Value) {
        let op = Op {
            id: self.next_id(),
            parents: parents::ParentSet::None,
            actor: ActorId(0),
            clock: clock::Clock::UnixMs(
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis() as u64,
            ),
            scope: scope::ScopeRef::None,
            tags: tags::Tags::TOOL,
            kind: op::OpKind::Tool(op::ToolOp {
                tool_call_id: payload::Payload::Inline(tool_call_id.as_bytes().to_vec()),
                tool_name: payload::Payload::Inline(tool_name.as_bytes().to_vec()),
                stage: op::ToolStage::Start,
                content: payload::Payload::Inline(
                    serde_json::to_vec(arguments).unwrap_or_default(),
                ),
            }),
        };
        self.ops.push(op);
    }

    /// Record a command output.
    pub fn record_command_output(&mut self, command_id: &str, output: &str, returncode: i32) {
        let content = format!("<returncode>{}</returncode>\n<output>\n{}</output>", returncode, output);
        let op = Op {
            id: self.next_id(),
            parents: parents::ParentSet::None,
            actor: ActorId(0),
            clock: clock::Clock::UnixMs(
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis() as u64,
            ),
            scope: scope::ScopeRef::None,
            tags: tags::Tags::COMMAND,
            kind: op::OpKind::Command(op::CommandOp {
                command_id: payload::Payload::Inline(command_id.as_bytes().to_vec()),
                content: payload::Payload::Inline(content.as_bytes().to_vec()),
                stage: op::CommandStage::Finish,
            }),
        };
        self.ops.push(op);
    }

    /// Record exit status and submission.
    pub fn record_exit(&mut self, exit_status: &str, submission: &str) {
        let op = Op {
            id: self.next_id(),
            parents: parents::ParentSet::None,
            actor: ActorId(0),
            clock: clock::Clock::UnixMs(
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis() as u64,
            ),
            scope: scope::ScopeRef::None,
            tags: tags::Tags::NOTE,
            kind: op::OpKind::Note(op::NoteOp {
                target_ids: Vec::new(),
                relationship: op::NoteRelationship::Explains,
                content: payload::Payload::Inline(
                    serde_json::json!({"exit_status": exit_status, "submission": submission})
                        .to_string()
                        .as_bytes()
                        .to_vec(),
                ),
            }),
        };
        self.ops.push(op);
    }

    /// Save trajectory to a JSON file (editchain ops format).
    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let json_ops: Vec<Value> = self.ops.iter().map(|op| {
            serde_json::to_value(op).unwrap_or_default()
        }).collect();
        let data = serde_json::json!({
            "trajectory_format": "editchain-1.0",
            "ops": json_ops,
        });
        std::fs::write(path, serde_json::to_string_pretty(&data)?)?;
        Ok(())
    }

    fn next_id(&self) -> OpId {
        OpId::new(NodeId(0), 0, self.ops.len() as u64)
    }
}

impl Default for TrajectoryRecorder {
    fn default() -> Self {
        Self::new()
    }
}