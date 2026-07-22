//! Import Claude Code sessions into the edit chain.

use std::path::PathBuf;

use crate::segment::SegmentStore;
use editchain_codec::frame::encode_op;
use editchain_codec::page::Page;
use editchain_import::import::import_claude_code;
use editchain_import::model::{DiscoveryRequest, ImportOptions};
use editchain_import::sink::{MemoryBlobSink, MemoryCursorStore, MemoryOpSink};

/// Run the `import` command.
///
/// # Errors
///
/// Returns an error if session files cannot be discovered or imported.
#[expect(
    clippy::needless_pass_by_value,
    clippy::print_stdout,
    reason = "CLI command; strings consumed by design"
)]
pub fn run(
    sessions_dir: String,
    workspace: String,
    chain: String,
    dry_run: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let sessions_path = if sessions_dir.is_empty() {
        // Try to auto-detect the Claude Code project directory.
        let cwd = std::env::current_dir()?;
        let cwd_str = cwd.to_string_lossy().to_string();
        let encoded = cwd_str.replace(['/', '.'], "-");
        let home = dirs::home_dir().ok_or("no home directory")?;
        home.join(".claude").join("projects").join(encoded)
    } else {
        PathBuf::from(&sessions_dir)
    };

    let request = DiscoveryRequest {
        workspace_path: PathBuf::from(&workspace),
        sessions_dir: sessions_path,
        chain_dir: PathBuf::from(&chain),
    };

    let options = ImportOptions::default();
    let mut ops_sink = MemoryOpSink::new();
    let mut blobs_sink = MemoryBlobSink::new();
    let mut cursors = MemoryCursorStore::new();

    let report = import_claude_code(
        &request,
        &options,
        &mut ops_sink,
        &mut blobs_sink,
        &mut cursors,
    )?;

    println!("Import complete:");
    println!("  Files discovered: {}", report.files_discovered);
    println!("  Files processed: {}", report.files_processed);
    println!("  Raw ops: {}", report.raw_ops);
    println!("  Normalized ops: {}", report.normalized_ops);
    println!("  Duplicates: {}", report.duplicates);
    println!("  Malformed: {}", report.malformed);

    if !dry_run && !ops_sink.ops.is_empty() {
        // Write ops to the chain store.
        let mut store = SegmentStore::open(PathBuf::from(&chain))?;
        let mut page = Page::new(0);
        for op in &ops_sink.ops {
            let encoded = encode_op(op)?;
            page.add_record(0, encoded);
        }
        store.append_page(&page)?;
        println!("Wrote {} operations to chain.", ops_sink.ops.len());
    }

    if dry_run {
        println!("\n--- Dry run: first 5 ops ---");
        for op in ops_sink.ops.iter().take(5) {
            let json = serde_json::to_string(op)?;
            println!("{json}");
        }
    }

    Ok(())
}
