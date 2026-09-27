use editchain_engine::{
    queries::{ContentField, ContentQuery, PageRequest},
    ActorOp, ByteRange, ChainStart, CommandOp, CommandStage, Engine, ErrorOp, FileEdit, FileOp,
    FileStage, FrontierSet, GitLink, GitLinkKind, GitOid, ImportOp, MessageOp, NoteOp,
    NoteRelationship, OpKind, PathId, Payload, ReflectionOp, RepositoryId, ScopeRef, SessionId,
    SessionOp, ToolOp, ToolStage, UnknownOp, WindowRef,
};

use super::commit;
use crate::{append, found, id, record};

fn payload_cases(payload: &Payload) -> Vec<(OpKind, Vec<ContentField>)> {
    vec![
        (
            OpKind::Actor(ActorOp {
                label: payload.clone(),
                role: payload.clone(),
            }),
            vec![ContentField::ActorLabel, ContentField::ActorRole],
        ),
        (
            OpKind::Session(SessionOp {
                id: SessionId(23),
                parent: None,
                label: payload.clone(),
                metadata: payload.clone(),
            }),
            vec![ContentField::SessionLabel, ContentField::SessionMetadata],
        ),
        (
            OpKind::Message(MessageOp {
                content: payload.clone(),
                content_type: payload.clone(),
            }),
            vec![
                ContentField::MessageContent,
                ContentField::MessageContentType,
            ],
        ),
        (
            OpKind::Tool(ToolOp {
                tool_call_id: payload.clone(),
                tool_name: payload.clone(),
                stage: ToolStage::Finish,
                content: payload.clone(),
            }),
            vec![
                ContentField::ToolCallId,
                ContentField::ToolName,
                ContentField::ToolContent,
            ],
        ),
        (
            OpKind::Command(CommandOp {
                command_id: payload.clone(),
                content: payload.clone(),
                stage: CommandStage::Output,
            }),
            vec![ContentField::CommandId, ContentField::CommandContent],
        ),
        (
            OpKind::Reflection(ReflectionOp {
                scope: ScopeRef::None,
                covers: FrontierSet::new(),
                window: WindowRef {
                    start_seq: 0,
                    end_seq: 1,
                },
                summary: payload.clone(),
                anchors: payload.clone(),
            }),
            vec![
                ContentField::ReflectionSummary,
                ContentField::ReflectionAnchors,
            ],
        ),
        (
            OpKind::Import(ImportOp {
                raw_ref: payload.clone(),
                raw_hash: None,
            }),
            vec![ContentField::ImportRaw],
        ),
        (
            OpKind::Note(NoteOp {
                target_ids: vec![id(1)],
                relationship: NoteRelationship::Explains,
                content: payload.clone(),
            }),
            vec![ContentField::NoteContent],
        ),
        (
            OpKind::Error(ErrorOp {
                code: payload.clone(),
                message: payload.clone(),
            }),
            vec![ContentField::ErrorCode, ContentField::ErrorMessage],
        ),
        (
            OpKind::Unknown(UnknownOp {
                kind_discriminant: 255,
                raw_bytes: payload.clone(),
            }),
            vec![ContentField::UnknownRaw],
        ),
        (
            OpKind::GitLink(GitLink {
                source: id(1),
                target_repo: RepositoryId(1),
                target_oid: GitOid::from_sha1([7; 20]),
                kind: GitLinkKind::Custom(payload.clone()),
            }),
            vec![ContentField::GitLinkCustom],
        ),
    ]
}

#[test]
fn every_content_field_is_resolvable_and_searchable_with_its_original_record() {
    let directory = tempfile::tempdir().unwrap();
    let engine = Engine::open(directory.path()).unwrap();
    let bytes = b"all fields needle\0\xff\r\n";
    let blob = engine.store_blob(bytes).unwrap();
    let payload = Payload::Blob(blob);
    let mut cases = payload_cases(&payload);
    cases.push((
        OpKind::ChainStart(ChainStart {
            name: bytes.to_vec(),
            version: 1,
        }),
        vec![ContentField::ChainName],
    ));
    for edit in [
        FileEdit::Blob(blob),
        FileEdit::UnifiedDiff(payload.clone()),
        FileEdit::ReplaceBytes {
            range: ByteRange { start: 0, end: 1 },
            bytes: payload.clone(),
        },
    ] {
        cases.push((
            OpKind::File(FileOp {
                path: PathId(42),
                stage: FileStage::Observed,
                base: Some(blob.id),
                after: Some(blob.id),
                edit,
            }),
            vec![
                ContentField::FileBase,
                ContentField::FileAfter,
                ContentField::FileEdit,
            ],
        ));
    }
    let mut git = commit(99, RepositoryId(1));
    if let OpKind::GitCommit(commit) = &mut git.kind {
        commit.author.name = payload.clone();
        commit.author.email = payload.clone();
        commit.committer.name = payload.clone();
        commit.committer.email = payload.clone();
        commit.message = payload.clone();
        commit.imported_refs = vec![payload.clone(), payload.clone()];
        commit.live_refs = vec![payload];
    }
    cases.push((
        git.kind,
        vec![
            ContentField::GitAuthorName,
            ContentField::GitAuthorEmail,
            ContentField::GitCommitterName,
            ContentField::GitCommitterEmail,
            ContentField::GitMessage,
            ContentField::GitImportedRef(0),
            ContentField::GitImportedRef(1),
            ContentField::GitLiveRef(0),
        ],
    ));
    for (index, (kind, _)) in cases.iter().enumerate() {
        append(
            &engine,
            &[record(u64::try_from(index).unwrap(), kind.clone())],
        )
        .unwrap();
    }
    let queries = engine.queries().unwrap();
    let search = queries
        .search("needle", None, PageRequest::default())
        .unwrap();
    assert!(
        search.unavailable.is_empty(),
        "all supported fields have verified content dependencies"
    );
    assert_eq!(search.hits.len(), cases.len());
    for ((index, (_, fields)), hit) in cases.iter().enumerate().zip(&search.hits) {
        let operation = id(u64::try_from(index).unwrap());
        assert_eq!(hit.record_ref.operation, operation);
        assert_eq!(
            hit.fields
                .iter()
                .map(|matched| matched.field)
                .collect::<Vec<_>>(),
            *fields
        );
        for field in fields {
            let content = found(
                queries
                    .content(ContentQuery {
                        operation,
                        field: *field,
                    })
                    .unwrap(),
            )
            .unwrap();
            assert_eq!(content.record_ref, hit.record_ref);
            assert_eq!(content.value.bytes().unwrap(), bytes);
        }
    }
}
