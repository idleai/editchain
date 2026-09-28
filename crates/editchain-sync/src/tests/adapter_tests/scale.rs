//! The portable CLI adapter reuses evidence while owning the writer lock.

use super::*;

struct CountedLog {
    inner: SegmentStore,
    replays: Rc<Cell<usize>>,
}

impl AppendLog for CountedLog {
    fn visit_records(
        &self,
        visitor: &mut editchain_store::RecordVisitor<'_>,
    ) -> io::Result<editchain_store::LogReadStats> {
        self.replays.set(self.replays.get().saturating_add(1));
        self.inner.visit_records(visitor)
    }

    fn append_record(&mut self, flags: u8, bytes: &[u8]) -> io::Result<()> {
        self.inner.append_record(flags, bytes)
    }

    fn append_records(&mut self, records: &[(u8, &[u8])]) -> io::Result<()> {
        self.inner.append_records(records)
    }

    fn sync(&self) -> io::Result<()> {
        self.inner.sync()
    }
}

#[test]
fn portable_receipts_and_blob_arrivals_do_not_replay_the_existing_corpus() {
    let directory = tempfile::tempdir().expect("temporary storage");
    let old = (1..=20_000)
        .map(|seq| record(seq, &[42; 128]))
        .collect::<io::Result<Vec<_>>>()
        .expect("encoded seed records");
    seed(directory.path(), &old).expect("durable seed records");
    let replays = Rc::new(Cell::new(0));
    let mut storage = StoreReplica::new(
        CountedLog {
            inner: SegmentStore::open(directory.path()).expect("exclusive log"),
            replays: Rc::clone(&replays),
        },
        BlobStore::new(directory.path().join("blobs")).expect("blob store"),
        ExportScope::all("scale").expect("sharing scope"),
    );
    let frozen = storage.snapshot().expect("frozen evidence");
    let mut received = storage.receiving_snapshot().expect("receiving evidence");
    assert_eq!(replays.get(), 1);
    for seq in 20_001..=20_024 {
        let bytes = format!("blob for {seq}").into_bytes();
        let entry = blob_record(
            seq,
            &bytes,
            u32::try_from(bytes.len()).expect("small blob length"),
        )
        .expect("encoded blob reference");
        storage
            .ingest_records(std::slice::from_ref(&entry), &mut received)
            .expect("durable receipt");
        let hash = *blake3::hash(&bytes).as_bytes();
        storage
            .ingest_blob(entry.0, hash, &bytes)
            .expect("durable blob");
        assert_eq!(
            storage
                .read_blob(&received, entry.0, hash)
                .expect("read blob"),
            Some(bytes)
        );
    }
    let complete = storage.snapshot().expect("updated evidence");
    assert_eq!(complete.len(), 20_024);
    assert_eq!(frozen.len(), 20_000);
    assert_eq!(
        replays.get(),
        1,
        "every receipt must reuse the exclusively owned evidence"
    );
    let key = old.first().expect("seed fixture").0;
    assert_eq!(
        frozen.record(key).map(<[u8]>::as_ptr),
        complete.record(key).map(<[u8]>::as_ptr),
        "frozen snapshots share immutable bytes"
    );
    let (log, blobs, policy) = storage.into_parts();
    let reopened = StoreReplica::new(log, blobs, policy);
    assert_eq!(
        reopened.snapshot().expect("reopened evidence").len(),
        20_024
    );
    assert_eq!(
        replays.get(),
        2,
        "a new writer lifetime must establish its own evidence"
    );
}
