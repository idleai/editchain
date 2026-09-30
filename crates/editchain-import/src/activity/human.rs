//! The same native editor fields drive archive and live schema-three records.

use editchain_core::activity::{
    Author, AuthorRole, ChangeState, File, FileAction, Kind, Note, NoteKind, Operation,
    OriginalRef, Session, SessionAction, TextEdit, TextRange,
};
use editchain_core::{FileEdit, Op, OpId, Payload};
use serde_json::Value;

use super::{
    source::{identity, scoped},
    CONTRACT,
};
use crate::{BlobSink, ImportError};

pub(super) fn activities(
    original: &Operation,
    bytes: &[u8],
    blobs: &mut dyn BlobSink,
) -> Result<Vec<Op>, ImportError> {
    let Ok(value) = serde_json::from_slice::<Value>(bytes) else {
        return Ok(Vec::new());
    };
    let event = if value.get("format").and_then(Value::as_str) == Some("editchain-human-history")
        || value.get("source").and_then(Value::as_str) == Some("vscode.editor")
    {
        value.get("event").unwrap_or(&value)
    } else {
        return Ok(Vec::new());
    };
    let (Some(session), Some(sequence), Some(body)) = (
        event.get("session").and_then(Value::as_str),
        event.get("sequence").and_then(Value::as_u64),
        event.get("event"),
    ) else {
        return Ok(Vec::new());
    };
    let session = identity("human.session", session);
    let item = scoped("human.event", session, &sequence.to_string());
    let mut record = original.clone();
    record.id = OpId::from_bytes(blake3::derive_key(
        "editchain.human-activity.v1",
        original.id.as_bytes(),
    ));
    record.item = item;
    record.session = Some(session);
    record.turn = None;
    record.sequence = Some(sequence);
    record.time_ms = event.get("time_ms").and_then(Value::as_u64);
    record.legacy = None;
    record.original = Some(OriginalRef {
        operation: original.id,
        converter: CONTRACT.into(),
    });
    record.parents = vec![original.id];
    record.author = event
        .get("identity")
        .and_then(|identity| identity.get("guid"))
        .and_then(Value::as_str)
        .map(|guid| identity("human.author", guid));
    let event_type = body.get("type").and_then(Value::as_str).unwrap_or_default();
    let kind = match event_type {
        "tracking_started" | "tracking_stopped" => {
            record.item = session;
            Kind::Session(Session {
                action: if event_type == "tracking_started" {
                    SessionAction::Started
                } else {
                    SessionAction::Ended
                },
                label: Payload::Empty,
                settings: crate::payload_for(&serde_json::to_vec(body)?, blobs)?,
                participants: record.author.into_iter().collect(),
                parent: None,
                initiated_by: None,
            })
        }
        "human_edit" | "human_edit_batch" | "observed_edit_batch" => {
            let mut changes = Vec::new();
            if let Some(change) = body.get("change").and_then(Value::as_u64) {
                changes.push(change);
            }
            for change in body
                .get("edits")
                .or_else(|| body.get("changes"))
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                if let Some(change) = change
                    .as_u64()
                    .or_else(|| change.get("change").and_then(Value::as_u64))
                {
                    changes.push(change);
                }
            }
            Kind::Note(Note {
                category: NoteKind::Correction,
                targets: Vec::new(),
                items: changes
                    .into_iter()
                    .map(|change| scoped("human.event", session, &change.to_string()))
                    .collect(),
                version: 1,
                content: crate::payload_for(&serde_json::to_vec(body)?, blobs)?,
                code: Payload::Inline(event_type.as_bytes().to_vec()),
            })
        }
        "tracking_gap" => Kind::Note(Note {
            category: NoteKind::Gap,
            targets: Vec::new(),
            items: Vec::new(),
            version: 1,
            content: string_payload(body.get("reason")),
            code: Payload::Empty,
        }),
        "document_snapshot"
        | "document_changed"
        | "document_saved"
        | "document_renamed"
        | "editor_opened"
        | "editor_closed"
        | "visible_ranges_changed"
        | "code_exposure"
        | "code_read" => {
            let document = body.get("document").unwrap_or(body);
            let path = document
                .get("path")
                .or_else(|| body.get("from"))
                .or_else(|| body.get("uri"))
                .and_then(Value::as_str);
            let Some(path) = path else {
                return Ok(Vec::new());
            };
            let action = match event_type {
                "document_changed" => FileAction::Change,
                "document_saved" => FileAction::Save,
                "document_renamed" => FileAction::Rename,
                "editor_opened" => {
                    if body.get("restored").and_then(Value::as_bool) == Some(true) {
                        FileAction::Snapshot
                    } else {
                        FileAction::Open
                    }
                }
                "editor_closed" => FileAction::Close,
                "visible_ranges_changed" | "code_exposure" => FileAction::View,
                "code_read" => FileAction::Read,
                _ => FileAction::Snapshot,
            };
            let mut file = File {
                action,
                path: crate::derive_path_id(path),
                name: Payload::Inline(path.as_bytes().to_vec()),
                renamed_to: (action == FileAction::Rename).then(|| string_payload(body.get("to"))),
                revision: document
                    .get("id")
                    .and_then(Value::as_str)
                    .zip(document.get("version").and_then(Value::as_u64))
                    .map(|(id, version)| {
                        scoped("human.revision", session, &format!("{id}:{version}"))
                    }),
                before: None,
                after: None,
                edit: FileEdit::None,
                text_edits: Vec::new(),
                change: matches!(action, FileAction::Change | FileAction::Rename)
                    .then_some(ChangeState::Applied),
                caused_by: None,
                ranges: Vec::new(),
                text_ranges: Vec::new(),
                duration_ms: body.get("duration_ms").and_then(Value::as_u64),
            };
            if let Some(text) = body
                .get("text")
                .or_else(|| body.get("after"))
                .and_then(Value::as_str)
            {
                let reference = blobs.put(text.as_bytes())?;
                file.after = Some(reference.id);
                file.edit = FileEdit::Blob(reference);
            }
            if let Some(changes) = body.get("changes").and_then(Value::as_array) {
                for change in changes {
                    let (Some(offset), Some(length), Some(text)) = (
                        change.get("offset").and_then(Value::as_u64),
                        change.get("length").and_then(Value::as_u64),
                        change.get("text").and_then(Value::as_str),
                    ) else {
                        return Ok(Vec::new());
                    };
                    file.text_edits.push(TextEdit {
                        offset_utf16: offset,
                        length_utf16: length,
                        text: crate::payload_for(text.as_bytes(), blobs)?,
                    });
                }
            }
            if let Some(ranges) = body.get("ranges").and_then(Value::as_array) {
                for range in ranges {
                    let Ok(range) = serde_json::from_value::<TextRange>(range.clone()) else {
                        return Ok(Vec::new());
                    };
                    file.text_ranges.push(range);
                }
            }
            // The observation alone does not identify who caused a buffer change.
            if action == FileAction::Change {
                record.author = None;
            }
            if action == FileAction::Change && file.text_edits.is_empty() && file.after.is_none() {
                return Ok(Vec::new());
            }
            Kind::File(file)
        }
        _ => return Ok(Vec::new()),
    };
    record.kind = kind;
    let mut result = Vec::new();
    if event_type == "tracking_started" {
        if let Some(author) = record.author {
            let mut registration = record.clone();
            registration.id = OpId::from_bytes(blake3::derive_key(
                "editchain.human-author.v1",
                original.id.as_bytes(),
            ));
            registration.item = author;
            registration.kind = Kind::Author(Author {
                label: string_payload(event.get("user_name")),
                role: AuthorRole::Person,
                native_role: Payload::Empty,
                metadata: crate::payload_for(&serde_json::to_vec(&event.get("identity"))?, blobs)?,
            });
            result.push(
                registration
                    .into_op()
                    .map_err(|error| ImportError::OpSink(error.to_string()))?,
            );
        }
    }
    result.push(
        record
            .into_op()
            .map_err(|error| ImportError::OpSink(error.to_string()))?,
    );
    Ok(result)
}

fn string_payload(value: Option<&Value>) -> Payload {
    value
        .and_then(Value::as_str)
        .map_or(Payload::Empty, |text| {
            Payload::Inline(text.as_bytes().to_vec())
        })
}
