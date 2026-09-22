use super::*;
use editchain_node::Server;
use editchain_protocol::{Request, ResponseBody};

mod fixture {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/support/codex.rs"
    ));
}

fn view(server: &mut Server, body: Value) -> io::Result<Value> {
    let body = serde_json::from_value(body).map_err(io::Error::other)?;
    match server
        .handle(&Request { id: 1, body })
        .map_err(|error| io::Error::other(error.to_string()))?
        .body
    {
        ResponseBody::Ok(value) => Ok(value),
        ResponseBody::Error(error) => Err(io::Error::other(format!("view: {error:?}"))),
    }
}

fn field<'a>(value: &'a Value, key: &str) -> io::Result<&'a Value> {
    value
        .get(key)
        .ok_or_else(|| io::Error::other(format!("missing {key}")))
}

fn current_rows(server: &mut Server, snapshot: &Value) -> io::Result<Vec<Value>> {
    view(
        server,
        json!({"GetWindow": {
            "snapshot_id": snapshot, "offset":0, "limit":100, "include_layout":false
        }}),
    )?
    .get("rows")
    .and_then(Value::as_array)
    .cloned()
    .ok_or_else(|| io::Error::other("rows"))
}

fn poll(a: &mut Worker, b: &mut Worker) -> io::Result<()> {
    for _ in 0..2 {
        let response = b.ok(&json!({"type":"turn", "bytes":"", "tick":true}))?;
        connect(a, b, opaque(&response)?)?;
        let response = a.ok(&json!({"type":"turn", "bytes":"", "tick":true}))?;
        connect(b, a, opaque(&response)?)?;
    }
    Ok(())
}

fn sync_rows(server: &mut Server, opened: &Value, revision: &mut Value) -> io::Result<Vec<Value>> {
    let update = view(
        server,
        json!({"SyncLive": {
            "epoch":opened.pointer("/live/epoch"), "after_revision":revision, "codex":null
        }}),
    )?;
    *revision = field(&update, "revision")?.clone();
    let snapshot = field(&update, "deltas")?
        .as_array()
        .and_then(|d| d.last())
        .and_then(|d| d.get("snapshot_id"))
        .ok_or_else(|| io::Error::other("received data did not update view"))?;
    current_rows(server, snapshot)
}

#[test]
fn ongoing_codex_session_crosses_cutoff_and_updates_the_peer_view_live() -> io::Result<()> {
    let dir = tempfile::tempdir()?;
    let ar = dir.path().join("sender/.editchain");
    let workspace = dir.path().join("receiver");
    let br = workspace.join(".editchain");
    let ad = dir.path().join("sender-device");
    let bd = dir.path().join("receiver-device");
    fixture::append(&ar, &fixture::occurrence(1, 1, "private earlier message")?)?;
    let mut a = Worker::spawn()?;
    let mut b = Worker::spawn()?;
    let ai = a.ok(&json!({"type":"identity", "device_dir":ad}))?;
    let bi = b.ok(&json!({"type":"identity", "device_dir":bd}))?;
    for (worker, root, identity) in [(&mut a, &ar, &bi), (&mut b, &br, &ai)] {
        let _scope = worker.ok(&json!({"type":"set_scope", "chain_dir":root, "space":"process-space", "backfill":false}))?;
        let _approved = worker.ok(&json!({"type":"approve", "chain_dir":root, "space":"process-space", "certificate":identity.get("certificate")}))?;
    }
    let mut server = Server::new();
    let open_request =
        json!({"OpenLivePaged":{"workspace_path":workspace, "chain_dir":".editchain"}});
    let opened = view(&mut server, open_request.clone())?;
    require(
        current_rows(&mut server, field(&opened, "snapshot_id")?)?.is_empty(),
        "empty receiver",
    )?;
    let _bytes = open(&mut a, &ar, &ad, None)?;
    let initial = open(
        &mut b,
        &br,
        &bd,
        ai.get("certificate").and_then(Value::as_str),
    )?;
    connect(&mut a, &mut b, initial)?;
    let mut revision = json!(0);
    let mut identity = None;
    for (ordinal, incarnation, text, count) in [
        (2, 2, "shared session is visible", 1),
        (3, 3, "another received item stays visible", 2),
        (4, 2, "shared session updated live", 2),
    ] {
        // The relay can deliver raw previews before the occurrence's proof.
        // A later receipt must replace that preview with a verified item,
        // without hiding this session or its previously received items.
        let ops = fixture::occurrence(ordinal, incarnation, text)?;
        let (preview, materialization) = ops
            .split_first()
            .ok_or_else(|| io::Error::other("occurrence fixture"))?;
        fixture::append(&ar, std::slice::from_ref(preview))?;
        poll(&mut a, &mut b)?;
        let preview_rows = sync_rows(&mut server, &opened, &mut revision)?;
        require(
            preview_rows.len() == count + usize::from(ordinal != incarnation),
            "raw preview arrives alongside previously verified items",
        )?;
        if let Some(previous) = &identity {
            require(
                preview_rows
                    .iter()
                    .any(|row| row.get("continuity_key") == Some(previous)),
                "later incomplete receipts do not retract a verified item",
            )?;
        }
        fixture::append(&ar, materialization)?;
        poll(&mut a, &mut b)?;
        let rows = sync_rows(&mut server, &opened, &mut revision)?;
        require(
            rows.len() == count,
            "proof replaces the preview instead of making received items disappear",
        )?;
        let row = rows
            .iter()
            .find(|row| row.get("summary").and_then(Value::as_str) == Some(text))
            .ok_or_else(|| io::Error::other("received revision is not visible"))?;
        require(
            field(row, "group")? == "session:73",
            "the original session groups received work",
        )?;
        if incarnation == 2 {
            if let Some(previous) = &identity {
                require(
                    previous == field(row, "continuity_key")?,
                    "revision keeps the same row identity",
                )?;
            }
            identity = Some(field(row, "continuity_key")?.clone());
        }
    }
    require(
        CanonicalChain::read(&br)?.stats().accepted == 9,
        "only nine post-cutoff records reached receiver",
    )?;
    drop((a, b, server));
    let mut restarted = Server::new();
    let opened = view(&mut restarted, open_request)?;
    let rows = current_rows(&mut restarted, field(&opened, "snapshot_id")?)?;
    require(
        rows.len() == 2
            && rows.iter().any(|row| {
                row.get("summary").and_then(Value::as_str) == Some("shared session updated live")
            }),
        "received session stays visible after reopening",
    )?;
    Ok(())
}
