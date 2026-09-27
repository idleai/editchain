//! Copied archives retain the existing viewer row count.

use std::path::{Path, PathBuf};

use editchain_import::batch::ImportBatch;
use editchain_import::codex::{CodexDiscoveryRequest, HelperCommand};
use editchain_import::{
    capture_import, DiscoveryRequest, FsBlobSink, ImportOptions, ImportSource, MemoryCursorStore,
};
use editchain_project::HistoryProjection;

type Result<T = ()> = std::result::Result<T, Box<dyn std::error::Error>>;

fn same_count(actual: usize, expected: usize, message: &str) -> Result {
    if actual != expected {
        return Err(format!("{message}: expected {expected}, got {actual}").into());
    }
    Ok(())
}

fn capture(root: &Path, codex: bool) -> Result<ImportBatch> {
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("../editchain-import/tests/fixtures");
    let helper = HelperCommand::new(
        "sh",
        vec![
            "-c".into(),
            "awk 'NR == FNR {n++; next} FNR <= n' \"$2\" \"$1\"".into(),
            "recorded-exporter".into(),
            fixtures
                .join("codex/projection.ndjson")
                .to_string_lossy()
                .into_owned(),
        ],
    );
    let claude = DiscoveryRequest {
        workspace_path: "/workspace".into(),
        sessions_dir: root.into(),
        chain_dir: PathBuf::new(),
    };
    let request = CodexDiscoveryRequest {
        workspace_path: "/workspace".into(),
        raw_root: root.into(),
        selected_paths: Vec::new(),
        repositories: &(),
    };
    let source = if codex {
        ImportSource::Codex {
            request: &request,
            helper: &helper,
        }
    } else {
        ImportSource::Claude(&claude)
    };
    Ok(capture_import(
        source,
        &ImportOptions::default(),
        &mut FsBlobSink::new(root.join("blobs"))?,
        &MemoryCursorStore::new(),
    )?)
}

fn copied_views(codex: bool, bytes: &[u8]) -> Result<(HistoryProjection, HistoryProjection)> {
    let temp = tempfile::tempdir()?;
    let (original, copy) = if codex {
        ("rollout-contract.jsonl", "rollout-copy.jsonl")
    } else {
        ("session.jsonl", "copied.jsonl")
    };
    std::fs::write(temp.path().join(original), bytes)?;
    let initial = HistoryProjection::from_ops(capture(temp.path(), codex)?.operations().to_vec());
    std::fs::write(temp.path().join(copy), bytes)?;
    let copied = HistoryProjection::from_ops(capture(temp.path(), codex)?.operations().to_vec());
    Ok((initial, copied))
}

#[test]
fn recorded_claude_copy_does_not_duplicate_visible_history() -> Result {
    let (initial, copied) = copied_views(
        false,
        include_bytes!("../../../editchain-import/tests/fixtures/claude/session.jsonl"),
    )?;
    same_count(
        copied.nodes().len(),
        initial.nodes().len(),
        "copied Claude events retain the existing visible history",
    )?;
    Ok(())
}

#[test]
fn recorded_codex_copy_does_not_duplicate_visible_revisions() -> Result {
    let (initial, copied) = copied_views(
        true,
        include_bytes!("../../../editchain-import/tests/fixtures/codex/rollout-contract.jsonl"),
    )?;
    same_count(
        initial.codex_logical_items().len(),
        3,
        "fixture contains three logical items",
    )?;
    same_count(
        copied.codex_logical_items().len(),
        initial.codex_logical_items().len(),
        "viewer consumes the shared reconciliation",
    )?;
    same_count(
        copied.nodes().len(),
        initial.nodes().len(),
        "copied revisions retain the existing visible history",
    )?;
    Ok(())
}
