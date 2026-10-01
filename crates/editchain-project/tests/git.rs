//! Git projection identity, observation order, and stored-link contracts.

use blake3 as _;
use editchain_engine as _;
use editchain_index as _;
use editchain_store as _;
use history_geometry as _;
use serde as _;
use serde_json as _;
use tokio as _;

use editchain_core::{
    ActorId, Clock, GitAvailability, GitCommitEntity, GitLink, GitLinkKind, GitOid, GitSignature,
    NodeId, Op, OpId, OpKind, ParentSet, Payload, RepositoryId, ScopeRef, Tags,
};
use editchain_project::GitProjection;

fn sha1(bytes: [u8; 20]) -> GitOid {
    GitOid::from_sha1(bytes)
}

fn commit_op(id: OpId, repo: RepositoryId, oid: GitOid) -> Op {
    let commit = GitCommitEntity {
        repository: repo,
        object_format: oid.format(),
        oid,
        imported_record: Some(id),
        availability: GitAvailability::ImportedOnly,
        tree: sha1([0x02; 20]),
        parents: Vec::new(),
        author: GitSignature {
            name: Payload::Inline(b"Alice".to_vec()),
            email: Payload::Inline(b"alice@example.com".to_vec()),
            when: 1_700_000_000,
        },
        committer: GitSignature {
            name: Payload::Inline(b"Alice".to_vec()),
            email: Payload::Inline(b"alice@example.com".to_vec()),
            when: 1_700_000_100,
        },
        authored_at: 1_700_000_000,
        committed_at: 1_700_000_100,
        message: Payload::Inline(b"commit".to_vec()),
        imported_refs: Vec::new(),
        live_refs: Vec::new(),
        changed_paths: Vec::new(),
    };
    Op {
        source: None,
        id,
        parents: ParentSet::None,
        actor: ActorId(3),
        clock: Clock::UnixMs(1_700_000_000),
        scope: ScopeRef::None,
        tags: Tags::IMPORT,
        kind: OpKind::GitCommit(Box::new(commit)),
    }
}

#[test]
fn projection_dedups_same_commit_by_repo_and_oid() {
    // Two different ops importing the same (repo, oid) collapse to one entity.
    let oid = sha1([0x01; 20]);
    let repo = RepositoryId(7);
    let op_a = commit_op(OpId::new(NodeId(1), 0, 5), repo, oid);
    let op_b = commit_op(OpId::new(NodeId(2), 0, 9), repo, oid);

    let proj = GitProjection::from_ops(&[op_a, op_b]);
    assert_eq!(proj.commits().len(), 1);
    assert!(proj.commit(repo, &oid).is_some());
}

#[test]
fn projection_distinguishes_repositories() {
    // Same OID in two different repositories are distinct entities.
    let oid = sha1([0x01; 20]);
    let op_a = commit_op(OpId::new(NodeId(1), 0, 5), RepositoryId(7), oid);
    let op_b = commit_op(OpId::new(NodeId(2), 0, 9), RepositoryId(8), oid);

    let proj = GitProjection::from_ops(&[op_a, op_b]);
    assert_eq!(proj.commits().len(), 2);
}

#[test]
fn later_observations_replace_the_view_without_changing_source_facts() {
    let repo = RepositoryId(7);
    let oid = sha1([0x01; 20]);
    let op = commit_op(OpId::new(NodeId(1), 0, 5), repo, oid);
    let mut projection = editchain_project::HistoryProjection::from_ops(vec![op.clone()]);
    let imported = projection.git().commit(repo, &oid).unwrap().clone();
    let mut live = imported.clone();
    live.availability = GitAvailability::Resolved;
    live.live_refs = vec![Payload::Inline(b"refs/heads/main".to_vec())];
    projection.merge_git_commits(vec![live.clone()]);
    assert_eq!(projection.git().commit(repo, &oid), Some(&live));
    assert_eq!(projection.ops(), &[op]);

    let mut moved_ref = live.clone();
    moved_ref.live_refs.clear();
    projection.merge_git_commits(vec![moved_ref.clone()]);
    assert_eq!(projection.git().commit(repo, &oid), Some(&moved_ref));
    assert_eq!(imported.availability, GitAvailability::ImportedOnly);
    assert!(imported.live_refs.is_empty());
    assert_eq!(live.live_refs.len(), 1);
}

#[test]
fn projection_groups_links_by_source() {
    let source = OpId::new(NodeId(1), 0, 5);
    let link_a = GitLink {
        source,
        target_repo: RepositoryId(7),
        target_oid: sha1([0x01; 20]),
        kind: GitLinkKind::CommittedAs,
    };
    let link_b = GitLink {
        source,
        target_repo: RepositoryId(7),
        target_oid: sha1([0x02; 20]),
        kind: GitLinkKind::BasedOn,
    };
    let op_a = Op {
        source: Some(editchain_core::SourceId::new(NodeId(2), 0, 10)),
        id: OpId::new(NodeId(2), 0, 10),
        parents: ParentSet::None,
        actor: ActorId(3),
        clock: Clock::UnixMs(1_700_000_000),
        scope: ScopeRef::None,
        tags: Tags::NOTE,
        kind: OpKind::GitLink(link_a),
    };
    let op_b = Op {
        source: Some(editchain_core::SourceId::new(NodeId(2), 0, 11)),
        id: OpId::new(NodeId(2), 0, 11),
        parents: ParentSet::None,
        actor: ActorId(3),
        clock: Clock::UnixMs(1_700_000_000),
        scope: ScopeRef::None,
        tags: Tags::NOTE,
        kind: OpKind::GitLink(link_b),
    };

    let proj = GitProjection::from_ops(&[op_a, op_b]);
    assert_eq!(proj.links_from(&source).len(), 2);
}

#[test]
fn projection_link_is_not_a_causal_parent() {
    // A git link op must not appear as a causal parent of the commit it links.
    let source = OpId::new(NodeId(1), 0, 5);
    let link_op = Op {
        source: Some(editchain_core::SourceId::new(NodeId(2), 0, 10)),
        id: OpId::new(NodeId(2), 0, 10),
        parents: ParentSet::None,
        actor: ActorId(3),
        clock: Clock::UnixMs(1_700_000_000),
        scope: ScopeRef::None,
        tags: Tags::NOTE,
        kind: OpKind::GitLink(GitLink {
            source,
            target_repo: RepositoryId(7),
            target_oid: sha1([0x01; 20]),
            kind: GitLinkKind::CommittedAs,
        }),
    };

    // The link's own causal parents are empty; the projection stores it under
    // `links`, never under `commits` or as a parent edge.
    assert!(matches!(link_op.parents, ParentSet::None));
    let proj = GitProjection::from_ops(&[link_op]);
    assert!(proj.commits().is_empty());
}

#[test]
fn migrated_commits_and_all_link_kinds_reach_the_editor_projection() {
    use editchain_import::batch::DurableOpSink as _;
    let temp = tempfile::tempdir().unwrap();
    let old = temp.path().join("old");
    let new = temp.path().join("new");
    let source = OpId::new(NodeId(1), 0, 5);
    let oid = sha1([1; 20]);
    let repo = RepositoryId(7);
    let mut ops = vec![
        commit_op(source, repo, oid),
        commit_op(OpId::new(NodeId(1), 0, 6), RepositoryId(8), oid),
    ];
    let kinds = [
        GitLinkKind::BasedOn,
        GitLinkKind::Checkpoint,
        GitLinkKind::CommittedAs,
        GitLinkKind::ProducedBy,
        GitLinkKind::Mentions,
        GitLinkKind::Custom(Payload::Inline(b"custom relation".to_vec())),
    ];
    for (index, kind) in kinds.iter().enumerate() {
        ops.push(Op {
            id: OpId::new(NodeId(2), 0, u64::try_from(index).unwrap()),
            kind: OpKind::GitLink(GitLink {
                source,
                target_repo: repo,
                target_oid: oid,
                kind: kind.clone(),
            }),
            ..ops.first().unwrap().clone()
        });
    }
    let mut log =
        editchain_store::LogStore::new(editchain_store::SegmentStore::open(&old).unwrap());
    let _admission = log.append_durable(&ops).unwrap();
    drop(log);
    let _report = editchain_import::activity::migrate(&old, &new, || false).unwrap();
    let mut converted = Vec::new();
    let _stats = editchain_store::visit_records(&new, &mut |_flags, bytes| {
        converted.push(editchain_store::format::decode_op(bytes).map_err(std::io::Error::other)?);
        Ok(())
    })
    .unwrap();
    let modern_source = converted
        .iter()
        .find_map(|op| {
            if let OpKind::Activity(record) = &op.kind {
                if record
                    .legacy
                    .as_ref()
                    .is_some_and(|old| old.operation == source)
                {
                    return Some(op.id);
                }
            }
            None
        })
        .unwrap();
    let projection = editchain_project::HistoryProjection::from_ops(converted);
    assert_eq!(projection.git().commits().len(), 2);
    assert_eq!(
        projection.git().commit(repo, &oid).unwrap().imported_record,
        Some(modern_source)
    );
    assert_eq!(
        projection
            .git()
            .links_from(&modern_source)
            .iter()
            .map(|link| &link.kind)
            .collect::<Vec<_>>(),
        kinds.iter().collect::<Vec<_>>()
    );
    assert!(projection
        .nodes()
        .iter()
        .any(|node| node.node_key() == editchain_core::GitCommitKey::new(repo, oid).to_string()));
}

#[test]
fn activity_git_links_keep_multiple_targets_and_ignore_unrelated_relations() {
    use editchain_core::activity::{Entity, Kind, Link, Operation};
    let source = OpId::new(NodeId(1), 0, 5);
    let repo = RepositoryId(7);
    let first = sha1([1; 20]);
    let second = sha1([2; 20]);
    let mut record = Operation::upgrade(&commit_op(source, repo, first)).unwrap();
    record.kind = Kind::Link(Link {
        from: Entity::Operation(source),
        to: vec![
            Entity::Git {
                repository: repo,
                oid: first,
            },
            Entity::Git {
                repository: repo,
                oid: second,
            },
        ],
        relation: "based_on".into(),
        content: Payload::Empty,
    });
    let op = record.clone().into_op().unwrap();
    assert_eq!(GitProjection::operation_links(&op).len(), 2);
    if let Kind::Link(link) = &mut record.kind {
        link.relation = "ProviderParent".into();
    }
    assert!(GitProjection::operation_links(&record.into_op().unwrap()).is_empty());
}
