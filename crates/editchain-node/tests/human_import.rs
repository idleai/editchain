//! CLI integration coverage for `editchain import --provider human`.
//!
//! These tests exercise the real binary end to end: a JSONL archive directory
//! is imported into a clean chain, re-imported for idempotence, previewed with
//! `--dry-run`, and rejected when its envelope schema is unsupported.

use blake3 as _;
use clap as _;
use ctrlc as _;
use dirs as _;
use editchain_core as _;
use editchain_git as _;
use editchain_import as _;
use editchain_index as _;
use editchain_node as _;
use editchain_project as _;
use editchain_protocol as _;
use serde as _;
use tantivy as _;

use editchain_store::CanonicalChain;
use serde_json::{json, Value};
use std::path::Path;
use std::process::{Command, Output};

const SESSION: &str = "11111111-1111-4111-8111-111111111111";

fn event(sequence: u64, data: &Value) -> Value {
    json!({
        "schema": 1,
        "session": SESSION,
        "sequence": sequence,
        "time_ms": 1_000_000_u64.saturating_add(sequence),
        "event": data,
    })
}

fn start() -> Value {
    event(
        1,
        &json!({"type":"tracking_started","dwell_ms":2000,"vscode_version":"1.90.0"}),
    )
}

fn document(version: u64) -> Value {
    json!({"id":"buffer-1","uri":"file:///notes.txt","path":"notes.txt","version":version})
}

fn snapshot(sequence: u64, text: &str) -> Value {
    event(
        sequence,
        &json!({"type":"document_snapshot","document":document(1),"text":text}),
    )
}

fn change(sequence: u64, before: &str, after: &str) -> Value {
    event(
        sequence,
        &json!({
            "type":"document_changed",
            "document":document(2),
            "before_version":1,
            "before":before,
            "after":after,
            "reason":null,
            "changes":[{"offset":0,"length":before.encode_utf16().count(),"text":after}],
        }),
    )
}

fn human_edit(sequence: u64, change: u64) -> Value {
    event(
        sequence,
        &json!({"type":"human_edit","change":change,"signal":"editor_input"}),
    )
}

fn saved(sequence: u64) -> Value {
    event(
        sequence,
        &json!({"type":"document_saved","document":document(2)}),
    )
}

fn stopped(sequence: u64) -> Value {
    event(sequence, &json!({"type":"tracking_stopped"}))
}

fn session_events(before: &str, after: &str) -> Vec<Value> {
    vec![
        start(),
        snapshot(2, before),
        change(3, before, after),
        human_edit(4, 3),
        saved(5),
        stopped(6),
    ]
}

fn archive(workspace: &Path, events: &[Value]) -> String {
    let mut body = String::new();
    for event in events {
        let record = json!({
            "format": "editchain-human-history",
            "schema": 1,
            "workspace_path": workspace.to_string_lossy(),
            "event": event,
        });
        body.push_str(&record.to_string());
        body.push('\n');
    }
    body
}

fn run_cli(
    sessions: &Path,
    workspace: &Path,
    chain: &Path,
    dry_run: bool,
) -> std::io::Result<Output> {
    let mut args: Vec<std::ffi::OsString> = ["import", "--provider", "human", "--sessions-dir"]
        .iter()
        .map(Into::into)
        .collect();
    args.push(sessions.into());
    args.push("--workspace".into());
    args.push(workspace.into());
    args.push("--chain".into());
    args.push(chain.into());
    if dry_run {
        args.push("--dry-run".into());
    }
    Command::new(env!("CARGO_BIN_EXE_editchain"))
        .args(args)
        .output()
}

fn accepted_ops(chain: &Path) -> std::io::Result<usize> {
    Ok(CanonicalChain::read(chain)?.into_located_ops().count())
}

#[test]
fn cli_imports_archived_human_history_and_replays_idempotently() {
    let tmp = tempfile::tempdir().unwrap();
    let workspace = tmp.path().join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    let sessions = tmp.path().join("sessions");
    std::fs::create_dir_all(&sessions).unwrap();
    let chain = tmp.path().join("chain");
    std::fs::write(
        sessions.join("2026-09-21-session-0001.jsonl"),
        archive(&workspace, &session_events("AI\n", "AIh\n")),
    )
    .unwrap();

    let first = run_cli(&sessions, &workspace, &chain, false).unwrap();
    assert!(
        first.status.success(),
        "{}",
        String::from_utf8_lossy(&first.stderr)
    );
    let written = accepted_ops(&chain).unwrap();
    assert!(written > 0, "the CLI must admit the archived source events");

    let second = run_cli(&sessions, &workspace, &chain, false).unwrap();
    assert!(
        second.status.success(),
        "{}",
        String::from_utf8_lossy(&second.stderr)
    );
    assert_eq!(
        accepted_ops(&chain).unwrap(),
        written,
        "re-importing the same archive must not append duplicates"
    );
}

#[test]
fn cli_dry_run_leaves_the_chain_absent() {
    let tmp = tempfile::tempdir().unwrap();
    let workspace = tmp.path().join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    let sessions = tmp.path().join("sessions");
    std::fs::create_dir_all(&sessions).unwrap();
    let chain = tmp.path().join("chain");
    std::fs::write(
        sessions.join("2026-09-21-session-0001.jsonl"),
        archive(&workspace, &session_events("dry\n", "dry!\n")),
    )
    .unwrap();

    let output = run_cli(&sessions, &workspace, &chain, true).unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!chain.exists(), "a dry run must not create the chain");
}

#[test]
fn cli_rejects_an_unsupported_archive_schema() {
    let tmp = tempfile::tempdir().unwrap();
    let workspace = tmp.path().join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    let sessions = tmp.path().join("sessions");
    std::fs::create_dir_all(&sessions).unwrap();
    let chain = tmp.path().join("chain");
    let record = json!({
        "format": "editchain-human-history",
        "schema": 2,
        "workspace_path": workspace.to_string_lossy(),
        "event": start(),
    });
    std::fs::write(
        sessions.join("2026-09-21-session-0001.jsonl"),
        format!("{record}\n"),
    )
    .unwrap();

    let output = run_cli(&sessions, &workspace, &chain, false).unwrap();
    assert!(!output.status.success(), "an unsupported schema must fail");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("unsupported human history schema"),
        "{stderr}"
    );
    assert!(
        !chain.exists(),
        "a rejected archive must not create a chain"
    );
}
