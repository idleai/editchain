//! Advertised Git and agent file changes and verified on-demand diffs.

use super::details::payload_text;
use super::legacy_preview::json_string_field;
use super::{parse_git_oid, parse_repository_id, BlobResolution, BlobResolver, Workspace};
use editchain_core::{GitOid, Op, OpId, OpKind, Payload, RepositoryId, ScopeRef, SessionId};
use editchain_git::{
    commit_file_changes, open_repository, resolve_blob as resolve_git_blob, resolve_path_at_commit,
    GitFileChange, GitFileStatus, RepositoryHandle,
};
use editchain_project::HistoryProjection;
use editchain_protocol::{
    FileChangeDto, FileChangeSource, FileChangeStatus, FileDiffDto, FileDiffHunkDto,
};
use editchain_store::BlobPreviewResolution;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

impl Workspace {
    /// Revalidate and materialize a file-row identity for VS Code's native
    /// diff editor.
    pub(crate) fn file_diff(&self, change: &FileChangeDto) -> Result<FileDiffDto, String> {
        match change.source {
            FileChangeSource::Git => self.git_file_diff(change),
            FileChangeSource::Agent | FileChangeSource::Human => self.agent_file_diff(change),
            FileChangeSource::Unknown => Err("unknown file-change source".to_string()),
        }
    }

    fn git_file_diff(&self, requested: &FileChangeDto) -> Result<FileDiffDto, String> {
        let repository = requested
            .repository
            .as_deref()
            .ok_or_else(|| "git file change has no repository".to_string())
            .and_then(parse_repository_id)?;
        let commit_oid = requested
            .commit_oid
            .as_deref()
            .ok_or_else(|| "git file change has no commit".to_string())
            .and_then(parse_git_oid)?;
        let discovery = self
            .repositories
            .iter()
            .find(|discovery| discovery.id == repository)
            .ok_or_else(|| "repository not found".to_string())?;
        let handle = open_repository(discovery).map_err(|error| error.to_string())?;
        let advertised = commit_file_changes(&handle, &commit_oid)
            .map_err(|error| error.to_string())?
            .into_iter()
            .map(|change| git_file_change_dto(repository, commit_oid, change))
            .find(|actual| same_git_file_change(actual, requested))
            .ok_or_else(|| "file change is not present in the commit".to_string())?;

        if advertised.binary {
            return Ok(FileDiffDto {
                path: advertised.path,
                old_path: advertised.old_path,
                status: advertised.status,
                binary: true,
                partial: false,
                before: String::new(),
                after: String::new(),
                hunks: Vec::new(),
                note: Some("Binary Git blobs cannot be opened as text.".to_string()),
            });
        }
        let before = git_diff_side(
            &handle,
            advertised.old_oid.as_deref(),
            advertised.old_mode.as_deref(),
        )?;
        let after = git_diff_side(
            &handle,
            advertised.new_oid.as_deref(),
            advertised.new_mode.as_deref(),
        )?;
        Ok(FileDiffDto {
            path: advertised.path,
            old_path: advertised.old_path,
            status: advertised.status,
            binary: false,
            partial: false,
            before,
            after,
            hunks: Vec::new(),
            note: None,
        })
    }

    fn agent_file_diff(&self, requested: &FileChangeDto) -> Result<FileDiffDto, String> {
        let op_id = requested
            .op_id
            .as_deref()
            .and_then(OpId::from_display_str)
            .ok_or_else(|| "agent file change has no valid operation id".to_string())?;
        let known = self
            .agent_file_changes
            .values()
            .flatten()
            .any(|change| change == requested);
        if !known {
            return Err("agent file change is not present in the canonical history".to_string());
        }
        let op = self
            .source_op(op_id)
            .ok_or_else(|| "agent edit operation not found".to_string())?;
        match &op.kind {
            OpKind::Tool(tool) => materialize_tool_diff(self, tool, requested),
            OpKind::File(file) => materialize_file_op_diff(self, file, requested),
            OpKind::Import(import) => materialize_codex_raw_file_diff(self, import, requested),
            OpKind::ChainStart(_)
            | OpKind::Actor(_)
            | OpKind::Message(_)
            | OpKind::Command(_)
            | OpKind::Reflection(_)
            | OpKind::Note(_)
            | OpKind::Error(_)
            | OpKind::GitCommit(_)
            | OpKind::GitLink(_)
            | OpKind::Unknown(_) => Err("operation is not a retained file edit".to_string()),
        }
    }
}

/// Session-level Git evidence retained by an importer.
#[derive(Debug, Clone)]
struct SessionGitContext {
    cwd: Option<PathBuf>,
    repository: RepositoryId,
    commit_oid: GitOid,
}

/// Complete file evidence carried by Codex's raw `item_completed/FileChange`
/// record. Reading this additive source lane keeps version-four chains useful:
/// their legacy normalized `FileOp` collapsed a multi-path change onto the
/// first path, while the byte-exact raw record still retains every path.
#[derive(Debug, Clone)]
enum RecordedCodexFileEdit {
    Add(String),
    Delete(String),
    Update(String),
}

#[derive(Debug, Clone)]
struct RecordedCodexFileChange {
    path: String,
    edit: RecordedCodexFileEdit,
}

#[derive(Debug, Clone, Copy)]
struct AgentFileEvidence {
    status: FileChangeStatus,
    binary: bool,
    partial: bool,
}

struct AgentFileIndexContext<'a> {
    workspace_root: &'a Path,
    session_contexts: &'a HashMap<SessionId, SessionGitContext>,
    repositories: &'a [editchain_git::RepositoryDiscovery],
}

impl RecordedCodexFileChange {
    const fn status(&self) -> FileChangeStatus {
        match &self.edit {
            RecordedCodexFileEdit::Add(_) => FileChangeStatus::Added,
            RecordedCodexFileEdit::Delete(_) => FileChangeStatus::Deleted,
            RecordedCodexFileEdit::Update(_) => FileChangeStatus::Modified,
        }
    }

    const fn partial(&self) -> bool {
        matches!(&self.edit, RecordedCodexFileEdit::Update(_))
    }

    fn binary(&self) -> bool {
        let content = match &self.edit {
            RecordedCodexFileEdit::Add(content)
            | RecordedCodexFileEdit::Delete(content)
            | RecordedCodexFileEdit::Update(content) => content,
        };
        bytes_are_binary(content.as_bytes())
    }
}

/// Build the backward-compatible agent file-row index from operations that are
/// already present in the chain. Claude edits are recovered from structured
/// tool inputs. Codex prefers the byte-exact raw `FileChange` record (which
/// repairs historical multi-path normalization loss at read time), then falls
/// back to normalized `FileOp`s plus their explicit path annotation notes.
pub(super) fn agent_file_change_index(
    ops: &[Op],
    workspace_root: &Path,
    resolver: Option<&BlobResolver>,
    repositories: &[editchain_git::RepositoryDiscovery],
) -> HashMap<OpId, Vec<FileChangeDto>> {
    let import_ids: std::collections::HashSet<OpId> = ops
        .iter()
        .filter(|op| matches!(op.kind, OpKind::Import(_)))
        .map(|op| op.id)
        .collect();
    let raw_codex_candidates: std::collections::HashSet<OpId> = ops
        .iter()
        .filter(|op| matches!(op.kind, OpKind::File(_)))
        .flat_map(|op| op.parents.iter())
        .filter(|parent| import_ids.contains(parent))
        .copied()
        .collect();
    let path_notes = agent_path_notes(ops);
    let session_contexts = session_git_contexts(ops, resolver);
    let op_by_id: HashMap<OpId, &Op> = ops.iter().map(|op| (op.id, op)).collect();
    let mut changes: HashMap<OpId, Vec<FileChangeDto>> = HashMap::new();
    let mut raw_codex_owners = std::collections::HashSet::new();
    let index_context = AgentFileIndexContext {
        workspace_root,
        session_contexts: &session_contexts,
        repositories,
    };

    // Old Codex chains remain append-only and cannot replace the legacy first
    // FileOp at the same deterministic ID. Recover the authoritative list from
    // its retained raw record and suppress only that record's lossy normalized
    // children. Fresh version-five imports take this path too, keeping one
    // canonical descriptor/materializer across generations. Restrict full raw
    // hydration to imports that own a normalized FileOp: unrelated command and
    // tool-result records can contain very large blob payloads.
    for op in ops {
        let OpKind::Import(import) = &op.kind else {
            continue;
        };
        if !raw_codex_candidates.contains(&op.id) {
            continue;
        }
        let Some(raw) = complete_payload_text(&import.raw_ref, resolver) else {
            continue;
        };
        let recorded = recorded_codex_file_changes(&raw);
        if recorded.is_empty() {
            continue;
        }
        let _inserted = raw_codex_owners.insert(op.id);
        let rows = changes.entry(op.id).or_default();
        for change in recorded {
            rows.push(agent_file_change_dto(
                op.id,
                op,
                &change.path,
                AgentFileEvidence {
                    status: change.status(),
                    binary: change.binary(),
                    partial: change.partial(),
                },
                &index_context,
            ));
        }
    }

    for op in ops {
        let owner = op
            .parents
            .iter()
            .find(|parent| import_ids.contains(parent))
            .copied()
            .unwrap_or(op.id);
        if raw_codex_owners.contains(&owner) {
            continue;
        }
        let path = match &op.kind {
            OpKind::Tool(tool)
                if matches!(tool.stage, editchain_core::op::ToolStage::Start)
                    && is_file_edit_tool(&payload_text(&tool.tool_name)) =>
            {
                let input = payload_preview_text(&tool.content, resolver);
                edit_tool_path(&input)
            }
            OpKind::File(file)
                if !matches!(file.edit, editchain_core::op::FileEdit::None)
                    || file.base.is_some()
                    || file.after.is_some() =>
            {
                path_notes.get(&op.id).cloned()
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
        };
        let Some(path) = path.filter(|path| !path.trim().is_empty()) else {
            continue;
        };
        let status = match &op.kind {
            OpKind::File(file) if matches!(file.stage, editchain_core::op::FileStage::Deleted) => {
                FileChangeStatus::Deleted
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
            | OpKind::Unknown(_) => FileChangeStatus::Modified,
        };
        let partial = !matches!(
            &op.kind,
            OpKind::File(file) if file.base.is_some() && file.after.is_some()
        );
        let owner_op = op_by_id.get(&owner).copied().unwrap_or(op);
        let mut change = agent_file_change_dto(
            op.id,
            owner_op,
            &path,
            AgentFileEvidence {
                status,
                binary: false,
                partial,
            },
            &index_context,
        );
        if op.tags.matches_any(editchain_core::Tags::HUMAN) {
            change.source = FileChangeSource::Human;
        }
        changes.entry(owner).or_default().push(change);
    }
    for rows in changes.values_mut() {
        rows.sort_by(|left, right| {
            left.path
                .cmp(&right.path)
                .then_with(|| left.op_id.cmp(&right.op_id))
        });
    }
    changes
}

fn agent_file_change_dto(
    source_op: OpId,
    owner: &Op,
    path: &str,
    evidence: AgentFileEvidence,
    index: &AgentFileIndexContext<'_>,
) -> FileChangeDto {
    let session = match owner.scope {
        ScopeRef::Session(session) => Some(session),
        ScopeRef::None | ScopeRef::Chain(_) | ScopeRef::Turn(_) | ScopeRef::File(_) => None,
    };
    let context = session.and_then(|session| index.session_contexts.get(&session));
    let (repository, commit_oid, repository_path) = context.map_or((None, None, None), |context| {
        let repository_path = index
            .repositories
            .iter()
            .find(|repo| repo.id == context.repository)
            .and_then(|repo| agent_repository_path(path, context.cwd.as_deref(), repo));
        (
            Some(context.repository.0.to_string()),
            Some(context.commit_oid.to_hex()),
            repository_path,
        )
    });
    FileChangeDto {
        source: FileChangeSource::Agent,
        path: display_agent_path(path, index.workspace_root),
        old_path: None,
        status: evidence.status,
        binary: evidence.binary,
        partial: evidence.partial,
        op_id: Some(source_op.to_string()),
        repository,
        repository_path,
        commit_oid,
        old_oid: None,
        new_oid: None,
        old_mode: None,
        new_mode: None,
    }
}

/// Compute immutable Git file rows once per loaded projection. The render
/// snapshot persists these additive DTOs, so reopening the same HEAD does not
/// repeat every historical tree diff.
pub(super) fn git_file_change_index(
    projection: &HistoryProjection,
    repositories: &[editchain_git::RepositoryDiscovery],
) -> HashMap<(RepositoryId, GitOid), Vec<FileChangeDto>> {
    let mut index = HashMap::new();
    for discovery in repositories {
        let Ok(handle) = open_repository(discovery) else {
            continue;
        };
        for commit in projection
            .git()
            .commits()
            .values()
            .filter(|commit| commit.repository == discovery.id)
        {
            let Ok(changes) = commit_file_changes(&handle, &commit.oid) else {
                continue;
            };
            let rows = changes
                .into_iter()
                .map(|change| git_file_change_dto(commit.repository, commit.oid, change))
                .collect();
            drop(index.insert((commit.repository, commit.oid), rows));
        }
    }
    index
}

fn git_file_change_dto(
    repository: RepositoryId,
    commit_oid: GitOid,
    change: GitFileChange,
) -> FileChangeDto {
    FileChangeDto {
        source: FileChangeSource::Git,
        repository: Some(repository.0.to_string()),
        repository_path: Some(change.path.clone()),
        commit_oid: Some(commit_oid.to_hex()),
        path: change.path,
        old_path: change.old_path,
        status: protocol_git_file_status(change.status),
        binary: change.binary,
        partial: false,
        op_id: None,
        old_oid: change.old_oid.map(|oid| oid.to_hex()),
        new_oid: change.new_oid.map(|oid| oid.to_hex()),
        old_mode: change.old_mode,
        new_mode: change.new_mode,
    }
}

#[must_use]
const fn protocol_git_file_status(status: GitFileStatus) -> FileChangeStatus {
    match status {
        GitFileStatus::Added => FileChangeStatus::Added,
        GitFileStatus::Deleted => FileChangeStatus::Deleted,
        GitFileStatus::Modified => FileChangeStatus::Modified,
        GitFileStatus::Renamed => FileChangeStatus::Renamed,
        GitFileStatus::Copied => FileChangeStatus::Copied,
        GitFileStatus::TypeChanged => FileChangeStatus::TypeChanged,
    }
}

fn agent_path_notes(ops: &[Op]) -> HashMap<OpId, String> {
    let mut paths = HashMap::new();
    for op in ops {
        let OpKind::Note(note) = &op.kind else {
            continue;
        };
        if note.relationship != editchain_core::op::NoteRelationship::Explains {
            continue;
        }
        let path = payload_text(&note.content);
        if path.trim().is_empty() {
            continue;
        }
        for target in &note.target_ids {
            let _path = paths.entry(*target).or_insert_with(|| path.clone());
        }
    }
    paths
}

fn session_git_contexts(
    ops: &[Op],
    resolver: Option<&BlobResolver>,
) -> HashMap<SessionId, SessionGitContext> {
    let mut cwd_by_session: HashMap<SessionId, PathBuf> = HashMap::new();
    for op in ops {
        let (ScopeRef::Session(session), OpKind::Import(import)) = (op.scope, &op.kind) else {
            continue;
        };
        if cwd_by_session.contains_key(&session) {
            continue;
        }
        let raw = payload_preview_text(&import.raw_ref, resolver);
        if let Some(cwd) = import_cwd(&raw) {
            drop(cwd_by_session.insert(session, PathBuf::from(cwd)));
        }
    }
    let mut contexts = HashMap::new();
    for op in ops {
        let (ScopeRef::Session(session), OpKind::GitLink(link)) = (op.scope, &op.kind) else {
            continue;
        };
        if link.kind != editchain_core::GitLinkKind::BasedOn {
            continue;
        }
        let _context = contexts
            .entry(session)
            .or_insert_with(|| SessionGitContext {
                cwd: cwd_by_session.get(&session).cloned(),
                repository: link.target_repo,
                commit_oid: link.target_oid,
            });
    }
    contexts
}

fn payload_preview_text(payload: &Payload, resolver: Option<&BlobResolver>) -> String {
    match payload {
        Payload::Inline(bytes) => String::from_utf8_lossy(bytes).into_owned(),
        Payload::Blob(blob) => resolver
            .and_then(|resolver| match resolver.preview(blob, 16 * 1024) {
                BlobPreviewResolution::Found(bytes) => {
                    Some(String::from_utf8_lossy(&bytes).into_owned())
                }
                BlobPreviewResolution::Missing
                | BlobPreviewResolution::Corrupt
                | BlobPreviewResolution::Unresolvable => None,
            })
            .unwrap_or_default(),
        Payload::Empty => String::new(),
    }
}

/// Resolve a complete UTF-8 payload for exact source-evidence parsing.
fn complete_payload_text(payload: &Payload, resolver: Option<&BlobResolver>) -> Option<String> {
    let bytes = match payload {
        Payload::Inline(bytes) => bytes.clone(),
        Payload::Blob(blob) => match resolver?.resolve(blob) {
            BlobResolution::Found(bytes) => bytes,
            BlobResolution::Missing | BlobResolution::Corrupt | BlobResolution::Unresolvable => {
                return None
            }
        },
        Payload::Empty => return None,
    };
    String::from_utf8(bytes).ok()
}

/// Parse the exact Codex rollout shape that carries completed file content.
/// Unknown generations/shapes remain on the normalized `FileOp` fallback.
fn recorded_codex_file_changes(raw: &str) -> Vec<RecordedCodexFileChange> {
    let Ok(root) = serde_json::from_str::<serde_json::Value>(raw) else {
        return Vec::new();
    };
    if root.get("type").and_then(serde_json::Value::as_str) != Some("event_msg") {
        return Vec::new();
    }
    let Some(payload) = root.get("payload") else {
        return Vec::new();
    };
    if payload.get("type").and_then(serde_json::Value::as_str) != Some("item_completed") {
        return Vec::new();
    }
    let Some(item) = payload.get("item") else {
        return Vec::new();
    };
    if !item
        .get("type")
        .and_then(serde_json::Value::as_str)
        .is_some_and(|kind| kind.eq_ignore_ascii_case("filechange"))
    {
        return Vec::new();
    }
    let Some(changes) = item.get("changes").and_then(serde_json::Value::as_object) else {
        return Vec::new();
    };
    changes
        .iter()
        .filter_map(|(path, change)| {
            if path.trim().is_empty() {
                return None;
            }
            let kind = change
                .get("type")
                .and_then(serde_json::Value::as_str)?
                .to_ascii_lowercase();
            let edit = match kind.as_str() {
                "add" => RecordedCodexFileEdit::Add(json_text(change.get("content"))?),
                "delete" => RecordedCodexFileEdit::Delete(json_text(change.get("content"))?),
                "update" => RecordedCodexFileEdit::Update(json_text(
                    change.get("unified_diff").or_else(|| change.get("diff")),
                )?),
                _ => return None,
            };
            Some(RecordedCodexFileChange {
                path: path.clone(),
                edit,
            })
        })
        .collect()
}

#[must_use]
fn is_file_edit_tool(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "edit" | "write" | "multiedit" | "notebookedit"
    )
}

fn edit_tool_path(input: &str) -> Option<String> {
    serde_json::from_str::<serde_json::Value>(input)
        .ok()
        .and_then(|value| {
            ["file_path", "notebook_path", "path"]
                .into_iter()
                .find_map(|key| value.get(key).and_then(serde_json::Value::as_str))
                .map(str::to_owned)
        })
        .or_else(|| {
            ["file_path", "notebook_path", "path"]
                .into_iter()
                .find_map(|key| json_string_field(input, key, 0).map(str::to_owned))
        })
}

fn import_cwd(raw: &str) -> Option<String> {
    serde_json::from_str::<serde_json::Value>(raw)
        .ok()
        .and_then(|value| {
            value
                .get("cwd")
                .or_else(|| value.get("payload").and_then(|payload| payload.get("cwd")))
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned)
        })
        .or_else(|| json_string_field(raw, "cwd", 0).map(str::to_owned))
}

fn display_agent_path(path: &str, workspace_root: &Path) -> String {
    let path_buf = Path::new(path);
    let displayed = if path_buf.is_absolute() {
        path_buf.strip_prefix(workspace_root).unwrap_or(path_buf)
    } else {
        path_buf
    };
    displayed.to_string_lossy().replace('\\', "/")
}

fn agent_repository_path(
    path: &str,
    cwd: Option<&Path>,
    repository: &editchain_git::RepositoryDiscovery,
) -> Option<String> {
    repository
        .relative_worktree_path(Path::new(path), cwd)
        .map(|relative| relative.to_string_lossy().replace('\\', "/"))
}

/// Require the complete immutable identity that was advertised in a Git file
/// row. This prevents a webview message from swapping paths or object IDs
/// before the service reads blob content.
#[must_use]
fn same_git_file_change(actual: &FileChangeDto, requested: &FileChangeDto) -> bool {
    actual == requested
}

fn git_diff_side(
    handle: &RepositoryHandle,
    oid: Option<&str>,
    mode: Option<&str>,
) -> Result<String, String> {
    let Some(oid) = oid else {
        return Ok(String::new());
    };
    if mode == Some("commit") {
        return Ok(format!("{oid}\n"));
    }
    let oid = parse_git_oid(oid)?;
    let blob = resolve_git_blob(handle, &oid).map_err(|error| error.to_string())?;
    if blob.binary {
        return Err("Git blob is not UTF-8 text".to_string());
    }
    String::from_utf8(blob.bytes).map_err(|error| format!("Git blob is not UTF-8 text: {error}"))
}

#[derive(Debug)]
enum AgentBaseline {
    Missing,
    Text(String),
    Binary,
}

#[derive(Debug)]
struct RecordedReplacement {
    old: String,
    new: String,
    replace_all: bool,
}

#[derive(Debug)]
struct AgentDiffContent {
    before: String,
    after: String,
    binary: bool,
    note: String,
}

fn materialize_tool_diff(
    workspace: &Workspace,
    tool: &editchain_core::op::ToolOp,
    requested: &FileChangeDto,
) -> Result<FileDiffDto, String> {
    let input = serde_json::from_str::<serde_json::Value>(&payload_text(&tool.content))
        .map_err(|error| format!("invalid retained tool input: {error}"))?;
    let name = payload_text(&tool.tool_name).to_ascii_lowercase();
    let baseline = agent_git_baseline(workspace, requested);
    let content = match name.as_str() {
        "edit" => materialize_replacements(&input, baseline, false)?,
        "multiedit" => materialize_replacements(&input, baseline, true)?,
        "write" => materialize_write(&input, baseline)?,
        "notebookedit" => materialize_notebook_edit(&input),
        _ => return Err("operation is not a supported file-edit tool".to_string()),
    };
    Ok(FileDiffDto {
        path: requested.path.clone(),
        old_path: requested.old_path.clone(),
        status: requested.status,
        binary: content.binary,
        partial: true,
        before: content.before,
        after: content.after,
        hunks: Vec::new(),
        note: Some(content.note),
    })
}

fn materialize_replacements(
    input: &serde_json::Value,
    baseline: Option<AgentBaseline>,
    multiple: bool,
) -> Result<AgentDiffContent, String> {
    let replacements = if multiple {
        input
            .get("edits")
            .and_then(serde_json::Value::as_array)
            .ok_or_else(|| "retained MultiEdit input has no edits".to_string())?
            .iter()
            .filter_map(recorded_replacement)
            .collect::<Vec<_>>()
    } else {
        recorded_replacement(input).into_iter().collect()
    };
    if replacements.is_empty() {
        return Err("retained edit input has no replacement text".to_string());
    }
    match baseline {
        Some(AgentBaseline::Binary) => Ok(binary_agent_diff(
            "The Git-anchored session baseline is binary; the recorded text edit cannot be previewed.",
        )),
        Some(AgentBaseline::Text(before)) => {
            if let Some(after) = apply_recorded_replacements(&before, &replacements) {
                return Ok(AgentDiffContent {
                    before,
                    after,
                    binary: false,
                    note: "Reconstructed from recorded edit arguments against the session's exact Git baseline; intervening agent edits may not be represented.".to_string(),
                });
            }
            let (before, after) = replacement_snippets(&replacements);
            Ok(AgentDiffContent {
                before,
                after,
                binary: false,
                note: "Recorded edit snippets; they did not apply uniquely to the session's Git baseline.".to_string(),
            })
        }
        Some(AgentBaseline::Missing) | None => {
            let (before, after) = replacement_snippets(&replacements);
            Ok(AgentDiffContent {
                before,
                after,
                binary: false,
                note: "Recorded edit snippets; full before/after file snapshots were not retained.".to_string(),
            })
        }
    }
}

fn materialize_write(
    input: &serde_json::Value,
    baseline: Option<AgentBaseline>,
) -> Result<AgentDiffContent, String> {
    let after = json_text(input.get("content"))
        .ok_or_else(|| "retained Write input has no content".to_string())?;
    match baseline {
        Some(AgentBaseline::Binary) => Ok(binary_agent_diff(
            "The Git-anchored session baseline is binary; the recorded Write content is text.",
        )),
        Some(AgentBaseline::Text(before)) => Ok(AgentDiffContent {
            before,
            after,
            binary: false,
            note: "Recorded full Write content compared with the session's exact Git baseline; intervening agent edits may not be represented.".to_string(),
        }),
        Some(AgentBaseline::Missing) => Ok(AgentDiffContent {
            before: String::new(),
            after,
            binary: false,
            note: "Recorded full Write content; the path did not exist in the session's Git baseline.".to_string(),
        }),
        None => Ok(AgentDiffContent {
            before: String::new(),
            after,
            binary: false,
            note: "Recorded full Write content; the preceding file snapshot was not retained.".to_string(),
        }),
    }
}

fn materialize_notebook_edit(input: &serde_json::Value) -> AgentDiffContent {
    let before = ["old_source", "old_content"]
        .into_iter()
        .find_map(|key| json_text(input.get(key)))
        .unwrap_or_default();
    let after = ["new_source", "new_content", "source"]
        .into_iter()
        .find_map(|key| json_text(input.get(key)))
        .unwrap_or_default();
    AgentDiffContent {
        before,
        after,
        binary: false,
        note: "Recorded notebook cell content; a complete notebook before/after snapshot was not retained.".to_string(),
    }
}

fn recorded_replacement(value: &serde_json::Value) -> Option<RecordedReplacement> {
    let old = json_text(value.get("old_string"))?;
    let new = json_text(value.get("new_string"))?;
    Some(RecordedReplacement {
        old,
        new,
        replace_all: value
            .get("replace_all")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false),
    })
}

fn json_text(value: Option<&serde_json::Value>) -> Option<String> {
    match value? {
        serde_json::Value::String(text) => Some(text.clone()),
        serde_json::Value::Array(lines) => lines
            .iter()
            .map(serde_json::Value::as_str)
            .collect::<Option<Vec<_>>>()
            .map(|lines| lines.join("\n")),
        serde_json::Value::Null
        | serde_json::Value::Bool(_)
        | serde_json::Value::Number(_)
        | serde_json::Value::Object(_) => None,
    }
}

fn apply_recorded_replacements(
    baseline: &str,
    replacements: &[RecordedReplacement],
) -> Option<String> {
    let mut after = baseline.to_string();
    for replacement in replacements {
        if replacement.old.is_empty() {
            return None;
        }
        let occurrences = after.match_indices(&replacement.old).count();
        if occurrences == 0 || (!replacement.replace_all && occurrences != 1) {
            return None;
        }
        after = if replacement.replace_all {
            after.replace(&replacement.old, &replacement.new)
        } else {
            after.replacen(&replacement.old, &replacement.new, 1)
        };
    }
    Some(after)
}

fn replacement_snippets(replacements: &[RecordedReplacement]) -> (String, String) {
    const SEPARATOR: &str = "\n\n… next recorded edit …\n\n";
    (
        replacements
            .iter()
            .map(|replacement| replacement.old.as_str())
            .collect::<Vec<_>>()
            .join(SEPARATOR),
        replacements
            .iter()
            .map(|replacement| replacement.new.as_str())
            .collect::<Vec<_>>()
            .join(SEPARATOR),
    )
}

fn binary_agent_diff(note: &str) -> AgentDiffContent {
    AgentDiffContent {
        before: String::new(),
        after: String::new(),
        binary: true,
        note: note.to_string(),
    }
}

fn agent_git_baseline(workspace: &Workspace, requested: &FileChangeDto) -> Option<AgentBaseline> {
    let repository = parse_repository_id(requested.repository.as_deref()?).ok()?;
    let commit_oid = parse_git_oid(requested.commit_oid.as_deref()?).ok()?;
    let repository_path = requested.repository_path.as_deref()?;
    let discovery = workspace
        .repositories
        .iter()
        .find(|discovery| discovery.id == repository)?;
    let handle = open_repository(discovery).ok()?;
    match resolve_path_at_commit(&handle, &commit_oid, repository_path).ok()? {
        None => Some(AgentBaseline::Missing),
        Some(object) => match object.blob {
            Some(blob) if blob.binary => Some(AgentBaseline::Binary),
            Some(blob) => String::from_utf8(blob.bytes)
                .ok()
                .map(AgentBaseline::Text)
                .or(Some(AgentBaseline::Binary)),
            None => Some(AgentBaseline::Binary),
        },
    }
}

fn materialize_codex_raw_file_diff(
    workspace: &Workspace,
    import: &editchain_core::op::ImportOp,
    requested: &FileChangeDto,
) -> Result<FileDiffDto, String> {
    let raw = payload_text(&import.raw_ref);
    let recorded = recorded_codex_file_changes(&raw)
        .into_iter()
        .find(|change| display_agent_path(&change.path, &workspace.root_path) == requested.path)
        .ok_or_else(|| "Codex file evidence is not present in the raw operation".to_string())?;
    match recorded.edit {
        RecordedCodexFileEdit::Add(after) => file_diff_from_bytes(
            requested,
            Some(Vec::new()),
            Some(after.into_bytes()),
            false,
            None,
        ),
        RecordedCodexFileEdit::Delete(before) => file_diff_from_bytes(
            requested,
            Some(before.into_bytes()),
            Some(Vec::new()),
            false,
            None,
        ),
        RecordedCodexFileEdit::Update(diff) => {
            if bytes_are_binary(diff.as_bytes()) {
                return Ok(FileDiffDto {
                    path: requested.path.clone(),
                    old_path: requested.old_path.clone(),
                    status: requested.status,
                    binary: true,
                    partial: true,
                    before: String::new(),
                    after: String::new(),
                    hunks: Vec::new(),
                    note: Some(
                        "Recorded Codex update evidence is binary and cannot be opened as text."
                            .to_string(),
                    ),
                });
            }
            Ok(recorded_unified_diff(
                requested,
                &diff,
                "Recorded Codex unified-diff hunks; complete sequential file snapshots were not retained.",
            ))
        }
    }
}

fn materialize_file_op_diff(
    workspace: &Workspace,
    file: &editchain_core::op::FileOp,
    requested: &FileChangeDto,
) -> Result<FileDiffDto, String> {
    let base = file
        .base
        .and_then(|id| workspace.blob_resolver.as_ref()?.resolve_content(id));
    let retained_after = file
        .after
        .and_then(|id| workspace.blob_resolver.as_ref()?.resolve_content(id));
    if base.is_some() && retained_after.is_some() {
        return file_diff_from_bytes(requested, base, retained_after, false, None);
    }
    if matches!(file.stage, editchain_core::op::FileStage::Deleted) && base.is_some() {
        return file_diff_from_bytes(requested, base, Some(Vec::new()), false, None);
    }

    match &file.edit {
        editchain_core::op::FileEdit::ReplaceBytes { range, bytes } => {
            let replacement = payload_bytes(workspace, bytes)
                .ok_or_else(|| "replacement bytes are unavailable".to_string())?;
            if let Some(before) = base {
                let after = apply_byte_replacement(&before, *range, &replacement)?;
                file_diff_from_bytes(requested, Some(before), Some(after), false, None)
            } else {
                file_diff_from_bytes(
                    requested,
                    None,
                    Some(replacement),
                    true,
                    Some(
                        "Recorded replacement bytes; the complete preceding file was not retained.",
                    ),
                )
            }
        }
        editchain_core::op::FileEdit::UnifiedDiff(payload) => {
            let bytes = payload_bytes(workspace, payload)
                .ok_or_else(|| "unified diff payload is unavailable".to_string())?;
            let diff = String::from_utf8(bytes)
                .map_err(|error| format!("unified diff payload is not UTF-8: {error}"))?;
            Ok(recorded_unified_diff(
                requested,
                &diff,
                "Recorded unified-diff hunks; complete before/after file snapshots were not retained.",
            ))
        }
        editchain_core::op::FileEdit::Blob(blob) => {
            let after = workspace
                .blob_resolver
                .as_ref()
                .and_then(|resolver| match resolver.resolve(blob) {
                    BlobResolution::Found(bytes) => Some(bytes),
                    BlobResolution::Missing
                    | BlobResolution::Corrupt
                    | BlobResolution::Unresolvable => None,
                })
                .ok_or_else(|| "result blob is unavailable".to_string())?;
            let partial = base.is_none();
            file_diff_from_bytes(
                requested,
                base,
                Some(after),
                partial,
                partial.then_some(
                    "Recorded result content; the complete preceding file was not retained.",
                ),
            )
        }
        editchain_core::op::FileEdit::None => file_diff_from_bytes(
            requested,
            base,
            retained_after,
            true,
            Some("Only one retained file snapshot is available for this operation."),
        ),
    }
}

fn payload_bytes(workspace: &Workspace, payload: &Payload) -> Option<Vec<u8>> {
    match payload {
        Payload::Inline(bytes) => Some(bytes.clone()),
        Payload::Empty => Some(Vec::new()),
        Payload::Blob(blob) => workspace
            .blob_resolver
            .as_ref()
            .and_then(|resolver| match resolver.resolve(blob) {
                BlobResolution::Found(bytes) => Some(bytes),
                BlobResolution::Missing
                | BlobResolution::Corrupt
                | BlobResolution::Unresolvable => None,
            }),
    }
}

fn apply_byte_replacement(
    before: &[u8],
    range: editchain_core::op::ByteRange,
    replacement: &[u8],
) -> Result<Vec<u8>, String> {
    let start = usize::try_from(range.start)
        .map_err(|error| format!("replacement range start is too large: {error}"))?;
    let end = usize::try_from(range.end)
        .map_err(|error| format!("replacement range end is too large: {error}"))?;
    if start > end || end > before.len() {
        return Err("replacement range is outside the retained base".to_string());
    }
    let mut after = Vec::with_capacity(
        before
            .len()
            .saturating_sub(end.saturating_sub(start))
            .saturating_add(replacement.len()),
    );
    let prefix = before
        .get(..start)
        .ok_or_else(|| "replacement start is outside the retained base".to_string())?;
    let suffix = before
        .get(end..)
        .ok_or_else(|| "replacement end is outside the retained base".to_string())?;
    after.extend_from_slice(prefix);
    after.extend_from_slice(replacement);
    after.extend_from_slice(suffix);
    Ok(after)
}

fn file_diff_from_bytes(
    requested: &FileChangeDto,
    before: Option<Vec<u8>>,
    after: Option<Vec<u8>>,
    partial: bool,
    note: Option<&str>,
) -> Result<FileDiffDto, String> {
    let before = before.unwrap_or_default();
    let after = after.unwrap_or_default();
    let binary = bytes_are_binary(&before) || bytes_are_binary(&after);
    if binary {
        return Ok(FileDiffDto {
            path: requested.path.clone(),
            old_path: requested.old_path.clone(),
            status: requested.status,
            binary: true,
            partial,
            before: String::new(),
            after: String::new(),
            hunks: Vec::new(),
            note: Some("Retained file content is binary and cannot be opened as text.".to_string()),
        });
    }
    let before = String::from_utf8(before)
        .map_err(|error| format!("before content is not UTF-8: {error}"))?;
    let after =
        String::from_utf8(after).map_err(|error| format!("after content is not UTF-8: {error}"))?;
    Ok(FileDiffDto {
        path: requested.path.clone(),
        old_path: requested.old_path.clone(),
        status: requested.status,
        binary: false,
        partial,
        before,
        after,
        hunks: Vec::new(),
        note: note.map(str::to_string),
    })
}

#[must_use]
fn bytes_are_binary(bytes: &[u8]) -> bool {
    bytes.contains(&0) || std::str::from_utf8(bytes).is_err()
}

fn recorded_unified_diff(requested: &FileChangeDto, diff: &str, note: &str) -> FileDiffDto {
    let hunks = unified_diff_hunks(diff);
    let (before, after) = match hunks.as_slice() {
        [hunk] => (hunk.before.clone(), hunk.after.clone()),
        [] => (String::new(), diff.to_string()),
        _ => (String::new(), String::new()),
    };
    FileDiffDto {
        path: requested.path.clone(),
        old_path: requested.old_path.clone(),
        status: requested.status,
        binary: false,
        partial: true,
        before,
        after,
        hunks,
        note: Some(note.to_string()),
    }
}

pub(super) fn unified_diff_hunks(diff: &str) -> Vec<FileDiffHunkDto> {
    let mut hunks = Vec::new();
    let mut header = None;
    let mut before = Vec::new();
    let mut after = Vec::new();
    for line in diff.lines() {
        if line.starts_with("@@") {
            if let Some(previous_header) = header.replace(line.to_string()) {
                hunks.push(FileDiffHunkDto {
                    header: previous_header,
                    before: before.join("\n"),
                    after: after.join("\n"),
                });
                before.clear();
                after.clear();
            }
            continue;
        }
        if header.is_none() {
            continue;
        }
        if let Some(context) = line.strip_prefix(' ') {
            before.push(context);
            after.push(context);
        } else if let Some(removed) = line.strip_prefix('-') {
            before.push(removed);
        } else if let Some(added) = line.strip_prefix('+') {
            after.push(added);
        }
    }
    if let Some(header) = header {
        hunks.push(FileDiffHunkDto {
            header,
            before: before.join("\n"),
            after: after.join("\n"),
        });
    }
    hunks
}
