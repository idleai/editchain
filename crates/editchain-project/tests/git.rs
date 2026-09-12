//! Git projection identity, observation order, and stored-link contracts.

use editchain_index as _;
use serde as _;
use serde_json as _;

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
        id: OpId::new(NodeId(2), 0, 10),
        parents: ParentSet::None,
        actor: ActorId(3),
        clock: Clock::UnixMs(1_700_000_000),
        scope: ScopeRef::None,
        tags: Tags::NOTE,
        kind: OpKind::GitLink(link_a),
    };
    let op_b = Op {
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
