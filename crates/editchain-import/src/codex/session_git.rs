//! Exact Codex session-start Git anchoring.

use std::path::Path;

use editchain_core::{
    ActorId, Clock, GitLink, GitLinkKind, GitOid, Op, OpKind, ParentSet, RepositoryId, ScopeRef,
    SessionId, Tags,
};

use super::projection::SessionMeta;
use crate::error::ImportError;
use crate::ids::{SourcePosition, SourceStream};

/// Reserved derived lane for the one session-start Git link.
///
/// Structural relationship notes reserve `0xFFFA..=0xFFFC`; keeping this lane
/// immediately below them avoids collisions with ordinary normalized lanes.
const SESSION_GIT_LINK_LANE: u16 = 0xFFF9;

/// Repository identity supplied by the host's catalog, independent of the
/// source-format importer. The unit implementation supplies no repositories.
pub trait RepositoryLookup: std::fmt::Debug {
    /// Resolve an exact recorded working directory to a repository identity.
    ///
    /// # Errors
    ///
    /// Returns an error when incomplete discovery cannot establish identity.
    fn repository_for_cwd(&self, cwd: &Path) -> Result<Option<RepositoryId>, ImportError>;
}

impl RepositoryLookup for () {
    fn repository_for_cwd(&self, _cwd: &Path) -> Result<Option<RepositoryId>, ImportError> {
        Ok(None)
    }
}

/// Build the exact `session_meta.git.commit_hash` relation for one Codex file.
///
/// No fallback is inferred. The link exists only when Codex supplied a valid
/// full SHA-1/SHA-256 hash and the recorded session cwd resolves to an actual
/// repository inside the imported workspace.
pub(super) fn session_git_link_op(
    repositories: &dyn RepositoryLookup,
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
    let cwd = Path::new(cwd);
    if !cwd.is_absolute() {
        return Ok(None);
    }
    let Some(target_repo) = repositories.repository_for_cwd(cwd)? else {
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
            target_repo,
            target_oid,
            kind: GitLinkKind::BasedOn,
        }),
    }))
}
