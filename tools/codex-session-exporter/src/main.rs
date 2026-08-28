use std::io::Write;
use std::path::Path;
use std::process::ExitCode;

use codex_session_exporter::bridge::process_file;
use codex_session_exporter::bridge::FileOptions;
use codex_session_exporter::schema::SCHEMA_VERSION;

const SCHEMA_DOCUMENT: &str = r#"{
  "schemaVersion": "editchain-v1",
  "recordTypes": ["line", "final"],
  "decodeStatuses": ["ok", "error"],
  "decodeKinds": ["sessionMeta", "eventMsg", "responseItem", "interAgentCommunication", "interAgentCommunicationMetadata", "compacted", "turnContext", "worldState", "securityRiskScore", "unknownJson", "sessionSummary"],
  "itemKinds": ["userMessage", "agentMessage", "reasoning", "plan", "commandExecution", "fileChange", "toolCall", "collabToolCall", "subAgentActivity", "contextCompaction", "hookPrompt", "reviewMode", "imageView", "opaque"],
  "dedupIdentity": ["sourcePath", "turnId", "itemId"],
  "notes": [
    "Every line record carries a 1-based physical sourceOrdinal plus optional Codex rolloutOrdinal.",
    "changedItems are lifecycle upserts keyed by deterministic item id; the --final record is a deduplicated per-turn snapshot with firstSeenOrdinal/lastSeenOrdinal/seenLineCount including response-derived items.",
    "Response-item messages and reasoning are projected as stable sourcePath-scoped semantic items with full text using Codex typed ids (msg_/rs_/fc_/fco_) or a deterministic response-<ordinal> fallback; legacy event_msg/response_item echoes fold to one logical item via typed ids plus turn context plus content correlation (contentHash (FNV-1a 64) is one signal, never the sole dedup key).",
    "Unknown or future Codex item kinds degrade to kind=opaque with a diagnostic typeName; unknown line shapes produce decode.status=error with the raw type discriminant preserved as kind.",
    "The projection carries typed content for neutral Message/Tool/Command/File/Reflection/Note ops: message text, reasoning summaries and content, command output and secret-redacted command strings, file diffs, tool arguments/results/errors, and plan/review/inter-agent text. Only genuinely non-text payloads (encrypted content, image/audio data URIs, session base instructions as raw values) remain length/presence-only.",
    "Files are projected independently; scope item identity by sourcePath to keep physical files distinct."
  ]
}"#;

fn print_usage() {
    eprintln!(
        "codex-session-exporter {ver}\n\
         Isolated Codex rollout JSONL -> editchain-v1 NDJSON projection bridge.\n\
         \n\
         USAGE:\n\
         \x20 codex-session-exporter [OPTIONS] <ROLLOUT_JSONL>...\n\
         \n\
         OPTIONS:\n\
         \x20 --final     emit a per-file final-session snapshot record after each file\n\
         \x20 --schema    print the editchain-v1 schema summary as JSON and exit\n\
         \x20 -h, --help  show this help\n\
         \x20 -V, --version  show version",
        ver = env!("CARGO_PKG_VERSION")
    );
}

fn main() -> ExitCode {
    let mut paths: Vec<String> = Vec::new();
    let mut emit_final = false;
    let mut show_schema = false;
    let mut only_paths = false;

    for arg in std::env::args().skip(1) {
        match arg.as_str() {
            "--" => only_paths = true,
            "--final" => emit_final = true,
            "--schema" => show_schema = true,
            "-h" | "--help" => {
                print_usage();
                return ExitCode::SUCCESS;
            }
            "-V" | "--version" => {
                println!("codex-session-exporter {}", env!("CARGO_PKG_VERSION"));
                return ExitCode::SUCCESS;
            }
            _ if arg.starts_with('-') && !only_paths => {
                eprintln!("codex-session-exporter: unknown option: {arg}");
                print_usage();
                return ExitCode::FAILURE;
            }
            _ => paths.push(arg),
        }
    }

    if show_schema {
        println!("{SCHEMA_DOCUMENT}");
        return ExitCode::SUCCESS;
    }
    if paths.is_empty() {
        eprintln!("codex-session-exporter: no rollout path given (schema: {SCHEMA_VERSION})");
        print_usage();
        return ExitCode::FAILURE;
    }

    let stdout = std::io::stdout();
    let mut out = std::io::BufWriter::new(stdout.lock());
    let opts = FileOptions { emit_final };
    let mut exit = ExitCode::SUCCESS;

    for path in &paths {
        match process_file(Path::new(path), &opts, &mut out) {
            Ok(result) => {
                if result.failed_lines > 0 {
                    eprintln!(
                        "codex-session-exporter: {}: {} of {} line(s) failed to decode",
                        result.path, result.failed_lines, result.physical_lines
                    );
                }
            }
            Err(err) => {
                eprintln!("codex-session-exporter: {path}: {err}");
                exit = ExitCode::FAILURE;
            }
        }
    }
    if out.flush().is_err() {
        eprintln!("codex-session-exporter: failed to flush stdout");
        exit = ExitCode::FAILURE;
    }
    exit
}
