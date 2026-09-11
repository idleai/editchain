//! Immutable, first-parent file changes for live Git commits.

use editchain_core::GitOid;

use crate::discover::RepositoryHandle;
use crate::resolve::{git_oid_from, git_oid_from_gix, ResolutionError};

/// Source-control status of one path in a commit tree diff.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GitFileStatus {
    /// Path exists only in the commit tree.
    Added,
    /// Path exists only in the first-parent tree.
    Deleted,
    /// Path contents or mode changed.
    Modified,
    /// A deletion/addition pair was detected as a rename.
    Renamed,
    /// An added path was detected as a copy of an existing path.
    Copied,
    /// The Git entry kind changed (for example file to symlink).
    TypeChanged,
}

/// One repository-relative path change in a commit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitFileChange {
    /// Destination/current path (or the deleted path for deletions).
    pub path: String,
    /// Source path for a rename or copy.
    pub old_path: Option<String>,
    /// Source-control status.
    pub status: GitFileStatus,
    /// Object on the first-parent side, if one exists.
    pub old_oid: Option<GitOid>,
    /// Object on the commit side, if one exists.
    pub new_oid: Option<GitOid>,
    /// Human-readable first-parent entry mode (`blob`, `exe`, `link`, or
    /// `commit`).
    pub old_mode: Option<String>,
    /// Human-readable commit-side entry mode.
    pub new_mode: Option<String>,
    /// Whether either blob cannot be represented faithfully as UTF-8 text.
    pub binary: bool,
}

/// One immutable Git blob.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitBlob {
    /// Exact object bytes.
    pub bytes: Vec<u8>,
    /// Whether the bytes should not be sent through a text document provider.
    pub binary: bool,
}

/// A non-tree entry resolved from a commit tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitPathObject {
    /// Exact object identity.
    pub oid: GitOid,
    /// Entry mode (`blob`, `exe`, `link`, or `commit`).
    pub mode: String,
    /// Blob bytes, or `None` for a gitlink entry.
    pub blob: Option<GitBlob>,
}

/// Diff a commit tree against its first parent, or the empty tree for a root
/// commit.
///
/// Rename detection is explicit and deterministic (Git's default 50%
/// similarity with the library's bounded candidate limit). Merge commits use
/// first-parent semantics, matching the ordinary `git show` file list.
///
/// # Errors
///
/// Returns an error when the commit, a parent tree, or the tree diff cannot be
/// decoded from the local object database.
pub fn commit_file_changes(
    handle: &RepositoryHandle,
    oid: &GitOid,
) -> Result<Vec<GitFileChange>, ResolutionError> {
    let commit_id = git_oid_from(oid)?;
    let commit = handle
        .repo
        .find_object(commit_id)
        .map_err(|error| ResolutionError::NotFound(error.to_string()))?
        .into_commit();
    let new_tree = commit
        .tree()
        .map_err(|error| ResolutionError::Decode(error.to_string()))?;
    let old_tree = commit
        .parent_ids()
        .next()
        .map(|parent| {
            parent
                .object()
                .map_err(|error| ResolutionError::NotFound(error.to_string()))?
                .into_commit()
                .tree()
                .map_err(|error| ResolutionError::Decode(error.to_string()))
        })
        .transpose()?;

    let options = gix::diff::Options::default().with_rewrites(Some(gix::diff::Rewrites::default()));
    let changes = handle
        .repo
        .diff_tree_to_tree(old_tree.as_ref(), Some(&new_tree), Some(options))
        .map_err(|error| ResolutionError::Decode(error.to_string()))?;

    let mut files = changes
        .into_iter()
        .filter_map(|change| change_from_gix(handle, change).transpose())
        .collect::<Result<Vec<_>, _>>()?;
    files.sort_by(|left, right| {
        left.path
            .cmp(&right.path)
            .then_with(|| left.old_path.cmp(&right.old_path))
    });
    Ok(files)
}

/// Resolve one blob object without consulting the index or working tree.
///
/// # Errors
///
/// Returns an error when the object is absent or is not a blob.
pub fn resolve_blob(handle: &RepositoryHandle, oid: &GitOid) -> Result<GitBlob, ResolutionError> {
    let object_id = git_oid_from(oid)?;
    let object = handle
        .repo
        .find_object(object_id)
        .map_err(|error| ResolutionError::NotFound(error.to_string()))?;
    if object.kind != gix_object::Kind::Blob {
        return Err(ResolutionError::Decode(format!(
            "expected blob object, found {}",
            object.kind
        )));
    }
    let mut blob = object.into_blob();
    let bytes = blob.take_data();
    Ok(GitBlob {
        binary: is_binary(&bytes),
        bytes,
    })
}

/// Resolve a repository-relative path from an immutable commit tree.
///
/// # Errors
///
/// Returns an error when the commit/tree/object cannot be decoded. A missing
/// path is `Ok(None)`; a submodule gitlink is returned with no blob bytes.
pub fn resolve_path_at_commit(
    handle: &RepositoryHandle,
    commit_oid: &GitOid,
    path: &str,
) -> Result<Option<GitPathObject>, ResolutionError> {
    let commit = handle
        .repo
        .find_object(git_oid_from(commit_oid)?)
        .map_err(|error| ResolutionError::NotFound(error.to_string()))?
        .into_commit();
    let tree = commit
        .tree()
        .map_err(|error| ResolutionError::Decode(error.to_string()))?;
    let Some(entry) = tree
        .lookup_entry_by_path(path)
        .map_err(|error| ResolutionError::Decode(error.to_string()))?
    else {
        return Ok(None);
    };
    let mode = entry.mode();
    if mode.is_tree() {
        return Ok(None);
    }
    let oid = git_oid_from_gix(&entry.object_id())?;
    let blob = if mode.is_commit() {
        None
    } else {
        Some(resolve_blob(handle, &oid)?)
    };
    Ok(Some(GitPathObject {
        oid,
        mode: mode.as_str().to_owned(),
        blob,
    }))
}

fn change_from_gix(
    handle: &RepositoryHandle,
    change: gix::diff::tree_with_rewrites::Change,
) -> Result<Option<GitFileChange>, ResolutionError> {
    use gix::diff::tree_with_rewrites::Change;

    let file = match change {
        Change::Addition {
            location,
            entry_mode,
            id,
            ..
        } => {
            if entry_mode.is_tree() {
                return Ok(None);
            }
            GitFileChange {
                path: path_text(location.as_ref()),
                old_path: None,
                status: GitFileStatus::Added,
                old_oid: None,
                new_oid: Some(git_oid_from_gix(&id)?),
                old_mode: None,
                new_mode: Some(entry_mode.as_str().to_owned()),
                binary: object_is_binary(handle, id, entry_mode),
            }
        }
        Change::Deletion {
            location,
            entry_mode,
            id,
            ..
        } => {
            if entry_mode.is_tree() {
                return Ok(None);
            }
            GitFileChange {
                path: path_text(location.as_ref()),
                old_path: None,
                status: GitFileStatus::Deleted,
                old_oid: Some(git_oid_from_gix(&id)?),
                new_oid: None,
                old_mode: Some(entry_mode.as_str().to_owned()),
                new_mode: None,
                binary: object_is_binary(handle, id, entry_mode),
            }
        }
        Change::Modification {
            location,
            previous_entry_mode,
            previous_id,
            entry_mode,
            id,
        } => {
            if previous_entry_mode.is_tree() && entry_mode.is_tree() {
                return Ok(None);
            }
            let status = if previous_entry_mode.kind() == entry_mode.kind() {
                GitFileStatus::Modified
            } else {
                GitFileStatus::TypeChanged
            };
            GitFileChange {
                path: path_text(location.as_ref()),
                old_path: None,
                status,
                old_oid: Some(git_oid_from_gix(&previous_id)?),
                new_oid: Some(git_oid_from_gix(&id)?),
                old_mode: Some(previous_entry_mode.as_str().to_owned()),
                new_mode: Some(entry_mode.as_str().to_owned()),
                binary: object_is_binary(handle, previous_id, previous_entry_mode)
                    || object_is_binary(handle, id, entry_mode),
            }
        }
        Change::Rewrite {
            source_location,
            source_entry_mode,
            source_id,
            entry_mode,
            id,
            location,
            copy,
            ..
        } => GitFileChange {
            path: path_text(location.as_ref()),
            old_path: Some(path_text(source_location.as_ref())),
            status: if copy {
                GitFileStatus::Copied
            } else {
                GitFileStatus::Renamed
            },
            old_oid: Some(git_oid_from_gix(&source_id)?),
            new_oid: Some(git_oid_from_gix(&id)?),
            old_mode: Some(source_entry_mode.as_str().to_owned()),
            new_mode: Some(entry_mode.as_str().to_owned()),
            binary: object_is_binary(handle, source_id, source_entry_mode)
                || object_is_binary(handle, id, entry_mode),
        },
    };
    Ok(Some(file))
}

fn path_text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

fn object_is_binary(
    handle: &RepositoryHandle,
    id: gix::hash::ObjectId,
    mode: gix_object::tree::EntryMode,
) -> bool {
    if mode.is_commit() {
        return false;
    }
    if !mode.is_blob_or_symlink() {
        return true;
    }
    handle
        .repo
        .find_object(id)
        .map_or(true, |object| is_binary(&object.data))
}

fn is_binary(bytes: &[u8]) -> bool {
    bytes.iter().take(8_000).any(|byte| *byte == 0) || std::str::from_utf8(bytes).is_err()
}
