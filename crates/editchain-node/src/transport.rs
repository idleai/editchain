//! Stateful dispatch for the framed history request/response protocol.

use editchain_protocol::{
    ErrorCode, FileChangeSource, OpenResponse, Request, RequestBody, Response, ResponseBody,
    ServiceError, SnapshotResult, PROTOCOL_VERSION,
};

use crate::history::{
    build_lexical_index, parse_git_oid, parse_repository_id, resolve_git_commit,
    resolved_object_from_commit, stale_snapshot, HistoryWindowOptions, SearchIndexState, Workspace,
};

/// A stateful server that owns a loaded workspace across requests.
#[derive(Debug)]
pub struct Server {
    live: Option<crate::history::LiveWorkspace>,
    /// The currently loaded workspace (None until `Open`).
    pub workspace: Option<Workspace>,
    /// The immutable lexical search index bound to the opened snapshot (built
    /// lazily on first `FindInHistory`).
    pub lexical: Option<SearchIndexState>,
}

impl Server {
    /// Create a new empty server.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            live: None,
            workspace: None,
            lexical: None,
        }
    }

    /// Handle a single request against the current state.
    ///
    /// # Errors
    ///
    /// Returns an error if the request cannot be handled.
    pub fn handle(&mut self, request: &Request) -> Result<Response, Box<dyn std::error::Error>> {
        let id = request.id;
        if let Err(error) = request.body.validate() {
            return Ok(Response {
                id,
                body: ResponseBody::Error(error),
            });
        }
        if let RequestBody::OpenLive(open) = &request.body {
            let live = crate::history::LiveWorkspace::open(open)?;
            let body = ResponseBody::Ok(serde_json::to_value(live.opened())?);
            self.live = Some(live);
            self.workspace = None;
            self.lexical = None;
            return Ok(Response { id, body });
        }
        if matches!(request.body, RequestBody::Open(_) | RequestBody::Refresh(_)) {
            self.live = None;
        } else if let Some(live) = &mut self.live {
            return Ok(Response {
                id,
                body: live.handle(&request.body)?,
            });
        }
        if let Some(requested) = request.body.snapshot_id() {
            let workspace = self.workspace.as_ref().ok_or_else(no_workspace)?;
            if requested.is_empty() {
                return Err(ServiceError::new(
                    ErrorCode::UnsupportedProtocol,
                    "This request requires the snapshot_id returned by Open protocol version 2.",
                )
                .into());
            }
            if requested != workspace.snapshot_id() {
                return Err(stale_snapshot().into());
            }
        }
        let reads_sources = match &request.body {
            RequestBody::Open(_)
            | RequestBody::OpenLive(_)
            | RequestBody::SyncLive(_)
            | RequestBody::Refresh(_)
            | RequestBody::GetWindow(_)
            | RequestBody::LocateRows(_) => false,
            RequestBody::FindInHistory(_) => self.lexical.is_none(),
            RequestBody::GetNodeDetails(_)
            | RequestBody::ResolveObject(_)
            | RequestBody::GetFileDiff(_) => true,
        };
        if reads_sources {
            self.workspace
                .as_ref()
                .ok_or_else(no_workspace)?
                .ensure_sources_current()?;
        }
        let body = match &request.body {
            RequestBody::OpenLive(_) | RequestBody::SyncLive(_) => {
                return Err(no_workspace().into())
            }
            RequestBody::Open(req) | RequestBody::Refresh(req) => {
                let workspace = if matches!(&request.body, RequestBody::Open(_)) {
                    Workspace::open(&req.workspace_path, &req.chain_dir)?
                } else {
                    Workspace::refresh(&req.workspace_path, &req.chain_dir)?
                };
                let diagnostics = workspace.diagnostics;
                let warnings = workspace.diagnostics.warnings();
                let response = OpenResponse {
                    live: None,
                    protocol_version: PROTOCOL_VERSION,
                    live_updates: true,
                    snapshot_id: workspace.snapshot_id().clone(),
                    workspace: req.workspace_path.clone(),
                    chain: req.chain_dir.clone(),
                    repos: workspace.repositories().len(),
                    nodes: workspace.node_count(),
                    chain_generation: workspace.chain_generation(),
                    render_snapshot: workspace.render_snapshot_status().to_owned(),
                    diagnostics: serde_json::to_value(diagnostics)?,
                    warnings,
                };
                // The lexical index is built lazily on first find request (it is
                // expensive for large chains and unnecessary for the graph view).
                self.workspace = Some(workspace);
                self.lexical = None;
                ResponseBody::Ok(serde_json::to_value(response)?)
            }
            RequestBody::GetWindow(req) => {
                let ws = self.workspace.as_mut().ok_or_else(no_workspace)?;
                let window = ws.history_window(HistoryWindowOptions {
                    offset: req.offset,
                    limit: req.limit,
                    include_layout: req.include_layout,
                })?;
                ResponseBody::Ok(serde_json::to_value(window)?)
            }
            RequestBody::LocateRows(req) => {
                let ws = self.workspace.as_mut().ok_or_else(no_workspace)?;
                ResponseBody::Ok(serde_json::to_value(ws.locate_rows(&req.keys)?)?)
            }
            RequestBody::GetNodeDetails(req) => {
                let ws = self.workspace.as_ref().ok_or_else(no_workspace)?;
                match ws.node_details(Some(req.op_id.clone()), None) {
                    Some(details) => ResponseBody::Ok(serde_json::to_value(SnapshotResult {
                        snapshot_id: ws.snapshot_id().clone(),
                        value: details,
                    })?),
                    None => ResponseBody::Error(ServiceError::new(
                        ErrorCode::UnavailableObject,
                        "node not found",
                    )),
                }
            }
            RequestBody::ResolveObject(req) => {
                let ws = self.workspace.as_ref().ok_or_else(no_workspace)?;
                let parsed = parse_repository_id(&req.repository).and_then(|repository_id| {
                    parse_git_oid(&req.oid).map(|oid| (repository_id, oid))
                });
                match parsed {
                    Ok((repository_id, oid)) => {
                        match resolve_git_commit(ws, repository_id, &oid)? {
                            Some(commit) => {
                                ResponseBody::Ok(serde_json::to_value(SnapshotResult {
                                    snapshot_id: ws.snapshot_id().clone(),
                                    value: resolved_object_from_commit(&commit),
                                })?)
                            }
                            None => ResponseBody::Error(ServiceError::new(
                                ErrorCode::UnavailableObject,
                                "object not found",
                            )),
                        }
                    }
                    Err(msg) => {
                        ResponseBody::Error(ServiceError::new(ErrorCode::InvalidInput, msg))
                    }
                }
            }
            RequestBody::GetFileDiff(req) => {
                let ws = self.workspace.as_mut().ok_or_else(no_workspace)?;
                if req.change.source == FileChangeSource::Agent {
                    ws.ensure_projection_loaded()?;
                }
                match ws.file_diff(&req.change) {
                    Ok(diff) => ResponseBody::Ok(serde_json::to_value(SnapshotResult {
                        snapshot_id: ws.snapshot_id().clone(),
                        value: diff,
                    })?),
                    Err(message) => ResponseBody::Error(ServiceError::new(
                        ErrorCode::UnavailableObject,
                        message,
                    )),
                }
            }
            RequestBody::FindInHistory(req) => {
                // Build the lexical index lazily on first search.
                if self.lexical.is_none() {
                    let ws = self.workspace.as_mut().ok_or_else(no_workspace)?;
                    self.lexical = Some(build_lexical_index(ws)?);
                }
                let lexical = self.lexical.as_ref().ok_or("no index built")?;
                let ws = self.workspace.as_mut().ok_or_else(no_workspace)?;
                let response = lexical.find(ws, &req.query, req.top_k)?;
                ResponseBody::Ok(serde_json::to_value(response)?)
            }
        };
        if reads_sources {
            self.workspace
                .as_ref()
                .ok_or_else(no_workspace)?
                .ensure_sources_current()?;
        }
        Ok(Response { id, body })
    }
}

impl Default for Server {
    fn default() -> Self {
        Self::new()
    }
}

fn no_workspace() -> ServiceError {
    ServiceError::new(ErrorCode::NoWorkspace, "no workspace open")
}
