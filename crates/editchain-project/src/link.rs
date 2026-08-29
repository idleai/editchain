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
//! 2. **Session-to-closest-commit** — retain weak `BasedOn` provenance from a
//!    session's last wall-clocked op to the closest commit. This metadata does
//!    not become a causal graph parent in the history projection.
//!
//! Op→git links are returned as [`GitLink`] records, which the projection stores
//! in `GitProjection.links`. The projection decides which explicit relation
//! kinds are graph-bearing; inferred `BasedOn` links remain provenance only.
//!
//! All linking is deterministic (stable sort by timestamp, tie-break by `OpId`).

use std::collections::BTreeMap;

use editchain_core::{
    Clock, GitCommitEntity, GitLink, GitLinkKind, GitOid, Op, Payload, RepositoryId, ScopeRef,
};

/// The result of linking: unchanged session ops plus the op→git links created.
#[derive(Debug, Clone)]
pub struct LinkResult {
    /// The imported ops; unrelated session parents remain unchanged.
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
    let git_links = link_history_links(ops, commits);
    LinkResult {
        ops: ops.to_vec(),
        git_links,
    }
}

/// Compute only the Git links for a set of operations and commits.
///
/// Unlike [`link_history`], this does not clone the operation corpus. The
/// projection uses this path because linking never mutates operation parents;
/// retaining the legacy [`LinkResult::ops`] API would otherwise duplicate every
/// payload during workspace open.
#[must_use]
pub fn link_history_links(ops: &[Op], commits: &[GitCommitEntity]) -> Vec<GitLink> {
    // Git stores Unix seconds while operation clocks store Unix milliseconds.
    // Keep the source unit explicit here; `closest_commit` normalizes before
    // comparing so every session does not accidentally select the newest commit.
    let commit_times_seconds: Vec<(i64, GitOid)> =
        commits.iter().map(|c| (c.committed_at, c.oid)).collect();

    // Note: No blanket session-to-session stitching. Each Claude Code session is
    // its own chain (the chain count maps 1:1 to imported source sessions). Cross-
    // session relationships are represented explicitly by typed `ForkOf` /
    // `SubagentOf` relationship notes and op→git links, never by forced linear
    // chaining of unrelated sessions. A forced trunk here is what made a short
    // first session appear to "spawn" an entire month of later activity.

    // 1. Git-command detection: link ops that ran git commands to commits.
    let mut git_links = link_git_commands(ops, &commit_times_seconds);

    // 2. Weak session-to-closest-commit provenance fallback.
    git_links.extend(link_sessions_to_commits(ops, &commit_times_seconds));
    git_links
}

/// Link ops that ran git commit/push commands to the commit with the closest
/// timestamp. Returns the created links.
fn link_git_commands(ops: &[Op], commit_times_seconds: &[(i64, GitOid)]) -> Vec<GitLink> {
    if commit_times_seconds.is_empty() {
        return Vec::new();
    }
    let mut links = Vec::new();
    for op in ops {
        if !op_is_git_command(op) {
            continue;
        }
        let Some(ts_ms) = op_unix_ms(op) else {
            continue;
        };
        if let Some((_, oid)) = closest_commit(commit_times_seconds, ts_ms) {
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

/// Retain each session's last wall-clocked op as weak provenance to its closest
/// commit. Returns the created links.
fn link_sessions_to_commits(ops: &[Op], commit_times_seconds: &[(i64, GitOid)]) -> Vec<GitLink> {
    if commit_times_seconds.is_empty() {
        return Vec::new();
    }
    // Group ops by session; the BTreeMap keeps emitted link order stable.
    let mut by_session: BTreeMap<u64, Vec<&Op>> = BTreeMap::new();
    for op in ops {
        if let ScopeRef::Session(sid) = op.scope {
            by_session.entry(sid.0).or_default().push(op);
        }
    }
    let mut links = Vec::new();
    for (_, session_ops) in by_session {
        // Only wall-clocked operations can participate in timestamp matching.
        // Pick the latest Unix timestamp, then OpId for a deterministic tie.
        let Some((last, ts_ms)) = session_ops
            .into_iter()
            .filter_map(|op| op_unix_ms(op).map(|ts| (op, ts)))
            .max_by_key(|(op, ts)| (*ts, op.id))
        else {
            continue;
        };
        if let Some((_, oid)) = closest_commit(commit_times_seconds, ts_ms) {
            links.push(GitLink {
                source: last.id,
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

/// Find the commit whose Unix-seconds timestamp is closest to `ts_ms`.
fn closest_commit(commit_times_seconds: &[(i64, GitOid)], ts_ms: u64) -> Option<(i64, GitOid)> {
    let ts_ms = i128::from(ts_ms);
    commit_times_seconds
        .iter()
        .min_by_key(|(seconds, _)| i128::from(*seconds).saturating_mul(1_000).abs_diff(ts_ms))
        .copied()
}

/// Extract text from a payload.
fn payload_text(payload: &Payload) -> String {
    match payload {
        Payload::Inline(b) => String::from_utf8_lossy(b).to_string(),
        Payload::Empty | Payload::Blob(_) => String::new(),
    }
}

/// Return an operation's wall-clock timestamp in Unix milliseconds.
///
/// Lamport and absent clocks provide ordering but cannot be compared to Git's
/// wall-clock timestamps, so inferred Git links deliberately skip them.
fn op_unix_ms(op: &Op) -> Option<u64> {
    match op.clock {
        Clock::UnixMs(ms) | Clock::Hybrid { ms, .. } => Some(ms),
        Clock::None | Clock::Lamport(_) => None,
    }
}
