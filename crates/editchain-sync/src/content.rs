//! Enumerate schema-defined content references; arbitrary JSON is not authority.

use std::collections::{BTreeMap, BTreeSet};

use editchain_core::{
    human::HumanWorkRecord, ContentId, FileEdit, GitLinkKind, Op, OpKind, Payload,
};

pub(crate) fn hashes(op: &Op) -> BTreeSet<[u8; 32]> {
    references(op).into_keys().collect()
}

pub(crate) type References = BTreeMap<[u8; 32], BTreeSet<u32>>;

pub(crate) fn matches_len(references: &References, hash: [u8; 32], length: u64) -> bool {
    references.get(&hash).is_some_and(|lengths| {
        lengths
            .iter()
            .all(|expected| u64::from(*expected) == length)
    })
}

pub(crate) fn structured_payload(op: &Op) -> Option<&Payload> {
    match &op.kind {
        OpKind::Import(value) => Some(&value.raw_ref),
        OpKind::Note(value) => Some(&value.content),
        OpKind::ChainStart(_)
        | OpKind::Actor(_)
        | OpKind::Message(_)
        | OpKind::Tool(_)
        | OpKind::Command(_)
        | OpKind::File(_)
        | OpKind::Reflection(_)
        | OpKind::Error(_)
        | OpKind::GitCommit(_)
        | OpKind::GitLink(_)
        | OpKind::Unknown(_) => None,
    }
}

pub(crate) fn nested(found: &mut References, bytes: &[u8]) {
    // Parse only the retained schema: a coincidental hash in a message, path,
    // unknown source or future schema never expands the content capability.
    if let Ok(work) = serde_json::from_slice::<HumanWorkRecord>(bytes) {
        if work.source == "vscode.work" && work.schema == 1 {
            for revision in [work.before, work.after].into_iter().flatten() {
                insert(found, revision.content, None);
            }
        }
    }
}

pub(crate) fn references(op: &Op) -> References {
    let mut found = BTreeMap::new();
    let mut payloads = Vec::new();
    match &op.kind {
        OpKind::ChainStart(_) => {}
        OpKind::Actor(value) => payloads.extend([&value.label, &value.role]),
        OpKind::Message(value) => payloads.extend([&value.content, &value.content_type]),
        OpKind::Tool(value) => {
            payloads.extend([&value.tool_call_id, &value.tool_name, &value.content]);
        }
        OpKind::Command(value) => payloads.extend([&value.command_id, &value.content]),
        OpKind::File(value) => {
            for id in [value.base, value.after].into_iter().flatten() {
                insert(&mut found, id, None);
            }
            match &value.edit {
                FileEdit::None => {}
                FileEdit::ReplaceBytes { bytes, .. } | FileEdit::UnifiedDiff(bytes) => {
                    payloads.push(bytes);
                }
                FileEdit::Blob(reference) => insert(&mut found, reference.id, Some(reference.len)),
            }
        }
        OpKind::Reflection(value) => payloads.extend([&value.summary, &value.anchors]),
        OpKind::Import(value) => payloads.push(&value.raw_ref),
        OpKind::Note(value) => payloads.push(&value.content),
        OpKind::Error(value) => payloads.extend([&value.code, &value.message]),
        OpKind::Unknown(value) => payloads.push(&value.raw_bytes),
        OpKind::GitLink(value) => {
            if let GitLinkKind::Custom(payload) = &value.kind {
                payloads.push(payload);
            }
        }
        OpKind::GitCommit(value) => {
            payloads.extend([
                &value.author.name,
                &value.author.email,
                &value.committer.name,
                &value.committer.email,
                &value.message,
            ]);
            payloads.extend(&value.imported_refs);
            payloads.extend(&value.live_refs);
        }
    }
    for payload in payloads {
        if let Payload::Blob(reference) = payload {
            insert(&mut found, reference.id, Some(reference.len));
        }
    }
    if let Some(Payload::Inline(bytes)) = structured_payload(op) {
        nested(&mut found, bytes);
    }
    found
}

fn insert(found: &mut BTreeMap<[u8; 32], BTreeSet<u32>>, id: ContentId, length: Option<u32>) {
    if let ContentId::Hash256(hash) = id {
        found.entry(hash).or_default().extend(length);
    }
}
