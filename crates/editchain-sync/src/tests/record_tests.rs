//! Shared session records retain their schema-defined replication capabilities.

use editchain_core::{BlobRef, ContentId, OpKind, Payload, SessionId, SessionOp};
use editchain_store::format::encode_op;

use super::{operation, seed};
use crate::{RecordKey, Replica};

#[test]
fn session_records_authorize_their_label_and_metadata_blobs() {
    let directory = tempfile::tempdir().unwrap();
    let mut record = operation(1, Payload::Empty);
    record.kind = OpKind::Session(SessionOp {
        id: SessionId(u64::MAX),
        parent: Some(SessionId(1)),
        label: Payload::Blob(BlobRef {
            id: ContentId::Hash256([1; 32]),
            len: 5,
        }),
        metadata: Payload::Blob(BlobRef {
            id: ContentId::Hash256([2; 32]),
            len: 9,
        }),
    });
    let bytes = encode_op(&record).unwrap();
    let key = RecordKey::from_encoded(&bytes).unwrap();
    seed(directory.path(), &[(key, bytes)]).unwrap();
    let replica = Replica::open(directory.path(), "session-record-test", true).unwrap();
    let snapshot = replica.snapshot().unwrap();
    assert_eq!(
        replica.blob_hashes(&snapshot, key).unwrap(),
        std::collections::BTreeSet::from([[1; 32], [2; 32]])
    );
}
