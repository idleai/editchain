//! Search content v1: public message/tool/command content, reflection summaries,
//! known file paths, Git messages/refs/full OIDs, and Git-link labels. Raw JSON,
//! private operations, file bodies, and auxiliary payloads are not searchable.

use std::collections::HashMap;

use editchain_core::{GitCommitEntity, GitCommitKey, Op, OpId, OpKind, Payload, Tags};
use editchain_index::{
    ChunkOptions, DocumentId, LexicalHit, LexicalIndex, LexicalIndexBuilder, SearchDocument,
};
use editchain_project::activity_view::ActivityView;
use editchain_project::NodeKey;
use editchain_protocol::{
    FileChangeDto, FindInHistoryMatch, FindInHistoryResponse, SnapshotId, MAX_QUERY_BYTES,
    MAX_SEARCH_RESULTS,
};

use super::{stale_snapshot, BlobResolution, ExpandedChildRow, Workspace};

/// A completed index paired with the exact source/view it may resolve against.
#[derive(Debug)]
pub struct SearchIndexState {
    snapshot_id: SnapshotId,
    index: LexicalIndex,
}

/// Build search content from the opened corpus, verifying its pinned inputs
/// before and after reading. Only eligible payload fields are fully resolved.
///
/// # Errors
///
/// Returns stale-source, initialization, or document indexing errors. Publication
/// happens only after the complete build and both source checks succeed.
pub fn build_lexical_index(
    workspace: &mut Workspace,
) -> Result<SearchIndexState, Box<dyn std::error::Error>> {
    workspace.ensure_projection_loaded()?;
    workspace.ensure_sources_current()?;
    let mut builder = LexicalIndexBuilder::new(ChunkOptions::default())?;
    let mut source_commits = HashMap::new();
    for op in &workspace.source_ops {
        if let OpKind::GitCommit(commit) = &op.kind {
            let _previous = source_commits.insert(commit.key(), (op, commit.as_ref()));
            continue;
        }
        let Some(text) = operation_text(op, &workspace.agent_file_changes, |payload| {
            payload_text(workspace, payload)
        }) else {
            continue;
        };
        let oid = if let OpKind::GitLink(link) = &op.kind {
            Some(link.target_oid.to_hex())
        } else {
            None
        };
        let exact_terms: Vec<_> = if matches!(op.kind, OpKind::File(_)) {
            text.lines().collect()
        } else {
            oid.iter().map(String::as_str).collect()
        };
        builder.add_document(&SearchDocument {
            id: DocumentId::Operation(op.id),
            text: &text,
            exact_terms: &exact_terms,
        })?;
    }
    for commit in workspace.projection.git().commits().values() {
        let source = source_commits.get(&commit.key()).copied();
        if source.is_some_and(|(op, _)| op.tags.matches_any(Tags::PRIVATE)) {
            continue;
        }
        let original = source.map_or(commit, |(_, original)| original);
        let files = workspace
            .git_file_changes
            .get(&(commit.repository, commit.oid))
            .map(Vec::as_slice);
        let text = git_text(workspace, original, commit, files);
        let oid = commit.oid.to_hex();
        let mut exact_terms = vec![oid.as_str()];
        if let Some(files) = files {
            exact_terms.extend(files.iter().flat_map(|change| {
                std::iter::once(change.path.as_str()).chain(change.old_path.as_deref())
            }));
        }
        builder.add_document(&SearchDocument {
            id: DocumentId::GitCommit(GitCommitKey::new(commit.repository, commit.oid)),
            text: &text,
            exact_terms: &exact_terms,
        })?;
    }
    let index = builder.publish()?;
    workspace.ensure_sources_current()?;
    Ok(SearchIndexState {
        snapshot_id: workspace.snapshot_id.clone(),
        index,
    })
}

fn operation_text(
    op: &Op,
    files: &HashMap<OpId, Vec<FileChangeDto>>,
    mut resolve: impl FnMut(&Payload) -> Option<String>,
) -> Option<String> {
    if op.tags.matches_any(Tags::PRIVATE) {
        return None;
    }
    match &op.kind {
        OpKind::Message(message) => resolve(&message.content),
        OpKind::Tool(tool) => resolve(&tool.content),
        OpKind::Command(command) => resolve(&command.content),
        OpKind::Reflection(reflection) => resolve(&reflection.summary),
        OpKind::File(_) => files
            .get(&op.id)
            .or_else(|| op.parents.iter().find_map(|parent| files.get(parent)))
            .map(|changes| file_paths(changes)),
        OpKind::GitLink(link) => Some(format!("git:{} {:?}", link.target_oid, link.kind)),
        OpKind::ChainStart(_)
        | OpKind::Actor(_)
        | OpKind::Import(_)
        | OpKind::Note(_)
        | OpKind::Error(_)
        | OpKind::GitCommit(_)
        | OpKind::Unknown(_) => None,
    }
}

fn payload_text(workspace: &Workspace, payload: &Payload) -> Option<String> {
    match payload {
        Payload::Inline(bytes) => Some(String::from_utf8_lossy(bytes).into_owned()),
        Payload::Blob(blob) => match workspace.blob_resolver.as_ref()?.resolve(blob) {
            BlobResolution::Found(bytes) => Some(String::from_utf8_lossy(&bytes).into_owned()),
            BlobResolution::Missing | BlobResolution::Corrupt | BlobResolution::Unresolvable => {
                None
            }
        },
        Payload::Empty => None,
    }
}

fn file_paths(changes: &[FileChangeDto]) -> String {
    changes
        .iter()
        .flat_map(|change| std::iter::once(change.path.as_str()).chain(change.old_path.as_deref()))
        .collect::<Vec<_>>()
        .join("\n")
}

fn git_text(
    workspace: &Workspace,
    commit: &GitCommitEntity,
    observed: &GitCommitEntity,
    files: Option<&[FileChangeDto]>,
) -> String {
    let mut parts = vec![commit.oid.to_hex()];
    parts.extend(payload_text(workspace, &commit.message));
    for reference in commit
        .imported_refs
        .iter()
        .chain(&commit.live_refs)
        .chain(&observed.live_refs)
    {
        if let Some(text) = payload_text(workspace, reference) {
            if !parts.contains(&text) {
                parts.push(text);
            }
        }
    }
    if let Some(files) = files {
        parts.push(file_paths(files));
    }
    parts.join("\n")
}

#[derive(Debug, Default)]
struct VisibleMatches {
    best: HashMap<usize, f64>,
}

impl VisibleMatches {
    fn add(&mut self, view: &ActivityView<ExpandedChildRow>, hits: &[LexicalHit]) {
        for hit in hits {
            let source = match hit.document {
                DocumentId::Operation(id) => NodeKey::Op(id),
                DocumentId::GitCommit(key) => NodeKey::Git(key),
            };
            if let Some(row) = view.source_row(source) {
                let score = self.best.entry(row).or_insert(hit.score);
                *score = score.max(hit.score);
            }
        }
    }

    fn ranked(&self) -> Vec<(usize, f64)> {
        let mut ranked: Vec<_> = self
            .best
            .iter()
            .map(|(row, score)| (*row, *score))
            .collect();
        ranked.sort_unstable_by(|(row_a, score_a), (row_b, score_b)| {
            score_b.total_cmp(score_a).then(row_a.cmp(row_b))
        });
        ranked
    }
}

fn resolve_rows(
    view: &ActivityView<ExpandedChildRow>,
    ranked: &[(usize, f64)],
) -> Vec<FindInHistoryMatch> {
    ranked
        .iter()
        .filter_map(|(row, _)| {
            let node = view.entries().get(*row)?.node();
            Some(FindInHistoryMatch {
                node_key: node.node_key(),
                row: u64::try_from(*view.starts().get(*row)?).ok()?,
            })
        })
        .collect()
}

#[cfg(test)]
impl Workspace {
    /// Resolve real document identities to distinct visible rows of this fixed
    /// Activity view, keeping the best score and breaking ties by newest row.
    #[must_use]
    pub(super) fn find_in_history(&mut self, chunks: &[LexicalHit]) -> Vec<FindInHistoryMatch> {
        self.ensure_view_snapshot();
        let Some(view) = self.current_view.as_ref() else {
            return Vec::new();
        };
        let mut matches = VisibleMatches::default();
        matches.add(view, chunks);
        resolve_rows(view, &matches.ranked())
    }
}

impl SearchIndexState {
    /// Inspect real candidate identities without changing the published index.
    #[must_use]
    pub const fn index(&self) -> &LexicalIndex {
        &self.index
    }

    /// Find up to `top_k` distinct visible rows in the compatible opened view.
    /// Candidate continuation scans at most 16,384 chunks; `more` remains true
    /// when unseen candidates could contain further visible matches.
    ///
    /// # Errors
    ///
    /// Rejects mismatched snapshots, invalid result limits, and query failures.
    pub fn find(
        &self,
        workspace: &mut Workspace,
        query: &str,
        top_k: usize,
    ) -> Result<FindInHistoryResponse, Box<dyn std::error::Error>> {
        self.find_with_budget(workspace, query, top_k, 16_384)
    }

    fn find_with_budget(
        &self,
        workspace: &mut Workspace,
        query: &str,
        top_k: usize,
        budget: usize,
    ) -> Result<FindInHistoryResponse, Box<dyn std::error::Error>> {
        if self.snapshot_id != workspace.snapshot_id {
            return Err(stale_snapshot().into());
        }
        if top_k == 0 || top_k > MAX_SEARCH_RESULTS {
            return Err("visible search limit must be between 1 and 1000".into());
        }
        if query.len() > MAX_QUERY_BYTES {
            return Err("search query exceeds 16384 bytes".into());
        }
        workspace.ensure_view_snapshot();
        let view = workspace.current_view.as_ref().ok_or("no search view")?;
        let mut candidates = self.index.candidates(query, budget)?;
        let mut matches = VisibleMatches::default();
        let mut page_size = 256;
        loop {
            let page = candidates.next_page(page_size)?;
            matches.add(view, &page.hits);
            let mut ranked = matches.ranked();
            let enough = ranked.len() > top_k;
            // Finish the boundary score group so equal BM25 scores use the
            // view's newest-row ordering, including across candidate pages.
            let crossed_boundary = ranked
                .get(top_k.saturating_sub(1))
                .zip(page.hits.last())
                .is_some_and(|((_, score), hit)| hit.score < *score);
            if !page.more || candidates.remaining_budget() == 0 || (enough && crossed_boundary) {
                ranked.truncate(top_k);
                return Ok(FindInHistoryResponse {
                    snapshot_id: self.snapshot_id.clone(),
                    matches: resolve_rows(view, &ranked),
                    more: enough || page.more,
                });
            }
            page_size = page_size.saturating_mul(2).min(4096);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use editchain_core::{
        ActorId, BlobRef, Clock, CommandOp, CommandStage, ContentId, FileEdit, FileOp, FileStage,
        FrontierSet, GitLink, GitLinkKind, GitOid, ImportOp, MessageOp, NodeId, ParentSet, PathId,
        ReflectionOp, RepositoryId, ScopeRef, SessionId, ToolOp, ToolStage, WindowRef,
    };
    use editchain_project::HistoryProjection;

    fn operation(seq: u64, kind: OpKind) -> Op {
        Op {
            id: OpId::new(NodeId(1), 0, seq),
            parents: ParentSet::None,
            actor: ActorId(1),
            clock: Clock::UnixMs(seq),
            scope: ScopeRef::None,
            tags: Tags::MESSAGE | Tags::AGENT,
            kind,
        }
    }

    fn message(seq: u64, text: &str) -> Op {
        operation(
            seq,
            OpKind::Message(MessageOp {
                content: Payload::Inline(text.as_bytes().to_vec()),
                content_type: Payload::Empty,
            }),
        )
    }

    fn file_change() -> FileChangeDto {
        serde_json::from_value(serde_json::json!({
            "source": "agent", "path": "src/new_module.rs", "old_path": "src/old_module.rs", "status": "renamed"
        })).unwrap()
    }

    #[test]
    fn eligible_content_is_selected_before_any_payload_resolution() {
        let content = Payload::Blob(BlobRef {
            id: ContentId::Hash256([1; 32]),
            len: 99,
        });
        let auxiliary = Payload::Blob(BlobRef {
            id: ContentId::Hash256([2; 32]),
            len: 100_000,
        });
        let kinds = [
            OpKind::Message(MessageOp {
                content: content.clone(),
                content_type: auxiliary.clone(),
            }),
            OpKind::Tool(ToolOp {
                tool_call_id: auxiliary.clone(),
                tool_name: auxiliary.clone(),
                stage: ToolStage::Start,
                content: content.clone(),
            }),
            OpKind::Command(CommandOp {
                command_id: auxiliary.clone(),
                content: content.clone(),
                stage: CommandStage::Output,
            }),
            OpKind::Reflection(ReflectionOp {
                scope: ScopeRef::None,
                covers: FrontierSet::new(),
                window: WindowRef {
                    start_seq: 0,
                    end_seq: 1,
                },
                summary: content.clone(),
                anchors: auxiliary.clone(),
            }),
        ];
        for kind in kinds {
            let mut op = operation(1, kind);
            let mut resolved = Vec::new();
            let text = operation_text(&op, &HashMap::new(), |payload| {
                resolved.push(payload.clone());
                Some("hello world".to_owned())
            });
            assert_eq!(text.as_deref(), Some("hello world"));
            assert_eq!(resolved, vec![content.clone()]);
            op.tags |= Tags::PRIVATE;
            let private = operation_text(&op, &HashMap::new(), |payload| {
                resolved.push(payload.clone());
                Some("secret".to_owned())
            });
            assert!(private.is_none());
            assert_eq!(resolved.len(), 1, "private operations resolve no payloads");
        }
        let raw = operation(
            2,
            OpKind::Import(ImportOp {
                raw_ref: auxiliary.clone(),
                raw_hash: None,
            }),
        );
        let file = operation(
            3,
            OpKind::File(FileOp {
                path: PathId(987_654_321),
                stage: FileStage::Applied,
                edit: FileEdit::UnifiedDiff(auxiliary),
                base: None,
                after: None,
            }),
        );
        let files = HashMap::from([(file.id, vec![file_change()])]);
        let mut reads = 0usize;
        for op in [&raw, &file] {
            let text = operation_text(op, &files, |_| {
                reads = reads.saturating_add(1);
                None
            });
            if op.id == file.id {
                assert_eq!(
                    text.as_deref(),
                    Some("src/new_module.rs\nsrc/old_module.rs")
                );
            } else {
                assert!(text.is_none());
            }
        }
        assert_eq!(
            reads, 0,
            "raw records and file bodies are never hydrated for search"
        );
    }

    #[test]
    fn git_link_text_and_known_paths_are_searchable_by_real_identity() {
        let oid = GitOid::from_sha256([0xab; 32]);
        let link = operation(
            1,
            OpKind::GitLink(GitLink {
                source: OpId::new(NodeId(1), 0, 9),
                target_repo: RepositoryId(7),
                target_oid: oid,
                kind: GitLinkKind::CommittedAs,
            }),
        );
        let text = operation_text(&link, &HashMap::new(), |_| None).unwrap();
        assert!(text.contains("git:"));
        assert!(text.contains(&oid.to_hex()));
        let file = operation(
            2,
            OpKind::File(FileOp {
                path: PathId(987_654_321),
                stage: FileStage::Applied,
                edit: FileEdit::None,
                base: None,
                after: None,
            }),
        );
        let mut workspace = Workspace::from_projection(HistoryProjection::from_ops(vec![
            link.clone(),
            file.clone(),
        ]));
        workspace.agent_file_changes = HashMap::from([(file.id, vec![file_change()])]);
        let state = build_lexical_index(&mut workspace).unwrap();
        for (query, id) in [
            (oid.to_hex(), link.id),
            ("\"src/new_module.rs\"".to_owned(), file.id),
            ("old_module".to_owned(), file.id),
        ] {
            let page = state
                .index
                .candidates(&query, 10)
                .unwrap()
                .next_page(10)
                .unwrap();
            assert!(
                page.hits
                    .iter()
                    .any(|hit| hit.document == DocumentId::Operation(id)),
                "query {query}"
            );
        }
        assert!(state
            .index
            .candidates("987654321", 10)
            .unwrap()
            .next_page(10)
            .unwrap()
            .hits
            .is_empty());
    }

    #[test]
    fn imported_git_uses_complete_source_text_and_one_real_identity() {
        use editchain_core::{GitAvailability, GitObjectFormat, GitSignature};
        use editchain_import::{BlobSink as _, FsBlobSink};
        let dir = tempfile::tempdir().unwrap();
        let mut blobs = FsBlobSink::new(dir.path().join("blobs")).unwrap();
        let body = format!("prefix {} tailneedle", "body ".repeat(10_000));
        let payload = Payload::Blob(blobs.put(body.as_bytes()).unwrap());
        let signature = GitSignature {
            name: Payload::Empty,
            email: Payload::Empty,
            when: 1,
        };
        let commit = GitCommitEntity {
            repository: RepositoryId(7),
            object_format: GitObjectFormat::Sha1,
            oid: GitOid::from_sha1([7; 20]),
            tree: GitOid::from_sha1([8; 20]),
            imported_record: None,
            availability: GitAvailability::ImportedOnly,
            parents: Vec::new(),
            author: signature.clone(),
            committer: signature,
            authored_at: 1,
            committed_at: 1,
            message: payload,
            imported_refs: vec![Payload::Inline(b"refs/heads/importneedle".to_vec())],
            live_refs: Vec::new(),
            changed_paths: Vec::new(),
        };
        let op = operation(1, OpKind::GitCommit(Box::new(commit.clone())));
        let mut workspace = Workspace::from_projection(HistoryProjection::from_ops(vec![op]));
        workspace.blob_resolver = Some(super::super::BlobResolver::open(dir.path()).unwrap());
        let mut preview = commit.clone();
        preview.message = Payload::Inline(b"prefix preview only".to_vec());
        preview.live_refs = vec![Payload::Inline(b"refs/heads/liveneedle".to_vec())];
        workspace.projection.merge_git_commits(vec![preview]);
        let state = build_lexical_index(&mut workspace).unwrap();
        for query in ["tailneedle", "importneedle", "liveneedle"] {
            let page = state
                .index
                .candidates(query, 100)
                .unwrap()
                .next_page(100)
                .unwrap();
            assert!(
                !page.hits.is_empty(),
                "full source and observed refs: {query}"
            );
            assert!(page
                .hits
                .iter()
                .all(|hit| hit.document == DocumentId::GitCommit(commit.key())));
        }
        workspace.blob_resolver = None;
        let unavailable = build_lexical_index(&mut workspace).unwrap();
        assert!(
            unavailable
                .index
                .candidates("preview", 10)
                .unwrap()
                .next_page(10)
                .unwrap()
                .hits
                .is_empty(),
            "an unavailable source payload cannot fall back to a display preview"
        );
        workspace.source_ops.first_mut().unwrap().tags |= Tags::PRIVATE;
        let private = build_lexical_index(&mut workspace).unwrap();
        assert_eq!(
            private.index.num_docs(),
            0,
            "the projected commit cannot bypass a private source operation"
        );
    }

    #[test]
    fn visible_limit_continues_past_hidden_and_repeated_chunks() {
        let visible_long = message(1, &"needle ".repeat(10_000));
        let visible_short = message(2, "needle another visible row");
        let mut hidden = operation(
            3,
            OpKind::Import(ImportOp {
                raw_ref: Payload::Inline(br#"{"type":"user"}"#.to_vec()),
                raw_hash: None,
            }),
        );
        hidden.clock = Clock::None;
        hidden.scope = ScopeRef::Session(SessionId(20));
        let mut child = message(4, &"needle ".repeat(140_000));
        child.parents = ParentSet::One(hidden.id);
        child.scope = hidden.scope;
        child.clock = Clock::None;
        let mut workspace = Workspace::from_projection(HistoryProjection::from_ops(vec![
            visible_long.clone(),
            visible_short.clone(),
            hidden,
            child,
        ]));
        let state = build_lexical_index(&mut workspace).unwrap();
        assert!(
            state.index.num_docs() > 256,
            "fixture requires candidate continuation"
        );
        let bounded = state
            .find_with_budget(&mut workspace, "needle", 2, 4)
            .unwrap();
        assert!(bounded.matches.len() < 2);
        assert!(
            bounded.more,
            "unscanned candidates are reported even without visible matches"
        );
        let complete = state.find(&mut workspace, "needle", 2).unwrap();
        assert_eq!(complete.matches.len(), 2);
        assert!(
            !complete.more,
            "all remaining chunks belong to hidden or already returned rows"
        );
        let mut keys: Vec<_> = complete
            .matches
            .iter()
            .map(|hit| hit.node_key.clone())
            .collect();
        keys.sort();
        assert_eq!(
            keys,
            vec![visible_long.id.to_string(), visible_short.id.to_string()]
        );
    }

    #[test]
    fn equal_scores_finish_across_pages_and_snapshot_mismatch_is_rejected() {
        let ops: Vec<_> = (1..=300).map(|seq| message(seq, "needle")).collect();
        let newest = ops.last().unwrap().id.to_string();
        let mut workspace = Workspace::from_projection(HistoryProjection::from_ops(ops));
        let state = build_lexical_index(&mut workspace).unwrap();
        let first = state.find(&mut workspace, "needle", 1).unwrap();
        assert_eq!(first.matches.first().unwrap().node_key, newest);
        assert!(first.more);
        let complete = state.find(&mut workspace, "needle", 300).unwrap();
        assert_eq!(complete.matches.len(), 300);
        assert!(
            !complete.more,
            "exactly reaching the visible limit is not truncation"
        );
        assert!(state.find(&mut workspace, "needle", 0).is_err());
        assert!(state.find(&mut workspace, "needle", 1001).is_err());
        let empty = state.find(&mut workspace, "missing", 1).unwrap();
        assert!(empty.matches.is_empty());
        assert!(!empty.more);
        let mut other = Workspace::from_projection(HistoryProjection::new());
        let error = state.find(&mut other, "needle", 1).unwrap_err();
        assert_eq!(
            error
                .downcast_ref::<editchain_protocol::ServiceError>()
                .unwrap()
                .code,
            editchain_protocol::ErrorCode::StaleSnapshot
        );
    }

    #[test]
    fn a_long_matching_document_is_one_exhausted_visible_match() {
        let op = message(1, &"needle ".repeat(5000));
        let mut workspace =
            Workspace::from_projection(HistoryProjection::from_ops(vec![op.clone()]));
        let state = build_lexical_index(&mut workspace).unwrap();
        assert!(state.index.num_docs() > 1);
        let response = state.find(&mut workspace, "needle", 1).unwrap();
        assert_eq!(response.matches.len(), 1);
        assert_eq!(
            response.matches.first().unwrap().node_key,
            op.id.to_string()
        );
        assert!(!response.more);
    }
}
