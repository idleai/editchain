//! Exhaustive extraction of references from recorded schema fields.

use editchain_core::{FileEdit, GitLinkKind, Op, OpKind, Payload};

use crate::ContentReference;

pub(crate) fn references(op: &Op) -> Vec<ContentReference> {
    let mut result = Vec::new();
    if let OpKind::File(file) = &op.kind {
        result.extend(file.base.into_iter().map(ContentReference::from));
        result.extend(file.after.into_iter().map(ContentReference::from));
        if let FileEdit::Blob(reference) = file.edit {
            result.push(ContentReference::from(reference));
        }
    }
    for payload in payloads(&op.kind) {
        if let Payload::Blob(reference) = payload {
            result.push(ContentReference::from(*reference));
        }
    }
    let mut seen = std::collections::HashSet::new();
    result.retain(|reference| seen.insert(*reference));
    result
}

fn payloads(kind: &OpKind) -> Vec<&Payload> {
    match kind {
        OpKind::ChainStart(_) => Vec::new(),
        OpKind::Actor(op) => vec![&op.label, &op.role],
        OpKind::Session(op) => vec![&op.label, &op.metadata],
        OpKind::Message(op) => vec![&op.content, &op.content_type],
        OpKind::Tool(op) => vec![&op.tool_call_id, &op.tool_name, &op.content],
        OpKind::Command(op) => vec![&op.command_id, &op.content],
        OpKind::File(op) => match &op.edit {
            FileEdit::None | FileEdit::Blob(_) => Vec::new(),
            FileEdit::ReplaceBytes { bytes, .. } | FileEdit::UnifiedDiff(bytes) => vec![bytes],
        },
        OpKind::Reflection(op) => vec![&op.summary, &op.anchors],
        OpKind::Import(op) => vec![&op.raw_ref],
        OpKind::Note(op) => vec![&op.content],
        OpKind::Error(op) => vec![&op.code, &op.message],
        OpKind::Unknown(op) => vec![&op.raw_bytes],
        OpKind::GitCommit(op) => [
            &op.author.name,
            &op.author.email,
            &op.committer.name,
            &op.committer.email,
            &op.message,
        ]
        .into_iter()
        .chain(&op.imported_refs)
        .chain(&op.live_refs)
        .collect(),
        OpKind::GitLink(op) => match &op.kind {
            GitLinkKind::Custom(payload) => vec![payload],
            GitLinkKind::BasedOn
            | GitLinkKind::Checkpoint
            | GitLinkKind::CommittedAs
            | GitLinkKind::ProducedBy
            | GitLinkKind::Mentions => Vec::new(),
        },
    }
}
