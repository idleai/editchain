//! Import agent sessions (Claude Code or Codex) into the edit chain.

mod claude_session_git;
mod git_commit_links;
mod persistence;

use std::path::{Path, PathBuf};

use super::Provider;
use crate::segment::SegmentStore;
use editchain_core::Op;
use editchain_import::batch::ImportBatch;
use editchain_import::codex::{import_codex, CodexDiscoveryRequest, HelperCommand};
use editchain_import::import::import_claude_code;
use editchain_import::model::{DiscoveryRequest, ImportOptions};
use editchain_import::sink::{
    BlobSink, CursorStore, FsBlobSink, FsCursorStore, MemoryBlobSink, MemoryCursorStore,
};
use editchain_store::CanonicalChain;

/// Default Codex helper program, resolved from `PATH` when unconfigured.
const DEFAULT_CODEX_HELPER: &str = "codex-session-exporter";

/// Run the `import` command.
///
/// For `--provider codex`, `workspace` scopes the import: rollouts whose
/// projected `sessionMeta.cwd` is equal to or within the workspace are
/// imported, rollouts with an explicitly foreign cwd are skipped before any
/// ops or cursors are written, and rollouts without a classifiable cwd are
/// imported for compatibility (see `CodexDiscoveryRequest`).
///
/// # Errors
///
/// Returns an error if session files cannot be discovered or imported, or if
/// Codex-only helper options are used with the Claude provider.
#[expect(
    clippy::needless_pass_by_value,
    clippy::print_stdout,
    clippy::too_many_arguments,
    reason = "CLI command; strings consumed by design"
)]
pub fn run(
    sessions_dir: String,
    workspace: String,
    chain: String,
    dry_run: bool,
    provider: Provider,
    codex_helper: Option<String>,
    codex_helper_args: Vec<String>,
) -> Result<(), Box<dyn std::error::Error>> {
    check_codex_only_helper_args(provider, codex_helper.as_deref(), &codex_helper_args)?;

    let chain_path = PathBuf::from(&chain);
    // Hold the writer lock before reading cursors, capturing sources, or
    // reconciling evidence, and through the final checkpoint commit.
    let mut store = if dry_run {
        None
    } else {
        Some(SegmentStore::open(&chain_path)?)
    };
    let (mut blobs, mut cursors) = storage_sinks(&chain_path, dry_run)?;

    let mut batch = ImportBatch::capture(cursors.as_ref(), |ops, pending| match provider {
        Provider::Claude => {
            let sessions_path = if sessions_dir.is_empty() {
                claude_auto_detect_sessions_dir()
                    .map_err(|error| editchain_import::ImportError::OpSink(error.to_string()))?
            } else {
                PathBuf::from(&sessions_dir)
            };
            let request = DiscoveryRequest {
                workspace_path: PathBuf::from(&workspace),
                sessions_dir: sessions_path,
                chain_dir: chain_path.clone(),
            };

            let options = ImportOptions::default();
            import_claude_code(&request, &options, ops, blobs.as_mut(), pending)
        }
        Provider::Codex => {
            let raw_root = if sessions_dir.is_empty() {
                codex_default_sessions_dir()
                    .map_err(|error| editchain_import::ImportError::OpSink(error.to_string()))?
            } else {
                PathBuf::from(&sessions_dir)
            };
            let request = CodexDiscoveryRequest {
                workspace_path: PathBuf::from(&workspace),
                raw_root,
            };
            let helper = codex_helper_command(codex_helper, codex_helper_args);

            let options = ImportOptions::default();
            import_codex(&request, &options, &helper, ops, blobs.as_mut(), pending)
        }
    })?;

    if let Some(store) = store.as_mut() {
        let mut session_base_links = 0usize;
        let mut produced_links = 0usize;
        match reconcile_git_links(
            provider,
            Path::new(&workspace),
            &chain_path,
            batch.operations(),
        ) {
            Ok(GitReconciliation {
                base_links,
                produced_links: commit_links,
            }) => {
                session_base_links = base_links.len();
                produced_links = commit_links.len();
                batch.extend_operations(base_links);
                batch.extend_operations(commit_links);
            }
            Err(error) => {
                println!("Git-link reconciliation failed (session import will continue): {error}");
            }
        }
        let outcome = batch.persist(&mut persistence::ImportWriter { store }, cursors.as_mut())?;
        println!("Import complete:");
        println!("{}", capture_report(&outcome.report));
        if provider == Provider::Claude {
            println!("  Claude session Git anchors: {session_base_links}");
        }
        println!("  Produced commit links: {produced_links}");
        println!(
            "  Written operation variants: {}",
            outcome.admission.written
        );
        println!("  Exact duplicates: {}", outcome.admission.duplicates);
        println!(
            "  New conflicting variants: {}",
            outcome.admission.conflicts
        );

        // Render rows, expansion offsets, graph geometry, and operation lookup
        // locations are deterministic derived data. Build them only after the
        // append and cursor checkpoint are durable; a failed build leaves the
        // authoritative import intact and can be retried with `prepare-view`.
        let workspace_path = PathBuf::from(&workspace);
        let snapshot_chain_path = if chain_path.is_absolute() {
            chain_path.clone()
        } else {
            std::env::current_dir()?.join(&chain_path)
        };
        let snapshot = editchain_vscode_service::prepare_render_snapshot(
            &workspace_path,
            &snapshot_chain_path,
        );
        match snapshot {
            Ok(snapshot) => println!(
                "Render snapshot {}: {} rows at {}",
                if snapshot.reused { "reused" } else { "generated" },
                snapshot.rows,
                snapshot.path.display()
            ),
            Err(error) => println!(
                "Render snapshot preparation failed (import remains durable; run prepare-view to retry): {error}"
            ),
        }
    } else {
        println!("Import preview:");
        println!("{}", capture_report(batch.report()));
        println!("\n--- Dry run: first 5 ops ---");
        for op in batch.operations().iter().take(5) {
            let json = serde_json::to_string(op)?;
            println!("{json}");
        }
    }

    Ok(())
}

fn capture_report(report: &editchain_import::ImportReport) -> String {
    format!(
        "  Files discovered: {}\n  Files processed: {}\n  Captured raw ops: {}\n  Derived ops: {}\n  Malformed source records: {}",
        report.files_discovered, report.files_processed, report.raw_ops,
        report.normalized_ops, report.malformed,
    )
}

/// Reconcile all Git relationships that depend on live repository evidence.
fn reconcile_git_links(
    provider: Provider,
    workspace: &Path,
    chain: &Path,
    imported: &[Op],
) -> Result<GitReconciliation, Box<dyn std::error::Error>> {
    let mut all_ops = reconciliation_ops(chain, imported)?;
    let blob_reader = FsBlobSink::open_read_only(chain.join("blobs"))?;
    let base_links = if provider == Provider::Claude {
        claude_session_git::derive_session_base_links(workspace, &all_ops, blob_reader.as_ref())?
    } else {
        Vec::new()
    };
    all_ops.extend(base_links.iter().cloned());
    let produced_links =
        git_commit_links::derive_produced_commit_links(workspace, &all_ops, blob_reader.as_ref())?;
    Ok(GitReconciliation {
        base_links,
        produced_links,
    })
}

struct GitReconciliation {
    base_links: Vec<Op>,
    produced_links: Vec<Op>,
}

/// Read accepted chain operations and merge the current import batch for
/// produced-commit reconciliation.
///
/// Exact replay duplicates collapse by ID. Conflicting same-ID records are
/// excluded entirely, matching the authoritative reader's quarantine rule, so
/// malformed history can never become relationship evidence.
fn reconciliation_ops(chain: &Path, imported: &[Op]) -> Result<Vec<Op>, std::io::Error> {
    let mut corpus = CanonicalChain::read(chain)?;
    for op in imported {
        let _: editchain_core::Admission = corpus.insert(op.clone())?;
    }
    Ok(corpus.into_located_ops().map(|(op, _)| op).collect())
}

/// Build the blob and cursor sinks for an import run.
///
/// Durable filesystem stores live under the chain directory (`blobs/` and
/// `cursors/` subdirectories), so spilled payloads and per-source read cursors
/// survive restarts. `--dry-run` keeps the in-memory stores so a dry run
/// validates an import without persisting anything.
///
/// # Errors
///
/// Returns an error if a durable store directory cannot be created.
#[expect(
    clippy::type_complexity,
    reason = "the two trait-object boxes are the small, explicit storage_sinks contract"
)]
fn storage_sinks(
    chain: &Path,
    dry_run: bool,
) -> Result<(Box<dyn BlobSink>, Box<dyn CursorStore>), Box<dyn std::error::Error>> {
    if dry_run {
        Ok((
            Box::new(MemoryBlobSink::new()),
            Box::new(MemoryCursorStore::new()),
        ))
    } else {
        Ok((
            Box::new(FsBlobSink::new(chain.join("blobs"))?),
            Box::new(FsCursorStore::new(chain.join("cursors"))?),
        ))
    }
}

/// Auto-detect the Claude Code project sessions directory for the current
/// working directory (`~/.claude/projects/<encoded-cwd>`).
///
/// # Errors
///
/// Returns an error if the current directory or home directory cannot be
/// resolved.
fn claude_auto_detect_sessions_dir() -> Result<PathBuf, Box<dyn std::error::Error>> {
    let cwd = std::env::current_dir()?;
    let cwd_str = cwd.to_string_lossy().to_string();
    let encoded = cwd_str.replace(['/', '.'], "-");
    let home = dirs::home_dir().ok_or("no home directory")?;
    Ok(home.join(".claude").join("projects").join(encoded))
}

/// Resolve the default Codex sessions root (`~/.codex/sessions`).
///
/// # Errors
///
/// Returns an error if the home directory cannot be resolved.
fn codex_default_sessions_dir() -> Result<PathBuf, Box<dyn std::error::Error>> {
    let home = dirs::home_dir().ok_or("no home directory")?;
    Ok(home.join(".codex").join("sessions"))
}

/// Reject Codex-only helper options when the provider is Claude.
fn check_codex_only_helper_args(
    provider: Provider,
    codex_helper: Option<&str>,
    codex_helper_args: &[String],
) -> Result<(), String> {
    if provider == Provider::Claude && (codex_helper.is_some() || !codex_helper_args.is_empty()) {
        return Err("--codex-helper and --codex-helper-arg require --provider codex".to_string());
    }
    Ok(())
}

/// Build the Codex helper command, defaulting the program to
/// [`DEFAULT_CODEX_HELPER`] when unconfigured. The rollout path is appended by
/// the importer; the process is spawned directly with no shell.
#[must_use]
fn codex_helper_command(
    codex_helper: Option<String>,
    codex_helper_args: Vec<String>,
) -> HelperCommand {
    HelperCommand::new(
        codex_helper.unwrap_or_else(|| DEFAULT_CODEX_HELPER.to_string()),
        codex_helper_args,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::{Cli, Commands};
    use clap::Parser;
    use editchain_codec::frame::{decode_op, encode_op};
    use editchain_codec::page::Page;
    use std::collections::HashSet;

    struct ImportArgs {
        sessions_dir: String,
        workspace: String,
        chain: String,
        dry_run: bool,
        provider: Provider,
        codex_helper: Option<String>,
        codex_helper_arg: Vec<String>,
    }

    fn import_args(args: &[&str]) -> Option<ImportArgs> {
        let mut tokens = vec!["editchain", "import"];
        tokens.extend_from_slice(args);
        let cli = Cli::try_parse_from(tokens).ok()?;
        match cli.command {
            Commands::Import {
                sessions_dir,
                workspace,
                chain,
                dry_run,
                provider,
                codex_helper,
                codex_helper_arg,
            } => Some(ImportArgs {
                sessions_dir,
                workspace,
                chain,
                dry_run,
                provider,
                codex_helper,
                codex_helper_arg,
            }),
            Commands::PrepareView { .. } => None,
        }
    }

    #[test]
    fn import_defaults_to_claude_provider() {
        let args = import_args(&[]).unwrap();
        assert_eq!(args.sessions_dir, "");
        assert_eq!(args.workspace, ".");
        assert_eq!(args.chain, ".editchain");
        assert!(!args.dry_run);
        assert_eq!(args.provider, Provider::Claude);
        assert!(args.codex_helper.is_none());
        assert!(args.codex_helper_arg.is_empty());
    }

    #[test]
    fn import_parses_codex_provider() {
        let args = import_args(&["--provider", "codex"]).unwrap();
        assert_eq!(args.provider, Provider::Codex);
    }

    #[test]
    fn import_rejects_unknown_provider() {
        let err = Cli::try_parse_from(["editchain", "import", "--provider", "bogus"])
            .expect_err("unknown provider must fail to parse");
        assert!(err.to_string().contains("bogus"));
    }

    #[test]
    fn import_parses_repeatable_codex_helper_args() {
        let args = import_args(&[
            "--provider",
            "codex",
            "--codex-helper-arg",
            "rollout-export",
            "--codex-helper-arg",
            "--format",
            "--codex-helper-arg",
            "editchain-v1",
        ])
        .unwrap();
        assert_eq!(
            args.codex_helper_arg,
            vec!["rollout-export", "--format", "editchain-v1"]
        );
    }

    #[test]
    fn import_parses_codex_helper_program() {
        let args = import_args(&["--codex-helper", "/usr/local/bin/codex-export"]).unwrap();
        assert_eq!(
            args.codex_helper.as_deref(),
            Some("/usr/local/bin/codex-export")
        );
    }

    #[test]
    fn claude_rejects_codex_only_helper_options() {
        let args = import_args(&["--codex-helper-arg", "rollout-export"]).unwrap();
        assert_eq!(args.provider, Provider::Claude);
        let err = check_codex_only_helper_args(
            args.provider,
            args.codex_helper.as_deref(),
            &args.codex_helper_arg,
        )
        .expect_err("codex-only options must be rejected for claude");
        assert!(err.contains("--provider codex"));

        let args = import_args(&["--codex-helper", "codex-session-exporter"]).unwrap();
        assert!(check_codex_only_helper_args(
            args.provider,
            args.codex_helper.as_deref(),
            &args.codex_helper_arg,
        )
        .is_err());
    }

    #[test]
    fn claude_accepts_missing_helper_options() {
        let args = import_args(&[]).unwrap();
        assert!(check_codex_only_helper_args(
            args.provider,
            args.codex_helper.as_deref(),
            &args.codex_helper_arg,
        )
        .is_ok());
    }

    #[test]
    fn claude_auto_detects_projects_dir() {
        let dir = claude_auto_detect_sessions_dir().unwrap();
        let home = dirs::home_dir().unwrap();
        let cwd = std::env::current_dir().unwrap();
        let encoded = cwd.to_string_lossy().replace(['/', '.'], "-");
        assert_eq!(dir, home.join(".claude").join("projects").join(encoded));
    }

    #[test]
    fn codex_defaults_to_home_codex_sessions() {
        let dir = codex_default_sessions_dir().unwrap();
        let home = dirs::home_dir().unwrap();
        assert_eq!(dir, home.join(".codex").join("sessions"));
    }

    #[test]
    fn codex_helper_command_defaults_program() {
        let cmd = codex_helper_command(None, Vec::new());
        assert_eq!(cmd.program, "codex-session-exporter");
        assert!(cmd.args.is_empty());
    }

    #[test]
    fn codex_helper_command_uses_configured_program_and_args() {
        let cmd = codex_helper_command(
            Some("my-helper".to_string()),
            vec!["--flag".to_string(), "value".to_string()],
        );
        assert_eq!(cmd.program, "my-helper");
        assert_eq!(cmd.args, vec!["--flag", "value"]);
    }

    #[test]
    fn writer_lock_precedes_source_discovery_and_cursor_store_creation() {
        let dir = tempfile::tempdir().unwrap();
        let chain = dir.path().join("chain");
        let held = SegmentStore::open(&chain).unwrap();
        let error = run(
            dir.path()
                .join("missing-sources")
                .to_string_lossy()
                .into_owned(),
            dir.path().to_string_lossy().into_owned(),
            chain.to_string_lossy().into_owned(),
            false,
            Provider::Claude,
            None,
            Vec::new(),
        )
        .unwrap_err();
        assert_eq!(
            error.downcast_ref::<std::io::Error>().unwrap().kind(),
            std::io::ErrorKind::WouldBlock
        );
        assert!(!chain.join("blobs").exists());
        assert!(!chain.join("cursors").exists());
        drop(held);
    }

    #[test]
    fn storage_sinks_are_durable_unless_dry_run() {
        use editchain_import::sink::CursorValue;

        let chain = tempfile::tempdir().unwrap();

        // Dry run: in-memory stores; nothing is persisted under the chain.
        let (mut dry_blobs, mut dry_cursors) = storage_sinks(chain.path(), true).unwrap();
        dry_blobs.store_blob(&[1, 2, 3]).unwrap();
        dry_cursors
            .set_cursor(
                "/sessions/x.jsonl",
                &CursorValue {
                    accepted_generation: None,
                    file_size: 1,
                    byte_offset: 1,
                    ops_emitted: 1,
                    content_hash: [0u8; 32],
                    content_hash_version: 0,
                    source_node: None,
                    normalization_version: 0,
                    session_title_hash: None,
                },
            )
            .unwrap();
        assert!(!chain.path().join("blobs").exists());
        assert!(!chain.path().join("cursors").exists());

        // Real run: durable stores under `<chain>/blobs` and `<chain>/cursors`
        // that survive a fresh set of sink instances (process restart).
        let (mut blobs, mut cursors) = storage_sinks(chain.path(), false).unwrap();
        blobs.store_blob(&[1, 2, 3]).unwrap();
        cursors
            .set_cursor(
                "/sessions/x.jsonl",
                &CursorValue {
                    accepted_generation: None,
                    file_size: 1,
                    byte_offset: 1,
                    ops_emitted: 1,
                    content_hash: [0u8; 32],
                    content_hash_version: 0,
                    source_node: None,
                    normalization_version: 0,
                    session_title_hash: None,
                },
            )
            .unwrap();
        // The command commits cursors only after the chain append succeeds.
        cursors.commit().unwrap();
        assert!(chain.path().join("blobs").is_dir());
        assert!(chain.path().join("cursors").is_dir());

        let reopened_blobs = FsBlobSink::new(chain.path().join("blobs")).unwrap();
        assert_eq!(reopened_blobs.len().unwrap(), 1);
        let reopened_cursors = FsCursorStore::new(chain.path().join("cursors")).unwrap();
        assert_eq!(
            reopened_cursors
                .get_cursor("/sessions/x.jsonl")
                .unwrap()
                .unwrap()
                .ops_emitted,
            1
        );
    }

    fn write_rollout_lines(path: &Path, lines: &[String]) {
        let mut content = String::new();
        for line in lines {
            content.push_str(line);
            content.push('\n');
        }
        std::fs::write(path, content).unwrap();
    }

    fn session_meta_line() -> String {
        "{\"timestamp\":\"2026-08-26T12:00:00.000Z\",\"type\":\"session_meta\",\"payload\":{\"session_id\":\"s\",\"id\":\"thread-1\",\"timestamp\":\"t\",\"cwd\":\"/tmp\"}}"
            .to_string()
    }

    fn event_line(token: &str) -> String {
        format!(
            "{{\"timestamp\":\"2026-08-26T12:00:01.000Z\",\"type\":\"event_msg\",\"payload\":{{\"type\":\"agent_message\",\"token\":\"{token}\",\"session_id\":\"parent-session\"}}}}"
        )
    }

    /// Write a fake Codex helper that projects every line of its rollout via
    /// awk (`session_meta` lines emit bridge metadata, everything else becomes
    /// an `agentMessage` item). Invoked as `sh <script> <rollout>`; a plain
    /// script file works because it runs as an `sh` argument.
    fn write_codex_test_helper(dir: &Path, name: &str) -> PathBuf {
        let awk = r#"
{
  if ($0 ~ /"type":"session_meta"/) {
    printf "{\"schemaVersion\":\"editchain-v1\",\"recordType\":\"line\",\"sourcePath\":\"x\",\"sourceOrdinal\":%d,\"decode\":{\"status\":\"ok\",\"kind\":\"sessionMeta\"},\"projection\":{\"changedItems\":[],\"changedTurns\":[],\"removedTurnIds\":[],\"sessionMeta\":{\"sessionId\":\"s\",\"threadId\":\"thread-1\"}}}\n", NR
    next
  }
  printf "{\"schemaVersion\":\"editchain-v1\",\"recordType\":\"line\",\"sourcePath\":\"x\",\"sourceOrdinal\":%d,\"decode\":{\"status\":\"ok\",\"kind\":\"eventMsg\",\"eventType\":\"agent_message\"},\"projection\":{\"changedItems\":[{\"turnId\":\"turn-1\",\"item\":{\"kind\":\"agentMessage\",\"id\":\"item-%d\",\"text\":\"line-%d\",\"contentHash\":\"h\"}}],\"changedTurns\":[],\"removedTurnIds\":[]}}\n", NR, NR, NR
}
"#;
        let script = format!("#!/bin/sh\nfor last in \"$@\"; do :; done\nawk '{awk}' \"$last\"\n");
        let path = dir.join(name);
        std::fs::write(&path, script).unwrap();
        path
    }

    /// Decode every op currently stored in the chain, in append order.
    fn read_chain_ops(chain: &Path) -> Vec<Op> {
        let store = SegmentStore::open(chain).unwrap();
        let mut ops = Vec::new();
        for page in store.read_all().unwrap() {
            for record in page.records {
                ops.push(decode_op(&record.data).unwrap());
            }
        }
        ops
    }

    #[test]
    fn reconciliation_and_viewer_share_conflict_admission() {
        use editchain_core::{
            ActorId, Clock, MessageOp, NodeId, OpId, OpKind, ParentSet, Payload, ScopeRef, Tags,
        };

        let candidate = |seq, text: &[u8]| Op {
            id: OpId::new(NodeId(1), 0, seq),
            parents: ParentSet::None,
            actor: ActorId(1),
            clock: Clock::UnixMs(1),
            scope: ScopeRef::None,
            tags: Tags::MESSAGE,
            kind: OpKind::Message(MessageOp {
                content: Payload::Inline(text.to_vec()),
                content_type: Payload::Empty,
            }),
        };
        let stable = candidate(2, b"stable");
        for (first, conflicting) in [
            (b"one".as_slice(), b"two".as_slice()),
            (b"two".as_slice(), b"one".as_slice()),
        ] {
            let dir = tempfile::tempdir().unwrap();
            let chain = dir.path().join(".editchain");
            let first = candidate(1, first);
            let conflicting = candidate(1, conflicting);
            let mut store = SegmentStore::open(&chain).unwrap();
            let mut page = Page::new(0);
            for op in [&first, &first, &stable] {
                page.add_record(0, encode_op(op).unwrap());
            }
            store.append_page(&page).unwrap();
            assert_eq!(
                reconciliation_ops(&chain, &[conflicting.clone(), first.clone()]).unwrap(),
                vec![stable.clone()]
            );
            let mut next_page = Page::new(1);
            for op in [&conflicting, &first, &conflicting] {
                next_page.add_record(0, encode_op(op).unwrap());
            }
            store.append_page(&next_page).unwrap();
            let reconciled = reconciliation_ops(&chain, &[]).unwrap();
            let viewer = editchain_vscode_service::Workspace::open(
                dir.path().to_str().unwrap(),
                ".editchain",
            )
            .unwrap();
            assert_eq!(reconciled, vec![stable.clone()]);
            assert_eq!(viewer.projection().ops(), reconciled);
            assert_eq!(viewer.diagnostics.chain.quarantined, 2);
            assert_eq!(viewer.diagnostics.chain.duplicates, 3);
        }
    }

    #[test]
    fn codex_truncate_to_empty_commits_generation_and_regrowth_uses_new_boot() {
        let dir = tempfile::tempdir().unwrap();
        let sessions = dir.path().join("sessions");
        std::fs::create_dir_all(&sessions).unwrap();
        let rollout = sessions.join("rollout-1.jsonl");
        let cursor_key =
            editchain_import::cursor::canonical_source_key("codex", &sessions, &rollout).unwrap();
        let chain = dir.path().join("chain");

        let helper = write_codex_test_helper(dir.path(), "helper.sh");
        let helper_args = vec![helper.to_string_lossy().into_owned()];
        let import = |sessions: &str, chain: &str| {
            run(
                sessions.to_string(),
                "/workspace".to_string(),
                chain.to_string(),
                false,
                Provider::Codex,
                Some("sh".to_string()),
                helper_args.clone(),
            )
            .unwrap();
        };
        let sessions_str = sessions.to_string_lossy().into_owned();
        let chain_str = chain.to_string_lossy().into_owned();

        // Nonempty source: boot-0 ops land in the chain and the cursor is
        // committed by the command.
        write_rollout_lines(&rollout, &[session_meta_line(), event_line("first")]);
        import(&sessions_str, &chain_str);
        let first = read_chain_ops(&chain);
        assert!(!first.is_empty(), "first import must emit ops");
        assert!(first.iter().all(|op| op.id.boot == 0));

        // Truncate to empty and import: zero ops are emitted, but the staged
        // generation bump (1) and empty-file cursor must still be committed
        // even though there is no page to append.
        std::fs::write(&rollout, b"").unwrap();
        import(&sessions_str, &chain_str);
        assert_eq!(read_chain_ops(&chain).len(), first.len());
        let reopened = FsCursorStore::new(chain.join("cursors")).unwrap();
        assert_eq!(reopened.get_generation(&cursor_key).unwrap(), 1);
        assert_eq!(
            reopened.get_cursor(&cursor_key).unwrap().unwrap().file_size,
            0
        );

        // Regrow with different, larger content: the regrowth is an append
        // from the empty cursor, so its ops continue at boot 1 — never a
        // boot-0 continuation from a stale pre-rewrite cursor.
        write_rollout_lines(
            &rollout,
            &[
                session_meta_line(),
                event_line("regrown-1"),
                event_line("regrown-2"),
            ],
        );
        import(&sessions_str, &chain_str);
        let all = read_chain_ops(&chain);
        let regrown: Vec<Op> = all.iter().filter(|op| op.id.boot == 1).cloned().collect();
        assert!(!regrown.is_empty(), "regrown content must emit boot-1 ops");
        assert_eq!(all.len(), first.len() + regrown.len());
        let boot0: HashSet<_> = all
            .iter()
            .filter(|op| op.id.boot == 0)
            .map(|op| op.id)
            .collect();
        for op in &regrown {
            assert!(
                !boot0.contains(&op.id),
                "boot-1 ids never collide with boot-0 ids"
            );
        }
    }
}
