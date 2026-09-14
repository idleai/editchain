use super::*;
use editchain_protocol::{CachedRow, ReconcileRowsRequest, ReconciledWindow};

fn reconcile(workspace: &mut LiveWorkspace, known: Vec<CachedRow>) -> ReconciledWindow {
    let request = RequestBody::ReconcileRows(ReconcileRowsRequest {
        snapshot_id: workspace.snapshot_id.clone(),
        keys: Vec::new(),
        anchors: Vec::new(),
        offset: 0,
        before: 0,
        limit: 100,
        known,
    });
    request.validate().unwrap();
    response(workspace, &request).unwrap()
}

fn response(workspace: &mut LiveWorkspace, request: &RequestBody) -> Result<ReconciledWindow> {
    match workspace.handle(request)? {
        ResponseBody::Ok(value) => Ok(serde_json::from_value(value)?),
        ResponseBody::Error(error) => Err(format!("conditional window failed: {error:?}").into()),
    }
}

fn compare_window(
    workspace: &mut LiveWorkspace,
    cache: &mut HashMap<String, (CachedRow, editchain_protocol::HistoryRow)>,
) -> ReconciledWindow {
    let patch = reconcile(
        workspace,
        cache.values().map(|(known, _)| known.clone()).collect(),
    );
    let mut rows = Vec::new();
    for entry in &patch.rows {
        if let Some(content) = &entry.content {
            drop(cache.insert(
                entry.cached.key.clone(),
                (entry.cached.clone(), content.clone()),
            ));
        }
        let (known, row) = cache.get(&entry.cached.key).unwrap();
        assert_eq!(known.version, entry.cached.version);
        rows.push(row.clone());
    }
    let full = window(workspace).unwrap();
    assert_eq!(patch.snapshot_id, full.snapshot_id);
    assert_eq!(patch.total, full.total);
    assert_eq!(
        serde_json::to_value(rows).unwrap(),
        serde_json::to_value(full.rows).unwrap()
    );
    patch
}

#[test]
fn conditional_windows_match_authoritative_rows_across_disclosure_and_graph_updates() {
    let root = tempfile::tempdir().unwrap();
    let mut workspace = fixture(root.path()).unwrap();
    let mut cache = HashMap::new();
    let first = compare_window(&mut workspace, &mut cache);
    assert!(first.rows.iter().all(|entry| entry.content.is_some()));
    let unchanged = compare_window(&mut workspace, &mut cache);
    assert!(unchanged.rows.iter().all(|entry| entry.content.is_none()));
    assert!(
        serde_json::to_vec(&unchanged).unwrap().len() < serde_json::to_vec(&first).unwrap().len()
    );
    for task in [true, false, false, true, true] {
        workspace.toggle_disclosure("item:4", task).unwrap();
        let patch = compare_window(&mut workspace, &mut cache);
        assert!(
            patch.rows.iter().any(|entry| entry.content.is_some()),
            "disclosure changes must not reuse stale presentation"
        );
    }
    let stale = workspace.snapshot_id.clone();
    let added = stage(&mut workspace, 5, Some("item:4"), false).unwrap();
    drop(workspace.connect(&[], vec![added]).unwrap());
    workspace
        .publish(
            Vec::new(),
            Vec::new(),
            editchain_protocol::LiveWork::default(),
        )
        .unwrap();
    let patch = compare_window(&mut workspace, &mut cache);
    assert!(
        patch.rows.iter().any(|entry| entry.content.is_some()),
        "new graph geometry is part of the fingerprint"
    );
    assert!(workspace
        .handle(&RequestBody::ReconcileRows(ReconcileRowsRequest {
            snapshot_id: stale,
            keys: Vec::new(),
            anchors: Vec::new(),
            offset: 0,
            before: 0,
            limit: 1,
            known: Vec::new(),
        }))
        .is_err());
}

#[test]
fn conditional_anchor_lookup_and_false_cache_claims_remain_bounded_and_authoritative() {
    let root = tempfile::tempdir().unwrap();
    let mut workspace = fixture(root.path()).unwrap();
    workspace.toggle_disclosure("item:4", true).unwrap();
    let original = reconcile(&mut workspace, Vec::new());
    let false_known = original
        .rows
        .iter()
        .map(|entry| CachedRow {
            key: entry.cached.key.clone(),
            version: "0".repeat(64),
        })
        .collect();
    assert!(reconcile(&mut workspace, false_known)
        .rows
        .iter()
        .all(|entry| entry.content.is_some()));
    let request = RequestBody::ReconcileRows(ReconcileRowsRequest {
        snapshot_id: workspace.snapshot_id.clone(),
        keys: vec!["missing".into(), "item:2".into()],
        anchors: vec!["missing".into(), "item:2".into()],
        offset: 9999,
        before: 1,
        limit: 3,
        known: original
            .rows
            .into_iter()
            .map(|entry| entry.cached)
            .collect(),
    });
    request.validate().unwrap();
    let patch = response(&mut workspace, &request).unwrap();
    let anchor = patch
        .locations
        .iter()
        .find(|row| row.key == "item:2")
        .unwrap();
    assert_eq!(patch.offset, anchor.row.saturating_sub(1));
    assert_eq!(patch.rows.len(), 3);
    assert!(patch.rows.iter().all(|entry| entry.content.is_none()));
}
