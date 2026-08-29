//! Import agent sessions (Claude Code or Codex) into the edit chain.

use std::path::{Path, PathBuf};

use super::Provider;
use crate::segment::SegmentStore;
use editchain_codec::frame::encode_op;
use editchain_codec::page::Page;
use editchain_import::codex::{import_codex, CodexDiscoveryRequest, HelperCommand};
use editchain_import::import::import_claude_code;
use editchain_import::model::{DiscoveryRequest, ImportOptions};
use editchain_import::sink::{
    BlobSink, CursorStore, FsBlobSink, FsCursorStore, MemoryBlobSink, MemoryCursorStore,
    MemoryOpSink,
};

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
    let mut ops_sink = MemoryOpSink::new();
    let (mut blobs, mut cursors) = storage_sinks(&chain_path, dry_run)?;

    let report = match provider {
        Provider::Claude => {
            let sessions_path = if sessions_dir.is_empty() {
                claude_auto_detect_sessions_dir()?
            } else {
                PathBuf::from(&sessions_dir)
            };
            let request = DiscoveryRequest {
                workspace_path: PathBuf::from(&workspace),
                sessions_dir: sessions_path,
                chain_dir: chain_path.clone(),
            };

            let options = ImportOptions::default();
            import_claude_code(
                &request,
                &options,
                &mut ops_sink,
                blobs.as_mut(),
                cursors.as_mut(),
            )?
        }
        Provider::Codex => {
            let raw_root = if sessions_dir.is_empty() {
                codex_default_sessions_dir()?
            } else {
                PathBuf::from(&sessions_dir)
            };
            let request = CodexDiscoveryRequest {
                workspace_path: PathBuf::from(&workspace),
                raw_root,
            };
            let helper = codex_helper_command(codex_helper, codex_helper_args);

            let options = ImportOptions::default();
            import_codex(
                &request,
                &options,
                &helper,
                &mut ops_sink,
                blobs.as_mut(),
                cursors.as_mut(),
            )?
        }
    };

    println!("Import complete:");
    println!("  Files discovered: {}", report.files_discovered);
    println!("  Files processed: {}", report.files_processed);
    println!("  Raw ops: {}", report.raw_ops);
    println!("  Normalized ops: {}", report.normalized_ops);
    println!("  Duplicates: {}", report.duplicates);
    println!("  Malformed: {}", report.malformed);

    if !dry_run {
        if !ops_sink.ops.is_empty() {
            // Write ops to the chain store.
            let mut store = SegmentStore::open(&chain_path)?;
            let mut page = Page::new(0);
            for op in &ops_sink.ops {
                let encoded = encode_op(op)?;
                page.add_record(0, encoded);
            }
            store.append_page(&page)?;
            println!("Wrote {} operations to chain.", ops_sink.ops.len());
        }
        // `append_page` has flushed the segment (and, for a brand-new segment
        // file, its directory entry) to stable storage; only now make the
        // staged cursors durable, so a failed encode/append never advances a
        // cursor past operations that were not stored. The ordering is
        // intentionally at-least-once, not atomic: a crash between the append
        // and this commit re-imports the same sources and replays identical
        // ops, which `Open` canonicalizes through the core `OpSet`. The commit
        // also runs when no ops were emitted: a fresh or truncated-to-empty
        // source stages a cursor (and possibly a generation bump) with zero
        // ops, and that checkpoint must survive the restart so a later
        // regrowth continues at the correct boot generation.
        cursors.commit()?;
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
    use editchain_codec::frame::decode_op;
    use editchain_core::Op;

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
            Commands::Init { .. }
            | Commands::Append { .. }
            | Commands::Dump { .. }
            | Commands::Merge { .. }
            | Commands::Search { .. }
            | Commands::Tail { .. }
            | Commands::Retrieve { .. } => None,
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
                    file_size: 1,
                    byte_offset: 1,
                    ops_emitted: 1,
                    content_hash: [0u8; 32],
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
                    file_size: 1,
                    byte_offset: 1,
                    ops_emitted: 1,
                    content_hash: [0u8; 32],
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
    fn codex_truncate_to_empty_commits_generation_and_regrowth_uses_new_boot() {
        let dir = tempfile::tempdir().unwrap();
        let sessions = dir.path().join("sessions");
        std::fs::create_dir_all(&sessions).unwrap();
        let rollout = sessions.join("rollout-1.jsonl");
        let cursor_key = rollout.to_string_lossy().into_owned();
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
        let boot0: std::collections::HashSet<_> = all
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
