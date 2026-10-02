//! Concurrent-process tests for subscriptions, cancellation and pipe transport.

use std::{
    io::{self, BufRead, Read},
    process::Child,
    sync::mpsc,
    time::Duration,
};

use super::*;

struct Running(Child);

impl Drop for Running {
    fn drop(&mut self) {
        let _killed = self.0.kill();
        let _waited = self.0.wait();
    }
}

fn event(receiver: &mpsc::Receiver<Value>, child: &mut Running, phase: &str) -> Value {
    receiver
        .recv_timeout(Duration::from_secs(10))
        .map_err(|error| {
            let _killed = child.0.kill();
            let status = child.0.wait().unwrap();
            let mut diagnostic = String::new();
            let _read = child
                .0
                .stderr
                .take()
                .unwrap()
                .read_to_string(&mut diagnostic)
                .unwrap();
            format!("waiting for {phase}: {error}; child {status}: {diagnostic}")
        })
        .unwrap()
}

#[test]
fn subscription_observes_lower_ids_conflict_retractions_and_late_content() {
    let temp = tempfile::tempdir().unwrap();
    let engine = Engine::open(temp.path()).unwrap();
    let bytes = b"late";
    let reference = BlobRef {
        id: ContentId::Hash256(*blake3::hash(bytes).as_bytes()),
        len: 4,
    };
    append(temp.path(), &message(100, Payload::Blob(reference)));
    let mut child = Running(
        command(temp.path())
            .args([
                "follow",
                "--output",
                "jsonl",
                "--no-initial",
                "--poll-ms",
                "5",
                "--count",
                "3",
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    let stdout = child.0.stdout.take().unwrap();
    let (sender, receiver) = mpsc::channel();
    let reader = std::thread::spawn(move || {
        for line in io::BufReader::new(stdout).lines() {
            let parsed: Value = serde_json::from_str(&line.unwrap()).unwrap();
            if sender.send(parsed).is_err() {
                break;
            }
        }
    });
    assert_eq!(
        event(&receiver, &mut child, "ready").get("type"),
        Some(&json!("ready"))
    );
    let _busy = run(temp.path(), &["history"], b"", 5);
    append(
        temp.path(),
        &message(1, Payload::Inline(b"older id".to_vec())),
    );
    let added = event(&receiver, &mut child, "added record");
    assert_eq!(added.get("added").unwrap().as_array().unwrap().len(), 1);
    let _blob = engine.store_blob(bytes).unwrap();
    let content = event(&receiver, &mut child, "late content");
    assert_eq!(
        content
            .get("content_changed")
            .unwrap()
            .as_array()
            .unwrap()
            .len(),
        1
    );
    let _conflict = run(
        temp.path(),
        &["append"],
        &serde_json::to_vec(&message(1, Payload::Inline(b"conflict".to_vec()))).unwrap(),
        4,
    );
    let removed = event(&receiver, &mut child, "conflict retraction");
    assert_eq!(removed.get("removed").unwrap().as_array().unwrap().len(), 1);
    assert!(
        child.0.wait().unwrap().success(),
        "bounded subscription completes"
    );
    reader.join().unwrap();
}

#[test]
fn two_stdio_processes_exchange_real_framed_replication() {
    let temp = tempfile::tempdir().unwrap();
    let left = temp.path().join("left");
    let right = temp.path().join("right");
    let local = Engine::open(&left).unwrap();
    let remote = Engine::open(&right).unwrap();
    let reference = local.store_blob(&vec![b'x'; 200_000]).unwrap();
    append(&left, &message(1, Payload::Blob(reference)));
    append(&right, &message(2, Payload::Inline(b"remote".to_vec())));
    let start = |chain: &Path| {
        Running(
            command(chain)
                .args([
                    "replicate",
                    "--stdio",
                    "--namespace",
                    "pipe-test",
                    "--share-all",
                    "--timeout-ms",
                    "5000",
                ])
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap(),
        )
    };
    let mut a = start(&left);
    let mut b = start(&right);
    let mut a_in = a.0.stdin.take().unwrap();
    let mut a_out = a.0.stdout.take().unwrap();
    let mut b_in = b.0.stdin.take().unwrap();
    let mut b_out = b.0.stdout.take().unwrap();
    let to_b = std::thread::spawn(move || forward(&mut a_out, &mut b_in));
    let to_a = std::thread::spawn(move || forward(&mut b_out, &mut a_in));
    let a_status = a.0.wait().unwrap();
    let b_status = b.0.wait().unwrap();
    assert!(
        a_status.success() && b_status.success(),
        "both stdio peers completed: {a_status} {b_status}"
    );
    assert!(
        to_a.join().unwrap().is_ok(),
        "left receives the complete stream"
    );
    assert!(
        to_b.join().unwrap().is_ok(),
        "right receives the complete stream"
    );
    assert_eq!(
        local.snapshot().unwrap().evidence(),
        remote.snapshot().unwrap().evidence()
    );
    assert_eq!(
        remote.resolve_blob(&reference).unwrap(),
        editchain_engine::BlobResolution::Found(vec![b'x'; 200_000])
    );
}

fn forward(reader: &mut impl Read, writer: &mut impl Write) -> io::Result<()> {
    let mut bytes = [0; 997];
    loop {
        let count = reader.read(&mut bytes)?;
        if count == 0 {
            return Ok(());
        }
        if let Some(bytes) = bytes.get(..count) {
            writer.write_all(bytes)?;
        }
    }
}

#[cfg(unix)]
#[test]
fn interrupted_json_subscription_closes_its_array_and_returns_130() {
    let temp = tempfile::tempdir().unwrap();
    let _engine = Engine::open(temp.path()).unwrap();
    let mut child = Running(
        command(temp.path())
            .args(["follow", "--no-initial", "--poll-ms", "10"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    let stdout = child.0.stdout.take().unwrap();
    let (sender, receiver) = mpsc::channel();
    let reader = std::thread::spawn(move || {
        let mut body = String::new();
        for line in io::BufReader::new(stdout).lines() {
            let line = line.unwrap();
            if line.contains("ready") {
                sender.send(()).unwrap();
            }
            body.push_str(&line);
            body.push('\n');
        }
        body
    });
    receiver.recv_timeout(Duration::from_secs(10)).unwrap();
    assert!(
        Command::new("kill")
            .args(["-INT", &child.0.id().to_string()])
            .status()
            .unwrap()
            .success(),
        "signal delivered"
    );
    assert_eq!(child.0.wait().unwrap().code(), Some(130));
    let value: Value = serde_json::from_str(&reader.join().unwrap()).unwrap();
    assert_eq!(value.as_array().unwrap().len(), 1);
}

#[test]
fn early_eof_and_broken_replication_output_cannot_report_success() {
    let temp = tempfile::tempdir().unwrap();
    let _engine = Engine::open(temp.path()).unwrap();
    let args = [
        "replicate",
        "--stdio",
        "--namespace",
        "pipe-test",
        "--share-all",
        "--timeout-ms",
        "1000",
    ];
    let _eof = run(temp.path(), &args, b"", 3);
    let mut child = Running(
        command(temp.path())
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    drop(child.0.stdout.take());
    drop(child.0.stdin.take());
    assert!(
        !child.0.wait().unwrap().success(),
        "broken transport cannot be a successful result pipe close"
    );
}

#[test]
fn closed_append_result_pipe_reports_partial_durable_work() {
    let temp = tempfile::tempdir().unwrap();
    let engine = Engine::open(temp.path()).unwrap();
    let mut child = command(temp.path())
        .arg("append")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    drop(child.stdout.take());
    child
        .stdin
        .take()
        .unwrap()
        .write_all(&serde_json::to_vec(&message(1, Payload::Empty)).unwrap())
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("partially durable"),
        "retry guidance is explicit"
    );
    assert_eq!(
        engine.snapshot().unwrap().stats().accepted,
        1,
        "the acknowledged storage boundary is retained"
    );
}
