//! History linking — relate sessions and git commits into the unified graph.
//!
//! The import produces per-session linear chains (`ParentSet::One(prev_op)`),
//! and git commits are a separate disconnected component. This module does NOT
//! force all sessions into one linear trunk: each source Claude Code session is
//! its own chain (the chain count maps 1:1 to imported sessions). Cross-session
//! relationships come from typed `ForkOf` / `SubagentOf` relationship notes
//! (read as virtual edges by the projection), never from forced stitching of
//! unrelated sessions.
//!
//! What this module does:
//!
//! 1. **Git-command detection** — link ops that ran git commit/push commands to
//!    the commit with the closest timestamp.
//! 2. **Session-to-closest-commit** — for sessions still unlinked to git, link
//!    their last op to the closest commit (weak linkage).
//!
//! Op→git links are returned as [`GitLink`] records (the existing mechanism for
//! op→git edges), which the projection stores in `GitProjection.links`.
//!
//! All linking is deterministic (stable sort by timestamp, tie-break by `OpId`).

use std::collections::HashMap;

use editchain_core::{
    GitCommitEntity, GitLink, GitLinkKind, GitOid, Op, Payload, RepositoryId, ScopeRef,
};

/// The result of linking: session-stitched ops plus the op→git links created.
#[derive(Debug, Clone)]
pub struct LinkResult {
    /// The ops with session-stitching applied (parents augmented).
    pub ops: Vec<Op>,
    /// Op→git links created by git-command detection and closest-commit fallback.
    pub git_links: Vec<GitLink>,
}

/// Link a set of operations and git commits into the unified projection graph.
///
/// Sessions are left as their own chains (no forced stitching); only op→git
/// links are added. Returns the ops (unchanged in parents) and the op→git links
/// to store in the projection.
#[must_use]
pub fn link_history(ops: &[Op], commits: &[GitCommitEntity]) -> LinkResult {
    let mut ops = ops.to_vec();

    // Build a timestamp index of commits for closest-commit lookup.
    let commit_times: Vec<(i64, GitOid)> =
        commits.iter().map(|c| (c.committed_at, c.oid)).collect();

    // Note: No blanket session-to-session stitching. Each Claude Code session is
    // its own chain (the chain count maps 1:1 to imported source sessions). Cross-
    // session relationships are represented explicitly by typed `ForkOf` /
    // `SubagentOf` relationship notes and op→git links, never by forced linear
    // chaining of unrelated sessions. A forced trunk here is what made a short
    // first session appear to "spawn" an entire month of later activity.

    // 1. Git-command detection: link ops that ran git commands to commits.
    let mut git_links = link_git_commands(&mut ops, &commit_times);

    // 2. Session-to-closest-commit fallback for sessions still unlinked.
    git_links.extend(link_sessions_to_commits(&ops, &commit_times));

    LinkResult { ops, git_links }
}

/// Link ops that ran git commit/push commands to the commit with the closest
/// timestamp. Returns the created links.
fn link_git_commands(ops: &mut [Op], commit_times: &[(i64, GitOid)]) -> Vec<GitLink> {
    if commit_times.is_empty() {
        return Vec::new();
    }
    let mut links = Vec::new();
    for op in ops.iter() {
        if !op_is_git_command(op) {
            continue;
        }
        let ts = op_clock(op);
        if let Some((_, oid)) = closest_commit(commit_times, ts) {
            links.push(GitLink {
                source: op.id,
                target_repo: RepositoryId(0),
                target_oid: oid,
                kind: GitLinkKind::ProducedBy,
            });
        }
    }
    links
}

/// For sessions still unlinked to git, link their last op to the closest commit.
/// Returns the created links.
#[expect(
    clippy::indexing_slicing,
    reason = "indices are guaranteed non-empty per session group"
)]
fn link_sessions_to_commits(ops: &[Op], commit_times: &[(i64, GitOid)]) -> Vec<GitLink> {
    if commit_times.is_empty() {
        return Vec::new();
    }
    // Group ops by session; find each session's last op.
    let mut by_session: HashMap<u64, Vec<usize>> = HashMap::new();
    for (i, op) in ops.iter().enumerate() {
        if let ScopeRef::Session(sid) = op.scope {
            by_session.entry(sid.0).or_default().push(i);
        }
    }
    let mut links = Vec::new();
    for (_, indices) in by_session {
        // Find the last op (max clock).
        let mut last = indices[0];
        for &i in &indices {
            if op_clock(&ops[i]) > op_clock(&ops[last]) {
                last = i;
            }
        }
        let ts = op_clock(&ops[last]);
        if let Some((_, oid)) = closest_commit(commit_times, ts) {
            links.push(GitLink {
                source: ops[last].id,
                target_repo: RepositoryId(0),
                target_oid: oid,
                kind: GitLinkKind::BasedOn,
            });
        }
    }
    links
}

/// Returns true if an op's content indicates a git commit/push command.
fn op_is_git_command(op: &Op) -> bool {
    use editchain_core::OpKind;
    match &op.kind {
        OpKind::Command(c) => {
            payload_text(&c.content).contains("git commit")
                || payload_text(&c.content).contains("git push")
        }
        OpKind::Tool(t) => {
            payload_text(&t.content).contains("git commit")
                || payload_text(&t.content).contains("git push")
        }
        OpKind::ChainStart(_)
        | OpKind::Actor(_)
        | OpKind::Message(_)
        | OpKind::File(_)
        | OpKind::Reflection(_)
        | OpKind::Import(_)
        | OpKind::Note(_)
        | OpKind::Error(_)
        | OpKind::GitCommit(_)
        | OpKind::GitLink(_)
        | OpKind::Unknown(_) => false,
    }
}

/// Find the commit whose timestamp is closest to `ts`.
fn closest_commit(commit_times: &[(i64, GitOid)], ts: u64) -> Option<(i64, GitOid)> {
    let ts_i64 = i64::try_from(ts).unwrap_or(i64::MAX);
    commit_times
        .iter()
        .min_by_key(|(t, _)| t.abs_diff(ts_i64))
        .copied()
}

/// Extract text from a payload.
fn payload_text(payload: &Payload) -> String {
    match payload {
        Payload::Inline(b) => String::from_utf8_lossy(b).to_string(),
        Payload::Empty | Payload::Blob(_) => String::new(),
    }
}

/// Extract the clock value of an op as u64.
fn op_clock(op: &Op) -> u64 {
    op.clock.as_u64()
}
