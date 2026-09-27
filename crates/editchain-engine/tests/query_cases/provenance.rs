use editchain_engine::{
    queries::{
        ContentField, ContentQuery, EntityRef, GitQuery, Lookup, PageRequest, RelationshipKind,
    },
    ActorOp, Admission, Engine, GitLink, GitLinkKind, GitOid, NoteOp, NoteRelationship, OpKind,
    ParentSet, Payload, RepositoryId, ScopeRef, SessionId, SessionOp,
};

use super::commit;
use crate::{append, found, id, message, record};

#[test]
fn provenance_retains_attribution_and_opaque_relationships_without_inference() {
    let directory = tempfile::tempdir().unwrap();
    let engine = Engine::open(directory.path()).unwrap();
    let actor = record(
        1,
        OpKind::Actor(ActorOp {
            label: Payload::Inline(b"person".to_vec()),
            role: Payload::Inline(b"producer-defined".to_vec()),
        }),
    );
    let session = record(
        2,
        OpKind::Session(SessionOp {
            id: SessionId(23),
            parent: Some(SessionId(99)),
            label: Payload::Empty,
            metadata: Payload::Inline(b"opaque metadata".to_vec()),
        }),
    );
    let mut child = message(5, Payload::Empty);
    child.parents = ParentSet::Two(id(3), id(80));
    let parent = message(3, Payload::Inline(b"raw source".to_vec()));
    let note = record(
        6,
        OpKind::Note(NoteOp {
            target_ids: vec![id(5)],
            relationship: NoteRelationship::Rejects,
            content: Payload::Inline(b"{\"status\":\"done\",\"priority\":1}".to_vec()),
        }),
    );
    let later_actor = record(
        7,
        OpKind::Actor(ActorOp {
            label: Payload::Inline(b"later observation".to_vec()),
            role: Payload::Inline(b"agent".to_vec()),
        }),
    );
    append(
        &engine,
        &[
            actor.clone(),
            session.clone(),
            child.clone(),
            parent.clone(),
            note.clone(),
            later_actor.clone(),
        ],
    )
    .unwrap();
    let mut queries = engine.queries().unwrap();
    let provenance = found(queries.provenance(child.id).unwrap()).unwrap();
    assert_eq!(provenance.record.operation, child);
    assert_eq!(
        provenance
            .actor_records
            .iter()
            .map(|entry| &entry.operation)
            .collect::<Vec<_>>(),
        vec![&actor, &later_actor]
    );
    assert_eq!(
        provenance.session_records.first().unwrap().operation,
        session
    );
    assert_eq!(
        provenance
            .parents
            .iter()
            .map(|parent| parent.operation)
            .collect::<Vec<_>>(),
        vec![id(3), id(80)]
    );
    assert_eq!(provenance.parents.last().unwrap().record, Lookup::Missing);
    assert!(
        provenance.relationships.iter().any(|relation| relation.kind
            == RelationshipKind::Annotation(NoteRelationship::Rejects)
            && relation.evidence.operation == note.id),
        "annotations remain facts, not controller decisions"
    );
    assert_eq!(
        found(queries.operation(id(5)).unwrap()).unwrap().operation,
        child
    );
    let graph = queries.ancestors(id(5), 100).unwrap();
    assert_eq!(
        graph
            .operations
            .iter()
            .map(|operation| operation.operation)
            .collect::<Vec<_>>(),
        vec![id(3), id(5), id(80)]
    );
    assert!(
        graph.frontier.is_empty(),
        "missing parents terminate traversal explicitly"
    );
    assert_eq!(graph.relationships.len(), 2);
    let bounded = queries.ancestors(id(5), 1).unwrap();
    assert_eq!(bounded.frontier, vec![id(3), id(80)]);

    assert_eq!(
        engine
            .append(&message(3, Payload::Inline(b"conflicting parent".to_vec())))
            .unwrap(),
        Admission::Conflict
    );
    drop(queries.refresh().unwrap());
    assert!(
        matches!(
            found(queries.provenance(id(5)).unwrap())
                .unwrap()
                .parents
                .first()
                .unwrap()
                .record,
            Lookup::Conflicted(_)
        ),
        "conflicted parents remain visible as gaps"
    );

    let mut unscoped = message(10, Payload::Empty);
    unscoped.scope = ScopeRef::Turn(editchain_engine::TurnId(23));
    append(&engine, &[unscoped]).unwrap();
    drop(queries.refresh().unwrap());
    assert!(
        found(queries.provenance(id(10)).unwrap())
            .unwrap()
            .session_records
            .is_empty(),
        "equal numeric IDs never infer session membership"
    );
    let session_edges = queries
        .relationships(
            Some(EntityRef::Session(SessionId(23))),
            PageRequest::default(),
        )
        .unwrap();
    assert_eq!(session_edges.items.len(), 1);
    assert_eq!(
        session_edges.items.first().unwrap().target,
        EntityRef::Session(SessionId(99))
    );
}

#[test]
fn traversal_preserves_recorded_cycles_and_does_not_follow_notes() {
    let directory = tempfile::tempdir().unwrap();
    let engine = Engine::open(directory.path()).unwrap();
    let mut a = message(1, Payload::Empty);
    a.parents = ParentSet::Two(id(2), id(1));
    let mut b = message(2, Payload::Empty);
    b.parents = ParentSet::One(id(1));
    append(
        &engine,
        &[
            a,
            b,
            record(
                3,
                OpKind::Note(NoteOp {
                    target_ids: vec![id(1)],
                    relationship: NoteRelationship::ProviderParent,
                    content: Payload::Empty,
                }),
            ),
        ],
    )
    .unwrap();
    let queries = engine.queries().unwrap();
    let graph = queries.ancestors(id(1), 1000).unwrap();
    assert_eq!(graph.operations.len(), 2);
    assert_eq!(graph.relationships.len(), 3);
    assert!(
        graph.frontier.is_empty(),
        "visited IDs terminate cycles without erasing assertions"
    );
    assert!(
        queries.ancestors(id(1), 0).is_err(),
        "traversal budget must be positive"
    );
    assert_eq!(
        queries
            .ancestors(id(99), 1)
            .unwrap()
            .operations
            .first()
            .unwrap()
            .record,
        Lookup::Missing
    );
}

#[test]
fn git_refs_and_parentage_are_recorded_and_repository_qualified() {
    let directory = tempfile::tempdir().unwrap();
    let engine = Engine::open(directory.path()).unwrap();
    let first = commit(1, RepositoryId(1));
    let other_repository = commit(2, RepositoryId(2));
    let mut later = commit(3, RepositoryId(1));
    if let OpKind::GitCommit(commit) = &mut later.kind {
        commit.live_refs = vec![Payload::Inline(b"refs/heads/moved".to_vec())];
    }
    let link = record(
        4,
        OpKind::GitLink(GitLink {
            source: id(99),
            target_repo: RepositoryId(1),
            target_oid: GitOid::from_sha1([7; 20]),
            kind: GitLinkKind::ProducedBy,
        }),
    );
    let missing_link = record(
        5,
        OpKind::GitLink(GitLink {
            source: id(99),
            target_repo: RepositoryId(1),
            target_oid: GitOid::from_sha256([9; 32]),
            kind: GitLinkKind::BasedOn,
        }),
    );
    append(
        &engine,
        &[
            first.clone(),
            other_repository,
            later.clone(),
            link.clone(),
            missing_link,
        ],
    )
    .unwrap();
    let queries = engine.queries().unwrap();
    let query = GitQuery {
        repository: RepositoryId(1),
        oid: Some(GitOid::from_sha1([7; 20])),
    };
    assert_eq!(
        queries
            .git(query, PageRequest::default())
            .unwrap()
            .items
            .iter()
            .map(|entry| &entry.operation)
            .collect::<Vec<_>>(),
        vec![&first, &later, &link]
    );
    let content = |operation, field| {
        found(
            queries
                .content(ContentQuery {
                    operation: id(operation),
                    field,
                })
                .unwrap(),
        )
        .unwrap()
    };
    assert_eq!(
        content(1, ContentField::GitImportedRef(0))
            .value
            .bytes()
            .unwrap(),
        b"refs/heads/main"
    );
    assert_eq!(
        content(1, ContentField::GitLiveRef(0))
            .value
            .bytes()
            .unwrap(),
        b"refs/heads/topic"
    );
    assert_eq!(
        content(3, ContentField::GitLiveRef(0))
            .value
            .bytes()
            .unwrap(),
        b"refs/heads/moved"
    );
    assert_eq!(
        queries
            .search("refs/heads/topic", None, PageRequest::default())
            .unwrap()
            .hits
            .len(),
        2
    );
    let entity = EntityRef::Git {
        repository: RepositoryId(1),
        oid: GitOid::from_sha1([7; 20]),
    };
    let relationships = queries
        .relationships(Some(entity), PageRequest::default())
        .unwrap();
    assert_eq!(relationships.items.len(), 5);
    assert!(
        relationships
            .items
            .iter()
            .all(|relation| relation.evidence.operation != id(2)),
        "same OID in another repository is separate evidence"
    );
    assert!(
        relationships
            .items
            .iter()
            .any(|relation| relation.source == EntityRef::Operation(id(99))
                && relation.kind == RelationshipKind::GitLink(GitLinkKind::ProducedBy)),
        "Git links keep recorded direction, without viewer edge inversion"
    );
    assert_eq!(
        queries
            .git(
                GitQuery {
                    oid: Some(GitOid::from_sha256([9; 32])),
                    ..query
                },
                PageRequest::default()
            )
            .unwrap()
            .items
            .len(),
        1
    );
    let filtered = queries
        .git(
            query,
            PageRequest {
                after: Some(id(1)),
                limit: 1,
            },
        )
        .unwrap();
    assert!(
        filtered.items.is_empty(),
        "a page containing a different repository can be empty"
    );
    assert_eq!(filtered.next_after, Some(id(2)));
}
