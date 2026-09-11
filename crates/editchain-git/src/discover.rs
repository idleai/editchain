//! Explicit repository paths and workspace-scoped discovery.

use std::path::{Path, PathBuf};

use editchain_core::RepositoryId;

use crate::{repository_id_from_path, RepositoryCatalog};

/// Description of one repository or worktree source.
#[derive(Debug, Clone)]
pub struct RepositoryDiscovery {
    /// Legacy identity derived from the canonical `.git` marker (or bare root).
    pub id: RepositoryId,
    /// Worktree `.git` marker, or the Git directory for a bare repository.
    pub marker_path: PathBuf,
    /// Checked-out worktree root, absent for bare repositories.
    pub worktree_root: Option<PathBuf>,
    /// Resolved Git directory; linked worktrees have their own directory.
    pub git_dir: PathBuf,
    /// Shared object/ref directory, distinct from a linked worktree's Git directory.
    pub common_dir: PathBuf,
}

impl RepositoryDiscovery {
    /// Describe an existing worktree root, `.git` marker, or bare repository.
    ///
    /// # Errors
    ///
    /// Returns an error if Git metadata or its filesystem paths cannot be read.
    pub fn from_path(path: &Path) -> Result<Self, Box<dyn std::error::Error>> {
        let absolute_path = std::path::absolute(path)?;
        let path = absolute_path.as_path();
        let open_path = if path.file_name().is_some_and(|name| name == ".git") && path.is_file() {
            path.parent().ok_or("Git marker has no worktree root")?
        } else {
            path
        };
        let repo = gix::open(open_path)?;
        let git_dir = repo.path().canonicalize()?;
        let common_dir = repo.common_dir().canonicalize()?;
        let worktree_root = repo.workdir().map(Path::canonicalize).transpose()?;
        let marker_path = if path.file_name().is_some_and(|name| name == ".git") {
            path.parent()
                .ok_or("Git marker has no parent")?
                .canonicalize()?
                .join(".git")
        } else if path.join(".git").exists() {
            path.canonicalize()?.join(".git")
        } else {
            git_dir.clone()
        };
        Ok(Self {
            id: repository_id_from_path(&marker_path),
            marker_path,
            worktree_root,
            git_dir,
            common_dir,
        })
    }

    /// Whether this source is a linked worktree sharing another Git directory.
    #[must_use]
    pub fn is_linked_worktree(&self) -> bool {
        self.worktree_root.is_some() && self.git_dir != self.common_dir
    }

    /// Resolve a recorded file path within this worktree, using a recorded cwd
    /// for relative paths. Lexical `..` components cannot escape the worktree.
    #[must_use]
    pub fn relative_worktree_path(&self, path: &Path, cwd: Option<&Path>) -> Option<PathBuf> {
        let absolute = if path.is_absolute() {
            path.to_path_buf()
        } else {
            cwd?.join(path)
        };
        let absolute = crate::catalog::normalize_absolute(&absolute)?;
        absolute
            .strip_prefix(self.worktree_root.as_ref()?)
            .ok()
            .filter(|relative| !relative.as_os_str().is_empty())
            .map(Path::to_path_buf)
    }
}

/// A handle to an opened repository. `gix::Repository` is intentionally not `Sync`.
pub struct RepositoryHandle {
    /// Underlying object database and ref access.
    pub repo: gix::Repository,
    /// Explicit repository/worktree paths and legacy identity.
    pub discovery: RepositoryDiscovery,
}

impl std::fmt::Debug for RepositoryHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RepositoryHandle")
            .field("discovery", &self.discovery)
            .finish_non_exhaustive()
    }
}

/// Open a described repository for read-only object resolution.
///
/// # Errors
///
/// Returns an error if the repository can no longer be opened.
pub fn open_repository(
    discovery: &RepositoryDiscovery,
) -> Result<RepositoryHandle, Box<dyn std::error::Error>> {
    Ok(RepositoryHandle {
        repo: gix::open(&discovery.git_dir)?,
        discovery: discovery.clone(),
    })
}

/// Discover repositories only when the entire scoped catalog could be read.
/// Use [`RepositoryCatalog`] when partial results and diagnostics are useful.
///
/// # Errors
///
/// Returns an error if any directory or Git marker in the traversal scope fails.
pub fn discover_repositories(
    workspace: &Path,
) -> Result<Vec<RepositoryDiscovery>, Box<dyn std::error::Error>> {
    let catalog = RepositoryCatalog::discover(workspace)?;
    if let Some(issue) = catalog.issues().first() {
        return Err(format!("{}: {}", issue.path.display(), issue.message).into());
    }
    Ok(catalog.entries().to_vec())
}
