//! Stable derived IDs keep retries and reprocessing byte-idempotent.

use editchain_core::{
    human::{HumanWorkKind, HumanWorkRecord},
    ActorId, Clock, FileEdit, FileOp, FileStage, GitLink, GitLinkKind, GitOid, ImportOp, NoteOp,
    NoteRelationship, Op, OpId, OpKind, ParentSet, Payload, ScopeRef, Tags,
};
use editchain_protocol::editor::EditorEvent;

fn identity(event: &EditorEvent, lane: u32) -> OpId {
    let name = format!("vscode.work.v1:{}", event.session);
    OpId::new(
        editchain_import::derive_node_id(&name),
        lane,
        event.sequence,
    )
}

fn operation(event: &EditorEvent, lane: u32, parent: Option<OpId>, kind: OpKind, tags: Tags) -> Op {
    Op {
        id: identity(event, lane),
        parents: parent.map_or(ParentSet::None, ParentSet::One),
        actor: ActorId(editchain_import::derive_node_id(&event.session).0),
        clock: Clock::UnixMs(event.time_ms),
        scope: ScopeRef::Session(editchain_import::derive_session_id(&format!(
            "vscode:{}",
            event.session
        ))),
        tags: tags | Tags::HUMAN,
        kind,
    }
}

pub(in crate::editor) fn observation(event: &EditorEvent, source: OpId) -> Op {
    operation(
        event,
        0xfffe,
        Some(source),
        OpKind::Note(NoteOp {
            target_ids: vec![source],
            relationship: NoteRelationship::Explains,
            content: Payload::Inline(b"vscode.editor.observation.v1".to_vec()),
        }),
        Tags::NOTE | Tags::META,
    )
}

pub(super) fn work(
    event: &EditorEvent,
    record: &HumanWorkRecord,
    previous: Option<OpId>,
    link: bool,
) -> super::Result<Vec<Op>> {
    let anchor = operation(
        event,
        0,
        previous,
        OpKind::Import(ImportOp {
            raw_ref: Payload::Inline(serde_json::to_vec(record)?),
            raw_hash: None,
        }),
        Tags::IMPORT | Tags::INFERRED,
    );
    let source = anchor.id;
    let mut ops = vec![anchor];
    if record.kind == HumanWorkKind::Edit {
        if let (Some(path), Some(before), Some(after)) =
            (&record.path, &record.before, &record.after)
        {
            let mut file = operation(
                event,
                1,
                Some(source),
                OpKind::File(FileOp {
                    path: editchain_import::derive_path_id(path),
                    stage: FileStage::Applied,
                    base: Some(before.content),
                    after: Some(after.content),
                    edit: FileEdit::None,
                }),
                Tags::FILE | Tags::INFERRED,
            );
            file.scope = ScopeRef::Turn(editchain_core::TurnId(record.turn));
            ops.push(operation(
                event,
                2,
                Some(source),
                OpKind::Note(NoteOp {
                    target_ids: vec![file.id],
                    relationship: NoteRelationship::Explains,
                    content: Payload::Inline(path.as_bytes().to_vec()),
                }),
                Tags::NOTE | Tags::META,
            ));
            ops.push(file);
        }
    }
    if link {
        if let Some(context) = &record.git {
            if let Some(oid) = context.head.as_deref().and_then(GitOid::from_hex) {
                ops.push(operation(
                    event,
                    3,
                    Some(source),
                    OpKind::GitLink(GitLink {
                        source,
                        target_repo: editchain_core::RepositoryId(context.repository.parse()?),
                        target_oid: oid,
                        kind: GitLinkKind::BasedOn,
                    }),
                    Tags::META,
                ));
            }
        }
    }
    Ok(ops)
}
