//! Import-to-view contracts owned by the presentation consumer.
use editchain_engine as _;
use editchain_index as _;
use serde as _;
use tokio as _;

#[cfg(unix)]
#[path = "imports/copies.rs"]
mod copies;

#[path = "imports/claude_import.rs"]
mod claude_import;
#[cfg(unix)]
#[path = "imports/codex_import.rs"]
mod codex_import;
#[cfg(unix)]
#[path = "imports/codex_provider_contract.rs"]
mod codex_provider_contract;
#[cfg(unix)]
#[path = "imports/codex_subagent_projection.rs"]
mod codex_subagent_projection;
#[cfg(unix)]
#[path = "imports/common.rs"]
mod common;
#[path = "imports/subagent_real.rs"]
mod subagent_real;
