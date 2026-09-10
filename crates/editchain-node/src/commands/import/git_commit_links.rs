//! Durable links from successful imported shell commands to Git commits.

use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::Path;

use editchain_core::{
    ContentId, GitLink, GitLinkKind, GitOid, NodeId, Op, OpId, OpKind, ParentSet, Payload,
    RepositoryId, Tags,
};
use editchain_git::{
    discover_repositories, open_repository, resolve_commit_prefix, RepositoryHandle,
};
use editchain_import::sink::FsBlobSink;
use serde_json::Value;

/// Exact evidence that one imported completion record produced commit objects.
#[derive(Debug)]
struct CommitEvidence<'a> {
    source: &'a Op,
    command: String,
    prefixes: Vec<String>,
}

/// Provider call identity scoped to one imported source generation.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct ClaudeCallKey {
    node: NodeId,
    boot: u32,
    call_id: String,
}

/// A successful Claude shell-tool result waiting for its exactly correlated call.
#[derive(Debug)]
struct ClaudeResult<'a> {
    source: &'a Op,
    key: ClaudeCallKey,
    prefixes: Vec<String>,
}

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
    workspace: &Path,
    ops: &[Op],
    blobs: Option<&FsBlobSink>,
) -> Result<Vec<Op>, Box<dyn std::error::Error>> {
    let repositories = open_repositories(workspace)?;
    if repositories.is_empty() {
        return Ok(Vec::new());
    }

    let evidence = collect_evidence(ops, blobs);
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
        if !command_invokes_git_commit(&item.command) {
            continue;
        }
        for prefix in &item.prefixes {
            let Some((repository, oid)) = unique_commit(&repositories, prefix) else {
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
    Ok(links)
}

/// Open every repository discovered under the workspace, skipping individual
/// repositories that cannot currently be opened.
pub(super) fn open_repositories(
    workspace: &Path,
) -> Result<Vec<RepositoryHandle>, Box<dyn std::error::Error>> {
    let discoveries = discover_repositories(workspace)?;
    discoveries.iter().map(open_repository).collect()
}

/// Resolve one Git-issued abbreviation uniquely across all workspace repos.
fn unique_commit(
    repositories: &[RepositoryHandle],
    prefix: &str,
) -> Option<(RepositoryId, GitOid)> {
    let matches: BTreeSet<(RepositoryId, GitOid)> = repositories
        .iter()
        .filter_map(|repository| {
            resolve_commit_prefix(repository, prefix)
                .map(|resolved| (repository.discovery.id, resolved.commit.oid))
        })
        .collect();
    if matches.len() == 1 {
        matches.into_iter().next()
    } else {
        None
    }
}

/// Collect provider-neutral commit evidence from byte-exact raw imports.
fn collect_evidence<'a>(ops: &'a [Op], blobs: Option<&FsBlobSink>) -> Vec<CommitEvidence<'a>> {
    let mut evidence = Vec::new();
    let mut claude_calls: HashMap<ClaudeCallKey, Vec<String>> = HashMap::new();
    let mut claude_results = Vec::new();

    for op in ops {
        let OpKind::Import(import) = &op.kind else {
            continue;
        };
        let Some(raw) = payload_bytes(&import.raw_ref, blobs) else {
            continue;
        };
        if !(contains_bytes(&raw, b"commit")
            || contains_bytes(&raw, b"tool_result") && contains_bytes(&raw, b"["))
        {
            continue;
        }
        let Ok(value) = serde_json::from_slice::<Value>(&raw) else {
            continue;
        };
        if let Some((command, prefixes)) = codex_command_evidence(&value) {
            evidence.push(CommitEvidence {
                source: op,
                command,
                prefixes,
            });
        }
        for (call_id, command) in claude_shell_calls(&value) {
            claude_calls
                .entry(ClaudeCallKey {
                    node: op.id.node,
                    boot: op.id.boot,
                    call_id,
                })
                .or_default()
                .push(command);
        }
        for (call_id, prefixes) in claude_successful_results(&value) {
            claude_results.push(ClaudeResult {
                source: op,
                key: ClaudeCallKey {
                    node: op.id.node,
                    boot: op.id.boot,
                    call_id,
                },
                prefixes,
            });
        }
    }

    for result in claude_results {
        let Some(commands) = claude_calls.get(&result.key) else {
            continue;
        };
        let [command] = commands.as_slice() else {
            continue;
        };
        evidence.push(CommitEvidence {
            source: result.source,
            command: command.clone(),
            prefixes: result.prefixes,
        });
    }
    evidence
}

/// Extract a successful Codex `CommandExecution` completion.
fn codex_command_evidence(value: &Value) -> Option<(String, Vec<String>)> {
    if value.get("type").and_then(Value::as_str) != Some("event_msg")
        || value.pointer("/payload/type").and_then(Value::as_str) != Some("item_completed")
    {
        return None;
    }
    let item = value.pointer("/payload/item")?;
    let item_type = item.get("type").and_then(Value::as_str)?;
    if !matches!(item_type, "CommandExecution" | "command_execution")
        || item.get("status").and_then(Value::as_str) != Some("completed")
        || item
            .get("exit_code")
            .or_else(|| item.get("exitCode"))
            .and_then(Value::as_i64)
            != Some(0)
    {
        return None;
    }
    let command = command_value(item.get("command")?)?;
    let output = [
        "aggregated_output",
        "aggregatedOutput",
        "stdout",
        "formatted_output",
        "formattedOutput",
    ]
    .iter()
    .find_map(|key| item.get(*key).and_then(value_text))?;
    let prefixes = git_commit_prefixes(&output);
    (!prefixes.is_empty()).then_some((command, prefixes))
}

/// Extract Claude `Bash`/`PowerShell` call IDs and command strings.
fn claude_shell_calls(value: &Value) -> Vec<(String, String)> {
    if value.get("type").and_then(Value::as_str) != Some("assistant") {
        return Vec::new();
    }
    value
        .pointer("/message/content")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|block| {
            let kind = block.get("type").and_then(Value::as_str)?;
            let name = block.get("name").and_then(Value::as_str)?;
            if kind != "tool_use" || !matches!(name, "Bash" | "PowerShell") {
                return None;
            }
            Some((
                block.get("id").and_then(Value::as_str)?.to_string(),
                block
                    .pointer("/input/command")
                    .and_then(Value::as_str)?
                    .to_string(),
            ))
        })
        .collect()
}

/// Extract successful Claude tool-result IDs and Git-issued OID prefixes.
fn claude_successful_results(value: &Value) -> Vec<(String, Vec<String>)> {
    if value.get("type").and_then(Value::as_str) != Some("user") {
        return Vec::new();
    }
    value
        .pointer("/message/content")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|block| {
            if block.get("type").and_then(Value::as_str) != Some("tool_result")
                || block.get("is_error").and_then(Value::as_bool) != Some(false)
            {
                return None;
            }
            let output = block.get("content").and_then(value_text)?;
            let prefixes = git_commit_prefixes(&output);
            if prefixes.is_empty() {
                return None;
            }
            Some((
                block
                    .get("tool_use_id")
                    .and_then(Value::as_str)?
                    .to_string(),
                prefixes,
            ))
        })
        .collect()
}

/// Turn provider command representations into a shell script string.
fn command_value(value: &Value) -> Option<String> {
    if let Some(command) = value.as_str() {
        return Some(command.to_string());
    }
    let args: Vec<&str> = value.as_array()?.iter().filter_map(Value::as_str).collect();
    let mut args_iter = args.iter().copied();
    while let Some(arg) = args_iter.next() {
        if matches!(arg, "-c" | "-lc") {
            return args_iter.next().map(ToString::to_string);
        }
    }
    (!args.is_empty()).then(|| args.join(" "))
}

/// Convert a JSON string or text-block array to plain text.
fn value_text(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => Some(text.clone()),
        Value::Array(parts) => {
            let joined = parts
                .iter()
                .filter_map(|part| {
                    part.as_str().map(ToString::to_string).or_else(|| {
                        part.get("text")
                            .and_then(Value::as_str)
                            .map(ToString::to_string)
                    })
                })
                .collect::<Vec<_>>()
                .join("\n");
            (!joined.is_empty()).then_some(joined)
        }
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::Object(_) => None,
    }
}

/// Extract OID abbreviations from Git's standard successful commit header.
fn git_commit_prefixes(output: &str) -> Vec<String> {
    let mut prefixes = Vec::new();
    for line in output.lines() {
        let Some(after_open) = line.strip_prefix('[') else {
            continue;
        };
        let Some((header, _)) = after_open.split_once(']') else {
            continue;
        };
        let Some(prefix) = header.split_ascii_whitespace().next_back() else {
            continue;
        };
        if (7..=64).contains(&prefix.len()) && prefix.as_bytes().iter().all(u8::is_ascii_hexdigit) {
            let prefix = prefix.to_ascii_lowercase();
            if !prefixes.contains(&prefix) {
                prefixes.push(prefix);
            }
        }
    }
    prefixes
}

/// Whether a shell script executes `git commit` at a command boundary.
fn command_invokes_git_commit(script: &str) -> bool {
    shell_segments(script).iter().any(|segment| {
        let Some(words) = tokenize_shell_segment(segment) else {
            return false;
        };
        words.iter().enumerate().any(|(index, word)| {
            is_git_executable(word)
                && words.get(..index).is_some_and(valid_command_prefix)
                && words
                    .get(index.saturating_add(1)..)
                    .and_then(git_subcommand)
                    == Some("commit")
        })
    })
}

/// Split a shell script at unquoted control operators.
fn shell_segments(script: &str) -> Vec<String> {
    let mut segments = Vec::new();
    let mut current = String::new();
    let mut quote = None;
    let mut escaped = false;
    for character in script.chars() {
        if escaped {
            current.push(character);
            escaped = false;
            continue;
        }
        if character == '\\' && quote != Some('\'') {
            current.push(character);
            escaped = true;
            continue;
        }
        if let Some(delimiter) = quote {
            current.push(character);
            if character == delimiter {
                quote = None;
            }
            continue;
        }
        if matches!(character, '\'' | '"') {
            quote = Some(character);
            current.push(character);
        } else if matches!(character, '\n' | ';' | '&' | '|' | '(' | ')') {
            if !current.trim().is_empty() {
                segments.push(std::mem::take(&mut current));
            }
        } else {
            current.push(character);
        }
    }
    if !current.trim().is_empty() {
        segments.push(current);
    }
    segments
}

/// Tokenize one shell command segment while respecting simple quoting and
/// escapes. Invalid unterminated quoting is rejected conservatively.
fn tokenize_shell_segment(segment: &str) -> Option<Vec<String>> {
    let mut words = Vec::new();
    let mut current = String::new();
    let mut quote = None;
    let mut escaped = false;
    for character in segment.chars() {
        if escaped {
            current.push(character);
            escaped = false;
            continue;
        }
        if character == '\\' && quote != Some('\'') {
            escaped = true;
            continue;
        }
        if let Some(delimiter) = quote {
            if character == delimiter {
                quote = None;
            } else {
                current.push(character);
            }
            continue;
        }
        if matches!(character, '\'' | '"') {
            quote = Some(character);
        } else if character.is_whitespace() {
            if !current.is_empty() {
                words.push(std::mem::take(&mut current));
            }
        } else {
            current.push(character);
        }
    }
    if escaped || quote.is_some() {
        return None;
    }
    if !current.is_empty() {
        words.push(current);
    }
    Some(words)
}

/// Whether words preceding `git` can legally occupy shell command position.
fn valid_command_prefix(words: &[String]) -> bool {
    words.iter().all(|word| {
        matches!(
            word.as_str(),
            "!" | "command" | "do" | "else" | "env" | "if" | "then" | "time" | "while"
        ) || shell_assignment(word)
    })
}

/// Whether a token is a shell environment assignment.
fn shell_assignment(word: &str) -> bool {
    word.split_once('=').is_some_and(|(name, _)| {
        !name.is_empty()
            && name
                .as_bytes()
                .iter()
                .all(|byte| byte.is_ascii_alphanumeric() || *byte == b'_')
    })
}

/// Whether a command-position token names the Git executable.
fn is_git_executable(word: &str) -> bool {
    word == "git" || word.rsplit_once('/').is_some_and(|(_, name)| name == "git")
}

/// Return Git's subcommand after consuming global options.
fn git_subcommand(args: &[String]) -> Option<&str> {
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        if arg == "--" {
            return args.next().map(String::as_str);
        }
        if !arg.starts_with('-') || arg == "-" {
            return Some(arg);
        }
        if matches!(
            arg.as_str(),
            "-C" | "-c"
                | "--config-env"
                | "--exec-path"
                | "--git-dir"
                | "--namespace"
                | "--work-tree"
        ) {
            let _: &String = args.next()?;
        }
    }
    None
}

/// Read one inline or verified content-addressed payload.
pub(super) fn payload_bytes<'a>(
    payload: &'a Payload,
    blobs: Option<&FsBlobSink>,
) -> Option<Cow<'a, [u8]>> {
    match payload {
        Payload::Empty => None,
        Payload::Inline(bytes) => Some(Cow::Borrowed(bytes)),
        Payload::Blob(blob) => {
            let ContentId::Hash256(hash) = blob.id else {
                return None;
            };
            let bytes = blobs?.get(&hash).ok()??;
            if usize::try_from(blob.len).ok()? != bytes.len()
                || editchain_import::hash_raw(&bytes) != hash
            {
                return None;
            }
            Some(Cow::Owned(bytes))
        }
    }
}

/// A byte-slice substring check without assuming UTF-8.
fn contains_bytes(haystack: &[u8], needle: &[u8]) -> bool {
    !needle.is_empty()
        && haystack
            .windows(needle.len())
            .any(|window| window == needle)
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

        let links = derive_produced_commit_links(temp.path(), &ops, None).unwrap();
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
            derive_produced_commit_links(temp.path(), &ops, None)
                .unwrap()
                .is_empty(),
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
            derive_produced_commit_links(temp.path(), &[command], None)
                .unwrap()
                .is_empty(),
            "a cross-repository ambiguous prefix must not choose an arbitrary target"
        );
    }

    #[test]
    fn unsuccessful_provider_completions_are_not_evidence() {
        let failed_codex = serde_json::json!({
            "type": "event_msg",
            "payload": {
                "type": "item_completed",
                "item": {
                    "type": "CommandExecution",
                    "command": ["git", "commit", "-m", "fixture"],
                    "status": "completed",
                    "exit_code": 1,
                    "stdout": "[main abcdef1] subject",
                },
            },
        });
        assert!(codex_command_evidence(&failed_codex).is_none());

        let failed_claude = serde_json::json!({
            "type": "user",
            "message": {"content": [{
                "type": "tool_result",
                "tool_use_id": "call-1",
                "content": "[main abcdef1] subject",
                "is_error": true,
            }]},
        });
        assert!(claude_successful_results(&failed_claude).is_empty());
    }

    #[test]
    fn command_detection_rejects_mentions_and_accepts_git_global_options() {
        assert!(command_invokes_git_commit(
            "git -C /repo -c user.name=Agent commit -m subject"
        ));
        assert!(command_invokes_git_commit(
            "cargo test && git commit --amend --no-edit"
        ));
        assert!(
            !command_invokes_git_commit("rg -n 'git commit' sessions | head"),
            "quoted search text is not an executed Git command"
        );
        assert!(
            !command_invokes_git_commit("echo git commit"),
            "Git words in command arguments are not executable position"
        );
    }

    #[test]
    fn commit_headers_require_git_shape_and_strong_prefix_length() {
        assert_eq!(
            git_commit_prefixes("[main abcdef1] subject\n 1 file changed"),
            vec!["abcdef1"]
        );
        assert_eq!(
            git_commit_prefixes("[main (root-commit) 0123456] root"),
            vec!["0123456"]
        );
        assert!(git_commit_prefixes("abcdef1 subject").is_empty());
        assert!(git_commit_prefixes("[main abc123] too short").is_empty());
    }
}
