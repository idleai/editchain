//! CLI cancellation across the helper and durable checkpoint boundary.
#![cfg(unix)]

use blake3 as _;
use clap as _;
use ctrlc as _;
use dirs as _;
use editchain_core as _;
use editchain_git as _;
use editchain_node as _;
use editchain_project as _;
use editchain_protocol as _;
use serde as _;
use serde_json as _;
use tantivy as _;

use editchain_import::sink::CursorStore;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

#[test]
fn interrupt_cancels_helper_and_leaves_no_accepted_source() {
    let dir = tempfile::tempdir().unwrap();
    let sessions = dir.path().join("sessions");
    let chain = dir.path().join("chain");
    std::fs::create_dir(&sessions).unwrap();
    let source = sessions.join("rollout-1.jsonl");
    std::fs::write(&source, "{}\n").unwrap();
    let script = dir.path().join("helper.sh");
    let child_pid = dir.path().join("child.pid");
    std::fs::write(&script, "sleep 30 &\nprintf '%s' \"$!\" > \"$1\"\nwait\n").unwrap();
    let mut import = Command::new(env!("CARGO_BIN_EXE_editchain"))
        .args(["import", "--provider", "codex", "--sessions-dir"])
        .arg(&sessions)
        .arg("--workspace")
        .arg(dir.path())
        .arg("--chain")
        .arg(&chain)
        .args(["--codex-helper", "sh", "--codex-helper-arg"])
        .arg(&script)
        .arg("--codex-helper-arg")
        .arg(&child_pid)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let started = Instant::now();
    while !child_pid.exists() && started.elapsed() < Duration::from_secs(5) {
        std::thread::sleep(Duration::from_millis(10));
    }
    let ready = child_pid.exists();
    if ready {
        assert!(Command::new("kill")
            .args(["-INT", &import.id().to_string()])
            .status()
            .unwrap()
            .success());
    } else {
        import.kill().unwrap();
    }
    let interrupted = Instant::now();
    let (status, timely) = loop {
        if let Some(status) = import.try_wait().unwrap() {
            break (status, true);
        }
        if interrupted.elapsed() > Duration::from_secs(5) {
            import.kill().unwrap();
            break (import.wait().unwrap(), false);
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    assert!(
        ready,
        "helper must have started before interrupting the CLI"
    );
    assert!(timely, "interrupted import must exit promptly");
    assert!(!status.success());
    let pid = std::fs::read_to_string(&child_pid).unwrap();
    let observed = Command::new("ps")
        .args(["-o", "stat=", "-p", pid.trim()])
        .output()
        .unwrap();
    let state = String::from_utf8_lossy(&observed.stdout);
    assert!(state.trim().is_empty() || state.trim().starts_with('Z'));
    let cursors = editchain_import::FsCursorStore::new(chain.join("cursors")).unwrap();
    let key = editchain_import::cursor::canonical_source_key("codex", &sessions, &source).unwrap();
    assert!(cursors.get_cursor(&key).unwrap().is_none());
    assert!(cursors.get_reservation(&key).unwrap().is_none());
    assert!(editchain_store::CanonicalChain::read(&chain)
        .unwrap()
        .into_located_ops()
        .next()
        .is_none());
}
