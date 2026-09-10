//! Shared harness for Codex import integration tests (Unix fake helper).
#![cfg(unix)]
#![expect(
    clippy::unwrap_used,
    clippy::wildcard_enum_match_arm,
    reason = "test harness helpers assert directly on known shapes"
)]

use std::path::{Path, PathBuf};

use editchain_core::op::{ImportOp, OpKind};
use editchain_core::payload::{ContentId, Payload};
use editchain_core::Op;

use editchain_import::codex::{import_codex, CodexDiscoveryRequest, HelperCommand};
use editchain_import::error::ImportError;
use editchain_import::model::{ImportOptions, ImportReport};
use editchain_import::sink::{ContentAddressedBlobSink, MemoryCursorStore, MemoryOpSink};

/// Everything collected by one import run.
#[derive(Debug)]
pub(crate) struct Harness {
    /// Import report.
    pub report: ImportReport,
    /// Ops emitted, in order.
    pub ops: MemoryOpSink,
    /// Blob storage (content-addressed, retrievable by hash).
    pub blobs: ContentAddressedBlobSink,
}

/// Run an import with explicit cursors, returning any error.
pub(crate) fn try_import(
    root: &Path,
    helper: &HelperCommand,
    options: &ImportOptions,
    cursors: &mut MemoryCursorStore,
) -> Result<Harness, ImportError> {
    let mut ops_sink = MemoryOpSink::new();
    let mut blobs_sink = ContentAddressedBlobSink::new();
    let request = CodexDiscoveryRequest {
        repositories: &(),
        workspace_path: PathBuf::from("/workspace"),
        raw_root: root.to_path_buf(),
    };
    let report = import_codex(
        &request,
        options,
        helper,
        &mut ops_sink,
        &mut blobs_sink,
        cursors,
    )?;
    Ok(Harness {
        report,
        ops: ops_sink,
        blobs: blobs_sink,
    })
}

/// Run a full import with default options and fresh cursors.
pub(crate) fn import(root: &Path, helper: &HelperCommand) -> Harness {
    let mut cursors = MemoryCursorStore::new();
    try_import(root, helper, &ImportOptions::default(), &mut cursors).unwrap()
}

/// Run a full import with explicit options and fresh cursors.
pub(crate) fn import_with_options(
    root: &Path,
    helper: &HelperCommand,
    options: &ImportOptions,
) -> Harness {
    let mut cursors = MemoryCursorStore::new();
    try_import(root, helper, options, &mut cursors).unwrap()
}

/// Run a full import with explicit options into an existing cursor store.
pub(crate) fn import_with_options_into(
    root: &Path,
    helper: &HelperCommand,
    options: &ImportOptions,
    cursors: &mut MemoryCursorStore,
) -> Harness {
    try_import(root, helper, options, cursors).unwrap()
}

/// Write a rollout file from complete lines (a trailing newline is added).
pub(crate) fn write_rollout(dir: &Path, name: &str, lines: &[String]) {
    let mut content = String::new();
    for line in lines {
        content.push_str(line);
        content.push('\n');
    }
    let path = dir.join(name);
    std::fs::write(&path, content).unwrap();
}

/// Write an executable fake helper: it runs `awk_program` over its last
/// argument (the rollout path) and writes the projection records to stdout.
pub(crate) fn write_fake_helper(dir: &Path, name: &str, awk_program: &str) -> PathBuf {
    let path = dir.join(name);
    let script =
        format!("#!/bin/sh\nfor last in \"$@\"; do :; done\nawk '{awk_program}' \"$last\"\n");
    std::fs::write(&path, script).unwrap();
    make_executable(&path);
    path
}

/// Write a helper that cats a fixed projection file, ignoring its rollout
/// argument (for byte-exact control over the projection stream).
pub(crate) fn write_fixed_helper(dir: &Path, name: &str, projection: &[u8]) -> PathBuf {
    let proj_path = dir.join(format!("{name}.projection"));
    std::fs::write(&proj_path, projection).unwrap();
    let script = format!("#!/bin/sh\ncat '{}'\n", proj_path.display());
    let path = dir.join(name);
    std::fs::write(&path, script).unwrap();
    make_executable(&path);
    path
}

/// Write a helper that cats a per-file projection stream, dispatching on the
/// rollout path argument.
///
/// Each entry maps a filename suffix pattern to projection bytes; the helper
/// selects the matching projection with a shell `case` pattern. This avoids
/// embedding bridge JSON in shell/awk quoting entirely — callers build the
/// projection bytes with `serde_json` instead.
pub(crate) fn write_dispatching_helper(
    dir: &Path,
    name: &str,
    entries: &[(&str, &[u8])],
) -> PathBuf {
    use std::fmt::Write as _;
    let mut script = String::from("#!/bin/sh\ncase \"$1\" in\n");
    for (index, (pattern, projection)) in entries.iter().enumerate() {
        let proj_path = dir.join(format!("{name}.{index}.projection"));
        std::fs::write(&proj_path, projection).unwrap();
        writeln!(script, "  *{pattern}*) cat '{}';;", proj_path.display()).unwrap();
    }
    writeln!(script, "  *) exit 2;;").unwrap();
    writeln!(script, "esac").unwrap();
    let path = dir.join(name);
    std::fs::write(&path, script).unwrap();
    make_executable(&path);
    path
}

/// Build a robust `HelperCommand` that runs a fake-helper script through the
/// shell interpreter (`sh <script> <prefix args...> <rollout path>`).
///
/// Exec'ing a just-written script file can transiently fail with `ETXTBSY`
/// ("Text file busy") under parallel test execution on some filesystems;
/// invoking `/bin/sh` with the script as an argument never execs the freshly
/// written file while still exercising the same spawn/stdout/exit contract.
pub(crate) fn sh_helper(script: &Path, prefix_args: &[String]) -> HelperCommand {
    let mut args = vec![script.to_string_lossy().into_owned()];
    args.extend(prefix_args.iter().cloned());
    HelperCommand::new("sh", args)
}

fn make_executable(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

/// Make `path` executable (fake helper scripts).
pub(crate) fn chmod_x(path: &Path) {
    make_executable(path);
}

/// The raw bytes stored for an op (inline or spilled to the blob sink).
pub(crate) fn raw_bytes(op: &Op, blobs: &ContentAddressedBlobSink) -> Vec<u8> {
    match &op.kind {
        OpKind::Import(ImportOp { raw_ref, .. }) => match raw_ref {
            Payload::Inline(bytes) => bytes.clone(),
            Payload::Blob(bref) => match bref.id {
                ContentId::Hash256(hash) => blobs.get(&hash).unwrap().to_vec(),
                _ => Vec::new(),
            },
            Payload::Empty => Vec::new(),
        },
        _ => Vec::new(),
    }
}

/// Default awk helper: `session_meta` lines emit bridge session metadata,
/// everything else becomes an `agentMessage` item with content `line-<ordinal>`.
pub(crate) fn messages_awk(thread: &str) -> String {
    format!(
        r#"
{{
  if ($0 ~ /"type":"session_meta"/) {{
    printf "{{\"schemaVersion\":\"editchain-v1\",\"recordType\":\"line\",\"sourcePath\":\"x\",\"sourceOrdinal\":%d,\"decode\":{{\"status\":\"ok\",\"kind\":\"sessionMeta\"}},\"projection\":{{\"changedItems\":[],\"changedTurns\":[],\"removedTurnIds\":[],\"sessionMeta\":{{\"sessionId\":\"s\",\"threadId\":\"{thread}\"}}}}}}\n", NR
    next
  }}
  printf "{{\"schemaVersion\":\"editchain-v1\",\"recordType\":\"line\",\"sourcePath\":\"x\",\"sourceOrdinal\":%d,\"decode\":{{\"status\":\"ok\",\"kind\":\"eventMsg\",\"eventType\":\"agent_message\"}},\"projection\":{{\"changedItems\":[{{\"turnId\":\"turn-1\",\"item\":{{\"kind\":\"agentMessage\",\"id\":\"item-%d\",\"text\":\"line-%d\",\"contentHash\":\"h\"}}}}],\"changedTurns\":[],\"removedTurnIds\":[]}}}}\n", NR, NR, NR
}}
"#
    )
}
