//! Selected row content, resolved while normalized source children are present.
//! Authored formatting belongs to the renderer; provider extraction stays here.

use crate::labels;
use std::collections::HashSet;

use editchain_core::op::{CommandStage, ToolStage};
use editchain_core::{Op, OpId, OpKind, Payload};

/// Selected text and whether it retains the complete source text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContentText {
    /// Authored text or a display summary, without a provider envelope.
    pub text: String,
    /// False for an excerpt, derived label, unavailable payload, or input that
    /// was already shortened before projection.
    pub complete: bool,
}

impl ContentText {
    fn derived(text: String) -> Self {
        Self {
            text,
            complete: false,
        }
    }

    fn from_payload(text: String, payload: &Payload, source_complete: bool) -> Self {
        let complete = source_complete
            && match payload {
                Payload::Inline(bytes) => bytes.as_slice() == text.as_bytes(),
                Payload::Empty => text.is_empty(),
                Payload::Blob(_) => false,
            };
        Self { text, complete }
    }
}

/// Content roles selected independently of formatted summary prefixes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DisplayContent {
    /// Tool name selected from a normalized Tool operation.
    pub tool_label: Option<ContentText>,
    /// Message, invocation, or derived activity summary.
    pub authored_summary: Option<ContentText>,
    /// Output selected from a completed tool/command, when available.
    pub output_preview: Option<ContentText>,
}

impl DisplayContent {
    /// A derived summary has no claim to contain a complete source payload.
    #[must_use]
    pub fn summary(text: String) -> Self {
        Self::authored(ContentText::derived(text))
    }

    fn authored(text: ContentText) -> Self {
        Self {
            tool_label: None,
            authored_summary: Some(text),
            output_preview: None,
        }
    }

    fn output(text: ContentText) -> Self {
        Self {
            tool_label: None,
            authored_summary: None,
            output_preview: Some(text),
        }
    }

    pub(super) fn with_results(&self, sub_ops: &[std::sync::Arc<Op>]) -> Self {
        let results: Vec<_> = sub_ops
            .iter()
            .filter_map(|op| labels::sub_op_content(op))
            .filter(|text| !text.is_empty())
            .collect();
        if results.is_empty() {
            return self.clone();
        }
        let mut content = self.clone();
        if content.tool_label.is_some() {
            content.output_preview = Some(ContentText::derived(labels::truncate_line(
                &results.join(" "),
            )));
        } else {
            let target = content
                .authored_summary
                .as_mut()
                .or(content.output_preview.as_mut());
            if let Some(target) = target {
                let joined = std::iter::once(target.text.as_str())
                    .chain(results.iter().map(String::as_str))
                    .filter(|text| !text.is_empty() && *text != "(no summary)")
                    .collect::<Vec<_>>()
                    .join(" ");
                *target = ContentText::derived(labels::truncate_line(&joined));
            }
        }
        content
    }
}

/// Existing aggregate-summary text and its structured display sources. This
/// immutable pair is constructed together; callers never recover fields from
/// the compatibility summary string.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SelectedContent {
    /// Established summary used by aggregate Activity labels and old clients.
    pub summary: String,
    /// Structured content for current clients.
    pub display: DisplayContent,
}

impl SelectedContent {
    /// Construct a derived aggregate or metadata label.
    #[must_use]
    pub fn summary(summary: String) -> Self {
        Self {
            display: DisplayContent::summary(summary.clone()),
            summary,
        }
    }

    fn authored(text: ContentText) -> Self {
        Self {
            summary: text.text.clone(),
            display: DisplayContent::authored(text),
        }
    }

    fn output(text: ContentText) -> Self {
        Self {
            summary: text.text.clone(),
            display: DisplayContent::output(text),
        }
    }
}

/// Content for a standalone or expanded normalized operation. The caller
/// records whether its input payloads are complete, independently of length.
#[must_use]
pub fn operation(op: &Op, source_complete: bool) -> SelectedContent {
    let summary = labels::op_summary(op);
    match &op.kind {
        OpKind::Message(message) => SelectedContent::authored(ContentText::from_payload(
            summary,
            &message.content,
            source_complete,
        )),
        OpKind::Tool(tool) => {
            let label = labels::payload_text(&tool.tool_name);
            if matches!(tool.stage, ToolStage::Finish) && label.is_empty() {
                return SelectedContent::output(ContentText::from_payload(
                    summary,
                    &tool.content,
                    source_complete,
                ));
            }
            let detail =
                labels::normalized_tool_invocation_detail(tool).unwrap_or_else(|| summary.clone());
            SelectedContent {
                summary,
                display: DisplayContent {
                    tool_label: (!label.is_empty()).then(|| {
                        ContentText::from_payload(label, &tool.tool_name, source_complete)
                    }),
                    authored_summary: Some(ContentText::from_payload(
                        detail,
                        &tool.content,
                        source_complete,
                    )),
                    output_preview: None,
                },
            }
        }
        OpKind::Command(command) => {
            let text = ContentText::from_payload(summary, &command.content, source_complete);
            if matches!(command.stage, CommandStage::Finish) {
                SelectedContent::output(text)
            } else {
                SelectedContent::authored(text)
            }
        }
        OpKind::Reflection(reflection) => SelectedContent::authored(ContentText::from_payload(
            summary,
            &reflection.summary,
            source_complete,
        )),
        OpKind::Note(note) => SelectedContent::authored(ContentText::from_payload(
            summary,
            &note.content,
            source_complete,
        )),
        OpKind::Error(error) => SelectedContent::authored(ContentText::from_payload(
            summary,
            &error.message,
            source_complete,
        )),
        OpKind::GitCommit(commit) => SelectedContent::authored(ContentText::from_payload(
            summary,
            &commit.message,
            source_complete,
        )),
        OpKind::File(_)
        | OpKind::ChainStart(_)
        | OpKind::Actor(_)
        | OpKind::Import(_)
        | OpKind::GitLink(_)
        | OpKind::Unknown(_) => SelectedContent::summary(summary),
    }
}

#[derive(Default)]
struct ImportParts {
    message: Option<SelectedContent>,
    tool: Option<SelectedContent>,
    command: Option<SelectedContent>,
    file: Option<String>,
}

impl ImportParts {
    fn add(&mut self, raw: &Op, child: &Op, siblings: &[&Op], incomplete: &HashSet<OpId>) {
        let complete = !incomplete.contains(&child.id);
        match &child.kind {
            OpKind::Message(message) if empty(self.message.as_ref()) => {
                let text = labels::message_summary(&labels::payload_text(&message.content));
                self.message = Some(SelectedContent::authored(ContentText::from_payload(
                    text,
                    &message.content,
                    complete,
                )));
            }
            OpKind::Tool(tool) if empty(self.tool.as_ref()) => {
                let label = labels::payload_text(&tool.tool_name);
                if matches!(tool.stage, ToolStage::Finish) && label.is_empty() {
                    let text = labels::tool_result_summary(&labels::payload_text(&tool.content));
                    self.tool = Some(SelectedContent::output(ContentText::from_payload(
                        text,
                        &tool.content,
                        complete,
                    )));
                } else {
                    let detail = labels::tool_invocation_detail(raw, tool);
                    self.tool = Some(SelectedContent {
                        summary: label.clone(),
                        display: DisplayContent {
                            tool_label: Some(ContentText::from_payload(
                                label,
                                &tool.tool_name,
                                complete,
                            )),
                            authored_summary: Some(ContentText::derived(detail)),
                            output_preview: None,
                        },
                    });
                }
            }
            OpKind::Command(command) if empty(self.command.as_ref()) => {
                self.command = Some(if matches!(command.stage, CommandStage::Finish) {
                    let text = labels::command_output_summary(raw, command);
                    SelectedContent::output(ContentText::derived(text))
                } else {
                    let text = labels::payload_text(&command.content);
                    SelectedContent::authored(ContentText::from_payload(
                        text,
                        &command.content,
                        complete,
                    ))
                });
            }
            OpKind::File(_) if self.file.is_none() => {
                self.file = labels::annotated_file_path(child, siblings)
                    .map(|path| format!("file: {path}"));
            }
            OpKind::ChainStart(_)
            | OpKind::Actor(_)
            | OpKind::Message(_)
            | OpKind::Tool(_)
            | OpKind::Command(_)
            | OpKind::File(_)
            | OpKind::Reflection(_)
            | OpKind::Import(_)
            | OpKind::Note(_)
            | OpKind::Error(_)
            | OpKind::GitCommit(_)
            | OpKind::GitLink(_)
            | OpKind::Unknown(_) => {}
        }
    }

    fn select(self) -> Option<SelectedContent> {
        if let Some(message) = self.message.filter(|text| !text.summary.is_empty()) {
            return Some(message);
        }
        if let Some(mut tool) = self.tool.filter(|text| !text.summary.is_empty()) {
            if tool.display.tool_label.is_some() {
                let detail = tool.display.authored_summary.as_mut()?;
                if detail.text.is_empty() {
                    if let Some(command) = &self.command {
                        detail.text.clone_from(&command.summary);
                    }
                }
                tool.summary = if detail.text.is_empty() {
                    format!("tool: {}", tool.summary)
                } else {
                    format!("tool: {} {}", tool.summary, detail.text)
                };
            }
            return Some(tool);
        }
        if let Some(mut command) = self.command.filter(|text| !text.summary.is_empty()) {
            if command.display.output_preview.is_none() {
                command.summary = format!("$ {}", command.summary);
                if let Some(text) = &mut command.display.authored_summary {
                    text.text.clone_from(&command.summary);
                    text.complete = false;
                }
            }
            return Some(command);
        }
        self.file.map(SelectedContent::summary)
    }
}

fn empty(content: Option<&SelectedContent>) -> bool {
    content.is_none_or(|content| content.summary.is_empty())
}

pub(super) fn collapsed_import(
    raw: &Op,
    children: Option<&Vec<&Op>>,
    incomplete: &HashSet<OpId>,
) -> SelectedContent {
    if let Some(record) = crate::human::work_record(raw) {
        return SelectedContent::summary(record.summary);
    }
    let mut parts = ImportParts::default();
    if let Some(children) = children {
        for child in children {
            parts.add(raw, child, children, incomplete);
        }
    }
    parts.select().unwrap_or_else(|| match &raw.kind {
        OpKind::Import(import) => SelectedContent::summary(labels::raw_import_label(import)),
        OpKind::ChainStart(_)
        | OpKind::Actor(_)
        | OpKind::Message(_)
        | OpKind::Tool(_)
        | OpKind::Command(_)
        | OpKind::File(_)
        | OpKind::Reflection(_)
        | OpKind::Note(_)
        | OpKind::Error(_)
        | OpKind::GitCommit(_)
        | OpKind::GitLink(_)
        | OpKind::Unknown(_) => operation(raw, !incomplete.contains(&raw.id)),
    })
}

/// Content for a Git entity, whose immutable message may not be an operation.
pub(super) fn git_message(message: &Payload, fallback: String) -> DisplayContent {
    match message {
        Payload::Inline(bytes) => DisplayContent::authored(ContentText::from_payload(
            String::from_utf8_lossy(bytes).into_owned(),
            message,
            true,
        )),
        Payload::Empty | Payload::Blob(_) => DisplayContent::summary(fallback),
    }
}
