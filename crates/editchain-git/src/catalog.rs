//! Deterministic workspace repository catalog and path queries.

use std::collections::BTreeSet;
use std::io;
use std::path::{Path, PathBuf};

use editchain_core::RepositoryId;

use crate::RepositoryDiscovery;

/// A discovery failure retained alongside successfully described repositories.
#[derive(Debug, Clone)]
pub struct DiscoveryIssue {
    /// Directory or Git marker that could not be inspected.
    pub path: PathBuf,
    /// Filesystem or Git metadata failure.
    pub message: String,
}

/// One deterministic repository catalog for a workspace.
///
/// Directory traversal skips hidden directories, build outputs, and symlinks.
/// Failures remain observable; a partial catalog cannot prove global uniqueness.
#[derive(Debug, Clone, Default)]
pub struct RepositoryCatalog {
    entries: Vec<RepositoryDiscovery>,
    issues: Vec<DiscoveryIssue>,
}

impl<'a> IntoIterator for &'a RepositoryCatalog {
    type Item = &'a RepositoryDiscovery;
    type IntoIter = std::slice::Iter<'a, RepositoryDiscovery>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

impl RepositoryCatalog {
    /// Discover repositories within the supplied workspace tree.
    ///
    /// # Errors
    ///
    /// Returns an error if the workspace itself cannot be resolved or read.
    pub fn discover(workspace: &Path) -> io::Result<Self> {
        let workspace = workspace.canonicalize()?;
        drop(std::fs::read_dir(&workspace)?);
        let mut catalog = Self::default();
        let mut pending = vec![workspace];
        let mut seen = BTreeSet::new();
        while let Some(dir) = pending.pop() {
            catalog.visit(&dir, &mut pending, &mut seen);
        }
        catalog
            .entries
            .sort_by(|left, right| left.marker_path.cmp(&right.marker_path));
        catalog
            .issues
            .sort_by(|left, right| left.path.cmp(&right.path));
        Ok(catalog)
    }

    /// Build a catalog from already resolved repository descriptions.
    #[must_use]
    pub fn from_entries(mut entries: Vec<RepositoryDiscovery>) -> Self {
        entries.sort_by(|left, right| left.marker_path.cmp(&right.marker_path));
        entries.dedup_by(|left, right| left.marker_path == right.marker_path);
        Self {
            entries,
            issues: Vec::new(),
        }
    }

    /// Descriptions in deterministic marker-path order.
    #[must_use]
    pub fn entries(&self) -> &[RepositoryDiscovery] {
        &self.entries
    }

    /// Iterate over repository descriptions.
    pub fn iter(&self) -> std::slice::Iter<'_, RepositoryDiscovery> {
        self.entries.iter()
    }

    /// Whether the catalog contains no successfully described repositories.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Number of successfully described repository sources.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Errors that prevented a complete discovery within the traversal scope.
    #[must_use]
    pub fn issues(&self) -> &[DiscoveryIssue] {
        &self.issues
    }

    /// Whether every directory and repository within the traversal scope was read.
    #[must_use]
    pub fn is_complete(&self) -> bool {
        self.issues.is_empty()
    }

    /// Repository with this durable identity, if it was discovered.
    #[must_use]
    pub fn by_id(&self, id: RepositoryId) -> Option<&RepositoryDiscovery> {
        self.entries.iter().find(|entry| entry.id == id)
    }

    /// Nearest containing worktree for an existing absolute cwd.
    #[must_use]
    pub fn repository_for_path(&self, path: &Path) -> Option<&RepositoryDiscovery> {
        if !path.is_absolute() {
            return None;
        }
        let path = path.canonicalize().ok()?;
        self.entries
            .iter()
            .filter_map(|entry| {
                let root = entry.worktree_root.as_ref()?;
                path.starts_with(root)
                    .then_some((root.components().count(), entry))
            })
            .max_by_key(|(depth, _)| *depth)
            .map(|(_, entry)| entry)
    }

    /// Whether this worktree is strictly within another discovered worktree.
    #[must_use]
    pub fn is_nested(&self, id: RepositoryId) -> bool {
        let Some(root) = self
            .by_id(id)
            .and_then(|entry| entry.worktree_root.as_ref())
        else {
            return false;
        };
        self.entries.iter().any(|other| {
            other.id != id
                && other
                    .worktree_root
                    .as_ref()
                    .is_some_and(|other_root| root != other_root && root.starts_with(other_root))
        })
    }

    fn visit(&mut self, dir: &Path, pending: &mut Vec<PathBuf>, seen: &mut BTreeSet<PathBuf>) {
        if !seen.insert(dir.to_path_buf()) {
            return;
        }
        let marker = dir.join(".git");
        match std::fs::symlink_metadata(&marker) {
            Ok(_) => self.add_repository(&marker),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                if dir.join("HEAD").is_file()
                    && dir.join("objects").is_dir()
                    && dir.join("refs").is_dir()
                {
                    self.add_repository(dir);
                    return;
                }
            }
            Err(error) => self.issue(&marker, error),
        }
        let entries = match std::fs::read_dir(dir) {
            Ok(entries) => entries,
            Err(error) => {
                self.issue(dir, error);
                return;
            }
        };
        for entry in entries {
            match entry {
                Ok(entry) => self.queue_directory(&entry, pending),
                Err(error) => self.issue(dir, error),
            }
        }
    }

    fn queue_directory(&mut self, entry: &std::fs::DirEntry, pending: &mut Vec<PathBuf>) {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.starts_with('.') || name == "target" || name == "node_modules" {
            return;
        }
        match entry.file_type() {
            Ok(kind) if kind.is_dir() => pending.push(entry.path()),
            Ok(_) => {}
            Err(error) => self.issue(&entry.path(), error),
        }
    }

    fn add_repository(&mut self, marker: &Path) {
        match RepositoryDiscovery::from_path(marker) {
            Ok(entry) => self.entries.push(entry),
            Err(error) => self.issue(marker, error),
        }
    }

    fn issue(&mut self, path: &Path, error: impl std::fmt::Display) {
        self.issues.push(DiscoveryIssue {
            path: path.to_path_buf(),
            message: error.to_string(),
        });
    }
}

pub(crate) fn normalize_absolute(path: &Path) -> Option<PathBuf> {
    if !path.is_absolute() {
        return None;
    }
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::ParentDir => {
                if !normalized.pop() {
                    return None;
                }
            }
            std::path::Component::CurDir => {}
            std::path::Component::Prefix(_)
            | std::path::Component::RootDir
            | std::path::Component::Normal(_) => normalized.push(component.as_os_str()),
        }
    }
    Some(normalized)
}
