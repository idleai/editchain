//! Create a synthetic AI file and its canonical provenance for VS Code tests.

use clap as _;
use ctrlc as _;
use dirs as _;
use editchain_index as _;
use editchain_project as _;
use editchain_protocol as _;
use serde as _;
use serde_json as _;
use std::fmt::Write as _;
use tantivy as _;
use tempfile as _;

use editchain_core::{
    ActorId, Clock, ContentId, FileEdit, FileOp, FileStage, GitLink, GitLinkKind, GitOid, ImportOp,
    NodeId, NoteOp, NoteRelationship, Op, OpId, OpKind, ParentSet, Payload, ScopeRef, Tags,
};
use editchain_store::{
    format::{encode_op, Page},
    BlobStore, SegmentStore,
};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let root = std::path::PathBuf::from(
        std::env::args()
            .nth(1)
            .ok_or("expected synthetic workspace path")?,
    );
    if root.join(".git").exists() || root.join(".editchain").exists() || root.join("ai.ts").exists()
    {
        return Err("fixture destination already exists".into());
    }
    let (repository, head) = seed_git(&root)?;
    let mut text = String::new();
    for index in 0..200 {
        writeln!(&mut text, "const ai_line_{index:03} = {index};")?;
    }
    let mut store = SegmentStore::open(root.join(".editchain"))?;
    let mut blobs = BlobStore::new(root.join(".editchain/blobs"))?;
    blobs.write(b"")?;
    blobs.write(text.as_bytes())?;
    let id = OpId::new(NodeId(83), 0, 1);
    let anchor = Op {
        id,
        parents: ParentSet::None,
        actor: ActorId(83),
        clock: Clock::UnixMs(2000),
        scope: ScopeRef::Session(editchain_core::SessionId(83)),
        tags: Tags::IMPORT,
        kind: OpKind::Import(ImportOp {
            raw_ref: Payload::Inline(b"{\"type\":\"agent_generated_example\"}".to_vec()),
            raw_hash: None,
        }),
    };
    let file_id = OpId::new(NodeId(83), 0, 2);
    let file = Op {
        id: file_id,
        parents: ParentSet::One(id),
        actor: ActorId(83),
        clock: Clock::UnixMs(2000),
        scope: ScopeRef::Session(editchain_core::SessionId(83)),
        tags: Tags::AGENT | Tags::FILE,
        kind: OpKind::File(FileOp {
            path: editchain_import::derive_path_id("ai.ts"),
            stage: FileStage::Applied,
            base: Some(ContentId::Hash256(*blake3::hash(b"").as_bytes())),
            after: Some(ContentId::Hash256(
                *blake3::hash(text.as_bytes()).as_bytes(),
            )),
            edit: FileEdit::None,
        }),
    };
    let note = Op {
        id: OpId::new(NodeId(83), 0, 3),
        parents: ParentSet::One(id),
        actor: ActorId(83),
        clock: Clock::UnixMs(2000),
        scope: ScopeRef::Session(editchain_core::SessionId(83)),
        tags: Tags::NOTE,
        kind: OpKind::Note(NoteOp {
            target_ids: vec![file_id],
            relationship: NoteRelationship::Explains,
            content: Payload::Inline(b"ai.ts".to_vec()),
        }),
    };
    let link = Op {
        id: OpId::new(NodeId(83), 0, 4),
        parents: ParentSet::One(id),
        tags: Tags::META,
        kind: OpKind::GitLink(GitLink {
            source: id,
            target_repo: repository,
            target_oid: head,
            kind: GitLinkKind::BasedOn,
        }),
        ..anchor.clone()
    };
    let mut page = Page::new(0);
    for op in [&anchor, &file, &note, &link] {
        page.add_record(0, encode_op(op)?);
    }
    store.append_page(&page)?;
    std::fs::write(root.join("ai.ts"), text)?;
    drop(store);
    let _prepared =
        editchain_node::history::prepare_live_checkpoint(&root, &root.join(".editchain"))?;
    Ok(())
}

fn seed_git(
    root: &std::path::Path,
) -> Result<(editchain_core::RepositoryId, GitOid), Box<dyn std::error::Error>> {
    for args in [
        vec!["init", "-q"],
        vec!["commit", "-q", "--allow-empty", "-m", "Shared Git baseline"],
    ] {
        let result = std::process::Command::new("git")
            .args([
                "-c",
                "user.name=Fixture",
                "-c",
                "user.email=fixture@example.test",
            ])
            .args(args)
            .current_dir(root)
            .env("GIT_AUTHOR_DATE", "1970-01-01T00:00:01+0000")
            .env("GIT_COMMITTER_DATE", "1970-01-01T00:00:01+0000")
            .output()?;
        if !result.status.success() {
            return Err(String::from_utf8_lossy(&result.stderr).into_owned().into());
        }
    }
    let discovery = editchain_git::RepositoryDiscovery::from_path(root)?;
    let handle = editchain_git::open_repository(&discovery)?;
    let head =
        GitOid::from_hex(&handle.repo.head_id()?.to_string()).ok_or("fixture HEAD invalid")?;
    Ok((discovery.id, head))
}
