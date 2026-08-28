use std::path::{Path, PathBuf};

/// Information about a discovered Codex rollout file.
#[derive(Debug, Clone)]
pub struct RolloutFile {
    /// Absolute path to the rollout JSONL file.
    pub path: PathBuf,
    /// File size in bytes.
    pub file_size: u64,
    /// Deterministic session-scope fallback: the rollout filename stem.
    pub session_id: String,
}

/// Recursively discover all `rollout-*.jsonl` files under a raw Codex sessions
/// root. Codex stores sessions in date trees
/// (e.g. `~/.codex/sessions/2026-08-26/rollout-2026-08-26T12-00-00-000.jsonl`),
/// so the walk descends into subdirectories at any depth and collects every
/// file whose name starts with `rollout-` and ends with `.jsonl`. Results are
/// sorted by path for deterministic ordering; directory enumeration order is
/// never relied upon. Symbolic links are not followed, preventing cycles and
/// keeping source identity tied to physical files beneath `raw_root`.
///
/// # Errors
///
/// Returns a descriptive error string if the root directory cannot be read.
pub fn discover_rollouts(raw_root: &Path) -> Result<Vec<RolloutFile>, String> {
    let mut rollouts = Vec::new();
    let mut pending = vec![raw_root.to_path_buf()];

    while let Some(dir) = pending.pop() {
        let entries =
            std::fs::read_dir(&dir).map_err(|e| format!("reading {}: {}", dir.display(), e))?;
        for entry in entries {
            let entry = entry.map_err(|e| format!("entry: {e}"))?;
            let path = entry.path();
            let metadata = std::fs::symlink_metadata(&path)
                .map_err(|e| format!("metadata {}: {e}", path.display()))?;
            if metadata.is_dir() {
                pending.push(path);
            } else if metadata.is_file() && is_rollout_name(&path) {
                let file_name = path
                    .file_name()
                    .map_or_else(String::new, |n| n.to_string_lossy().to_string());
                let session_id = file_name
                    .strip_suffix(".jsonl")
                    .unwrap_or(&file_name)
                    .to_string();
                rollouts.push(RolloutFile {
                    path,
                    file_size: metadata.len(),
                    session_id,
                });
            }
        }
    }

    rollouts.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(rollouts)
}

/// Whether a path's file name matches the `rollout-*.jsonl` pattern.
#[must_use]
fn is_rollout_name(path: &Path) -> bool {
    path.file_name().is_some_and(|name| {
        let name = name.to_string_lossy();
        name.starts_with("rollout-") && name.ends_with(".jsonl")
    })
}
