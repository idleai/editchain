//! On-demand node details and resolved Git object presentation.

use super::Workspace;
use editchain_core::{GitOid, Op, OpId, OpKind, Payload};
use editchain_protocol::{NodeDetails, ResolvedObject};

impl Workspace {
    /// Get details for a specific node by operation ID or git OID.
    #[must_use]
    pub fn node_details(
        &self,
        op_id: Option<String>,
        git_oid: Option<GitOid>,
    ) -> Option<NodeDetails> {
        if let Some(op_id_str) = op_id {
            let op_id = OpId::from_display_str(&op_id_str)?;
            let op = self.source_op(op_id)?;
            return Some(node_details_from_op(&op));
        }
        if let Some(oid) = git_oid {
            let commit = self
                .projection
                .git()
                .commits()
                .values()
                .find(|c| c.oid == oid)?;
            return Some(node_details_from_commit(commit));
        }
        None
    }
}

/// Build node details from an `EditChain` operation.
#[must_use]
fn node_details_from_op(op: &Op) -> NodeDetails {
    NodeDetails {
        op_id: Some(op.id.to_string()),
        git_oid: None,
        repository: None,
        summary: op_summary(op),
        body: op_body(op),
        parents: op.parents.iter().map(ToString::to_string).collect(),
        git_parents: Vec::new(),
        refs: Vec::new(),
        changed_paths: Vec::new(),
    }
}

/// Build node details from a git commit entity.
#[must_use]
fn node_details_from_commit(commit: &editchain_core::GitCommitEntity) -> NodeDetails {
    let body = match &commit.message {
        Payload::Inline(b) => String::from_utf8_lossy(b).to_string(),
        Payload::Empty | Payload::Blob(_) => String::new(),
    };
    let refs = commit
        .live_refs
        .iter()
        .chain(commit.imported_refs.iter())
        .filter_map(|r| match r {
            Payload::Inline(b) => Some(String::from_utf8_lossy(b).to_string()),
            Payload::Empty | Payload::Blob(_) => None,
        })
        .collect();
    let changed_paths = commit
        .changed_paths
        .iter()
        .map(|p| p.0.to_string())
        .collect();
    NodeDetails {
        op_id: commit.imported_record.map(|id| id.to_string()),
        git_oid: Some(commit.oid.to_hex()),
        repository: Some(commit.repository.0.to_string()),
        summary: body.clone(),
        body,
        parents: Vec::new(),
        git_parents: commit.parents.iter().map(GitOid::to_hex).collect(),
        refs,
        changed_paths,
    }
}

/// Build the JSON-safe resolved-object DTO from a git commit entity.
///
/// Every identity is carried as an exact string (decimal `RepositoryId` /
/// `PathId`, lowercase-hex `GitOid`, `"node:boot:seq"` `OpId`) so u64 values
/// above 2^53 round-trip through JavaScript without precision loss. Safe
/// enums, timestamps, signatures, and payloads are preserved as-is.
#[must_use]
pub(crate) fn resolved_object_from_commit(
    commit: &editchain_core::GitCommitEntity,
) -> ResolvedObject {
    ResolvedObject {
        repository: commit.repository.0.to_string(),
        object_format: commit.object_format,
        oid: commit.oid.to_hex(),
        imported_record: commit.imported_record.map(|id| id.to_string()),
        availability: commit.availability,
        tree: commit.tree.to_hex(),
        parents: commit.parents.iter().map(GitOid::to_hex).collect(),
        author: commit.author.clone(),
        committer: commit.committer.clone(),
        authored_at: commit.authored_at,
        committed_at: commit.committed_at,
        message: commit.message.clone(),
        imported_refs: commit.imported_refs.clone(),
        live_refs: commit.live_refs.clone(),
        changed_paths: commit
            .changed_paths
            .iter()
            .map(|p| p.0.to_string())
            .collect(),
    }
}

/// Produce a short summary for an `EditChain` operation.
#[must_use]
fn op_summary(op: &Op) -> String {
    match &op.kind {
        OpKind::Message(m) => payload_text(&m.content),
        OpKind::Tool(t) => payload_text(&t.tool_name),
        OpKind::Command(c) => payload_text(&c.content),
        OpKind::File(f) => format!("file:{}", f.path.0),
        OpKind::Reflection(r) => payload_text(&r.summary),
        OpKind::Note(n) => payload_text(&n.content),
        OpKind::Error(e) => payload_text(&e.message),
        OpKind::ChainStart(cs) => String::from_utf8_lossy(&cs.name).to_string(),
        OpKind::Actor(a) => payload_text(&a.label),
        OpKind::Import(i) => payload_text(&i.raw_ref),
        OpKind::GitCommit(c) => payload_text(&c.message),
        OpKind::GitLink(l) => format!("git:{}", l.target_oid),
        OpKind::Unknown(u) => format!("unknown kind={}", u.kind_discriminant),
    }
}

/// Produce the full body text for an `EditChain` operation.
#[must_use]
fn op_body(op: &Op) -> String {
    match &op.kind {
        OpKind::Message(m) => payload_text(&m.content),
        OpKind::Tool(t) => payload_text(&t.content),
        OpKind::Command(c) => payload_text(&c.content),
        OpKind::Reflection(r) => payload_text(&r.summary),
        OpKind::Note(n) => payload_text(&n.content),
        OpKind::Error(e) => payload_text(&e.message),
        OpKind::ChainStart(_)
        | OpKind::Actor(_)
        | OpKind::File(_)
        | OpKind::Import(_)
        | OpKind::GitCommit(_)
        | OpKind::GitLink(_)
        | OpKind::Unknown(_) => String::new(),
    }
}

/// Extract text from a payload, or empty string.
#[must_use]
pub(super) fn payload_text(payload: &Payload) -> String {
    match payload {
        Payload::Inline(b) => String::from_utf8_lossy(b).to_string(),
        Payload::Empty | Payload::Blob(_) => String::new(),
    }
}
