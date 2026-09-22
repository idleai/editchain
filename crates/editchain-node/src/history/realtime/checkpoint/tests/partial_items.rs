use super::*;

mod codex {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/support/codex.rs"
    ));
}

#[test]
fn version_eight_reveals_received_session_items_without_reimport_or_backfill() {
    reveals_cached_item(8, 2);
}

#[test]
fn version_ten_reveals_items_whose_incarnations_were_excluded() {
    reveals_cached_item(10, 1);
}

fn reveals_cached_item(version: u64, incarnation: u64) {
    let root = tempfile::tempdir().unwrap();
    codex::append(
        &root.path().join(".editchain"),
        &codex::occurrence(2, incarnation, "received after cutoff").unwrap(),
    )
    .unwrap();
    let canonical = human_edits::canonical(root.path());
    let mut workspace = LiveWorkspace::open_paged(&request(root.path())).unwrap();
    assert_eq!(window(&mut workspace).unwrap().total, 1);

    // Version 8 retained the valid item reducer, but hid its presentation while
    // waiting for an earlier source prefix that consent deliberately excluded.
    let changes = workspace.projection.refresh_partial_items();
    let keys = changes.upserts.into_keys().collect();
    let (removed, blocks) = workspace
        .apply_blocks(editchain_project::live::LiveChanges {
            removed: keys,
            ..Default::default()
        })
        .unwrap();
    drop(workspace.connect(&removed, blocks).unwrap());
    assert_eq!(
        window(&mut workspace).unwrap().total,
        0,
        "legacy hidden-session cache"
    );
    workspace.rows.flush().unwrap();
    let mut saved = workspace.saved();
    saved.version = version;
    drop(
        workspace
            .checkpoint_store
            .commit::<_, Saved>(&saved)
            .unwrap(),
    );
    drop(workspace);

    let mut reopened = LiveWorkspace::open_paged(&request(root.path())).unwrap();
    assert!(reopened.reused_checkpoint);
    let rows = window(&mut reopened).unwrap().rows;
    assert_eq!(
        rows.len(),
        1,
        "previous receipts appear without any new import"
    );
    assert!(rows
        .iter()
        .any(|row| row.summary == "received after cutoff" && row.group == "session:73"));
    assert_eq!(
        human_edits::canonical(root.path()),
        canonical,
        "canonical history stays untouched"
    );
    assert_eq!(
        load(&reopened.checkpoint_store)
            .unwrap()
            .map(|saved| saved.version),
        Some(VERSION)
    );
    drop(reopened);
    let mut resumed = LiveWorkspace::open_paged(&request(root.path())).unwrap();
    assert_eq!(
        serde_json::to_value(window(&mut resumed).unwrap().rows).unwrap(),
        serde_json::to_value(rows).unwrap()
    );
}
