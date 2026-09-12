use super::*;

fn block(key: &str, summary: &str) -> Result<LiveBlock> {
    Ok(serde_json::from_value(serde_json::json!({
        "meta": {"key": key, "sort_time": 0, "row_count": 1, "spans": [], "node_key": key},
        "rows": [{"summary": summary, "timestamp_ms": 0, "group": "session",
            "node_key": key, "parents": [], "is_submodule": false}]
    }))?)
}

#[test]
fn paging_flushes_pending_writes_and_preserves_other_rows_during_reuse() -> Result<()> {
    let mut store = RowStore::new()?;
    let mut first = store.put(block("first", &"a".repeat(64 * 1024))?)?;
    let second = store.put(block("second", "untouched")?)?;
    let end = store.end;
    for length in [3, 8192, 64, 32 * 1024, 0, 100] {
        let summary = "b".repeat(length);
        store.remove(&first);
        first = store.put(block("first", &summary)?)?;
        equal(store.end, end)?;
        equal(
            store.rows(&second)?.first().map(|row| row.summary.as_str()),
            Some("untouched"),
        )?;
        equal(
            store.rows(&first)?.first().map(|row| row.summary.as_str()),
            Some(summary.as_str()),
        )?;
    }
    let third = store.put(block("third", "written after paging")?)?;
    equal(
        store.rows(&third)?.first().map(|row| row.summary.as_str()),
        Some("written after paging"),
    )?;
    equal(
        store.rows(&second)?.first().map(|row| row.summary.as_str()),
        Some("untouched"),
    )?;
    Ok(())
}

fn equal<T: std::fmt::Debug + PartialEq + Copy>(actual: T, expected: T) -> Result<()> {
    if actual == expected {
        Ok(())
    } else {
        Err(format!("expected {expected:?}, got {actual:?}").into())
    }
}
