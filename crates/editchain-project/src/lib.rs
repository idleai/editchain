//! UI-neutral history projections for the unified `EditChain` + `Git` viewer.
//!
//! This crate builds deterministic projections over `EditChain` operations and
//! `Git` commits, and provides windowed/paged access for the viewer. It is
//! intentionally free of filesystem and process dependencies so it can later
//! target WASM.

/// Deterministic lane layout for graph rendering.
pub mod layout;

use editchain_core::{GitCommitEntity, GitOid, GitProjection, Op, OpId, Payload, RepositoryId};

/// A unified history row — either an `EditChain` operation or a `Git` commit.
#[derive(Debug, Clone)]
pub enum HistoryNode {
    /// An `EditChain` operation.
    EditOperation(Op),
    /// A `Git` commit entity.
    GitCommit(GitCommitEntity),
}

impl HistoryNode {
    /// Returns a display summary for this node.
    #[must_use]
    pub fn summary(&self) -> String {
        match self {
            Self::EditOperation(op) => op_summary(op),
            Self::GitCommit(commit) => match &commit.message {
                Payload::Inline(b) => String::from_utf8_lossy(b).to_string(),
                Payload::Empty | Payload::Blob(_) => commit.oid.to_hex(),
            },
        }
    }

    /// Returns the timestamp in Unix ms (0 if unknown).
    ///
    /// Git commits store `committed_at` in Unix **seconds**; `EditChain` ops
    /// store their clock in Unix **milliseconds**. This converts git seconds
    /// to milliseconds so both render as correct dates.
    #[must_use]
    pub fn timestamp_ms(&self) -> u64 {
        match self {
            Self::EditOperation(op) => op.clock.as_u64(),
            Self::GitCommit(commit) => {
                let secs = u64::try_from(commit.committed_at).unwrap_or(0);
                secs.saturating_mul(1000)
            }
        }
    }

    /// Returns the operation ID, if this is an `EditChain` operation.
    #[must_use]
    pub fn op_id(&self) -> Option<OpId> {
        match self {
            Self::EditOperation(op) => Some(op.id),
            Self::GitCommit(_) => None,
        }
    }

    /// Returns the git commit OID, if this is a git commit.
    #[must_use]
    pub fn git_oid(&self) -> Option<GitOid> {
        match self {
            Self::EditOperation(_) => None,
            Self::GitCommit(commit) => Some(commit.oid),
        }
    }

    /// Returns the repository, if this is a git commit.
    #[must_use]
    pub fn repository(&self) -> Option<RepositoryId> {
        match self {
            Self::EditOperation(_) => None,
            Self::GitCommit(commit) => Some(commit.repository),
        }
    }

    /// Returns a grouping key for block separation.
    ///
    /// `EditChain` ops group by their session scope (or "ops" if unscoped);
    /// git commits group by their repository id.
    #[must_use]
    pub fn group(&self) -> String {
        match self {
            Self::EditOperation(op) => match op.scope {
                editchain_core::ScopeRef::Session(sid) => format!("session:{}", sid.0),
                editchain_core::ScopeRef::None
                | editchain_core::ScopeRef::Chain(_)
                | editchain_core::ScopeRef::Turn(_)
                | editchain_core::ScopeRef::File(_) => "ops".to_string(),
            },
            Self::GitCommit(commit) => format!("repo:{}", commit.repository.0),
        }
    }

    /// Returns a stable node key for graph wiring.
    ///
    /// `EditChain` ops use their `OpId` string; git commits use their OID hex.
    #[must_use]
    pub fn node_key(&self) -> String {
        match self {
            Self::EditOperation(op) => op.id.to_string(),
            Self::GitCommit(commit) => commit.oid.to_hex(),
        }
    }

    /// Returns the parent node keys for drawing graph edges.
    #[must_use]
    pub fn parent_keys(&self) -> Vec<String> {
        match self {
            Self::EditOperation(op) => op.parents.iter().map(ToString::to_string).collect(),
            Self::GitCommit(commit) => commit.parents.iter().map(GitOid::to_hex).collect(),
        }
    }
}

/// A unified history projection over `EditChain` ops and `Git` commits.
#[derive(Debug, Clone, Default)]
pub struct HistoryProjection {
    /// `EditChain` operations in canonical causal order (oldest-first).
    pub ops: Vec<Op>,
    /// `Git` commits keyed by `(RepositoryId, GitOid)`.
    pub git: GitProjection,
}

impl HistoryProjection {
    /// Create an empty projection.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            ops: Vec::new(),
            git: GitProjection::new(),
        }
    }

    /// Build a projection from a set of operations.
    ///
    /// Operations are stored in input order; git commits are projected into
    /// the `GitProjection` keyed by `(RepositoryId, GitOid)`.
    #[must_use]
    pub fn from_ops(ops: Vec<Op>) -> Self {
        let mut git = GitProjection::new();
        for op in &ops {
            git.reduce(op);
        }
        Self { ops, git }
    }

    /// Returns the number of history nodes (ops + git commits).
    #[must_use]
    pub fn len(&self) -> usize {
        self.ops.len().saturating_add(self.git.commits.len())
    }

    /// Returns true if the projection is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Returns all history nodes (newest-first).
    ///
    /// The flattened view: `EditChain` ops first (in reverse causal order),
    /// then git commits.
    #[must_use]
    pub fn nodes(&self) -> Vec<HistoryNode> {
        let mut nodes = Vec::with_capacity(self.len());
        // `EditChain` ops, newest-first.
        for op in self.ops.iter().rev() {
            nodes.push(HistoryNode::EditOperation(op.clone()));
        }
        // `Git` commits (deterministic BTreeMap order).
        for commit in self.git.commits.values() {
            nodes.push(HistoryNode::GitCommit(commit.clone()));
        }
        nodes
    }

    /// Returns a window of history nodes (newest-first).
    ///
    /// The window is a flattened view: `EditChain` ops first (in reverse causal
    /// order), then git commits. `offset`/`limit` provide cursor-based paging.
    #[must_use]
    pub fn window(&self, offset: usize, limit: usize) -> Vec<HistoryNode> {
        self.nodes().into_iter().skip(offset).take(limit).collect()
    }

    /// Merge resolved git commits into the projection.
    ///
    /// Commits are keyed by `(RepositoryId, GitOid)`; a later commit with the
    /// same key replaces an earlier one.
    pub fn merge_git_commits(&mut self, commits: Vec<GitCommitEntity>) {
        for commit in commits {
            drop(
                self.git
                    .commits
                    .insert((commit.repository, commit.oid), commit),
            );
        }
    }
}

/// Produce a short summary for an `EditChain` operation.
#[must_use]
fn op_summary(op: &Op) -> String {
    use editchain_core::OpKind;
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

/// Extract text from a payload, or empty string.
#[must_use]
fn payload_text(payload: &Payload) -> String {
    match payload {
        Payload::Inline(b) => String::from_utf8_lossy(b).to_string(),
        Payload::Empty | Payload::Blob(_) => String::new(),
    }
}
