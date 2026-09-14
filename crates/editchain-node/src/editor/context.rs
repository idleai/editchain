//! Read-only Git observation, independent of the agent orchestration loop.

use editchain_core::human::HumanGitContext;
use editchain_git::{open_repository, RepositoryCatalog};
use editchain_protocol::OpenRequest;
use std::{
    path::Path,
    time::{SystemTime, UNIX_EPOCH},
};

pub(crate) fn observe_context(
    request: &OpenRequest,
) -> Result<serde_json::Value, Box<dyn std::error::Error>> {
    let catalog = RepositoryCatalog::discover(Path::new(&request.workspace_path))?;
    let mut repositories = Vec::new();
    for discovery in catalog.entries() {
        let Some(root) = &discovery.worktree_root else {
            continue;
        };
        let handle = open_repository(discovery)?;
        let head = handle.repo.head_id().ok().map(|oid| oid.to_string());
        repositories.push(HumanGitContext {
            repository: discovery.id.0.to_string(),
            root: root.to_string_lossy().into_owned(),
            head,
        });
    }
    if repositories.len() > 64 {
        return Err("workspace exceeds the 64-repository editor context limit".into());
    }
    let observed_ms = u64::try_from(SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis())?;
    Ok(
        serde_json::json!({"observed_ms": observed_ms, "workspace_path": request.workspace_path, "repositories": repositories}),
    )
}
