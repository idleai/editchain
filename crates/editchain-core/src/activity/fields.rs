//! Exhaustive payload inventory for indexing, search, and replication.

use super::Kind;
use crate::{ContentId, FileEdit, Payload};
use serde::{Deserialize, Serialize};

/// Address of a payload in a schema-three record.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Field {
    /// Display label.
    Label,
    /// Session settings or author metadata.
    Metadata,
    /// One message block's bytes.
    MessageBlock(usize),
    /// One message block's MIME type.
    MediaType(usize),
    /// Summary anchors.
    SummaryAnchors,
    /// Native call ID.
    CallId,
    /// Tool name.
    ToolName,
    /// Invocation arguments.
    Arguments,
    /// Call output.
    Output,
    /// Output MIME type.
    OutputType,
    /// Terminal command.
    Command,
    /// Working directory.
    Cwd,
    /// Completion details.
    Outcome,
    /// Recorded file path.
    Path,
    /// Rename destination.
    RenamedTo,
    /// Replacement or patch bytes.
    Edit,
    /// Inserted text from an editor change, in recorded order.
    TextEdit(usize),
    /// Commit or note text, link annotation, or original record bytes.
    Content,
    /// Note diagnostic code.
    Code,
    /// Recorded role text.
    Role,
    /// Git signature name: false for author, true for committer.
    GitName(bool),
    /// Git signature email.
    GitEmail(bool),
    /// Imported Git ref.
    GitImportedRef(usize),
    /// Live Git ref.
    GitLiveRef(usize),
}

impl Kind {
    /// Payloads in schema order, including all attachments and completion details.
    #[must_use]
    pub fn fields(&self) -> Vec<(Field, &Payload)> {
        match self {
            Self::Session(value) => vec![
                (Field::Label, &value.label),
                (Field::Metadata, &value.settings),
            ],
            Self::Turn(value) => value
                .outcome
                .iter()
                .map(|result| (Field::Outcome, &result.detail))
                .collect(),
            Self::Message(value) => {
                let mut fields = Vec::new();
                for (index, block) in value.blocks.iter().enumerate() {
                    fields.push((Field::MessageBlock(index), &block.content));
                    fields.push((Field::MediaType(index), &block.media_type));
                }
                fields.extend(
                    value
                        .coverage
                        .iter()
                        .map(|coverage| (Field::SummaryAnchors, &coverage.anchors)),
                );
                fields.extend(
                    value
                        .outcome
                        .iter()
                        .map(|result| (Field::Outcome, &result.detail)),
                );
                fields
            }
            Self::Tool(value) => {
                let mut fields = vec![
                    (Field::CallId, &value.native_call),
                    (Field::ToolName, &value.name),
                    (Field::Arguments, &value.arguments),
                ];
                if let Some(output) = &value.output {
                    fields.extend([
                        (Field::Output, &output.content),
                        (Field::OutputType, &output.media_type),
                    ]);
                }
                if let Some(terminal) = &value.terminal {
                    fields.extend([
                        (Field::Command, &terminal.command),
                        (Field::Cwd, &terminal.cwd),
                    ]);
                }
                fields.extend(
                    value
                        .outcome
                        .iter()
                        .map(|result| (Field::Outcome, &result.detail)),
                );
                fields
            }
            Self::File(value) => {
                let mut fields = vec![(Field::Path, &value.name)];
                fields.extend(value.renamed_to.iter().map(|path| (Field::RenamedTo, path)));
                fields.extend(
                    value
                        .text_edits
                        .iter()
                        .enumerate()
                        .map(|(index, edit)| (Field::TextEdit(index), &edit.text)),
                );
                match &value.edit {
                    FileEdit::ReplaceBytes { bytes, .. } | FileEdit::UnifiedDiff(bytes) => {
                        fields.push((Field::Edit, bytes));
                    }
                    FileEdit::None | FileEdit::Blob(_) => {}
                }
                fields
            }
            Self::Commit(value) => {
                let mut fields = vec![
                    (Field::Content, &value.message),
                    (Field::GitName(false), &value.author.name),
                    (Field::GitEmail(false), &value.author.email),
                    (Field::GitName(true), &value.committer.name),
                    (Field::GitEmail(true), &value.committer.email),
                ];
                fields.extend(
                    value
                        .imported_refs
                        .iter()
                        .enumerate()
                        .map(|(index, value)| (Field::GitImportedRef(index), value)),
                );
                fields.extend(
                    value
                        .live_refs
                        .iter()
                        .enumerate()
                        .map(|(index, value)| (Field::GitLiveRef(index), value)),
                );
                fields
            }
            Self::Note(value) => vec![(Field::Content, &value.content), (Field::Code, &value.code)],
            Self::Author(value) => vec![
                (Field::Label, &value.label),
                (Field::Role, &value.native_role),
                (Field::Metadata, &value.metadata),
            ],
            Self::Link(value) => vec![(Field::Content, &value.content)],
            Self::Original(value) => vec![(Field::Content, &value.bytes)],
        }
    }

    /// Mutable payloads for bounded display copies; never rewrite stored records.
    #[must_use]
    pub fn fields_mut(&mut self) -> Vec<(Field, &mut Payload)> {
        match self {
            Self::Session(value) => vec![
                (Field::Label, &mut value.label),
                (Field::Metadata, &mut value.settings),
            ],
            Self::Turn(value) => value
                .outcome
                .iter_mut()
                .map(|result| (Field::Outcome, &mut result.detail))
                .collect(),
            Self::Message(value) => {
                let mut fields = Vec::new();
                for (index, block) in value.blocks.iter_mut().enumerate() {
                    fields.push((Field::MessageBlock(index), &mut block.content));
                    fields.push((Field::MediaType(index), &mut block.media_type));
                }
                fields.extend(
                    value
                        .coverage
                        .iter_mut()
                        .map(|coverage| (Field::SummaryAnchors, &mut coverage.anchors)),
                );
                fields.extend(
                    value
                        .outcome
                        .iter_mut()
                        .map(|result| (Field::Outcome, &mut result.detail)),
                );
                fields
            }
            Self::Tool(value) => {
                let mut fields = vec![
                    (Field::CallId, &mut value.native_call),
                    (Field::ToolName, &mut value.name),
                    (Field::Arguments, &mut value.arguments),
                ];
                if let Some(output) = &mut value.output {
                    fields.extend([
                        (Field::Output, &mut output.content),
                        (Field::OutputType, &mut output.media_type),
                    ]);
                }
                if let Some(terminal) = &mut value.terminal {
                    fields.extend([
                        (Field::Command, &mut terminal.command),
                        (Field::Cwd, &mut terminal.cwd),
                    ]);
                }
                fields.extend(
                    value
                        .outcome
                        .iter_mut()
                        .map(|result| (Field::Outcome, &mut result.detail)),
                );
                fields
            }
            Self::File(value) => {
                let mut fields = vec![(Field::Path, &mut value.name)];
                fields.extend(
                    value
                        .renamed_to
                        .iter_mut()
                        .map(|path| (Field::RenamedTo, path)),
                );
                fields.extend(
                    value
                        .text_edits
                        .iter_mut()
                        .enumerate()
                        .map(|(index, edit)| (Field::TextEdit(index), &mut edit.text)),
                );
                match &mut value.edit {
                    FileEdit::ReplaceBytes { bytes, .. } | FileEdit::UnifiedDiff(bytes) => {
                        fields.push((Field::Edit, bytes));
                    }
                    FileEdit::None | FileEdit::Blob(_) => {}
                }
                fields
            }
            Self::Commit(value) => {
                let mut fields = vec![
                    (Field::Content, &mut value.message),
                    (Field::GitName(false), &mut value.author.name),
                    (Field::GitEmail(false), &mut value.author.email),
                    (Field::GitName(true), &mut value.committer.name),
                    (Field::GitEmail(true), &mut value.committer.email),
                ];
                fields.extend(
                    value
                        .imported_refs
                        .iter_mut()
                        .enumerate()
                        .map(|(index, value)| (Field::GitImportedRef(index), value)),
                );
                fields.extend(
                    value
                        .live_refs
                        .iter_mut()
                        .enumerate()
                        .map(|(index, value)| (Field::GitLiveRef(index), value)),
                );
                fields
            }
            Self::Note(value) => vec![
                (Field::Content, &mut value.content),
                (Field::Code, &mut value.code),
            ],
            Self::Author(value) => vec![
                (Field::Label, &mut value.label),
                (Field::Role, &mut value.native_role),
                (Field::Metadata, &mut value.metadata),
            ],
            Self::Link(value) => vec![(Field::Content, &mut value.content)],
            Self::Original(value) => vec![(Field::Content, &mut value.bytes)],
        }
    }

    /// All blob and revision addresses, including lengths when supplied.
    #[must_use]
    pub fn content_addresses(&self) -> Vec<(ContentId, Option<u32>)> {
        let mut addresses: Vec<_> = self
            .fields()
            .into_iter()
            .filter_map(|(_, payload)| match payload {
                Payload::Blob(reference) => Some((reference.id, Some(reference.len))),
                Payload::Empty | Payload::Inline(_) => None,
            })
            .collect();
        if let Self::File(file) = self {
            addresses.extend(
                file.before
                    .into_iter()
                    .chain(file.after)
                    .map(|id| (id, None)),
            );
            if let FileEdit::Blob(reference) = file.edit {
                addresses.push((reference.id, Some(reference.len)));
            }
        }
        addresses
    }
}
