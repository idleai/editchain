use super::{boundary, Map, OrderedMap, Storage};
use serde::{Deserialize, Serialize};
use std::{
    fs::OpenOptions,
    io::{self, Write as _},
};

#[derive(Debug, Default, Serialize, Deserialize)]
struct State {
    values: Map<u64, Vec<u8>>,
    order: OrderedMap<u64, u64>,
}

#[test]
fn reopen_and_one_edit_reuse_old_pages_and_ignore_unpublished_suffix() -> io::Result<()> {
    boundary(|| {
        let dir = tempfile::tempdir()?;
        let storage = Storage::open(dir.path())?;
        let mut state = State::default();
        for key in 0..10_000 {
            drop(state.values.insert(key, vec![23; 512]));
            let _old = state.order.insert(key, key);
        }
        state = storage.commit(&state)?;
        let before = std::fs::metadata(dir.path().join("pages"))?.len();
        drop(state.values.insert(10_000, vec![99; 512]));
        let _old = state.order.insert(10_000, 10_000);
        state = storage.commit(&state)?;
        let growth = std::fs::metadata(dir.path().join("pages"))?
            .len()
            .saturating_sub(before);
        if growth >= 128 * 1024 {
            return Err(io::Error::other(format!("one edit rewrote {growth} bytes")));
        }
        equal(&state.values.get(&8), &Some(&vec![23; 512]))?;
        drop(state);
        drop(storage);
        OpenOptions::new()
            .append(true)
            .open(dir.path().join("pages"))?
            .write_all(b"unpublished interrupted transaction")?;
        let storage = Storage::open(dir.path())?;
        let state: State = storage.load()?;
        equal(&state.values.len(), &10_001)?;
        equal(&state.values.get(&10_000), &Some(&vec![99; 512]))?;
        equal(
            &state
                .order
                .range(9_995..)
                .map(|(key, _)| *key)
                .collect::<Vec<_>>(),
            &(9_995..=10_000).collect::<Vec<_>>(),
        )?;
        Ok(())
    })?
}

#[test]
fn corrupt_lazy_page_aborts_instead_of_becoming_an_absent_key() -> io::Result<()> {
    let dir = tempfile::tempdir()?;
    let storage = Storage::open(dir.path())?;
    let mut state = State::default();
    drop(state.values.insert(42, vec![1, 2, 3]));
    state = storage.commit(&state)?;
    OpenOptions::new()
        .write(true)
        .open(dir.path().join("pages"))?
        .write_all(&[0xff])?;
    if boundary(|| state.values.get(&42)).is_ok() {
        return Err(io::Error::other("corrupt page appeared to be valid data"));
    }
    Ok(())
}

fn equal<T: PartialEq + std::fmt::Debug>(actual: &T, expected: &T) -> io::Result<()> {
    if actual != expected {
        return Err(io::Error::other(format!(
            "actual {actual:?}, expected {expected:?}"
        )));
    }
    Ok(())
}
