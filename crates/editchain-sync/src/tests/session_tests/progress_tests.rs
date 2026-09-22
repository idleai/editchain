//! Progress follows a frozen, scoped inventory and durable page completion.

use super::*;
use crate::transfer::{Object, Source};

#[test]
fn checks_count_existing_records_and_sender_waits_for_page_confirmation() -> io::Result<()> {
    let dir = tempfile::tempdir()?;
    let records = (1..=300)
        .map(|seq| record(seq, b"already here"))
        .collect::<io::Result<Vec<_>>>()?;
    let ar = dir.path().join("a");
    let br = dir.path().join("b");
    seed(&ar, &records)?;
    seed(&br, &records)?;
    let a = Replica::open(&ar, "space-1", true)?;
    let mut source = Source::default();
    let mut receiver = session(&br)?;
    let _hello = receiver.receive(receiver.hello())?;
    check_eq!(
        receiver.progress().incoming.total_records,
        None,
        "no invented denominator before the peer offers one"
    );
    let mut page = source.inventory(&a, None)?;
    seed(&ar, &[record(301, b"next pass")?])?;
    loop {
        let replies = receiver.receive(page)?;
        check_eq!(
            receiver.progress().records,
            0,
            "checking existing bytes does not claim a new receipt"
        );
        check_eq!(
            receiver.progress().incoming.total_records,
            Some(300),
            "a local append cannot change the active total"
        );
        check!(
            source.progress.checked_records < receiver.progress().incoming.checked_records,
            "sending an inventory never claims remote completion"
        );
        let mut next = None;
        for reply in replies {
            match reply {
                Message::Checked { end } => source.checked(end)?,
                Message::Inventory { after } => {
                    next = Some(source.inventory(&a, after)?);
                }
                Message::Hello { .. }
                | Message::Page { .. }
                | Message::Need { .. }
                | Message::Chunk { .. }
                | Message::Missing { .. }
                | Message::Ack { .. } => {
                    return Err(io::Error::other("known records need no data transfer"));
                }
            }
        }
        if let Some(next) = next {
            page = next;
        } else {
            break;
        }
    }
    check_eq!(
        source.progress.checked_records,
        300,
        "sender sees confirmed checks including records already present"
    );
    check!(
        source.progress.complete && receiver.progress().incoming.complete,
        "both directions agree that this pass finished"
    );
    let _next = source.inventory(&a, None)?;
    check_eq!(
        source.progress.total_records,
        Some(301),
        "next pass includes concurrent edits"
    );
    check_eq!(
        source.progress.pass,
        2,
        "a new denominator belongs to a visibly new pass"
    );
    check_eq!(
        source.progress.checked_records,
        0,
        "progress resets independently of saved receipts"
    );
    Ok(())
}

#[test]
fn content_bytes_are_visible_before_durability_without_finishing_the_page() -> io::Result<()> {
    let dir = tempfile::tempdir()?;
    let ar = dir.path().join("a");
    let br = dir.path().join("b");
    let bytes = vec![19; 190_000];
    let entry = blob_record(1, &bytes, 190_000)?;
    seed(&ar, std::slice::from_ref(&entry))?;
    BlobStore::new(ar.join("blobs"))?.write(&bytes)?;
    let mut a = session(&ar)?;
    let mut b = session(&br)?;
    let mut queue = start(&a, &b);
    let mut partial = false;
    while !queue.is_empty() {
        deliver(&mut a, &mut b, &mut queue)?;
        let progress = b.progress();
        if let Some(download) = &progress.download {
            if download.content && download.received_bytes > 0 {
                partial = true;
                check_eq!(
                    download.total_bytes,
                    Some(190_000),
                    "current content size is known after its first chunk"
                );
                check_eq!(progress.blobs, 0, "buffered bytes are not saved content");
                check_eq!(
                    progress.pending_records,
                    0,
                    "record was saved before requesting content"
                );
                check_eq!(
                    progress.pending_blobs,
                    1,
                    "the active content request stays in remaining work"
                );
                check_eq!(
                    progress.incoming.checked_records,
                    0,
                    "a page cannot finish while content is in flight"
                );
                check!(!progress.incoming.complete, "no premature 100 percent");
            }
        }
    }
    check!(
        partial,
        "fragmented delivery exposes intermediate byte progress"
    );
    check_eq!(
        b.progress().blobs,
        1,
        "complete bytes are verified and durable"
    );
    check_eq!(b.progress().pending_blobs, 0, "nothing remains queued");
    check_eq!(
        b.progress().incoming.checked_records,
        1,
        "content completion finishes the record's check"
    );
    check!(
        b.progress().incoming.complete && a.progress().outgoing.complete,
        "the sender observes the receiver's completed page"
    );
    Ok(())
}

#[test]
fn totals_respect_exclusions_and_missing_content_stays_visible_after_checking() -> io::Result<()> {
    let dir = tempfile::tempdir()?;
    let ar = dir.path().join("a");
    let br = dir.path().join("b");
    seed(&ar, &[record(1, b"withheld history")?])?;
    let scoped = Replica::open(&ar, "space-1", false)?;
    seed(&ar, &[blob_record(2, b"late", 4)?])?;
    let mut a = Session::new(scoped);
    let mut b = session(&br)?;
    let mut queue = start(&a, &b);
    drain(&mut a, &mut b, &mut queue)?;
    check_eq!(
        b.progress().incoming.total_records,
        Some(1),
        "excluded history is absent even from totals"
    );
    check_eq!(
        b.progress().incoming.checked_records,
        1,
        "missing response concludes the check, not hydration"
    );
    check_eq!(
        b.progress().incoming.unavailable,
        1,
        "the receiver still reports an unsatisfied content request"
    );
    check_eq!(
        a.progress().outgoing.unavailable,
        1,
        "the sender also knows its content was unavailable"
    );
    check_eq!(
        a.progress().incoming.total_records,
        Some(0),
        "empty history has an exact zero total"
    );
    check!(a.progress().incoming.complete, "an empty pass terminates");
    BlobStore::new(ar.join("blobs"))?.write(b"late")?;
    queue.extend(b.tick()?.into_iter().map(|reply| (true, reply)));
    drain(&mut a, &mut b, &mut queue)?;
    check_eq!(
        b.progress().incoming.unavailable,
        0,
        "later content repairs the missing response"
    );
    check_eq!(
        a.progress().outgoing.unavailable,
        0,
        "outgoing missing work resets with the repaired pass"
    );
    check_eq!(b.progress().incoming.pass, 2, "repair has its own pass");
    let restarted = session(&br)?;
    check_eq!(
        restarted.progress().incoming.total_records,
        None,
        "reconnect never reuses a stale denominator"
    );
    Ok(())
}

#[test]
fn inconsistent_totals_and_premature_page_confirmations_fail_closed() -> io::Result<()> {
    let dir = tempfile::tempdir()?;
    let entry = record(1, b"known")?;
    seed(dir.path(), std::slice::from_ref(&entry))?;
    for (total, more) in [(0, false), (2, false), (1, true), (u64::MAX, true)] {
        let mut receiver = session(dir.path())?;
        let _hello = receiver.receive(receiver.hello())?;
        check!(
            receiver
                .receive(Message::Page {
                    offset: 0,
                    total,
                    records: vec![entry.0],
                    more
                })
                .is_err(),
            "contradictory or unrepresentable totals are rejected"
        );
    }
    let mut receiver = session(dir.path())?;
    let _hello = receiver.receive(receiver.hello())?;
    let _first = receiver.receive(Message::Page {
        offset: 0,
        total: 2,
        records: vec![entry.0],
        more: true,
    })?;
    check!(
        receiver
            .receive(Message::Page {
                offset: 1,
                total: 3,
                records: vec![entry.0],
                more: true
            })
            .is_err(),
        "a later page cannot change totals"
    );
    let replica = Replica::open(dir.path(), "space-1", true)?;
    let mut source = Source::default();
    check!(
        source.checked(0).is_err(),
        "no check confirmation before an inventory"
    );
    let _page = source.inventory(&replica, None)?;
    check!(
        source.checked(2).is_err(),
        "confirmation must name the current page's exact end"
    );
    let object = Object {
        record: entry.0,
        blob: None,
    };
    let _chunk = source.need(&replica, object, 0)?;
    check!(
        source.checked(1).is_err(),
        "unacknowledged records cannot be counted as checked"
    );
    source.ack(object)?;
    source.checked(1)?;
    check!(
        source.checked(1).is_err(),
        "duplicate confirmation is rejected"
    );
    for version in [1, 2] {
        let mut receiver = session(dir.path())?;
        let error = receiver
            .receive(Message::Hello {
                version,
                encoding: 1,
                space: "space-1".into(),
            })
            .err()
            .ok_or_else(|| io::Error::other("old protocol accepted"))?;
        check_eq!(
            error.kind(),
            io::ErrorKind::Unsupported,
            "older builds receive the explicit update error"
        );
    }
    Ok(())
}
