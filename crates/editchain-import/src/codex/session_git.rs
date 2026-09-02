//! Exact Codex session-start Git anchoring.

use std::path::{Path, PathBuf};

use editchain_core::{
    ActorId, Clock, GitLink, GitLinkKind, GitOid, Op, OpKind, ParentSet, RepositoryId, ScopeRef,
    SessionId, Tags,
};
use sha2::{Digest, Sha256};

use super::projection::SessionMeta;
use crate::error::ImportError;
use crate::ids::{SourcePosition, SourceStream};

/// Reserved derived lane for the one session-start Git link.
///
/// Structural relationship notes reserve `0xFFFA..=0xFFFC`; keeping this lane
/// immediately below them avoids collisions with ordinary normalized lanes.
const SESSION_GIT_LINK_LANE: u16 = 0xFFF9;

/// Build the exact `session_meta.git.commit_hash` relation for one Codex file.
///
/// No fallback is inferred. The link exists only when Codex supplied a valid
/// full SHA-1/SHA-256 hash and the recorded session cwd resolves to an actual
/// repository inside the imported workspace.
pub(super) fn session_git_link_op(
    workspace: &Path,
    meta: &SessionMeta,
    source_ordinal: u64,
    stream: &SourceStream,
    session_id: SessionId,
) -> Result<Option<Op>, ImportError> {
    let Some(commit_hash) = meta.git.as_ref().and_then(|git| git.commit_hash.as_deref()) else {
        return Ok(None);
    };
    let Some(target_oid) = GitOid::from_hex(commit_hash) else {
        return Ok(None);
    };
    let Some(cwd) = meta.cwd.as_deref() else {
        return Ok(None);
    };
    let Some(repository_marker) = repository_marker(workspace, cwd) else {
        return Ok(None);
    };

    let source = stream.op_from_position(SourcePosition::raw(source_ordinal))?;
    let id = stream.op_from_position(SourcePosition::derived(
        source_ordinal,
        SESSION_GIT_LINK_LANE,
    ))?;
    Ok(Some(Op {
        id,
        parents: ParentSet::One(source),
        actor: ActorId(0),
        clock: Clock::None,
        scope: ScopeRef::Session(session_id),
        tags: Tags::META | Tags::IMPORT,
        kind: OpKind::GitLink(GitLink {
            source,
            target_repo: repository_id_from_path(&repository_marker),
            target_oid,
            kind: GitLinkKind::BasedOn,
        }),
    }))
}

/// Resolve the nearest repository marker at or above the recorded cwd, never
/// escaping the imported workspace. This is path identity, not guesswork: if
/// the marker is absent, the session gets no Git relation.
fn repository_marker(workspace: &Path, cwd: &str) -> Option<PathBuf> {
    let cwd = Path::new(cwd);
    if cwd.is_relative() {
        return None;
    }
    let workspace = canonical_or_literal(&absolute_workspace(workspace));
    let mut current = canonical_or_literal(cwd);
    if !current.starts_with(&workspace) {
        return None;
    }

    loop {
        let marker = current.join(".git");
        if marker.exists() {
            return Some(marker);
        }
        if current == workspace || !current.pop() {
            return None;
        }
    }
}

fn absolute_workspace(workspace: &Path) -> PathBuf {
    if workspace.is_absolute() {
        workspace.to_path_buf()
    } else {
        std::env::current_dir().map_or_else(|_| workspace.to_path_buf(), |cwd| cwd.join(workspace))
    }
}

fn canonical_or_literal(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

/// Mirror `editchain_git::repository_id_from_path` without coupling the
/// source-format importer to the live Git/object-resolution crate.
#[expect(
    clippy::indexing_slicing,
    reason = "SHA-256 output is always at least 8 bytes"
)]
fn repository_id_from_path(path: &Path) -> RepositoryId {
    let canonical = canonical_or_literal(path);
    let digest = Sha256::digest(canonical.to_string_lossy().as_bytes());
    let mut bytes = [0u8; 8];
    bytes.copy_from_slice(&digest[..8]);
    RepositoryId(u64::from_le_bytes(bytes))
}
