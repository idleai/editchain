//! History linking — stitch disconnected session chains and git history into a
//! single edit chain.
//!
//! The import produces per-session linear chains (`ParentSet::One(prev_op)`),
//! and git commits are a separate disconnected component. This module augments
//! operation parents so the whole history reads as one continuous chain:
//!
//! 1. **Session-to-session stitching** — link each session's first op to the
//!    previous session's last op (chronological order).
//! 2. **Git-command detection** — link ops that ran git commit/push commands to
//!    the commit with the closest timestamp.
//! 3. **Session-to-closest-commit** — for sessions still unlinked to git, link
//!    their last op to the closest commit (weak linkage).
//!
//! Session stitching mutates `Op.parents` (both ends are `OpId`). Op→git links
//! are returned as [`GitLink`] records (the existing mechanism for op→git
//! edges), which the projection stores in `GitProjection.links`.
//!
//! All linking is deterministic (stable sort by timestamp, tie-break by `OpId`).

use std::collections::HashMap;

use editchain_core::{
    GitCommitEntity, GitLink, GitLinkKind, GitOid, Op, OpId, ParentSet, Payload, RepositoryId,
    ScopeRef,
};

/// The result of linking: session-stitched ops plus the op→git links created.
#[derive(Debug, Clone)]
pub struct LinkResult {
    /// The ops with session-stitching applied (parents augmented).
    pub ops: Vec<Op>,
    /// Op→git links created by git-command detection and closest-commit fallback.
    pub git_links: Vec<GitLink>,
}

/// Link a set of operations and git commits into a single edit chain.
///
/// Returns session-stitched ops and the op→git links to store in the projection.
#[must_use]
pub fn link_history(ops: &[Op], commits: &[GitCommitEntity]) -> LinkResult {
    let mut ops = ops.to_vec();

    // Build a timestamp index of commits for closest-commit lookup.
    let commit_times: Vec<(i64, GitOid)> =
        commits.iter().map(|c| (c.committed_at, c.oid)).collect();

    // 1. Session-to-session stitching.
    stitch_sessions(&mut ops);

    // 2. Git-command detection: link ops that ran git commands to commits.
    let mut git_links = link_git_commands(&mut ops, &commit_times);

    // 3. Session-to-closest-commit fallback for sessions still unlinked.
    git_links.extend(link_sessions_to_commits(&ops, &commit_times));

    LinkResult { ops, git_links }
}

/// Stitch sessions together chronologically into one linear chain.
///
/// For each session, find its first and last op (by clock). Sort sessions by
/// first-op timestamp; link session N's first op → session N-1's last op.
#[expect(
    clippy::indexing_slicing,
    reason = "indices are guaranteed non-empty per session group"
)]
fn stitch_sessions(ops: &mut [Op]) {
    // Group ops by session id.
    let mut by_session: HashMap<u64, Vec<usize>> = HashMap::new();
    for (i, op) in ops.iter().enumerate() {
        if let ScopeRef::Session(sid) = op.scope {
            by_session.entry(sid.0).or_default().push(i);
        }
    }

    // For each session, find first and last op index by clock.
    let mut sessions: Vec<(u64, usize, usize)> = Vec::new(); // (sid, first_idx, last_idx)
    for (sid, indices) in by_session {
        let mut first = indices[0];
        let mut last = indices[0];
        for &i in &indices {
            if op_clock(&ops[i]) < op_clock(&ops[first]) {
                first = i;
            }
            if op_clock(&ops[i]) > op_clock(&ops[last]) {
                last = i;
            }
        }
        sessions.push((sid, first, last));
    }

    // Sort sessions by first-op clock (deterministic tie-break by sid).
    sessions.sort_by(|a, b| {
        op_clock(&ops[a.1])
            .cmp(&op_clock(&ops[b.1]))
            .then(a.0.cmp(&b.0))
    });

    // Link session N's first op → session N-1's last op.
    for w in sessions.windows(2) {
        let prev_last = w[0].2;
        let cur_first = w[1].1;
        let prev_id = ops[prev_last].id;
        add_parent(&mut ops[cur_first], prev_id);
    }
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

/// Add a parent to an op, respecting `ParentSet` capacity (max 2).
fn add_parent(op: &mut Op, parent: OpId) {
    match op.parents {
        ParentSet::None => op.parents = ParentSet::One(parent),
        ParentSet::One(existing) => {
            if existing != parent {
                op.parents = ParentSet::Two(existing, parent);
            }
        }
        ParentSet::Two(..) => {}
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
