//! Durable links from successful imported shell commands to Git commits.

use super::Repositories;
use editchain_core::{
    GitLink, GitLinkKind, GitOid, NodeId, Op, OpId, OpKind, ParentSet, RepositoryId, Tags,
};
use editchain_git::{resolve_commit_prefix, RepositoryHandle};
use editchain_import::git_evidence::collect_commit_evidence;
use editchain_import::sink::FsBlobSink;
use std::collections::{BTreeMap, BTreeSet, HashSet};

/// Derive missing durable `ProducedBy` links from imported provider evidence.
///
/// A relation is emitted only when all of these facts are present:
///
/// - the provider recorded a successful command completion;
/// - the command actually invokes `git commit` at a shell command boundary;
/// - Git's standard success output carries an abbreviated commit OID; and
/// - that prefix resolves uniquely to a commit across repositories discovered
///   inside the imported workspace.
///
/// Existing relations make this pass idempotent. The returned operations are
/// deterministic functions of the completion record and immutable Git target.
pub(super) fn derive_produced_commit_links(
    repositories: &Repositories,
    ops: &[Op],
    blobs: Option<&FsBlobSink>,
) -> Vec<Op> {
    if repositories.handles.is_empty() {
        return Vec::new();
    }

    let evidence = collect_commit_evidence(ops, blobs);
    let existing: HashSet<(OpId, RepositoryId, GitOid)> = ops
        .iter()
        .filter_map(|op| match &op.kind {
            OpKind::GitLink(link) if link.kind == GitLinkKind::ProducedBy => {
                Some((link.source, link.target_repo, link.target_oid))
            }
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
            | OpKind::GitLink(_)
            | OpKind::Unknown(_) => None,
        })
        .collect();
    let existing_ids: HashSet<OpId> = ops.iter().map(|op| op.id).collect();
    let mut relations: BTreeMap<(OpId, RepositoryId, GitOid), &Op> = BTreeMap::new();

    for item in &evidence {
        if !item.invokes_git_commit() {
            continue;
        }
        for prefix in &item.prefixes {
            let Some((repository, oid)) = unique_commit(&repositories.handles, prefix) else {
                continue;
            };
            let key = (item.source.id, repository, oid);
            if !existing.contains(&key) {
                let _: &mut &Op = relations.entry(key).or_insert(item.source);
            }
        }
    }

    let mut links = Vec::with_capacity(relations.len());
    for ((source, target_repo, target_oid), source_op) in relations {
        let id = produced_link_id(source, target_repo, target_oid);
        if existing_ids.contains(&id) {
            continue;
        }
        links.push(Op {
            id,
            parents: ParentSet::One(source),
            actor: source_op.actor,
            clock: source_op.clock,
            scope: source_op.scope,
            tags: Tags::IMPORT | Tags::META,
            kind: OpKind::GitLink(GitLink {
                source,
                target_repo,
                target_oid,
                kind: GitLinkKind::ProducedBy,
            }),
        });
    }
    links
}

/// Resolve one Git-issued abbreviation uniquely across all workspace repos.
fn unique_commit(
    repositories: &[RepositoryHandle],
    prefix: &str,
) -> Option<(RepositoryId, GitOid)> {
    let mut matches = BTreeSet::new();
    for repository in repositories {
        // An unreadable or ambiguous repository makes the entire uniqueness
        // claim unresolved; it must not disappear as a local non-match.
        if let Some(commit) = resolve_commit_prefix(repository, prefix).ok()? {
            let _: bool = matches.insert((repository.discovery.id, commit.oid));
        }
    }
    if matches.len() == 1 {
        matches.into_iter().next()
    } else {
        None
    }
}

/// Derive a stable operation ID from the immutable relation endpoints.
fn produced_link_id(source: OpId, repository: RepositoryId, oid: GitOid) -> OpId {
    let digest = editchain_import::hash_raw(
        format!(
            "editchain:git-link:produced-by:v1:{source}:{}:{}",
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
    use editchain_core::Payload;
    use serde_json::Value;
    use std::path::Path;
    use std::process::Command;

    use editchain_core::{ActorId, Clock, ImportOp, ScopeRef, SessionId};

    use super::*;

    fn run_git(repo: &Path, args: &[&str]) -> std::process::Output {
        let output = Command::new("git")
            .current_dir(repo)
            .args(args)
            .output()
            .expect("run git fixture command");
        assert!(
            output.status.success(),
            "git fixture command failed: {args:?}"
        );
        output
    }

    fn import_op(seq: u64, value: &Value) -> Op {
        Op {
            id: OpId::new(NodeId(1), 0, seq),
            parents: if seq == 1 {
                ParentSet::None
            } else {
                ParentSet::One(OpId::new(NodeId(1), 0, seq.saturating_sub(1)))
            },
            actor: ActorId(7),
            clock: Clock::UnixMs(seq),
            scope: ScopeRef::Session(SessionId(9)),
            tags: Tags::IMPORT,
            kind: OpKind::Import(ImportOp {
                raw_ref: Payload::Inline(serde_json::to_vec(value).unwrap()),
                raw_hash: None,
            }),
        }
    }

    fn committed_repo(root: &Path) -> (std::path::PathBuf, String, String) {
        let repo = root.join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        drop(run_git(&repo, &["init", "-q"]));
        std::fs::write(repo.join("file.txt"), b"content\n").unwrap();
        drop(run_git(&repo, &["add", "file.txt"]));
        let commit = run_git(
            &repo,
            &[
                "-c",
                "user.name=Agent",
                "-c",
                "user.email=agent@example.com",
                "commit",
                "-m",
                "fixture commit",
            ],
        );
        let output = String::from_utf8(commit.stdout).unwrap();
        let oid = String::from_utf8(run_git(&repo, &["rev-parse", "HEAD"]).stdout)
            .unwrap()
            .trim()
            .to_string();
        (repo, output, oid)
    }

    #[test]
    fn derives_codex_and_claude_links_and_is_idempotent() {
        let temp = tempfile::tempdir().unwrap();
        let (repo, output, oid) = committed_repo(temp.path());
        let repositories = Repositories::discover(temp.path()).unwrap();
        let codex = import_op(
            1,
            &serde_json::json!({
                "type": "event_msg",
                "payload": {
                    "type": "item_completed",
                    "item": {
                        "type": "CommandExecution",
                        "command": ["/bin/bash", "-lc", "git commit -m 'fixture commit'"],
                        "cwd": repo,
                        "status": "completed",
                        "exit_code": 0,
                        "stdout": output,
                    },
                },
            }),
        );
        let claude_call = import_op(
            2,
            &serde_json::json!({
                "type": "assistant",
                "message": {"content": [{
                    "type": "tool_use",
                    "id": "call-1",
                    "name": "Bash",
                    "input": {"command": "git -c advice.detachedHead=false commit -m fixture"},
                }]},
            }),
        );
        let claude_result = import_op(
            3,
            &serde_json::json!({
                "type": "user",
                "message": {"content": [{
                    "type": "tool_result",
                    "tool_use_id": "call-1",
                    "content": output,
                    "is_error": false,
                }]},
            }),
        );
        let mut ops = vec![codex.clone(), claude_call, claude_result.clone()];

        let links = derive_produced_commit_links(&repositories, &ops, None);
        assert_eq!(
            links.len(),
            2,
            "both provider completion shapes should link"
        );
        let targets: Vec<_> = links
            .iter()
            .filter_map(|op| {
                let OpKind::GitLink(link) = &op.kind else {
                    return None;
                };
                Some((link.source, link.target_oid.to_hex()))
            })
            .collect();
        assert!(
            targets.contains(&(codex.id, oid.clone())),
            "Codex completion should be the causal source"
        );
        assert!(
            targets.contains(&(claude_result.id, oid)),
            "Claude tool result should be the causal source"
        );

        ops.extend(links);
        assert!(
            derive_produced_commit_links(&repositories, &ops, None).is_empty(),
            "reconciliation must not append duplicate durable links"
        );
    }

    #[test]
    fn skips_a_commit_prefix_that_resolves_in_multiple_workspace_repositories() {
        let temp = tempfile::tempdir().unwrap();
        let (repo, output, _) = committed_repo(temp.path());
        let clone = temp.path().join("repo-copy");
        let clone_arg = clone.to_string_lossy().into_owned();
        drop(run_git(
            temp.path(),
            &["clone", "-q", repo.to_str().unwrap(), &clone_arg],
        ));
        let command = import_op(
            1,
            &serde_json::json!({
                "type": "event_msg",
                "payload": {
                    "type": "item_completed",
                    "item": {
                        "type": "CommandExecution",
                        "command": ["/bin/bash", "-lc", "git commit -m fixture"],
                        "status": "completed",
                        "exit_code": 0,
                        "stdout": output,
                    },
                },
            }),
        );

        assert!(
            derive_produced_commit_links(
                &Repositories::discover(temp.path()).unwrap(),
                &[command],
                None
            )
            .is_empty(),
            "a cross-repository ambiguous prefix must not choose an arbitrary target"
        );
    }

    #[test]
    fn an_unreadable_repository_cannot_establish_cross_repository_uniqueness() {
        let temp = tempfile::tempdir().unwrap();
        let (repo, _, oid) = committed_repo(temp.path());
        let clone = temp.path().join("repo-copy");
        drop(run_git(
            temp.path(),
            &[
                "clone",
                "-q",
                repo.to_str().unwrap(),
                clone.to_str().unwrap(),
            ],
        ));
        let object = clone
            .join(".git/objects")
            .join(oid.get(..2).unwrap())
            .join(oid.get(2..).unwrap());
        // Unlink the clone's hard link before writing corrupt fixture bytes.
        std::fs::remove_file(&object).unwrap();
        std::fs::write(object, b"corrupt object").unwrap();
        let repositories = Repositories::discover(temp.path()).unwrap();
        assert!(unique_commit(&repositories.handles, oid.get(..7).unwrap()).is_none());
        let good = repositories
            .handles
            .iter()
            .find(|handle| handle.discovery.worktree_root.as_ref() == Some(&repo))
            .unwrap();
        assert!(resolve_commit_prefix(good, oid.get(..7).unwrap())
            .unwrap()
            .is_some());
    }
}
