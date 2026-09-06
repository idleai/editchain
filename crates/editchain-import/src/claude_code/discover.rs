use std::path::{Path, PathBuf};

/// Information about a discovered session file.
#[derive(Debug, Clone)]
pub struct SessionFile {
    /// Absolute path to the JSONL file.
    pub path: PathBuf,
    /// Session UUID (filename without .jsonl).
    pub session_id: String,
    /// File size in bytes.
    pub file_size: u64,
    /// Whether this is a background subagent session.
    pub is_subagent: bool,
    /// Parent session UUID (for subagents).
    pub parent_session_id: Option<String>,
    /// The parent's `Agent` `tool_use` id that spawned this subagent (from the
    /// sibling `<name>.meta.json`'s `toolUseId`). This is the branch anchor used
    /// to link the subagent back into its parent's chain.
    pub tool_use_id: Option<String>,
}

/// Discover all Claude Code session files in a directory.
///
/// Scans for top-level `.jsonl` sessions (excluding misplaced `agent-.jsonl`
/// files), then recursively discovers every nested `.jsonl` source beneath the
/// matching `<session-id>/` directory. Recursion is deliberate: workflows and
/// future Claude Code subkeys can nest agent logs more than one directory deep.
///
/// # Errors
///
/// Returns a descriptive error string if the directory cannot be read.
pub fn discover_sessions(sessions_dir: &Path) -> Result<Vec<SessionFile>, String> {
    let mut sessions = Vec::new();

    // Discover main session files.
    let entries = std::fs::read_dir(sessions_dir)
        .map_err(|e| format!("reading {}: {}", sessions_dir.display(), e))?;

    for entry in entries {
        let entry = entry.map_err(|e| format!("entry: {e}"))?;
        let name = entry.file_name().to_string_lossy().to_string();

        if !name.to_lowercase().ends_with(".jsonl") || name.starts_with("agent-") {
            continue;
        }

        let path = entry.path();
        let metadata = std::fs::metadata(&path).map_err(|e| format!("metadata: {e}"))?;
        let session_id = name.trim_end_matches(".jsonl").to_string();

        sessions.push(SessionFile {
            path: path.clone(),
            session_id: session_id.clone(),
            file_size: metadata.len(),
            is_subagent: false,
            parent_session_id: None,
            tool_use_id: None,
        });

        // Discover nested execution/opaque sources for this session.
        sessions.extend(discover_nested_sources(&path, &session_id)?);
    }

    // Sort by path for deterministic ordering.
    sessions.sort_by(|a, b| a.path.cmp(&b.path));

    Ok(sessions)
}

/// Discover all nested JSONL sources owned by one top-level session.
fn discover_nested_sources(
    session_path: &Path,
    parent_session_id: &str,
) -> Result<Vec<SessionFile>, String> {
    let parent_dir = session_path.parent().unwrap_or(Path::new("."));
    let owner_dir = parent_dir.join(parent_session_id);

    if !owner_dir.exists() {
        return Ok(Vec::new());
    }

    let mut sources = Vec::new();
    let mut pending = vec![owner_dir.clone()];
    while let Some(dir) = pending.pop() {
        let entries = std::fs::read_dir(&dir)
            .map_err(|e| format!("reading nested source directory {}: {e}", dir.display()))?;
        for entry in entries {
            let entry = entry.map_err(|e| format!("reading entry in {}: {e}", dir.display()))?;
            let file_type = entry
                .file_type()
                .map_err(|e| format!("file type for {}: {e}", entry.path().display()))?;
            if file_type.is_dir() {
                pending.push(entry.path());
                continue;
            }
            // Do not follow symlinked directories/files: discovery identity is
            // the physical source path, and following links could duplicate or
            // cycle the source set.
            if !file_type.is_file() {
                continue;
            }

            let name = entry.file_name().to_string_lossy().to_string();
            if !name.to_lowercase().ends_with(".jsonl") {
                continue;
            }

            let path = entry.path();
            let metadata = std::fs::metadata(&path)
                .map_err(|e| format!("metadata for {}: {e}", path.display()))?;
            let is_subagent = name.starts_with("agent-");
            let source_id = if is_subagent {
                name.strip_prefix("agent-")
                    .and_then(|s| s.strip_suffix(".jsonl"))
                    .unwrap_or(&name)
                    .to_string()
            } else {
                path.strip_prefix(&owner_dir)
                    .unwrap_or(&path)
                    .with_extension("")
                    .to_string_lossy()
                    .to_string()
            };

            // Read exact spawn identity, when present, before import. Opaque
            // nested sources remain discoverable even without a sidecar.
            let tool_use_id = is_subagent.then(|| read_tool_use_id(&path)).flatten();
            sources.push(SessionFile {
                path,
                session_id: source_id,
                file_size: metadata.len(),
                is_subagent,
                parent_session_id: Some(parent_session_id.to_string()),
                tool_use_id,
            });
        }
    }

    Ok(sources)
}

/// Read the parent's `Agent` `tool_use` id from a subagent's sibling meta file.
///
/// Claude Code writes `<name>.meta.json` beside each `agent-*.jsonl` transcript
/// carrying `{"toolUseId": "...", "parentAgentId": "...", ...}`. The `toolUseId`
/// is the id of the parent session's `Agent` `tool_use` that spawned this
/// subagent — the branch anchor for linking. Returns `None` if the meta file is
/// missing or unparseable (the subagent then stays an unlinked branch).
///
/// The `Agent` `tool_use` is the parent's tool call that launched the subagent.
fn read_tool_use_id(subagent_jsonl_path: &Path) -> Option<String> {
    let meta_path = subagent_jsonl_path.with_extension("meta.json");
    let bytes = std::fs::read(&meta_path).ok()?;
    let value: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    value
        .get("toolUseId")
        .and_then(serde_json::Value::as_str)
        .map(ToString::to_string)
}
