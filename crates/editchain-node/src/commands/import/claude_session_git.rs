//! Historical Git baselines recovered for Claude Code sessions.

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};

use editchain_core::{
    ActorId, Clock, GitLink, GitLinkKind, GitOid, NodeId, Op, OpId, OpKind, ParentSet,
    RepositoryId, ScopeRef, SessionId, Tags,
};
use editchain_git::{resolve_branch_tip_at_time, RepositoryHandle};
use editchain_import::claude_code::normalize::parse_source_time;
use editchain_import::ids::derive_session_id;
use editchain_import::sink::FsBlobSink;
use serde_json::Value;

use super::git_commit_links::{open_repositories, payload_bytes};

/// One provider record that can recover a session's historical branch tip.
#[derive(Debug)]
struct StartEvidence {
    source_seq: u64,
    session: SessionId,
    cwd: PathBuf,
    branch: String,
    unix_ms: u64,
}

/// Raw records owned by one immutable imported source generation.
#[derive(Debug)]
struct SourceEvidence<'a> {
    root: &'a Op,
    start: Option<StartEvidence>,
}

/// Derive missing Claude `BasedOn` links from provider and local Git evidence.
///
/// Claude Code records the active branch and timestamp on its first substantive
/// event, but not the branch-tip OID. For a top-level session only, this pass
/// resolves that branch's own reflog at the event time. It emits nothing when
/// the branch, repository, reflog coverage, timestamp ordering, or commit
/// object is ambiguous. The relation is attached to the first physical record
/// of the source so metadata prepended by Claude does not create a second
/// junction after the session has already started.
pub(super) fn derive_session_base_links(
    workspace: &Path,
    ops: &[Op],
    blobs: Option<&FsBlobSink>,
) -> Result<Vec<Op>, Box<dyn std::error::Error>> {
    let repositories = open_repositories(workspace)?;
    if repositories.is_empty() {
        return Ok(Vec::new());
    }

    let catalog = editchain_git::RepositoryCatalog::from_entries(
        repositories
            .iter()
            .map(|repository| repository.discovery.clone())
            .collect(),
    );
    let mut based_sessions: HashSet<SessionId> = ops
        .iter()
        .filter_map(|op| match (&op.scope, &op.kind) {
            (ScopeRef::Session(session), OpKind::GitLink(link))
                if link.kind == GitLinkKind::BasedOn =>
            {
                Some(*session)
            }
            _ => None,
        })
        .collect();
    let existing_ids: HashSet<OpId> = ops.iter().map(|op| op.id).collect();
    let mut sources: BTreeMap<(u64, u32), SourceEvidence<'_>> = BTreeMap::new();

    for op in ops {
        let OpKind::Import(import) = &op.kind else {
            continue;
        };
        let key = (op.id.node.0, op.id.boot);
        let source = sources.entry(key).or_insert_with(|| SourceEvidence {
            root: op,
            start: None,
        });
        if op.id.seq < source.root.id.seq {
            source.root = op;
        }
        let Some(raw) = payload_bytes(&import.raw_ref, blobs) else {
            continue;
        };
        let Some(candidate) = claude_start_evidence(op, &raw) else {
            continue;
        };
        if source
            .start
            .as_ref()
            .is_none_or(|current| candidate.source_seq < current.source_seq)
        {
            source.start = Some(candidate);
        }
    }

    let mut candidates: Vec<_> = sources
        .into_values()
        .filter_map(|source| source.start.map(|start| (start, source.root)))
        .collect();
    candidates.sort_by_key(|(start, root)| (start.unix_ms, root.id));

    let mut links = Vec::new();
    for (start, root) in candidates {
        if based_sessions.contains(&start.session) {
            continue;
        }
        let Some(repository) = repository_for_cwd(workspace, &start.cwd, &catalog, &repositories)
        else {
            continue;
        };
        let Some(commit) = resolve_branch_tip_at_time(repository, &start.branch, start.unix_ms)
        else {
            continue;
        };
        let target_repo = repository.discovery.id;
        let target_oid = commit.commit.oid;
        let id = based_on_link_id(root.id, target_repo, target_oid);
        if existing_ids.contains(&id) {
            continue;
        }
        links.push(Op {
            id,
            parents: ParentSet::One(root.id),
            actor: ActorId(0),
            clock: Clock::None,
            scope: ScopeRef::Session(start.session),
            tags: Tags::IMPORT | Tags::META | Tags::INFERRED,
            kind: OpKind::GitLink(GitLink {
                source: root.id,
                target_repo,
                target_oid,
                kind: GitLinkKind::BasedOn,
            }),
        });
        let _inserted = based_sessions.insert(start.session);
    }
    Ok(links)
}

/// Parse only the exact Claude fields required by the historical resolver.
fn claude_start_evidence(op: &Op, raw: &[u8]) -> Option<StartEvidence> {
    let value: Value = serde_json::from_slice(raw).ok()?;
    if !matches!(
        value.get("type").and_then(Value::as_str),
        Some("user" | "assistant" | "system")
    ) {
        return None;
    }
    if value.get("isSidechain").and_then(Value::as_bool) == Some(true)
        || value
            .get("agentId")
            .and_then(Value::as_str)
            .is_some_and(|agent| !agent.is_empty())
    {
        return None;
    }
    let session_text = value.get("sessionId").and_then(Value::as_str)?;
    let session = derive_session_id(session_text);
    if op.scope != ScopeRef::Session(session) {
        return None;
    }
    let cwd = value.get("cwd").and_then(Value::as_str)?;
    let branch = value.get("gitBranch").and_then(Value::as_str)?;
    let timestamp = value.get("timestamp").and_then(Value::as_str)?;
    if cwd.is_empty() || branch.is_empty() {
        return None;
    }
    Some(StartEvidence {
        source_seq: op.id.seq,
        session,
        cwd: PathBuf::from(cwd),
        branch: branch.to_owned(),
        unix_ms: parse_source_time(timestamp)?,
    })
}

/// Select the deepest workspace repository containing the recorded cwd.
fn repository_for_cwd<'a>(
    workspace: &Path,
    cwd: &Path,
    catalog: &editchain_git::RepositoryCatalog,
    repositories: &'a [RepositoryHandle],
) -> Option<&'a RepositoryHandle> {
    let workspace = absolute_path(workspace).canonicalize().ok()?;
    let cwd = absolute_path(cwd).canonicalize().ok()?;
    if !cwd.starts_with(&workspace) {
        return None;
    }
    let descriptor = catalog.repository_for_path(&cwd)?;
    repositories
        .iter()
        .find(|repository| repository.discovery.id == descriptor.id)
}

fn absolute_path(path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir().map_or_else(|_| path.to_path_buf(), |cwd| cwd.join(path))
    }
}

/// Derive a stable operation identity from the immutable relation endpoints.
fn based_on_link_id(source: OpId, repository: RepositoryId, oid: GitOid) -> OpId {
    let digest = editchain_import::hash_raw(
        format!(
            "editchain:git-link:claude-reflog-based-on:v1:{source}:{}:{}",
            repository.0,
            oid.to_hex()
        )
        .as_bytes(),
    );
    let node = digest
        .get(0..8)
        .and_then(|bytes| bytes.try_into().ok())
        .map_or(0, u64::from_le_bytes);
    let boot = digest
        .get(8..12)
        .and_then(|bytes| bytes.try_into().ok())
        .map_or(0, u32::from_le_bytes);
    let seq = digest
        .get(12..20)
        .and_then(|bytes| bytes.try_into().ok())
        .map_or(0, u64::from_le_bytes);
    OpId::new(NodeId(node), boot, seq)
}

#[cfg(test)]
mod tests {
    use std::process::Command;

    use editchain_core::{ImportOp, Payload};

    use super::*;

    fn run_git(repo: &Path, args: &[&str], when: Option<&str>) -> std::process::Output {
        let mut command = Command::new("git");
        let _configured = command.current_dir(repo).args(args);
        if let Some(when) = when {
            let _dated = command
                .env("GIT_AUTHOR_DATE", when)
                .env("GIT_COMMITTER_DATE", when);
        }
        let output = command.output().expect("run git fixture command");
        assert!(
            output.status.success(),
            "git fixture command failed: {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        output
    }

    fn commit(repo: &Path, contents: &str, subject: &str, when: &str) -> String {
        std::fs::write(repo.join("file.txt"), contents).expect("write fixture file");
        drop(run_git(repo, &["add", "file.txt"], None));
        drop(run_git(
            repo,
            &[
                "-c",
                "user.name=Agent",
                "-c",
                "user.email=agent@example.com",
                "commit",
                "-qm",
                subject,
            ],
            Some(when),
        ));
        String::from_utf8(run_git(repo, &["rev-parse", "HEAD"], None).stdout)
            .expect("oid is utf8")
            .trim()
            .to_owned()
    }

    fn repository(root: &Path) -> (PathBuf, String, String) {
        let repo = root.join("repo");
        std::fs::create_dir_all(&repo).expect("create fixture repo");
        drop(run_git(&repo, &["init", "-q", "-b", "main"], None));
        let first = commit(&repo, "one\n", "first", "2026-07-10T00:00:00+00:00");
        let second = commit(&repo, "two\n", "second", "2026-07-10T00:10:00+00:00");
        (repo, first, second)
    }

    fn import_op(seq: u64, session: SessionId, value: &Value) -> Op {
        let id = OpId::new(NodeId(7), 0, raw_seq(seq));
        Op {
            id,
            parents: if seq == 1 {
                ParentSet::None
            } else {
                ParentSet::One(OpId::new(NodeId(7), 0, raw_seq(seq.saturating_sub(1))))
            },
            actor: ActorId(3),
            clock: Clock::UnixMs(seq),
            scope: ScopeRef::Session(session),
            tags: Tags::IMPORT,
            kind: OpKind::Import(ImportOp {
                raw_ref: Payload::Inline(serde_json::to_vec(&value).expect("encode fixture")),
                raw_hash: None,
            }),
        }
    }

    fn raw_seq(seq: u64) -> u64 {
        seq.checked_shl(16).expect("small fixture ordinal")
    }

    fn session_ops(repo: &Path, timestamp: &str, extra: &Value) -> Vec<Op> {
        let session = derive_session_id("session-1");
        let root = import_op(
            1,
            session,
            &serde_json::json!({
                "type": "custom-title",
                "sessionId": "session-1",
                "customTitle": "A title"
            }),
        );
        let mut event = serde_json::json!({
            "type": "user",
            "sessionId": "session-1",
            "cwd": repo,
            "gitBranch": "main",
            "timestamp": timestamp,
            "message": {"role": "user", "content": "start"}
        });
        if let (Some(event), Some(extra)) = (event.as_object_mut(), extra.as_object()) {
            event.extend(extra.clone());
        }
        vec![root, import_op(2, session, &event)]
    }

    #[test]
    fn anchors_the_physical_session_root_to_the_historical_branch_tip() {
        let temp = tempfile::tempdir().expect("tempdir");
        let (repo, first_oid, second_oid) = repository(temp.path());
        let mut ops = session_ops(&repo, "2026-07-10T00:05:00.500Z", &serde_json::json!({}));

        let links = derive_session_base_links(temp.path(), &ops, None).expect("derive link");

        assert_eq!(links.len(), 1);
        let (link_op, link) = links
            .iter()
            .find_map(|op| match &op.kind {
                OpKind::GitLink(link) => Some((op, link)),
                OpKind::ChainStart(_)
                | OpKind::Actor(_)
                | OpKind::Message(_)
                | OpKind::Tool(_)
                | OpKind::Command(_)
                | OpKind::File(_)
                | OpKind::Reflection(_)
                | OpKind::Import(_)
                | OpKind::Note(_)
                | OpKind::Error(_)
                | OpKind::GitCommit(_)
                | OpKind::Unknown(_) => None,
            })
            .expect("expected Git link");
        assert_eq!(link.kind, GitLinkKind::BasedOn);
        assert_eq!(
            link.source,
            ops.first().expect("root op").id,
            "metadata stays behind the fork"
        );
        assert_eq!(link.target_oid.to_hex(), first_oid);
        assert_ne!(
            link.target_oid.to_hex(),
            second_oid,
            "current tip is ignored"
        );
        assert!(link_op.tags.matches_all(Tags::INFERRED | Tags::META));

        ops.extend(links);
        assert!(
            derive_session_base_links(temp.path(), &ops, None)
                .expect("repeat reconciliation")
                .is_empty(),
            "an existing session baseline makes reconciliation idempotent"
        );
    }

    #[test]
    fn rejects_uncovered_same_second_and_subagent_evidence() {
        let temp = tempfile::tempdir().expect("tempdir");
        let (repo, _, _) = repository(temp.path());

        let before_reflog = session_ops(&repo, "2026-07-09T23:59:59.000Z", &serde_json::json!({}));
        assert!(derive_session_base_links(temp.path(), &before_reflog, None)
            .expect("uncovered lookup")
            .is_empty());

        let same_second = session_ops(&repo, "2026-07-10T00:10:00.500Z", &serde_json::json!({}));
        assert!(derive_session_base_links(temp.path(), &same_second, None)
            .expect("ambiguous lookup")
            .is_empty());

        let sidechain = session_ops(
            &repo,
            "2026-07-10T00:05:00.500Z",
            &serde_json::json!({"isSidechain": true, "agentId": "agent-1"}),
        );
        assert!(derive_session_base_links(temp.path(), &sidechain, None)
            .expect("sidechain lookup")
            .is_empty());
    }
}
