mod content;
mod fields;
mod history;
mod provenance;
mod replay;

use editchain_engine::{
    GitAvailability, GitCommitEntity, GitOid, GitSignature, Op, OpKind, Payload, RepositoryId,
};

use super::record;

fn commit(sequence: u64, repository: RepositoryId) -> Op {
    record(
        sequence,
        OpKind::GitCommit(Box::new(GitCommitEntity {
            repository,
            object_format: editchain_engine::GitObjectFormat::Sha1,
            oid: GitOid::from_sha1([7; 20]),
            imported_record: Some(super::id(9)),
            availability: GitAvailability::ImportedOnly,
            tree: GitOid::from_sha1([8; 20]),
            parents: vec![GitOid::from_sha1([6; 20])],
            author: GitSignature {
                name: Payload::Empty,
                email: Payload::Empty,
                when: 1,
            },
            committer: GitSignature {
                name: Payload::Empty,
                email: Payload::Empty,
                when: 2,
            },
            authored_at: 1,
            committed_at: 2,
            message: Payload::Inline(b"recorded commit".to_vec()),
            imported_refs: vec![Payload::Inline(b"refs/heads/main".to_vec())],
            live_refs: vec![Payload::Inline(b"refs/heads/topic".to_vec())],
            changed_paths: vec![editchain_engine::PathId(42)],
        })),
    )
}
