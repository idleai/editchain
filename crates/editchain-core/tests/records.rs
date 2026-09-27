//! Frozen wire fixtures for the shared record vocabulary.

use proptest as _;
use serde as _;

use editchain_core::records::{
    ActorRecord, AnnotationRecord, ChainRecord, OperationRecord, ReflectionRecord, RevisionRecord,
    SessionRecord,
};
use editchain_core::{
    ActorId, Clock, FileEdit, FileStage, Frontier, FrontierSet, NodeId, NoteRelationship, OpId,
    OpKind, ParentSet, PathId, Payload, ScopeRef, SessionId, Tags, UnknownOp, WindowRef,
};

fn binary() -> Payload {
    Payload::Inline(vec![0, 255])
}

#[test]
fn shared_names_preserve_existing_envelope_and_kind_wire_bytes() {
    let fixtures = [
        (
            OpKind::ChainStart(ChainRecord {
                name: vec![0, 255],
                version: 1,
            }),
            vec![0, 2, 0, 255, 1],
        ),
        (
            OpKind::Actor(ActorRecord {
                label: binary(),
                role: Payload::Empty,
            }),
            vec![1, 1, 2, 0, 255, 0],
        ),
        (
            OpKind::File(RevisionRecord {
                path: PathId(9),
                stage: FileStage::Saved,
                base: None,
                after: None,
                edit: FileEdit::None,
            }),
            vec![5, 9, 3, 0, 0, 0],
        ),
        (
            OpKind::Reflection(ReflectionRecord {
                scope: ScopeRef::Session(SessionId(6)),
                covers: FrontierSet(vec![Frontier {
                    node: NodeId(1),
                    boot: 2,
                    max_seq: 3,
                }]),
                window: WindowRef {
                    start_seq: 1,
                    end_seq: 4,
                },
                summary: binary(),
                anchors: Payload::Empty,
            }),
            vec![6, 2, 6, 1, 1, 2, 3, 1, 4, 1, 2, 0, 255, 0],
        ),
        (
            OpKind::Note(AnnotationRecord {
                target_ids: vec![OpId::new(NodeId(1), 2, 3)],
                relationship: NoteRelationship::Supersedes,
                content: binary(),
            }),
            vec![8, 1, 1, 2, 3, 1, 1, 2, 0, 255],
        ),
        (
            OpKind::Unknown(UnknownOp {
                kind_discriminant: 9,
                raw_bytes: binary(),
            }),
            vec![12, 9, 1, 2, 0, 255],
        ),
        (
            OpKind::Session(SessionRecord {
                id: SessionId(6),
                parent: Some(SessionId(5)),
                label: binary(),
                metadata: Payload::Empty,
            }),
            vec![13, 6, 1, 5, 1, 2, 0, 255, 0],
        ),
    ];
    for (kind, body) in fixtures {
        let operation = OperationRecord {
            id: OpId::new(NodeId(1), 2, 3),
            parents: ParentSet::None,
            actor: ActorId(4),
            clock: Clock::None,
            scope: ScopeRef::None,
            tags: Tags::NONE,
            kind,
        };
        // Existing envelope: node, boot, sequence, parents, actor, clock, scope, tags.
        let mut expected = vec![1, 2, 3, 0, 4, 0, 0, 0];
        expected.extend(body);
        assert_eq!(postcard::to_stdvec(&operation).unwrap(), expected);
        assert_eq!(
            postcard::from_bytes::<OperationRecord>(&expected).unwrap(),
            operation
        );
    }
}

#[test]
fn session_identity_and_opaque_metadata_survive_without_normalization() {
    let session = SessionRecord {
        id: SessionId(u64::MAX),
        parent: Some(SessionId(u64::MAX.saturating_sub(1))),
        label: Payload::Inline(b"duplicate label".to_vec()),
        metadata: Payload::Inline(b"native-provider-id:\0\xff\r\n".to_vec()),
    };
    let bytes = postcard::to_stdvec(&session).unwrap();
    let decoded: SessionRecord = postcard::from_bytes(&bytes).unwrap();
    assert_eq!(decoded, session);
    let different_session = SessionRecord {
        id: SessionId(1),
        ..session.clone()
    };
    assert_ne!(different_session, session);
    assert_ne!(postcard::to_stdvec(&different_session).unwrap(), bytes);
}
