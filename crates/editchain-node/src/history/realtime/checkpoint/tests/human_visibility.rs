use super::*;
use editchain_core::{
    human::{HumanWorkKind, HumanWorkRecord},
    ActorId, Clock, ImportOp, OpKind, ParentSet, Payload, ScopeRef, Tags,
};
use std::sync::Arc;

#[test]
fn version_four_opens_with_cached_exposure_removed_and_other_rows_retained() {
    let root = tempfile::tempdir().unwrap();
    let mut workspace = fixture(root.path()).unwrap();
    let id = OpId::new(NodeId(1), 0, 4);
    let work = HumanWorkRecord {
        source: "vscode.work".into(),
        schema: 1,
        session: "legacy-window".into(),
        turn: 1,
        source_event: OpId::new(NodeId(2), 0, 4),
        kind: HumanWorkKind::Exposure,
        path: Some("a.rs".into()),
        before: None,
        after: None,
        git: None,
        context_observed_ms: None,
        summary: "Brief exposure · a.rs · 500 ms".into(),
    };
    let op = Arc::new(Op {
        id,
        parents: ParentSet::One(OpId::new(NodeId(1), 0, 3)),
        actor: ActorId(1),
        clock: Clock::UnixMs(4),
        scope: ScopeRef::None,
        tags: Tags::HUMAN | Tags::IMPORT,
        kind: OpKind::Import(ImportOp {
            raw_ref: Payload::Inline(serde_json::to_vec(&work).unwrap()),
            raw_hash: None,
        }),
    });
    drop(
        workspace
            .projection
            .apply_shared(vec![Arc::clone(&op)], &[]),
    );
    let mut input = workspace.inputs.get("item:4").unwrap().clone();
    input.operations = vec![op];
    drop(workspace.inputs.insert("item:4".into(), input));

    // Retain an old, already-presented exposure row in the version-4 cache.
    // Source reducers and the other physical rows are deliberately unchanged.
    let order = workspace.orders.get("item:4").unwrap().clone();
    let block = workspace.blocks.get(&order).unwrap().clone();
    let mut rows = workspace.rows.rows(&block).unwrap();
    let row = rows.first_mut().expect("cached legacy exposure row");
    row.kind = "exposure".into();
    row.summary.clone_from(&work.summary);
    let old = workspace
        .rows
        .put(LiveBlock {
            meta: block.meta.clone(),
            rows,
        })
        .unwrap();
    drop(workspace.blocks.insert(
        order,
        old,
        editchain_protocol::rank::Measure {
            expanded: 2,
            visible: 1,
        },
    ));
    workspace.rows.flush().unwrap();
    let mut saved = workspace.saved();
    saved.version = 4;
    drop(
        workspace
            .checkpoint_store
            .commit::<_, Saved>(&saved)
            .unwrap(),
    );
    drop(workspace);

    let mut resumed = LiveWorkspace::open_paged(&request(root.path())).unwrap();
    assert!(
        resumed.reused_checkpoint,
        "opening upgrades the existing cache without prepare-view"
    );
    assert!(!resumed.orders.contains_key("item:4"));
    for sequence in 0..4 {
        assert!(resumed.orders.contains_key(&format!("item:{sequence}")));
    }
    assert!(window(&mut resumed)
        .unwrap()
        .rows
        .iter()
        .all(|row| row.kind != "exposure"));
    assert_eq!(load(&resumed.checkpoint_store).unwrap().unwrap().version, 5);
    drop(resumed);
    let mut reopened = LiveWorkspace::open_paged(&request(root.path())).unwrap();
    assert!(reopened.reused_checkpoint);
    assert!(window(&mut reopened)
        .unwrap()
        .rows
        .iter()
        .all(|row| row.kind != "exposure"));
}
