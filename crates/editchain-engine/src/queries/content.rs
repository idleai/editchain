//! Exact field content, including unavailable and unrecorded bytes.

use std::io;

use serde::{Deserialize, Serialize};

use crate::{BlobResolution, OpId, Payload};

use super::{
    fields, ChainQueries, ContentReference, ContentState, HistoryEntry, Lookup, RecordRef,
};

/// A content-bearing field in the immutable operation schema.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ContentField {
    /// Named field in a schema-three operation.
    Record(editchain_core::activity::Field),
    /// Chain's recorded name bytes.
    ChainName,
    /// Actor label.
    ActorLabel,
    /// Actor role, without interpreting its vocabulary.
    ActorRole,
    /// Session label.
    SessionLabel,
    /// Opaque session metadata.
    SessionMetadata,
    /// Message content.
    MessageContent,
    /// Message media type.
    MessageContentType,
    /// Tool invocation identity.
    ToolCallId,
    /// Recorded tool name.
    ToolName,
    /// Tool input or output for the recorded lifecycle stage.
    ToolContent,
    /// Command identity.
    CommandId,
    /// Command text or output for the recorded lifecycle stage.
    CommandContent,
    /// File's explicitly referenced base snapshot.
    FileBase,
    /// File's explicitly referenced resulting snapshot.
    FileAfter,
    /// Recorded replacement bytes, patch, or full-content blob, as specified by `FileEdit`.
    FileEdit,
    /// Reflection summary, without interpreting it as ground truth.
    ReflectionSummary,
    /// Opaque reflection anchors.
    ReflectionAnchors,
    /// Raw imported record bytes.
    ImportRaw,
    /// Opaque annotation content.
    NoteContent,
    /// Error code.
    ErrorCode,
    /// Error message.
    ErrorMessage,
    /// Opaque unknown-kind payload.
    UnknownRaw,
    /// Git author's recorded name.
    GitAuthorName,
    /// Git author's recorded email.
    GitAuthorEmail,
    /// Git committer's recorded name.
    GitCommitterName,
    /// Git committer's recorded email.
    GitCommitterEmail,
    /// Git commit message.
    GitMessage,
    /// One ref observed at import time, at its original vector position.
    GitImportedRef(usize),
    /// One recorded live-ref observation, at its original vector position.
    GitLiveRef(usize),
    /// Custom Git relationship payload.
    GitLinkCustom,
}

/// Select exact field bytes from a recorded operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContentQuery {
    /// Record whose field is requested.
    pub operation: OpId,
    /// Exact schema field; absent/inapplicable fields return `NotRecorded`.
    pub field: ContentField,
}

/// Byte availability without replacing unknown content with an empty file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ContentValue {
    /// Exact bytes, including valid empty content and non-UTF-8 content.
    Available(#[serde(with = "editchain_core::payload::text_bytes")] Vec<u8>),
    /// No value was recorded for this field, or the field is inapplicable.
    NotRecorded,
    /// A referenced blob has not arrived.
    Missing,
    /// Stored bytes fail address or declared-length verification.
    Corrupt,
    /// The adapter cannot resolve the recorded identity.
    Unresolvable,
}

impl ContentValue {
    /// Access only verified, available bytes.
    #[must_use]
    pub fn bytes(&self) -> Option<&[u8]> {
        match self {
            Self::Available(bytes) => Some(bytes),
            Self::NotRecorded | Self::Missing | Self::Corrupt | Self::Unresolvable => None,
        }
    }
}

/// Resolved bytes or an explicit gap, anchored to a field and original record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContentResult {
    /// Supporting record representation.
    pub record_ref: RecordRef,
    /// Requested field.
    pub field: ContentField,
    /// Original external reference, if any, including its declared length.
    pub reference: Option<ContentReference>,
    /// Exact content or the reason it is unavailable.
    pub value: ContentValue,
}

#[derive(Clone, Copy)]
pub(super) enum FieldSource<'a> {
    Payload(&'a Payload),
    Bytes(&'a [u8]),
    Reference(ContentReference),
    NotRecorded,
}

impl ChainQueries {
    /// Resolve every recorded content field in schema order for evidence readers.
    ///
    /// Uses the same exhaustive inventory as search, including legacy fields,
    /// binary Original records and file snapshots. Missing and conflicted
    /// observations remain distinct from unavailable field content.
    ///
    /// # Errors
    /// Returns storage/index errors while resolving the operation or its fields.
    pub fn contents(&self, operation: OpId) -> io::Result<Lookup<Vec<ContentResult>>> {
        self.operation(operation)?.try_map(|entry| {
            fields::fields(&entry.operation.kind)
                .into_iter()
                .map(|(field, source)| self.resolve_field(&entry, field, source))
                .collect()
        })
    }

    /// Resolve a field without decoding text, applying patches, or consulting live files.
    ///
    /// Missing/corrupt/unresolvable indexed content requires a successful index
    /// refresh before new bytes become visible. Available blobs are verified on
    /// each read; external deletion/corruption remains explicit even before refresh.
    ///
    /// # Errors
    /// Returns storage/index errors; absence and conflict are explicit results.
    pub fn content(&self, query: ContentQuery) -> io::Result<Lookup<ContentResult>> {
        self.operation(query.operation)?
            .try_map(|entry| self.field_content(&entry, query.field))
    }

    pub(super) fn field_content(
        &self,
        entry: &HistoryEntry,
        field: ContentField,
    ) -> io::Result<ContentResult> {
        let selected = if matches!(entry.operation.kind, crate::OpKind::Activity(_)) {
            match field {
                ContentField::ToolName => {
                    ContentField::Record(editchain_core::activity::Field::ToolName)
                }
                ContentField::GitImportedRef(index) => {
                    ContentField::Record(editchain_core::activity::Field::GitImportedRef(index))
                }
                ContentField::GitLiveRef(index) => {
                    ContentField::Record(editchain_core::activity::Field::GitLiveRef(index))
                }
                ContentField::Record(_)
                | ContentField::ChainName
                | ContentField::ActorLabel
                | ContentField::ActorRole
                | ContentField::SessionLabel
                | ContentField::SessionMetadata
                | ContentField::MessageContent
                | ContentField::MessageContentType
                | ContentField::ToolCallId
                | ContentField::ToolContent
                | ContentField::CommandId
                | ContentField::CommandContent
                | ContentField::FileBase
                | ContentField::FileAfter
                | ContentField::FileEdit
                | ContentField::ReflectionSummary
                | ContentField::ReflectionAnchors
                | ContentField::ImportRaw
                | ContentField::NoteContent
                | ContentField::ErrorCode
                | ContentField::ErrorMessage
                | ContentField::UnknownRaw
                | ContentField::GitAuthorName
                | ContentField::GitAuthorEmail
                | ContentField::GitCommitterName
                | ContentField::GitCommitterEmail
                | ContentField::GitMessage
                | ContentField::GitLinkCustom => field,
            }
        } else {
            field
        };
        let source = fields::fields(&entry.operation.kind)
            .into_iter()
            .find_map(|(candidate, source)| (candidate == selected).then_some(source))
            .unwrap_or(FieldSource::NotRecorded);
        self.resolve_field(entry, field, source)
    }

    pub(super) fn resolve_field(
        &self,
        entry: &HistoryEntry,
        field: ContentField,
        source: FieldSource<'_>,
    ) -> io::Result<ContentResult> {
        let reference = match source {
            FieldSource::Reference(reference) => Some(reference),
            FieldSource::Payload(Payload::Blob(reference)) => Some((*reference).into()),
            FieldSource::Payload(Payload::Empty | Payload::Inline(_))
            | FieldSource::Bytes(_)
            | FieldSource::NotRecorded => None,
        };
        let value = if let Some(reference) = reference {
            self.indexed_bytes(entry, reference)?
        } else {
            match source {
                FieldSource::Bytes(bytes) => ContentValue::Available(bytes.to_vec()),
                FieldSource::Payload(Payload::Inline(bytes)) => {
                    ContentValue::Available(bytes.clone())
                }
                FieldSource::Payload(Payload::Empty) => ContentValue::Available(Vec::new()),
                FieldSource::NotRecorded => ContentValue::NotRecorded,
                FieldSource::Reference(_) | FieldSource::Payload(Payload::Blob(_)) => {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "missing field content reference",
                    ));
                }
            }
        };
        Ok(ContentResult {
            record_ref: entry.record_ref,
            field,
            reference,
            value,
        })
    }

    fn indexed_bytes(
        &self,
        entry: &HistoryEntry,
        reference: ContentReference,
    ) -> io::Result<ContentValue> {
        let state = entry
            .content
            .iter()
            .find(|status| status.reference == reference)
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    "field lacks indexed content dependency",
                )
            })?
            .state;
        Ok(match state {
            ContentState::Missing => ContentValue::Missing,
            ContentState::Corrupt => ContentValue::Corrupt,
            ContentState::Unresolvable => ContentValue::Unresolvable,
            ContentState::Available => match reference.resolve(&self.blobs)? {
                BlobResolution::Found(bytes) => ContentValue::Available(bytes),
                BlobResolution::Missing => ContentValue::Missing,
                BlobResolution::Corrupt => ContentValue::Corrupt,
                BlobResolution::Unresolvable => ContentValue::Unresolvable,
            },
        })
    }
}
