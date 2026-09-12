//! Read-only Git relationship reconciliation over canonical imported history.
//!
//! Provider decoding belongs to `editchain_import::git_evidence`; Git lookup
//! belongs to `editchain_git`. This module decides which supported observations
//! justify durable links. Its returned operations are proposals for the caller's
//! existing append/checkpoint transaction, and no CLI or transport is required.

mod claude_baseline;
mod live;
mod produced;
pub(crate) use live::LiveReconciliation;

use std::path::{Path, PathBuf};

use editchain_core::Op;
use editchain_git::{discover_repositories, open_repository, RepositoryCatalog, RepositoryHandle};
use editchain_import::sink::FsBlobSink;
use editchain_store::CanonicalChain;

/// Optional historical session-baseline inference, distinct from exact commit output.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SessionBaselines {
    /// Reconcile successful produced-commit evidence only.
    Disabled,
    /// Also infer top-level Claude session baselines from covered branch reflogs.
    ClaudeReflog,
}

/// Missing links proposed by one complete reconciliation pass.
#[derive(Debug)]
pub(crate) struct GitReconciliation {
    /// Inferred Claude session baselines, carrying the existing inferred tag.
    pub(crate) base_links: Vec<Op>,
    /// Exact links supported by successful provider command completion output.
    pub(crate) produced_links: Vec<Op>,
}

/// Reconcile accepted stored and newly captured operations without writing.
///
/// Both policies share one complete discovered/opened repository set. Existing
/// link IDs and session baselines make replay idempotent. Ambiguous or missing
/// source/object evidence produces no link; stored operations remain unchanged.
/// The caller owns the writer lock when planning against a mutable chain and
/// persists these proposals together with its captured import batch.
///
/// # Errors
///
/// Returns canonical chain, payload-store, repository discovery, or repository
/// open errors. Incomplete discovery cannot establish cross-repository uniqueness.
pub(crate) fn reconcile_git_links(
    workspace: &Path,
    chain: &Path,
    imported: &[Op],
    baselines: SessionBaselines,
) -> Result<GitReconciliation, Box<dyn std::error::Error>> {
    let mut ops = reconciliation_ops(chain, imported)?;
    let blobs = FsBlobSink::open_read_only(chain.join("blobs"))?;
    let repositories = Repositories::discover(workspace)?;
    let base_links = match baselines {
        SessionBaselines::Disabled => Vec::new(),
        SessionBaselines::ClaudeReflog => {
            claude_baseline::derive_session_base_links(&repositories, &ops, blobs.as_ref())
        }
    };
    ops.extend(base_links.iter().cloned());
    let produced_links =
        produced::derive_produced_commit_links(&repositories, &ops, blobs.as_ref());
    Ok(GitReconciliation {
        base_links,
        produced_links,
    })
}

/// Fully opened catalog shared by every policy within one reconciliation.
#[derive(Debug)]
struct Repositories {
    workspace: PathBuf,
    catalog: RepositoryCatalog,
    handles: Vec<RepositoryHandle>,
}

impl Repositories {
    fn discover(workspace: &Path) -> Result<Self, Box<dyn std::error::Error>> {
        let entries = discover_repositories(workspace)?;
        let handles = entries
            .iter()
            .map(open_repository)
            .collect::<Result<_, _>>()?;
        Ok(Self {
            workspace: workspace.to_path_buf(),
            catalog: RepositoryCatalog::from_entries(entries),
            handles,
        })
    }
}

/// Read accepted chain operations and merge the current import batch for
/// produced-commit reconciliation.
///
/// Exact replay duplicates collapse by ID. Conflicting same-ID records are
/// excluded entirely, matching the authoritative reader's quarantine rule, so
/// malformed history can never become relationship evidence.
fn reconciliation_ops(chain: &Path, imported: &[Op]) -> Result<Vec<Op>, std::io::Error> {
    let mut corpus = CanonicalChain::read(chain)?;
    for op in imported {
        let _: editchain_core::Admission = corpus.insert(op.clone())?;
    }
    Ok(corpus.into_located_ops().map(|(op, _)| op).collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use editchain_store::format::encode_op;
    use editchain_store::format::Page;
    use editchain_store::SegmentStore;

    #[test]
    fn reconciliation_and_viewer_share_conflict_admission() {
        use editchain_core::{
            ActorId, Clock, MessageOp, NodeId, OpId, OpKind, ParentSet, Payload, ScopeRef, Tags,
        };

        let candidate = |seq, text: &[u8]| Op {
            id: OpId::new(NodeId(1), 0, seq),
            parents: ParentSet::None,
            actor: ActorId(1),
            clock: Clock::UnixMs(1),
            scope: ScopeRef::None,
            tags: Tags::MESSAGE,
            kind: OpKind::Message(MessageOp {
                content: Payload::Inline(text.to_vec()),
                content_type: Payload::Empty,
            }),
        };
        let stable = candidate(2, b"stable");
        for (first, conflicting) in [
            (b"one".as_slice(), b"two".as_slice()),
            (b"two".as_slice(), b"one".as_slice()),
        ] {
            let dir = tempfile::tempdir().unwrap();
            let chain = dir.path().join(".editchain");
            let first = candidate(1, first);
            let conflicting = candidate(1, conflicting);
            let mut store = SegmentStore::open(&chain).unwrap();
            let mut page = Page::new(0);
            for op in [&first, &first, &stable] {
                page.add_record(0, encode_op(op).unwrap());
            }
            store.append_page(&page).unwrap();
            assert_eq!(
                reconciliation_ops(&chain, &[conflicting.clone(), first.clone()]).unwrap(),
                vec![stable.clone()]
            );
            let mut next_page = Page::new(1);
            for op in [&conflicting, &first, &conflicting] {
                next_page.add_record(0, encode_op(op).unwrap());
            }
            store.append_page(&next_page).unwrap();
            let reconciled = reconciliation_ops(&chain, &[]).unwrap();
            let viewer =
                crate::history::Workspace::open(dir.path().to_str().unwrap(), ".editchain")
                    .unwrap();
            assert_eq!(reconciled, vec![stable.clone()]);
            assert_eq!(viewer.projection().ops(), reconciled);
            assert_eq!(viewer.diagnostics.chain.quarantined, 2);
            assert_eq!(viewer.diagnostics.chain.duplicates, 3);
        }
    }
}
