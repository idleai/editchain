//! Shared content semantics with explicit legacy and occurrence representations.

use editchain_core::{
    CommandOp, CommandStage, FileEdit, FileOp, FileStage, FrontierSet, MessageOp, NoteOp,
    NoteRelationship, Op, OpId, OpKind, ParentSet, Payload, ReflectionOp, Tags, ToolOp, ToolStage,
    WindowRef,
};
use serde::Serialize;
use serde_json::Value;

use super::envelope::{CcContentBlock, CcEnvelope};
use crate::ids::{derive_node_id, derive_path_id, SourcePosition, SourceStream};
use crate::sink::{payload_for, BlobSink};
use crate::ImportError;

#[derive(Clone, Copy)]
pub(super) enum Contract {
    Legacy,
    BlocksV1,
}

#[derive(Serialize)]
enum Slot {
    Block(usize),
    Command(usize),
    UserText,
    Record,
}

struct Builder<'a> {
    raw: &'a Op,
    contract: Contract,
    blobs: &'a mut dyn BlobSink,
    ops: Vec<Op>,
}

impl Builder<'_> {
    fn payload(&mut self, bytes: &[u8]) -> Result<Payload, ImportError> {
        match self.contract {
            Contract::Legacy => Ok(Payload::Inline(bytes.to_vec())),
            Contract::BlocksV1 => payload_for(bytes, self.blobs),
        }
    }

    fn push(&mut self, slot: Slot, tags: Tags, kind: OpKind) -> Result<(), ImportError> {
        let id = match self.contract {
            Contract::Legacy => {
                let lane = self
                    .ops
                    .len()
                    .checked_add(1)
                    .and_then(|len| u16::try_from(len).ok())
                    .ok_or_else(|| {
                        ImportError::OpSink("legacy Claude content lanes exhausted".into())
                    })?;
                SourceStream::new(self.raw.id.node, self.raw.id.boot)
                    .op_from_position(SourcePosition::derived(self.raw.id.seq >> 16, lane))?
            }
            Contract::BlocksV1 => OpId {
                node: derive_node_id(&serde_json::to_string(&(
                    super::materialize::CONTRACT,
                    self.raw.id.node,
                    slot,
                ))?),
                ..self.raw.id
            },
        };
        self.ops.push(Op {
            id,
            parents: ParentSet::One(self.raw.id),
            actor: self.raw.actor,
            clock: self.raw.clock,
            scope: self.raw.scope,
            tags,
            kind,
        });
        Ok(())
    }

    fn message(&mut self, slot: Slot, tags: Tags, text: &str) -> Result<(), ImportError> {
        let content = self.payload(text.as_bytes())?;
        self.push(
            slot,
            tags | Tags::MESSAGE,
            OpKind::Message(MessageOp {
                content,
                content_type: Payload::Inline(b"text/markdown".to_vec()),
            }),
        )
    }

    fn note(&mut self, tags: Tags, text: &str) -> Result<(), ImportError> {
        let content = self.payload(text.as_bytes())?;
        self.push(
            Slot::Record,
            tags,
            OpKind::Note(NoteOp {
                target_ids: Vec::new(),
                relationship: NoteRelationship::Explains,
                content,
            }),
        )
    }
}

pub(super) fn normalize_content(
    env: &CcEnvelope,
    raw: &Op,
    include_thinking: bool,
    contract: Contract,
    blobs: &mut dyn BlobSink,
) -> Result<Vec<Op>, ImportError> {
    let mut builder = Builder {
        raw,
        contract,
        blobs,
        ops: Vec::new(),
    };
    match env.record_type.as_str() {
        "user" => user_content(env, &mut builder)?,
        "assistant" => assistant_content(env, include_thinking, &mut builder)?,
        "attachment" => attachment_content(env, &mut builder)?,
        "system" => system_content(env, &mut builder)?,
        "mode" if !env.mode.is_empty() => {
            builder.note(Tags::IMPORT | Tags::NOTE, &format!("mode={}", env.mode))?;
        }
        "ai-title" if !env.ai_title.is_empty() => {
            builder.note(Tags::IMPORT | Tags::NOTE, &env.ai_title)?;
        }
        _ => {}
    }
    Ok(builder.ops)
}

fn user_content(env: &CcEnvelope, builder: &mut Builder<'_>) -> Result<(), ImportError> {
    let Some(message) = &env.message else {
        return Ok(());
    };
    for (index, block) in message.content.iter().enumerate() {
        if let CcContentBlock::ToolResult {
            tool_use_id,
            content,
            ..
        } = block
        {
            let tool_call_id = builder.payload(tool_use_id.as_bytes())?;
            let content = builder.payload(content.as_bytes())?;
            builder.push(
                Slot::Block(index),
                Tags::HUMAN | Tags::TOOL,
                OpKind::Tool(ToolOp {
                    tool_call_id,
                    tool_name: Payload::Empty,
                    stage: ToolStage::Finish,
                    content,
                }),
            )?;
        }
    }
    // Claude user records combine all text after their tool results.
    let text: String = message
        .content
        .iter()
        .filter_map(|block| match block {
            CcContentBlock::Text { text } => Some(text.as_str()),
            CcContentBlock::ToolUse { .. }
            | CcContentBlock::ToolResult { .. }
            | CcContentBlock::Thinking { .. } => None,
        })
        .collect();
    if !text.is_empty() {
        builder.message(Slot::UserText, Tags::HUMAN, &text)?;
    }
    Ok(())
}

fn assistant_content(
    env: &CcEnvelope,
    include_thinking: bool,
    builder: &mut Builder<'_>,
) -> Result<(), ImportError> {
    let Some(message) = &env.message else {
        return Ok(());
    };
    for (index, block) in message.content.iter().enumerate() {
        match block {
            CcContentBlock::Text { text } if !text.trim().is_empty() => {
                builder.message(Slot::Block(index), Tags::AGENT, text)?;
            }
            CcContentBlock::ToolUse { id, name, input } => {
                tool_use(index, id, name, input, builder)?;
            }
            CcContentBlock::Thinking { thinking, .. }
                if include_thinking && !thinking.trim().is_empty() =>
            {
                builder.message(Slot::Block(index), Tags::PRIVATE, thinking)?;
            }
            CcContentBlock::Text { .. }
            | CcContentBlock::ToolResult { .. }
            | CcContentBlock::Thinking { .. } => {}
        }
    }
    Ok(())
}

fn tool_use(
    index: usize,
    id: &str,
    name: &str,
    input: &Value,
    builder: &mut Builder<'_>,
) -> Result<(), ImportError> {
    let input_bytes = serde_json::to_vec(input)?;
    let tool_call_id = builder.payload(id.as_bytes())?;
    let tool_name = builder.payload(name.as_bytes())?;
    let content = builder.payload(&input_bytes)?;
    builder.push(
        Slot::Block(index),
        Tags::AGENT | Tags::TOOL,
        OpKind::Tool(ToolOp {
            tool_call_id,
            tool_name,
            stage: ToolStage::Start,
            content,
        }),
    )?;
    if name == "Bash" || name == "PowerShell" {
        let command = input.get("command").and_then(Value::as_str).unwrap_or("");
        let command_id = builder.payload(id.as_bytes())?;
        let content = builder.payload(command.as_bytes())?;
        builder.push(
            Slot::Command(index),
            Tags::AGENT | Tags::COMMAND,
            OpKind::Command(CommandOp {
                command_id,
                content,
                stage: CommandStage::Start,
            }),
        )?;
    }
    Ok(())
}

fn attachment_content(env: &CcEnvelope, builder: &mut Builder<'_>) -> Result<(), ImportError> {
    if env.attachment_type == "file" || env.attachment_type == "file_content" {
        builder.push(
            Slot::Record,
            Tags::FILE | Tags::IMPORT,
            OpKind::File(FileOp {
                path: derive_path_id(""),
                stage: FileStage::Observed,
                base: None,
                after: None,
                edit: FileEdit::None,
            }),
        )?;
    }
    if matches!(
        env.attachment_type.as_str(),
        "opened_file_in_ide" | "selected_lines_in_ide" | "already_read_file" | "plan_mode_reentry"
    ) {
        builder.note(
            Tags::FILE | Tags::IMPORT | Tags::NOTE,
            &format!("attachment={}", env.attachment_type),
        )?;
    }
    Ok(())
}

fn system_content(env: &CcEnvelope, builder: &mut Builder<'_>) -> Result<(), ImportError> {
    match env.subtype.as_str() {
        "compact_boundary" | "away_summary" => {
            builder.push(
                Slot::Record,
                Tags::REFLECTION | Tags::IMPORT,
                OpKind::Reflection(ReflectionOp {
                    scope: builder.raw.scope,
                    covers: FrontierSet(Vec::new()),
                    window: WindowRef {
                        start_seq: 0,
                        end_seq: 0,
                    },
                    summary: Payload::Empty,
                    anchors: Payload::Empty,
                }),
            )?;
        }
        "api_error" | "informational" => {
            let text = env
                .error
                .as_ref()
                .and_then(|error| error.get("message"))
                .and_then(Value::as_str)
                .unwrap_or("");
            if !text.is_empty() {
                builder.note(Tags::ERROR | Tags::IMPORT, text)?;
            }
        }
        _ => {}
    }
    Ok(())
}
