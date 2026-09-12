//! Object resolution via `gix`.

use editchain_core::{GitAvailability, GitCommitEntity, GitOid, GitSignature, Payload};

use crate::discover::RepositoryHandle;
use crate::{HistoryRead, HistoryReadIssue, RefSnapshot};

/// Errors that can occur during object resolution.
#[derive(Debug)]
pub enum ResolutionError {
    /// Prefix syntax or length is outside the supported 7–64 hex characters.
    InvalidPrefix(String),
    /// More than one object matches the requested hexadecimal prefix.
    AmbiguousPrefix(String),
    /// The repository could not be opened.
    Open(String),
    /// The object could not be found in the object database.
    NotFound(String),
    /// The object data could not be decoded.
    Decode(String),
    /// An object exists but is not the requested Git kind.
    WrongKind {
        /// Required object kind.
        expected: &'static str,
        /// Actual object kind supplied by the repository.
        actual: String,
    },
}

impl core::fmt::Display for ResolutionError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::InvalidPrefix(prefix) => write!(f, "invalid Git object prefix: {prefix}"),
            Self::AmbiguousPrefix(prefix) => write!(f, "ambiguous Git object prefix: {prefix}"),
            Self::Open(e) => write!(f, "failed to open repository: {e}"),
            Self::NotFound(e) => write!(f, "object not found: {e}"),
            Self::Decode(e) => write!(f, "failed to decode object: {e}"),
            Self::WrongKind { expected, actual } => {
                write!(f, "expected Git {expected}, found {actual}")
            }
        }
    }
}

impl std::error::Error for ResolutionError {}

/// Resolve a commit by OID from a live repository.
///
/// Reads the commit, tree, parents, author, committer, message, and refs
/// without mutating or fetching. Missing objects return `NotFound`; tree,
/// blob, and tag identities return `WrongKind`.
///
/// # Errors
///
/// Returns an error if the repository cannot be opened or the object cannot
/// be decoded.
pub fn resolve_commit(
    handle: &RepositoryHandle,
    oid: &GitOid,
) -> Result<GitCommitEntity, ResolutionError> {
    let refs = RefSnapshot::capture(handle)?;
    resolve_commit_with_refs(handle, oid, &refs)
}

/// Resolve one immutable commit using a previously captured ref observation.
/// This avoids rereading refs for each object in an incremental history walk.
///
/// # Errors
/// Returns object lookup or decoding errors.
pub fn resolve_commit_with_refs(
    handle: &RepositoryHandle,
    oid: &GitOid,
    refs: &RefSnapshot,
) -> Result<GitCommitEntity, ResolutionError> {
    let gix_oid = git_oid_from(oid)?;
    let id = handle.repo.find_object(gix_oid).map_err(|e| match e {
        gix_object::find::existing::Error::NotFound { .. } => {
            ResolutionError::NotFound(e.to_string())
        }
        gix_object::find::existing::Error::Find(_) => ResolutionError::Decode(e.to_string()),
    })?;

    if id.kind != gix_object::Kind::Commit {
        return Err(ResolutionError::WrongKind {
            expected: "commit",
            actual: id.kind.to_string(),
        });
    }
    let parsed = gix_object::CommitRef::from_bytes(&id.data, gix_oid.kind())
        .map_err(|e| ResolutionError::Decode(e.to_string()))?;

    let author = parsed
        .author()
        .map_err(|e| ResolutionError::Decode(e.to_string()))?;
    let committer = parsed
        .committer()
        .map_err(|e| ResolutionError::Decode(e.to_string()))?;

    let parents = parsed
        .parents()
        .map(|p| git_oid_from_gix(&p))
        .collect::<Result<_, _>>()?;
    let tree = git_oid_from_gix(&parsed.tree())?;

    Ok(GitCommitEntity {
        repository: handle.discovery.id,
        object_format: oid.format(),
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
        live_refs: refs
            .refs_for(oid)
            .iter()
            .cloned()
            .map(Payload::Inline)
            .collect(),
        changed_paths: Vec::new(),
    })
}

/// Resolve an unambiguous hexadecimal object prefix to a commit.
///
/// Git's normal commit output uses an abbreviated object ID. This helper asks
/// the repository object database to disambiguate that prefix, rejects matches
/// to non-commit objects, and then returns the same fully populated resolution
/// as [`resolve_commit`]. `Ok(None)` means no matching object exists.
///
/// # Errors
///
/// Invalid and ambiguous prefixes, wrong object kinds, and unavailable or
/// corrupt object data remain distinct errors. Callers must not use an error
/// as evidence that a prefix is unique in another repository.
pub fn resolve_commit_prefix(
    handle: &RepositoryHandle,
    prefix: &str,
) -> Result<Option<GitCommitEntity>, ResolutionError> {
    let width = handle.repo.object_hash().len_in_hex();
    if prefix.len() < 7 || prefix.len() > 64 || !prefix.as_bytes().iter().all(u8::is_ascii_hexdigit)
    {
        return Err(ResolutionError::InvalidPrefix(prefix.to_owned()));
    }
    if prefix.len() > width {
        // A valid SHA-256 abbreviation cannot match a shorter SHA-1 object.
        return Ok(None);
    }
    let padded = format!("{prefix:0<width$}");
    let oid = gix::hash::ObjectId::from_hex(padded.as_bytes())
        .map_err(|error| ResolutionError::Decode(error.to_string()))?;
    let lookup = gix::hash::Prefix::new(&oid, prefix.len())
        .map_err(|error| ResolutionError::Decode(error.to_string()))?;
    let candidate = handle
        .repo
        .objects
        .lookup_prefix(lookup, None)
        .map_err(|error| ResolutionError::Decode(error.to_string()))?;
    match candidate {
        None => Ok(None),
        Some(Err(())) => Err(ResolutionError::AmbiguousPrefix(prefix.to_owned())),
        Some(Ok(oid)) => resolve_commit(handle, &git_oid_from_gix(&oid)?).map(Some),
    }
}

/// Resolve the commit at the tip of a local branch at a historical wall time.
///
/// This is deliberately stricter than Git's `branch@{date}` revision syntax:
/// the branch must still exist, its own reflog must cover `unix_ms`, every
/// inspected reflog entry must decode, and the selected object must resolve as
/// a commit. Reflog timestamps have one-second precision, so an update in the
/// same second as `unix_ms` is rejected rather than ordered arbitrarily.
///
/// The result is suitable for recovering a durable object identity from local
/// historical evidence. It is not portable source metadata: callers should
/// persist the resulting full OID when they accept it.
#[must_use]
pub fn resolve_branch_tip_at_time(
    handle: &RepositoryHandle,
    branch: &str,
    unix_ms: u64,
) -> Option<GitCommitEntity> {
    if branch.is_empty() || branch == "HEAD" {
        return None;
    }
    let target_seconds = i64::try_from(unix_ms / 1_000).ok()?;
    let reference_name = format!("refs/heads/{branch}");
    let reference = handle.repo.find_reference(reference_name.as_str()).ok()?;
    let mut platform = reference.log_iter();
    let entries = platform.all().ok()??;
    let mut first_entry_seconds = None;
    let mut candidate = None;

    for entry in entries {
        let entry = entry.ok()?;
        let entry_seconds = entry.signature.time().ok()?.seconds;
        if first_entry_seconds.is_none() {
            first_entry_seconds = Some(entry_seconds);
        }
        if entry_seconds == target_seconds {
            return None;
        }
        if entry_seconds < target_seconds {
            candidate = Some(entry.new_oid());
        }
    }

    if first_entry_seconds? > target_seconds {
        return None;
    }
    let candidate = candidate.filter(|oid| !oid.is_null())?;
    let oid = git_oid_from_gix(&candidate).ok()?;
    resolve_commit(handle, &oid).ok()
}

/// Walk the commit history of a repository from HEAD, resolving each commit.
///
/// Returns commits newest-first. `limit` bounds the number of commits walked
/// (0 = unlimited). Available commits survive a partial read; ref errors,
/// missing/corrupt ancestors, shallow boundaries, and limits remain observable.
///
/// # Errors
///
/// Returns an error if the repository has no resolvable HEAD.
pub fn walk_history(
    handle: &RepositoryHandle,
    limit: usize,
) -> Result<HistoryRead, ResolutionError> {
    let mut result = HistoryRead::default();
    match RefSnapshot::capture(handle) {
        Ok(refs) => result.refs = refs,
        Err(error) => result.issues.push(HistoryReadIssue { oid: None, error }),
    }
    match handle.repo.shallow_commits() {
        Ok(boundary) => result.shallow = boundary.is_some(),
        Err(error) => result.issues.push(HistoryReadIssue {
            oid: None,
            error: ResolutionError::Decode(error.to_string()),
        }),
    }
    let head = handle
        .repo
        .head()
        .map_err(|e| ResolutionError::Open(e.to_string()))?;
    let Some(head_id) = head.id() else {
        return Ok(result); // unborn HEAD
    };

    let walk = head_id
        .ancestors()
        .all()
        .map_err(|e| ResolutionError::Decode(e.to_string()))?;

    for info in walk {
        if limit > 0 && result.commits.len() >= limit {
            result.truncated = true;
            break;
        }
        let info = match info {
            Ok(info) => info,
            Err(error) => {
                result.issues.push(HistoryReadIssue {
                    oid: None,
                    error: ResolutionError::Decode(error.to_string()),
                });
                continue;
            }
        };
        let git_oid = match git_oid_from_gix(&info.id) {
            Ok(oid) => oid,
            Err(error) => {
                result.issues.push(HistoryReadIssue { oid: None, error });
                continue;
            }
        };
        match resolve_commit_with_refs(handle, &git_oid, &result.refs) {
            Ok(commit) => result.commits.push(commit),
            Err(error) => result.issues.push(HistoryReadIssue {
                oid: Some(git_oid),
                error,
            }),
        }
    }
    Ok(result)
}

/// Convert an `editchain_core::GitOid` to a `gix::hash::ObjectId`.
#[expect(
    clippy::indexing_slicing,
    reason = "digest_len is 20 or 32, always within the 32-byte buffer"
)]
pub(crate) fn git_oid_from(oid: &GitOid) -> Result<gix::hash::ObjectId, ResolutionError> {
    let len = oid.digest_len();
    let bytes = &oid.as_bytes()[..len];
    gix::hash::ObjectId::try_from(bytes)
        .map_err(|e| ResolutionError::Decode(format!("invalid OID bytes: {e}")))
}

/// Convert a `gix::hash::ObjectId` to an `editchain_core::GitOid`.
pub(crate) fn git_oid_from_gix(id: &gix::hash::ObjectId) -> Result<GitOid, ResolutionError> {
    match id {
        gix::hash::ObjectId::Sha1(bytes) => Ok(GitOid::from_sha1(*bytes)),
        gix::hash::ObjectId::Sha256(bytes) => Ok(GitOid::from_sha256(*bytes)),
        _ => Err(ResolutionError::Decode(
            "unsupported Git object format".to_owned(),
        )),
    }
}
