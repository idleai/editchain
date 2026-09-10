//! Typed Git-related observations extracted from accepted provider records.
//!
//! These adapters read provider bytes without opening repositories or emitting
//! relationships. Missing or unverified payloads contribute no evidence.

use std::borrow::Cow;
use std::collections::HashMap;
use std::path::PathBuf;

use editchain_core::{ContentId, NodeId, Op, OpKind, Payload, ScopeRef, SessionId};
use serde_json::Value;

use crate::ids::derive_session_id;
use crate::sink::FsBlobSink;
use crate::source_time::parse_source_time;

/// Exact evidence that one imported completion record produced commit objects.
#[derive(Debug)]
pub struct CommitEvidence<'a> {
    /// Physical successful completion carrying the Git output.
    pub source: &'a Op,
    /// Recorded shell command, correlated by exact provider call identity.
    pub command: String,
    /// Git-issued commit abbreviations, without repository resolution.
    pub prefixes: Vec<String>,
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

impl CommitEvidence<'_> {
    /// Whether the recorded script executes Git's commit subcommand at a
    /// supported shell command boundary. Textual mentions are insufficient.
    #[must_use]
    pub fn invokes_git_commit(&self) -> bool {
        command_invokes_git_commit(&self.command)
    }
}

/// Collect provider-neutral commit evidence from byte-exact raw imports.
#[must_use]
pub fn collect_commit_evidence<'a>(
    ops: &'a [Op],
    blobs: Option<&FsBlobSink>,
) -> Vec<CommitEvidence<'a>> {
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
#[must_use]
pub fn payload_bytes<'a>(
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
            if usize::try_from(blob.len).ok()? != bytes.len() || crate::hash_raw(&bytes) != hash {
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

/// One provider record that can recover a session's historical branch tip.
#[derive(Debug)]
pub struct ClaudeStartEvidence {
    /// Stored source sequence of the record carrying the observation.
    pub source_seq: u64,
    /// Validated owning provider session.
    pub session: SessionId,
    /// Recorded working directory.
    pub cwd: PathBuf,
    /// Recorded active branch name.
    pub branch: String,
    /// Validated observed event time in milliseconds.
    pub unix_ms: u64,
}

/// Parse only the exact Claude fields required by the historical resolver.
#[must_use]
pub fn claude_start_evidence(op: &Op, raw: &[u8]) -> Option<ClaudeStartEvidence> {
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
    Some(ClaudeStartEvidence {
        source_seq: op.id.seq,
        session,
        cwd: PathBuf::from(cwd),
        branch: branch.to_owned(),
        unix_ms: parse_source_time(timestamp)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

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
