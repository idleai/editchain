//! Repository discovery and identity derivation.

use std::path::{Path, PathBuf};

use editchain_core::RepositoryId;

use crate::repository_id_from_path;

/// A discovered repository in a workspace.
#[derive(Debug, Clone)]
pub struct RepositoryDiscovery {
    /// Deterministic repository identity.
    pub id: RepositoryId,
    /// Path to the repository root (worktree root or bare repo dir).
    pub path: PathBuf,
    /// Whether this is a linked worktree (vs the main worktree).
    pub is_worktree: bool,
}

/// A handle to an opened repository.
///
/// Wraps a `gix::Repository` for object resolution. The `gix::Repository` is
/// not `Sync`, so this handle is intentionally single-threaded.
pub struct RepositoryHandle {
    /// The underlying `gix` repository.
    pub repo: gix::Repository,
    /// The discovery metadata.
    pub discovery: RepositoryDiscovery,
}

#[expect(
    clippy::missing_fields_in_debug,
    reason = "gix::Repository does not implement Debug; discovery metadata is the meaningful state"
)]
impl std::fmt::Debug for RepositoryHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RepositoryHandle")
            .field("discovery", &self.discovery)
            .finish()
    }
}

/// Discover repositories in a workspace directory.
///
/// Walks the workspace for `.git` directories and linked worktrees. Returns
/// an error if the workspace path cannot be read.
///
/// # Errors
///
/// Returns an error if the workspace directory cannot be read.
pub fn discover_repositories(
    workspace: &Path,
) -> Result<Vec<RepositoryDiscovery>, Box<dyn std::error::Error>> {
    let mut out = Vec::new();
    walk_for_repos(workspace, &mut out)?;
    Ok(out)
}

/// Recursively walk a directory looking for `.git` entries.
fn walk_for_repos(
    dir: &Path,
    out: &mut Vec<RepositoryDiscovery>,
) -> Result<(), Box<dyn std::error::Error>> {
    let entries = std::fs::read_dir(dir)?;
    for entry in entries {
        let entry = entry?;
        let path = entry.path();
        let name = entry.file_name();
        let name_str = name.to_string_lossy();

        if path.is_dir() {
            if name_str == ".git" {
                // A repository root (or a linked-worktree gitdir file).
                let id = repository_id_from_path(&path);
                out.push(RepositoryDiscovery {
                    id,
                    path: path.clone(),
                    is_worktree: false,
                });
            } else if !name_str.starts_with('.')
                && name_str != "target"
                && name_str != "node_modules"
            {
                // Recurse into subdirectories, skipping common noise.
                walk_for_repos(&path, out)?;
            }
        } else if name_str == ".git" {
            // A `.git` file (linked worktree pointer).
            let id = repository_id_from_path(&path);
            out.push(RepositoryDiscovery {
                id,
                path: path.clone(),
                is_worktree: true,
            });
        }
    }
    Ok(())
}
