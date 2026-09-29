//! Whole-process glob selection, mixed sources, and exact bulk admission.

use super::{result, run};
use serde_json::json;

#[test]
fn overlapping_globs_keep_nested_source_ids_and_deduplicate_files() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("human");
    std::fs::create_dir_all(root.join("nested")).unwrap();
    for path in [root.join("one.jsonl"), root.join("nested/two.jsonl")] {
        std::fs::write(
            path,
            include_bytes!("../../../editchain-import/tests/fixtures/human/session.jsonl"),
        )
        .unwrap();
    }
    std::fs::write(root.join("nested/unknown.jsonl"), b"unknown raw bytes\n").unwrap();
    std::fs::write(root.join("ignored.txt"), b"not a session\n").unwrap();
    let normal = temp.path().join("normal");
    let bulk = temp.path().join("bulk");
    let base = [
        "import",
        "--provider",
        "human",
        "--input",
        root.to_str().unwrap(),
    ];
    let expected = result(&normal, &base, b"", 3);
    let mut args = base.to_vec();
    args.extend([
        "--glob",
        "**/*.jsonl",
        "--glob",
        "nested/*.jsonl",
        "--progress",
    ]);
    let actual = result(&bulk, &args, b"", 3);
    assert_eq!(actual.get("batches"), Some(&json!(3)));
    assert_eq!(actual.get("files_discovered"), Some(&json!(3)));
    assert_eq!(actual.get("written"), expected.get("written"));
    assert_eq!(
        run(&bulk, &["export"], b"", 0).stdout,
        run(&normal, &["export"], b"", 0).stdout
    );
    assert_eq!(result(&bulk, &args, b"", 0).get("written"), Some(&json!(0)));
    let dry = temp.path().join("dry");
    args.push("--dry-run");
    let preview = result(&dry, &args, b"", 3);
    assert_eq!(preview.as_array().unwrap().len(), 4);
    assert_eq!(
        preview.as_array().unwrap().last().unwrap().get("malformed"),
        actual.get("malformed")
    );
    assert!(!dry.exists());
    let missing = temp.path().join("missing");
    let mut empty = base.to_vec();
    empty.extend(["--glob", "nothing-*.jsonl"]);
    let _result = run(&missing, &empty, b"", 2);
    assert!(!missing.exists());
}

#[test]
fn dry_run_retains_conflicts_and_duplicates_within_and_across_files() {
    let original = include_str!("../../../editchain-import/tests/fixtures/human/session.jsonl");
    let conflicting = original.replace("draft!", "other!");
    for separate_files in [false, true] {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("human");
        std::fs::create_dir(&root).unwrap();
        if separate_files {
            for (name, content) in [
                ("a.jsonl", original),
                ("b.jsonl", conflicting.as_str()),
                ("c.jsonl", conflicting.as_str()),
            ] {
                std::fs::write(root.join(name), content).unwrap();
            }
        } else {
            std::fs::write(
                root.join("session.jsonl"),
                format!("{original}{conflicting}{conflicting}"),
            )
            .unwrap();
        }
        let args = [
            "import",
            "--legacy",
            "--provider",
            "human",
            "--input",
            root.to_str().unwrap(),
            "--bulk",
        ];
        let durable = result(&temp.path().join("durable"), &args, b"", 4);
        assert_eq!(durable.get("conflicts"), Some(&json!(1)));
        assert_eq!(durable.get("duplicates"), Some(&json!(18)));
        let mut preview_args = args.to_vec();
        preview_args.push("--dry-run");
        let dry = temp.path().join("dry");
        let preview = result(&dry, &preview_args, b"", 4);
        let summary = preview.as_array().unwrap().last().unwrap();
        assert_eq!(summary.get("type"), Some(&json!("summary")));
        assert_eq!(summary.get("conflicts"), durable.get("conflicts"));
        assert_eq!(summary.get("duplicates"), durable.get("duplicates"));
        assert_eq!(summary.get("written"), Some(&json!(0)));
        assert!(!dry.exists());
    }
}

#[test]
fn manifest_dry_run_admits_all_sources_together() {
    let temp = tempfile::tempdir().unwrap();
    let original = include_str!("../../../editchain-import/tests/fixtures/human/session.jsonl");
    let conflicting = original.replace("draft!", "other!");
    let mut sources = Vec::new();
    for (name, content) in [
        ("first", original),
        ("conflicting", conflicting.as_str()),
        ("duplicate", conflicting.as_str()),
    ] {
        let root = temp.path().join(name);
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join(format!("{name}.jsonl")), content).unwrap();
        sources.push(json!({"provider":"human", "input":name}));
    }
    let manifest = temp.path().join("sources.json");
    std::fs::write(
        &manifest,
        serde_json::to_vec(&json!({"schema":1,"sources":sources})).unwrap(),
    )
    .unwrap();
    let args = [
        "import",
        "--legacy",
        "--manifest",
        manifest.to_str().unwrap(),
    ];
    let durable = result(&temp.path().join("durable"), &args, b"", 4);
    assert_eq!(durable.get("conflicts"), Some(&json!(1)));
    assert_eq!(durable.get("duplicates"), Some(&json!(18)));
    let mut preview_args = args.to_vec();
    preview_args.extend(["--dry-run", "--output", "jsonl"]);
    let dry = temp.path().join("dry");
    let preview = run(&dry, &preview_args, b"", 4);
    let records: Vec<serde_json::Value> = String::from_utf8(preview.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(records.len(), 4);
    let summary = records.last().unwrap();
    assert_eq!(summary.get("type"), Some(&json!("summary")));
    assert_eq!(summary.get("conflicts"), durable.get("conflicts"));
    assert_eq!(summary.get("duplicates"), durable.get("duplicates"));
    assert_eq!(summary.get("written"), Some(&json!(0)));
    assert!(!dry.exists());
}

#[cfg(unix)]
#[test]
fn one_manifest_imports_all_providers_and_matches_separate_imports() {
    let temp = tempfile::tempdir().unwrap();
    let roots = [
        (
            "claude",
            "session.jsonl",
            include_bytes!("../../../editchain-import/tests/fixtures/claude/session.jsonl")
                .as_slice(),
        ),
        (
            "human",
            "session.jsonl",
            include_bytes!("../../../editchain-import/tests/fixtures/human/session.jsonl")
                .as_slice(),
        ),
        (
            "codex",
            "rollout-contract.jsonl",
            include_bytes!("../../../editchain-import/tests/fixtures/codex/rollout-contract.jsonl")
                .as_slice(),
        ),
    ];
    let projection = temp.path().join("projection.ndjson");
    std::fs::write(
        &projection,
        include_bytes!("../../../editchain-import/tests/fixtures/codex/projection.ndjson"),
    )
    .unwrap();
    let helper = temp.path().join("helper.sh");
    std::fs::write(&helper, "cat \"$1\"\n").unwrap();
    let helper_args = [
        "--codex-helper",
        "sh",
        "--codex-helper-arg",
        helper.to_str().unwrap(),
        "--codex-helper-arg",
        projection.to_str().unwrap(),
    ];
    let normal = temp.path().join("normal");
    let bulk = temp.path().join("bulk");
    let mut sources = Vec::new();
    for (provider, filename, bytes) in roots {
        let root = temp.path().join(provider);
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join(filename), bytes).unwrap();
        let mut args = vec![
            "import",
            "--provider",
            provider,
            "--input",
            root.to_str().unwrap(),
            "--workspace",
            "/workspace",
        ];
        if provider == "codex" {
            args.extend(helper_args);
        }
        let _report = result(&normal, &args, b"", if provider == "codex" { 3 } else { 0 });
        sources.push(json!({"provider":provider, "input":provider, "workspace":"/workspace", "glob":["**/*.jsonl"]}));
    }
    let manifest = temp.path().join("sources.json");
    std::fs::write(
        &manifest,
        serde_json::to_vec(&json!({"schema":1,"sources":sources})).unwrap(),
    )
    .unwrap();
    let mut args = vec!["import", "--manifest", manifest.to_str().unwrap()];
    args.extend(helper_args);
    let report = result(&bulk, &args, b"", 3);
    assert_eq!(report.get("batches"), Some(&json!(3)));
    assert_eq!(report.get("sources").unwrap().as_array().unwrap().len(), 3);
    assert_eq!(
        run(&bulk, &["export"], b"", 0).stdout,
        run(&normal, &["export"], b"", 0).stdout
    );
    assert_eq!(result(&bulk, &args, b"", 0).get("written"), Some(&json!(0)));
}
