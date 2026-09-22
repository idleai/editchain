use super::*;
use crate::{transfer::Source, Session};
use std::collections::BTreeSet;

fn entry(seq: u64, parents: ParentSet, label: &[u8]) -> io::Result<(RecordKey, Vec<u8>)> {
    let mut op = operation(seq, Payload::Inline(label.to_vec()));
    op.parents = parents;
    let bytes = encode_op(&op).map_err(io::Error::other)?;
    Ok((RecordKey::from_encoded(&bytes)?, bytes))
}

fn id(seq: u64) -> OpId {
    OpId::new(NodeId(7), 1, seq)
}

#[test]
fn inventory_orders_merge_parents_and_all_conflicting_variants_before_descendants() -> io::Result<()>
{
    let dir = tempfile::tempdir()?;
    let records = [
        entry(1, ParentSet::Two(id(2), id(3)), b"merge")?,
        entry(2, ParentSet::One(id(9)), b"left")?,
        entry(3, ParentSet::One(id(9)), b"right variant one")?,
        entry(3, ParentSet::One(id(9)), b"right variant two")?,
        entry(9, ParentSet::None, b"root")?,
    ];
    seed(dir.path(), &records)?;
    let snapshot = Replica::open(dir.path(), "space-1", true)?.snapshot()?;
    let order = snapshot.ordered_keys()?;
    check_eq!(order.len(), records.len(), "no exact variant is lost");
    let position = |key| {
        order
            .iter()
            .position(|item| *item == key)
            .ok_or_else(|| io::Error::other("missing ordered key"))
    };
    for (key, bytes) in &records {
        let op = editchain_store::format::decode_op(bytes).map_err(io::Error::other)?;
        for (parent, _) in records
            .iter()
            .filter(|(record, _)| op.parents.iter().any(|id| *id == record.id))
        {
            check!(
                position(*parent)? < position(*key)?,
                "parent variant must be sent before descendant"
            );
        }
    }
    check_eq!(snapshot.ordered_keys()?, order, "order is deterministic");
    Ok(())
}

#[test]
fn inventory_walk_is_iterative_and_preserves_cycles_and_missing_parent_evidence() -> io::Result<()>
{
    let dir = tempfile::tempdir()?;
    let mut records = (10..=4106)
        .map(|seq| {
            entry(
                seq,
                ParentSet::One(id(seq.saturating_add(1))),
                b"deep ancestry",
            )
        })
        .collect::<io::Result<Vec<_>>>()?;
    records.extend([
        entry(1, ParentSet::One(id(2)), b"cycle one")?,
        entry(2, ParentSet::One(id(1)), b"cycle two")?,
        entry(3, ParentSet::One(id(3)), b"self cycle")?,
    ]);
    seed(dir.path(), &records)?;
    let snapshot = Replica::open(dir.path(), "space-1", true)?.snapshot()?;
    let order = snapshot.ordered_keys()?;
    check_eq!(
        order.len(),
        records.len(),
        "cycles are retained once and the walk terminates"
    );
    check_eq!(
        order.iter().copied().collect::<BTreeSet<_>>(),
        records.iter().map(|(key, _)| *key).collect::<BTreeSet<_>>(),
        "missing parents do not create invented records"
    );
    let seqs: Vec<_> = order
        .iter()
        .filter(|key| key.id.seq >= 10)
        .map(|key| key.id.seq)
        .collect();
    check!(
        seqs.windows(2).all(|pair| pair.first() > pair.last()),
        "deep reverse-ID ancestry is emitted in causal order"
    );
    Ok(())
}

#[test]
fn source_cursor_positions_stay_stable_across_appends_and_reject_wrong_continuations(
) -> io::Result<()> {
    let dir = tempfile::tempdir()?;
    seed(
        dir.path(),
        &(1..=300)
            .map(|seq| record(seq, b"entry"))
            .collect::<io::Result<Vec<_>>>()?,
    )?;
    let replica = Replica::open(dir.path(), "space-1", true)?;
    let mut source = Source::default();
    let Message::Page {
        offset,
        total: advertised_total,
        records,
        more,
    } = source.inventory(&replica, None)?
    else {
        return Err(io::Error::other("missing first page"));
    };
    check_eq!(offset, 0, "first page position");
    check_eq!(
        advertised_total,
        300,
        "a stable total is available before any transfer"
    );
    check!(more, "fixture spans pages");
    source.checked(u64::try_from(records.len()).map_err(io::Error::other)?)?;
    check!(
        source
            .inventory(&replica, records.first().copied())
            .is_err(),
        "only the preceding page's last key is a valid cursor"
    );
    seed(dir.path(), &[record(301, b"next round")?])?;
    let mut cursor = records.last().copied();
    let mut total = records.len();
    loop {
        let Message::Page {
            offset,
            total: advertised_total,
            records,
            more,
        } = source.inventory(&replica, cursor)?
        else {
            return Err(io::Error::other("missing next page"));
        };
        check_eq!(
            usize::try_from(offset).map_err(io::Error::other)?,
            total,
            "page positions increase by the number of records"
        );
        total = total.saturating_add(records.len());
        check_eq!(
            advertised_total,
            300,
            "concurrent appends cannot move the denominator"
        );
        source.checked(u64::try_from(total).map_err(io::Error::other)?)?;
        cursor = records.last().copied();
        if !more {
            break;
        }
    }
    check_eq!(total, 300, "in-flight inventory excludes later appends");
    check!(
        source.inventory(&replica, cursor).is_err(),
        "completed inventory cannot continue"
    );
    check_eq!(
        replica.snapshot()?.ordered_keys()?.len(),
        301,
        "later rounds include new work"
    );
    Ok(())
}

#[test]
fn receiver_rejects_page_gaps_replays_duplicates_and_empty_continuations() -> io::Result<()> {
    let dir = tempfile::tempdir()?;
    let (key, _) = record(1, b"known")?;
    seed(dir.path(), &[record(1, b"known")?])?;
    for bad in [
        Message::Page {
            offset: 1,
            total: 1,
            records: vec![key],
            more: false,
        },
        Message::Page {
            offset: 0,
            total: 2,
            records: vec![key, key],
            more: false,
        },
        Message::Page {
            offset: 0,
            total: 1,
            records: vec![],
            more: true,
        },
    ] {
        let mut peer = Session::new(Replica::open(dir.path(), "space-1", true)?);
        let _hello = peer.receive(peer.hello())?;
        check!(
            peer.receive(bad).is_err(),
            "invalid initial page must fail closed"
        );
    }
    let mut peer = Session::new(Replica::open(dir.path(), "space-1", true)?);
    let _hello = peer.receive(peer.hello())?;
    let page = Message::Page {
        offset: 0,
        total: 2,
        records: vec![key],
        more: true,
    };
    let _next = peer.receive(page.clone())?;
    check!(
        peer.receive(page).is_err(),
        "replayed page position must fail closed"
    );
    let mut peer = Session::new(Replica::open(dir.path(), "space-1", true)?);
    let error = peer
        .receive(Message::Hello {
            version: 1,
            encoding: 1,
            space: "space-1".into(),
        })
        .err()
        .ok_or_else(|| io::Error::other("old protocol must fail before inventory"))?;
    check_eq!(
        error.kind(),
        io::ErrorKind::Unsupported,
        "version mismatch is distinguishable from corrupt data"
    );
    check!(
        !peer.progress().accepted,
        "old peers cannot exchange inventory"
    );
    Ok(())
}
