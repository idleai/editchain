//! Enumerate schema-defined content references; arbitrary JSON is not authority.

use std::collections::{BTreeMap, BTreeSet};

use editchain_core::{ContentId, FileEdit, GitLinkKind, Op, OpKind, Payload};

pub(crate) fn hashes(op: &Op) -> BTreeSet<[u8; 32]> {
    references(op).into_keys().collect()
}

pub(crate) fn matches_len(op: &Op, hash: [u8; 32], length: u64) -> bool {
    references(op).get(&hash).is_some_and(|lengths| {
        lengths
            .iter()
            .all(|expected| u64::from(*expected) == length)
    })
}

fn references(op: &Op) -> BTreeMap<[u8; 32], BTreeSet<u32>> {
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
    found
}

fn insert(found: &mut BTreeMap<[u8; 32], BTreeSet<u32>>, id: ContentId, length: Option<u32>) {
    if let ContentId::Hash256(hash) = id {
        found.entry(hash).or_default().extend(length);
    }
}
