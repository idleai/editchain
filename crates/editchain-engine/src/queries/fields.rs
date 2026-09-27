//! Schema-order field selection shared by content and search.

use crate::{FileEdit, GitLinkKind, OpKind, Payload};

use super::{content::FieldSource, ContentField};

pub(super) fn fields(kind: &OpKind) -> Vec<(ContentField, FieldSource<'_>)> {
    match kind {
        OpKind::ChainStart(op) => vec![(ContentField::ChainName, FieldSource::Bytes(&op.name))],
        OpKind::Actor(op) => payloads(&[
            (ContentField::ActorLabel, &op.label),
            (ContentField::ActorRole, &op.role),
        ]),
        OpKind::Session(op) => payloads(&[
            (ContentField::SessionLabel, &op.label),
            (ContentField::SessionMetadata, &op.metadata),
        ]),
        OpKind::Message(op) => payloads(&[
            (ContentField::MessageContent, &op.content),
            (ContentField::MessageContentType, &op.content_type),
        ]),
        OpKind::Tool(op) => payloads(&[
            (ContentField::ToolCallId, &op.tool_call_id),
            (ContentField::ToolName, &op.tool_name),
            (ContentField::ToolContent, &op.content),
        ]),
        OpKind::Command(op) => payloads(&[
            (ContentField::CommandId, &op.command_id),
            (ContentField::CommandContent, &op.content),
        ]),
        OpKind::File(op) => vec![
            (
                ContentField::FileBase,
                op.base.map_or(FieldSource::NotRecorded, |id| {
                    FieldSource::Reference(id.into())
                }),
            ),
            (
                ContentField::FileAfter,
                op.after.map_or(FieldSource::NotRecorded, |id| {
                    FieldSource::Reference(id.into())
                }),
            ),
            (
                ContentField::FileEdit,
                match &op.edit {
                    FileEdit::None => FieldSource::NotRecorded,
                    FileEdit::Blob(reference) => FieldSource::Reference((*reference).into()),
                    FileEdit::ReplaceBytes { bytes, .. } | FileEdit::UnifiedDiff(bytes) => {
                        FieldSource::Payload(bytes)
                    }
                },
            ),
        ],
        OpKind::Reflection(op) => payloads(&[
            (ContentField::ReflectionSummary, &op.summary),
            (ContentField::ReflectionAnchors, &op.anchors),
        ]),
        OpKind::Import(op) => payloads(&[(ContentField::ImportRaw, &op.raw_ref)]),
        OpKind::Note(op) => payloads(&[(ContentField::NoteContent, &op.content)]),
        OpKind::Error(op) => payloads(&[
            (ContentField::ErrorCode, &op.code),
            (ContentField::ErrorMessage, &op.message),
        ]),
        OpKind::Unknown(op) => payloads(&[(ContentField::UnknownRaw, &op.raw_bytes)]),
        OpKind::GitCommit(op) => {
            let mut fields = payloads(&[
                (ContentField::GitAuthorName, &op.author.name),
                (ContentField::GitAuthorEmail, &op.author.email),
                (ContentField::GitCommitterName, &op.committer.name),
                (ContentField::GitCommitterEmail, &op.committer.email),
                (ContentField::GitMessage, &op.message),
            ]);
            fields.extend(op.imported_refs.iter().enumerate().map(|(i, payload)| {
                (
                    ContentField::GitImportedRef(i),
                    FieldSource::Payload(payload),
                )
            }));
            fields.extend(
                op.live_refs.iter().enumerate().map(|(i, payload)| {
                    (ContentField::GitLiveRef(i), FieldSource::Payload(payload))
                }),
            );
            fields
        }
        OpKind::GitLink(op) => match &op.kind {
            GitLinkKind::Custom(payload) => payloads(&[(ContentField::GitLinkCustom, payload)]),
            GitLinkKind::BasedOn
            | GitLinkKind::Checkpoint
            | GitLinkKind::CommittedAs
            | GitLinkKind::ProducedBy
            | GitLinkKind::Mentions => Vec::new(),
        },
    }
}

fn payloads<'a>(fields: &[(ContentField, &'a Payload)]) -> Vec<(ContentField, FieldSource<'a>)> {
    fields
        .iter()
        .map(|(field, payload)| (*field, FieldSource::Payload(payload)))
        .collect()
}
