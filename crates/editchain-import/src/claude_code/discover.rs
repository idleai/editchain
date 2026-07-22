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
}

/// Discover all Claude Code session files in a directory.
///
/// Scans for `.jsonl` files (excluding `agent-.jsonl` subagent files
/// which are discovered separately), and also discovers subagent files
/// within `subagents/` subdirectories.
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
        });

        // Discover subagents for this session.
        let subagents = discover_subagents(&path, &session_id);
        sessions.extend(subagents);
    }

    // Sort by path for deterministic ordering.
    sessions.sort_by(|a, b| a.path.cmp(&b.path));

    Ok(sessions)
}

/// Discover subagent files for a given session.
///
/// Claude Code stores subagent transcripts in `subagents/agent-*.jsonl`
/// relative to the main session file.
fn discover_subagents(session_path: &Path, parent_session_id: &str) -> Vec<SessionFile> {
    let mut subagents = Vec::new();

    // Subagents are stored in a `subagents/` directory next to the session file.
    let parent_dir = session_path.parent().unwrap_or(Path::new("."));
    let subagents_dir = parent_dir.join("subagents");

    if !subagents_dir.exists() {
        return subagents;
    }

    let Ok(entries) = std::fs::read_dir(&subagents_dir) else {
        return subagents;
    };

    for entry in entries {
        let Ok(entry) = entry else {
            continue;
        };
        let name = entry.file_name().to_string_lossy().to_string();

        if !name.to_lowercase().ends_with(".jsonl") {
            continue;
        }

        let path = entry.path();
        let Ok(metadata) = std::fs::metadata(&path) else {
            continue;
        };

        // agent-<uuid>.jsonl — strip prefix for session ID.
        let agent_id = name
            .strip_prefix("agent-")
            .and_then(|s| s.strip_suffix(".jsonl"))
            .unwrap_or(&name)
            .to_string();

        subagents.push(SessionFile {
            path,
            session_id: agent_id,
            file_size: metadata.len(),
            is_subagent: true,
            parent_session_id: Some(parent_session_id.to_string()),
        });
    }

    subagents
}
