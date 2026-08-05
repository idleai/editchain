//! Object resolution via `gix`.

use editchain_core::{
    GitAvailability, GitCommitEntity, GitObjectFormat, GitOid, GitSignature, Payload,
};

use crate::discover::RepositoryHandle;

/// The outcome of resolving a commit from a live repository.
#[derive(Debug, Clone)]
pub struct CommitResolution {
    /// The resolved commit entity.
    pub commit: GitCommitEntity,
    /// Whether the object was found in the object database.
    pub found: bool,
}

/// Errors that can occur during object resolution.
#[derive(Debug)]
pub enum ResolutionError {
    /// The repository could not be opened.
    Open(String),
    /// The object could not be found in the object database.
    NotFound(String),
    /// The object data could not be decoded.
    Decode(String),
}

impl core::fmt::Display for ResolutionError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Open(e) => write!(f, "failed to open repository: {e}"),
            Self::NotFound(e) => write!(f, "object not found: {e}"),
            Self::Decode(e) => write!(f, "failed to decode object: {e}"),
        }
    }
}

impl std::error::Error for ResolutionError {}

/// Resolve a commit by OID from a live repository.
///
/// Reads the commit, tree, parents, author, committer, message, and refs
/// without mutating or fetching. Returns `found: false` if the object is
/// missing from the object database (e.g. shallow clone).
///
/// # Errors
///
/// Returns an error if the repository cannot be opened or the object cannot
/// be decoded.
pub fn resolve_commit(
    handle: &RepositoryHandle,
    oid: &GitOid,
) -> Result<CommitResolution, ResolutionError> {
    let gix_oid = git_oid_from(oid)?;
    let id = handle.repo.find_object(gix_oid).map_err(|e| match e {
        gix_object::find::existing::Error::NotFound { .. } => {
            ResolutionError::NotFound(e.to_string())
        }
        gix_object::find::existing::Error::Find(_) => ResolutionError::Decode(e.to_string()),
    })?;

    let commit = id.into_commit();
    let parsed = gix_object::CommitRef::from_bytes(&commit.data, gix_oid.kind())
        .map_err(|e| ResolutionError::Decode(e.to_string()))?;

    let author = parsed
        .author()
        .map_err(|e| ResolutionError::Decode(e.to_string()))?;
    let committer = parsed
        .committer()
        .map_err(|e| ResolutionError::Decode(e.to_string()))?;

    let parents = parsed.parents().map(|p| git_oid_from_gix(&p)).collect();
    let tree = git_oid_from_gix(&parsed.tree());

    // Collect refs pointing at this commit.
    let mut refs = Vec::new();
    if let Ok(references) = handle.repo.references() {
        if let Ok(all) = references.all() {
            for r in all.flatten() {
                // Skip symbolic refs (e.g. HEAD -> refs/heads/main).
                if r.target().try_id().is_some() && r.id() == gix_oid {
                    refs.push(Payload::Inline(r.name().as_bstr().to_vec()));
                }
            }
        }
    }

    let commit_entity = GitCommitEntity {
        repository: handle.discovery.id,
        object_format: oid.format,
        oid: *oid,
        imported_record: None,
        availability: GitAvailability::Resolved,
        tree,
        parents,
        author: GitSignature {
            name: Payload::Inline(author.name.to_vec()),
            email: Payload::Inline(author.email.to_vec()),
            when: author.time().map_or(0, |t| t.seconds),
        },
        committer: GitSignature {
            name: Payload::Inline(committer.name.to_vec()),
            email: Payload::Inline(committer.email.to_vec()),
            when: committer.time().map_or(0, |t| t.seconds),
        },
        authored_at: author.time().map_or(0, |t| t.seconds),
        committed_at: committer.time().map_or(0, |t| t.seconds),
        message: Payload::Inline(parsed.message.to_vec()),
        imported_refs: Vec::new(),
        live_refs: refs,
        changed_paths: Vec::new(),
    };

    Ok(CommitResolution {
        commit: commit_entity,
        found: true,
    })
}

/// Walk the commit history of a repository from HEAD, resolving each commit.
///
/// Returns commits newest-first. `limit` bounds the number of commits walked
/// (0 = unlimited). Missing objects are skipped rather than aborting the walk.
///
/// # Errors
///
/// Returns an error if the repository has no resolvable HEAD.
pub fn walk_history(
    handle: &RepositoryHandle,
    limit: usize,
) -> Result<Vec<GitCommitEntity>, ResolutionError> {
    let head = handle
        .repo
        .head()
        .map_err(|e| ResolutionError::Open(e.to_string()))?;
    let Some(head_id) = head.id() else {
        return Ok(Vec::new()); // unborn HEAD
    };

    let walk = head_id
        .ancestors()
        .all()
        .map_err(|e| ResolutionError::Decode(e.to_string()))?;

    let mut commits = Vec::new();
    for info in walk.flatten() {
        if limit > 0 && commits.len() >= limit {
            break;
        }
        let git_oid = git_oid_from_gix(&info.id);
        if let Ok(res) = resolve_commit(handle, &git_oid) {
            commits.push(res.commit);
        }
    }
    Ok(commits)
}

/// Convert an `editchain_core::GitOid` to a `gix::hash::ObjectId`.
#[expect(
    clippy::indexing_slicing,
    reason = "digest_len is 20 or 32, always within the 32-byte buffer"
)]
fn git_oid_from(oid: &GitOid) -> Result<gix::hash::ObjectId, ResolutionError> {
    let len = oid.digest_len();
    let bytes = &oid.bytes[..len];
    gix::hash::ObjectId::try_from(bytes)
        .map_err(|e| ResolutionError::Decode(format!("invalid OID bytes: {e}")))
}

/// Convert a `gix::hash::ObjectId` to an `editchain_core::GitOid`.
#[expect(
    clippy::indexing_slicing,
    clippy::match_same_arms,
    reason = "SHA-1/SHA-256 digests are at most 32 bytes; Kind is non-exhaustive so a wildcard fallback is required"
)]
fn git_oid_from_gix(id: &gix::hash::ObjectId) -> GitOid {
    let format = match id.kind() {
        gix::hash::Kind::Sha1 => GitObjectFormat::Sha1,
        gix::hash::Kind::Sha256 => GitObjectFormat::Sha256,
        _ => GitObjectFormat::Sha256,
    };
    let mut bytes = [0u8; 32];
    bytes[..id.as_bytes().len()].copy_from_slice(id.as_bytes());
    GitOid { format, bytes }
}
