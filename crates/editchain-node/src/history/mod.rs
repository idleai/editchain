//! Shared history backend for CLI snapshot preparation and native viewer requests.
//!
//! Workspace lifetimes and pinned source reads live here. Payload previews,
//! file diffs, row presentation, search, and derived snapshots have dedicated
//! modules. Framed request dispatch is owned by the sibling transport adapter.

#[cfg(test)]
use editchain_import as _;
use serde as _;
#[cfg(test)]
use tempfile as _;

mod details;
mod files;
mod legacy_preview;
mod live;
mod realtime;
pub(crate) use realtime::LiveWorkspace;
mod payloads;
mod presentation;
mod search;
mod sessions;
mod snapshot;

pub(crate) use details::resolved_object_from_commit;
use editchain_store::OpRecordLocation;
pub(crate) use editchain_store::{BlobReader as BlobResolver, BlobResolution, ChainReadStats};
#[cfg(test)]
use payloads::hydrate_blob_payloads;
pub use payloads::BlobHydrationStats;
pub use search::{build_lexical_index, SearchIndexState};
pub use snapshot::RenderSnapshotReport;
use snapshot::{RenderSnapshot, SnapshotBuilder, SnapshotIdentity, SnapshotManifestData};

use editchain_core::{GitOid, Op, OpId, RepositoryId};
use editchain_git::{open_repository, resolve_commit, walk_history, RepositoryCatalog};
use editchain_project::activity_view::ActivityView;
use editchain_project::taxonomy::{ActivityKind, ChainState, Outcome, RecordRole, Visibility};
use editchain_project::HistoryProjection;
use editchain_protocol::{
    ErrorCode, FileChangeDto, RowContentDto, ServiceError, SessionMetaDto, SnapshotId,
};
use editchain_store::CanonicalChain;
use files::{agent_file_change_index, git_file_change_index};
use payloads::{hydrate_kind, projection_ops_with_previews};
use sessions::session_metadata_index;
use std::collections::HashMap;
use std::fmt::Write as _;
use std::io;
use std::path::{Path, PathBuf};

/// A loaded workspace: chain ops + git repositories.
#[derive(Debug)]
pub struct Workspace {
    /// The unified history projection.
    projection: HistoryProjection,
    /// Canonical decoded operations with durable blob references preserved.
    /// Full payload bytes are materialized from this corpus only for details or
    /// the lazy search index, never for graph projection/layout.
    source_ops: Vec<Op>,
    /// Constant-time lookup into `source_ops` for detail requests.
    source_op_index: HashMap<OpId, usize>,
    /// Small session provenance labels keyed by the same `session:<id>` group
    /// strings used by projected history rows.
    session_metadata: HashMap<String, SessionMetaDto>,
    /// Imported Claude/Codex file changes keyed by the raw history row that
    /// owns their normalized operation.
    agent_file_changes: HashMap<OpId, Vec<FileChangeDto>>,
    /// Immutable first-parent Git changes keyed by repository and commit.
    git_file_changes: HashMap<(RepositoryId, GitOid), Vec<FileChangeDto>>,
    /// Accepted operation ids and exact segment-record locations. This is
    /// persisted into render snapshots so details remain lazy on the fast path.
    source_op_locations: Vec<SnapshotOpLocator>,
    /// Read-only durable blob store used by on-demand details/search hydration.
    blob_resolver: Option<BlobResolver>,
    /// Discovered git repositories.
    repositories: RepositoryCatalog,
    /// Diagnostics for this open: chain canonicalization and bounded blob
    /// preview/deferred-hydration outcomes.
    pub diagnostics: OpenDiagnostics,
    /// Absolute workspace root used if a non-default request must lazily load
    /// the complete projection after a snapshot-backed Open.
    root_path: PathBuf,
    /// Absolute authoritative chain directory.
    chain_path: PathBuf,
    /// Inputs pinned at open; absent only for an in-memory projection.
    source_identity: Option<SnapshotIdentity>,
    /// Cached wire identity, computed once for this opened source version.
    snapshot_id: SnapshotId,
    /// Exactly one backend owns the fixed row order at a time.
    backend: WorkspaceBackend,
    /// The fixed Activity-view snapshot shared by window and find requests.
    current_view: Option<ActivityView<ExpandedChildRow>>,
}

#[derive(Debug)]
enum WorkspaceBackend {
    Cached(Box<RenderSnapshot>),
    Projected,
}

/// Parameters for one history-window read.
#[derive(Debug, Clone, Copy)]
pub struct HistoryWindowOptions {
    /// Expanded-row offset (zero is newest).
    pub offset: u64,
    /// Maximum expanded rows to return.
    pub limit: u64,
    /// Compute and attach global lane geometry before returning.
    pub include_layout: bool,
}

/// Display content supplied to the project-owned presentation tree.
#[derive(Debug)]
struct ExpandedChildRow {
    op_id: String,
    git_oid: Option<String>,
    repository: Option<String>,
    summary: String,
    content: RowContentDto,
    timestamp_ms: u64,
    kind: String,
    author: String,
    commit_id: String,
    is_system: bool,
    record_role: RecordRole,
    activity_kind: ActivityKind,
    visibility: Visibility,
    outcome: Outcome,
    chain_state: ChainState,
    turn_id: Option<String>,
    promoted: bool,
    activity_bundle: Option<editchain_protocol::ActivityBundleDto>,
    file_change: Option<FileChangeDto>,
}

/// Accepted operation identity paired with its authoritative record location.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SnapshotOpLocator {
    /// Canonical operation identity.
    pub(crate) id: OpId,
    /// First accepted record carrying this identity.
    pub(crate) location: OpRecordLocation,
}

/// Diagnostics reported when a workspace opens.
#[derive(Debug, Clone, Copy, Default, serde::Serialize, serde::Deserialize)]
pub struct OpenDiagnostics {
    /// Chain record canonicalization (dedup/quarantine) counts.
    pub chain: ChainReadStats,
    /// Durable blob hydration counts.
    pub blobs: BlobHydrationStats,
    /// Git discovery and history availability gaps.
    #[serde(default)]
    pub git: GitReadStats,
}

/// Observable gaps in live repository discovery and history reads.
#[derive(Debug, Clone, Copy, Default, serde::Serialize, serde::Deserialize)]
pub struct GitReadStats {
    /// Repository markers or directories that could not be inspected or opened.
    pub unavailable_repositories: usize,
    /// Incomplete history reads or unavailable exact linked commit targets.
    pub history_errors: usize,
}

impl OpenDiagnostics {
    /// Human-readable warnings for integrity gaps discovered during open.
    #[must_use]
    pub fn warnings(&self) -> Vec<String> {
        let mut warnings = Vec::new();
        if self.git.unavailable_repositories > 0 {
            warnings.push(format!(
                "{} Git repository location(s) could not be inspected",
                self.git.unavailable_repositories
            ));
        }
        if self.git.history_errors > 0 {
            warnings.push(format!(
                "{} Git history read(s) or linked target(s) were incomplete",
                self.git.history_errors
            ));
        }
        if self.chain.duplicates > 0 {
            warnings.push(format!(
                "{} exact replay record(s) ignored during open",
                self.chain.duplicates
            ));
        }
        if self.chain.quarantined > 0 {
            warnings.push(format!(
                "{} conflicting same-id record(s) quarantined during open",
                self.chain.quarantined
            ));
        }
        if self.chain.undecodable > 0 {
            warnings.push(format!(
                "{} record(s) could not be decoded; source bytes remain in the segments",
                self.chain.undecodable
            ));
        }
        if self.chain.incomplete_tails > 0 {
            warnings.push(format!(
                "{} incomplete segment tail(s); only complete records were loaded",
                self.chain.incomplete_tails
            ));
        }
        if self.blobs.missing > 0 {
            warnings.push(format!(
                "{} blob payload(s) missing from the durable store",
                self.blobs.missing
            ));
        }
        if self.blobs.corrupt > 0 {
            warnings.push(format!(
                "{} blob payload(s) failed length/hash validation",
                self.blobs.corrupt
            ));
        }
        if self.blobs.unresolved > 0 {
            warnings.push(format!(
                "{} blob reference(s) not addressable by this store",
                self.blobs.unresolved
            ));
        }
        warnings
    }
}

#[must_use]
fn hash_raw(data: &[u8]) -> [u8; 32] {
    blake3::hash(data).into()
}

fn hex_string(bytes: &[u8]) -> Result<String, std::fmt::Error> {
    let mut output = String::with_capacity(bytes.len().saturating_mul(2));
    for byte in bytes {
        write!(&mut output, "{byte:02x}")?;
    }
    Ok(output)
}

impl Workspace {
    /// Identity that scopes every request and response for this opened view.
    #[must_use]
    pub const fn snapshot_id(&self) -> &SnapshotId {
        &self.snapshot_id
    }

    /// Read the loaded projection without allowing independent cache mutation.
    #[must_use]
    pub const fn projection(&self) -> &HistoryProjection {
        &self.projection
    }

    /// Repository locations fixed when this workspace was opened.
    #[must_use]
    pub const fn repositories(&self) -> &RepositoryCatalog {
        &self.repositories
    }

    /// Create a workspace from an existing projection (used in tests).
    #[must_use]
    pub fn from_projection(projection: HistoryProjection) -> Self {
        let source_ops = projection.ops().to_vec();
        let session_metadata = session_metadata_index(projection.ops());
        let source_op_index = source_ops
            .iter()
            .enumerate()
            .map(|(index, op)| (op.id, index))
            .collect();
        Self {
            projection,
            source_ops,
            source_op_index,
            session_metadata,
            agent_file_changes: HashMap::new(),
            git_file_changes: HashMap::new(),
            source_op_locations: Vec::new(),
            blob_resolver: None,
            repositories: RepositoryCatalog::default(),
            diagnostics: OpenDiagnostics::default(),
            root_path: PathBuf::new(),
            chain_path: PathBuf::new(),
            source_identity: None,
            snapshot_id: unique_snapshot_id("memory"),
            backend: WorkspaceBackend::Projected,
            current_view: None,
        }
    }

    /// Load a workspace from a chain directory and discover git repos.
    ///
    /// # Errors
    ///
    /// Returns an error if the chain cannot be read or repos cannot be discovered.
    pub fn open(workspace_path: &str, chain_dir: &str) -> Result<Self, Box<dyn std::error::Error>> {
        Self::open_with_cache(workspace_path, chain_dir, true)
    }

    /// Reopen authoritative history without reusing a derived render cache.
    ///
    /// Every explicit refresh receives a new request identity even if the
    /// captured source fingerprint is unchanged. This includes availability
    /// changes outside that fingerprint, such as an alternate Git object store.
    ///
    /// # Errors
    ///
    /// Returns source, repository, or projection failures while opening history.
    pub fn refresh(
        workspace_path: &str,
        chain_dir: &str,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let mut workspace = Self::open_with_cache(workspace_path, chain_dir, false)?;
        workspace.snapshot_id =
            unique_snapshot_id(&format!("refresh:{}", workspace.snapshot_id.as_str()));
        Ok(workspace)
    }

    fn open_with_cache(
        workspace_path: &str,
        chain_dir: &str,
        reuse_cache: bool,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        // Resolve the chain directory relative to the workspace when it is a
        // relative path (e.g. ".editchain"). The service process's CWD is not
        // necessarily the workspace root, so we must join explicitly.
        let chain_path = if PathBuf::from(chain_dir).is_absolute() {
            PathBuf::from(chain_dir)
        } else {
            PathBuf::from(workspace_path).join(chain_dir)
        };
        let workspace_path = PathBuf::from(workspace_path);
        let repositories = RepositoryCatalog::discover(&workspace_path)?;
        if let Some(identity) = (reuse_cache && repositories.is_complete())
            .then(|| {
                SnapshotIdentity::capture(&workspace_path, &chain_path, repositories.entries())
            })
            .and_then(Result::ok)
        {
            if let Ok(Some(snapshot)) = RenderSnapshot::open(&chain_path, &identity) {
                let diagnostics = snapshot.diagnostics();
                let workspace = Self {
                    projection: HistoryProjection::from_ops(Vec::new()),
                    source_ops: Vec::new(),
                    source_op_index: HashMap::new(),
                    session_metadata: HashMap::new(),
                    agent_file_changes: HashMap::new(),
                    git_file_changes: HashMap::new(),
                    source_op_locations: Vec::new(),
                    blob_resolver: Some(BlobResolver::open(&chain_path)?),
                    repositories,
                    diagnostics,
                    root_path: workspace_path,
                    chain_path,
                    snapshot_id: snapshot.snapshot_id(),
                    source_identity: Some(identity),
                    backend: WorkspaceBackend::Cached(Box::new(snapshot)),
                    current_view: None,
                };
                workspace.ensure_sources_current()?;
                return Ok(workspace);
            }
        }
        Self::open_projection(workspace_path, chain_path, repositories)
    }

    /// Load the authoritative projection, bypassing any derived render cache.
    fn open_projection(
        workspace_path: PathBuf,
        chain_path: PathBuf,
        repositories: RepositoryCatalog,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let identity =
            SnapshotIdentity::capture(&workspace_path, &chain_path, repositories.entries())?;
        let (source_ops, chain_stats, source_op_locations) = read_chain_ops(&chain_path)?;
        // Keep durable references in the canonical source corpus. The graph
        // projection receives only bounded display previews, preventing large
        // payload bytes from being multiplied by collapse/view/layout clones.
        // Details and search hydrate a single source op at a time on demand.
        let resolver = BlobResolver::open(&chain_path)?;
        let (projection_ops, blob_stats, incomplete) =
            projection_ops_with_previews(&source_ops, &resolver);
        let mut diagnostics = OpenDiagnostics {
            chain: chain_stats,
            blobs: blob_stats,
            git: GitReadStats {
                unavailable_repositories: repositories.issues().len(),
                history_errors: 0,
            },
        };
        let mut projection =
            HistoryProjection::from_source_previews(&source_ops, projection_ops, &incomplete);
        // Walk each discovered repo's history into the projection.
        for discovery in &repositories {
            let opened = open_repository(discovery);
            let Ok(handle) = opened else {
                diagnostics.git.unavailable_repositories =
                    diagnostics.git.unavailable_repositories.saturating_add(1);
                continue;
            };
            let walked = walk_history(&handle, 0);
            if let Ok(history) = walked {
                if !history.is_complete() {
                    diagnostics.git.history_errors =
                        diagnostics.git.history_errors.saturating_add(1);
                }
                projection.merge_git_commits(history.commits);
            } else {
                diagnostics.git.history_errors = diagnostics.git.history_errors.saturating_add(1);
            }
        }
        // A session may have started on a commit that is no longer reachable
        // from the repository's current HEAD. Resolve only the exact OIDs
        // carried by durable GitLink ops; never guess from timestamps or text.
        let unresolved_targets =
            merge_exact_git_link_targets(&mut projection, repositories.entries());
        diagnostics.git.history_errors = diagnostics
            .git
            .history_errors
            .saturating_add(unresolved_targets);
        let source_op_index = source_ops
            .iter()
            .enumerate()
            .map(|(index, op)| (op.id, index))
            .collect();
        let session_metadata = session_metadata_index(projection.ops());
        let agent_file_changes = agent_file_change_index(
            &source_ops,
            &workspace_path,
            Some(&resolver),
            repositories.entries(),
        );
        let git_file_changes = git_file_change_index(&projection, repositories.entries());
        let workspace = Self {
            projection,
            source_ops,
            source_op_index,
            session_metadata,
            agent_file_changes,
            git_file_changes,
            source_op_locations,
            blob_resolver: Some(resolver),
            repositories,
            diagnostics,
            root_path: workspace_path,
            chain_path,
            snapshot_id: SnapshotId::new(identity.hash()?),
            source_identity: Some(identity),
            backend: WorkspaceBackend::Projected,
            current_view: None,
        };
        workspace.ensure_sources_current()?;
        Ok(workspace)
    }

    /// Materialize the complete projection when details, diffs, or find need
    /// source data that is not stored in the render snapshot.
    pub(crate) fn ensure_projection_loaded(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        if matches!(self.backend, WorkspaceBackend::Projected) {
            return Ok(());
        }
        self.ensure_sources_current()?;
        let loaded = Self::open_projection(
            self.root_path.clone(),
            self.chain_path.clone(),
            self.repositories.clone(),
        )?;
        if loaded.source_identity != self.source_identity {
            return Err(stale_snapshot().into());
        }
        // Publish the complete replacement only after validating its sources.
        // All subsequent pages and search use this same computed backend.
        *self = loaded;
        Ok(())
    }

    /// Guard work that reads authoritative files after an opened view exists.
    /// Pure paging and searches of an already-built index retain pinned data.
    pub(crate) fn ensure_sources_current(&self) -> Result<(), Box<dyn std::error::Error>> {
        if let Some(identity) = &self.source_identity {
            let current = SnapshotIdentity::capture(
                &self.root_path,
                &self.chain_path,
                self.repositories.entries(),
            )?;
            if &current != identity {
                return Err(stale_snapshot().into());
            }
        }
        Ok(())
    }

    /// Node count for the Open handshake, independent of backend.
    pub(crate) fn node_count(&self) -> u64 {
        match &self.backend {
            WorkspaceBackend::Cached(snapshot) => snapshot.projection_nodes(),
            WorkspaceBackend::Projected => u64::try_from(self.projection.len()).unwrap_or(u64::MAX),
        }
    }

    /// Accepted chain generation for the Open handshake, independent of backend.
    pub(crate) fn chain_generation(&self) -> u64 {
        match &self.backend {
            WorkspaceBackend::Cached(snapshot) => snapshot.chain_generation(),
            WorkspaceBackend::Projected => {
                u64::try_from(self.projection.ops().len()).unwrap_or(u64::MAX)
            }
        }
    }

    /// Human-readable render-cache status for diagnostics and performance tests.
    pub(crate) const fn render_snapshot_status(&self) -> &'static str {
        if matches!(self.backend, WorkspaceBackend::Cached(_)) {
            "hit"
        } else {
            "miss"
        }
    }

    /// Load and fully hydrate one canonical source operation from either the
    /// live corpus or a render snapshot's exact segment locator.
    fn source_op(&self, op_id: OpId) -> Option<Op> {
        let mut op = if let Some(index) = self.source_op_index.get(&op_id).copied() {
            self.source_ops.get(index)?.clone()
        } else {
            let WorkspaceBackend::Cached(snapshot) = &self.backend else {
                return None;
            };
            let location = snapshot.op_location(op_id)?;
            let decoded = read_op_at(snapshot.chain_dir(), location).ok()?;
            if decoded.id != op_id {
                return None;
            }
            decoded
        };
        if let Some(resolver) = &self.blob_resolver {
            let mut stats = BlobHydrationStats::default();
            hydrate_kind(&mut op.kind, resolver, &mut stats);
        }
        Some(op)
    }

    /// Returns true if a repository is nested inside another discovered repo
    /// (i.e. a submodule or vendored nested repo, not the workspace root).
    #[must_use]
    fn is_submodule(&self, discovery: &editchain_git::RepositoryDiscovery) -> bool {
        self.repositories.is_nested(discovery.id)
    }

    /// Returns true if the repository with the given id is a submodule.
    #[must_use]
    fn repo_is_submodule(&self, repository_id: RepositoryId) -> bool {
        self.repositories
            .iter()
            .any(|d| d.id == repository_id && self.is_submodule(d))
    }
}

/// Pregenerate the immutable fixed-view render snapshot used by the extension.
///
/// The source segment log and Git HEADs are fingerprinted before and after the
/// build. A concurrent append or checkout therefore aborts publication instead
/// of exposing rows derived from a mixed source generation.
///
/// # Errors
///
/// Returns an error when the chain/projection cannot be read, the source
/// changes during generation, or the snapshot cannot be written durably.
pub fn prepare_render_snapshot(
    workspace_path: &Path,
    chain_dir: &Path,
) -> Result<RenderSnapshotReport, Box<dyn std::error::Error>> {
    let chain_path = if chain_dir.is_absolute() {
        chain_dir.to_path_buf()
    } else {
        workspace_path.join(chain_dir)
    };
    let repositories = RepositoryCatalog::discover(workspace_path)?;
    if let Some(issue) = repositories.issues().first() {
        return Err(format!(
            "repository discovery is incomplete at {}: {}",
            issue.path.display(),
            issue.message
        )
        .into());
    }
    let identity = SnapshotIdentity::capture(workspace_path, &chain_path, repositories.entries())?;
    if let Ok(Some(snapshot)) = RenderSnapshot::open(&chain_path, &identity) {
        return snapshot.report();
    }

    let mut workspace = Workspace::open_projection(
        workspace_path.to_path_buf(),
        chain_path.clone(),
        repositories.clone(),
    )?;
    let page_limit = 4_096u64;
    let first = workspace.history_window(HistoryWindowOptions {
        offset: 0,
        limit: page_limit,
        include_layout: true,
    })?;
    let sub_op_counts = first
        .sub_op_counts
        .clone()
        .ok_or("snapshot first window omitted expansion index")?;
    let expansion_spans = first
        .expansion_spans
        .clone()
        .ok_or("snapshot first window omitted nested expansion index")?;
    let total = first.total;
    let max_lane = first.max_lane;
    let mut builder = SnapshotBuilder::new(&chain_path, identity.clone())?;
    builder.write_rows(&first.rows)?;
    let mut offset = u64::try_from(first.rows.len())?;
    while offset < total {
        let window = workspace.history_window(HistoryWindowOptions {
            offset,
            limit: page_limit,
            include_layout: true,
        })?;
        if window.rows.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "render snapshot projection ended before its declared total",
            )
            .into());
        }
        builder.write_rows(&window.rows)?;
        offset = offset.saturating_add(u64::try_from(window.rows.len())?);
    }

    let final_identity =
        SnapshotIdentity::capture(workspace_path, &chain_path, repositories.entries())?;
    if final_identity != identity {
        return Err(
            io::Error::other("chain or Git HEAD changed while preparing render snapshot").into(),
        );
    }
    builder.finish(
        SnapshotManifestData {
            projection_nodes: u64::try_from(workspace.projection.len()).unwrap_or(u64::MAX),
            chain_generation: u64::try_from(workspace.projection.ops().len()).unwrap_or(u64::MAX),
            max_lane,
            diagnostics: workspace.diagnostics,
        },
        &sub_op_counts,
        &expansion_spans,
        &workspace.source_op_locations,
    )
}

/// Prepare or incrementally advance the native live checkpoint used by VS Code.
/// Existing checkpoints resume their admission frontier and hydrate index pages
/// on demand. Source records remain authoritative.
///
/// # Errors
/// Returns source, checkpoint validation, locking or durable IO errors.
pub fn prepare_live_checkpoint(
    workspace_path: &Path,
    chain_dir: &Path,
) -> Result<editchain_protocol::OpenResponse, Box<dyn std::error::Error>> {
    editchain_index::boundary(|| {
        let mut workspace = LiveWorkspace::prepare(&editchain_protocol::OpenRequest {
            workspace_path: workspace_path.to_string_lossy().into_owned(),
            chain_dir: chain_dir.to_string_lossy().into_owned(),
        })?;
        let opened = workspace.opened();
        if let Some(baseline) = opened.live {
            let _update = workspace.sync(&editchain_protocol::SyncLiveRequest {
                epoch: baseline.epoch,
                after_revision: baseline.revision,
                codex: None,
            })?;
        }
        Ok(workspace.opened())
    })?
}

/// Parse an exact decimal `RepositoryId` string, rejecting anything else.
///
/// # Errors
///
/// Returns an error message when the string is not a valid `u64`.
pub fn parse_repository_id(s: &str) -> Result<RepositoryId, String> {
    s.parse::<u64>()
        .map(RepositoryId)
        .map_err(|_err| format!("invalid repository id: {s:?}"))
}

/// Parse a lowercase hex git OID string (40 chars SHA-1 / 64 SHA-256).
///
/// # Errors
///
/// Returns an error message when the string is not valid hex of a supported
/// length.
pub fn parse_git_oid(s: &str) -> Result<GitOid, String> {
    GitOid::from_hex(s).ok_or_else(|| format!("invalid git oid: {s:?}"))
}

/// Result of [`read_chain_ops`]: accepted ops plus canonicalization stats.
type ChainReadResult =
    Result<(Vec<Op>, ChainReadStats, Vec<SnapshotOpLocator>), Box<dyn std::error::Error>>;

/// Read canonical operations and their durable detail locations through the
/// shared store. Conflicted IDs are absent from every consumer's valid corpus.
fn read_chain_ops(chain_dir: &Path) -> ChainReadResult {
    let chain = CanonicalChain::read(chain_dir)?;
    let stats = chain.stats();
    let mut ops = Vec::with_capacity(stats.accepted);
    let mut locations = Vec::with_capacity(stats.accepted);
    for (op, location) in chain.into_located_ops() {
        let location = location.ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "stored operation has no location",
            )
        })?;
        locations.push(SnapshotOpLocator {
            id: op.id,
            location,
        });
        ops.push(op);
    }
    Ok((ops, stats, locations))
}

fn read_op_at(
    chain_dir: &Path,
    location: OpRecordLocation,
) -> Result<Op, Box<dyn std::error::Error>> {
    editchain_store::read_op_at(chain_dir, location).map_err(Into::into)
}

/// Resolve a git commit by OID in a discovered repository.
///
/// # Errors
///
/// Returns an error if the repository cannot be opened or the object cannot
/// be resolved.
pub fn resolve_git_commit(
    workspace: &Workspace,
    repository_id: RepositoryId,
    oid: &GitOid,
) -> Result<Option<editchain_core::GitCommitEntity>, Box<dyn std::error::Error>> {
    let Some(discovery) = workspace
        .repositories
        .iter()
        .find(|d| d.id == repository_id)
    else {
        return Ok(None);
    };
    let handle = open_repository(discovery)?;
    match resolve_commit(&handle, oid) {
        Ok(commit) => Ok(Some(commit)),
        Err(editchain_git::ResolutionError::NotFound(_)) => Ok(None),
        Err(error) => Err(error.into()),
    }
}

/// Resolve exact durable Git-link targets that the current HEAD walk did not
/// include (for example, a session started on a branch that was later switched).
fn merge_exact_git_link_targets(
    projection: &mut HistoryProjection,
    repositories: &[editchain_git::RepositoryDiscovery],
) -> usize {
    let targets: std::collections::BTreeSet<(RepositoryId, GitOid)> = projection
        .git()
        .links()
        .values()
        .flatten()
        .map(|link| (link.target_repo, link.target_oid))
        .filter(|target| !projection.git().commits().contains_key(target))
        .collect();

    let mut unresolved = targets.len();
    for discovery in repositories {
        let repository_targets: Vec<GitOid> = targets
            .iter()
            .filter_map(|(repository, oid)| (*repository == discovery.id).then_some(*oid))
            .collect();
        if repository_targets.is_empty() {
            continue;
        }
        let Ok(handle) = open_repository(discovery) else {
            continue;
        };
        let commits: Vec<_> = repository_targets
            .iter()
            .filter_map(|oid| resolve_commit(&handle, oid).ok())
            .collect();
        unresolved = unresolved.saturating_sub(commits.len());
        projection.merge_git_commits(commits);
    }
    unresolved
}

pub(crate) fn stale_snapshot() -> ServiceError {
    ServiceError::new(
        ErrorCode::StaleSnapshot,
        "History sources changed. Reopen history to refresh this view.",
    )
}

fn unique_snapshot_id(prefix: &str) -> SnapshotId {
    static NEXT_SNAPSHOT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    let serial = NEXT_SNAPSHOT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    SnapshotId::new(format!("{prefix}:{}:{serial}", std::process::id()))
}

#[cfg(test)]
#[expect(
    clippy::indexing_slicing,
    reason = "Tests index into vectors whose length is asserted immediately before"
)]
mod tests;
