//! Bounded windows, direct anchor lookup and a resident lexical index.

use super::{LiveWorkspace, Result};
use editchain_project::live::LiveRow;
use editchain_protocol::{
    rank::Axis, ErrorCode, FindInHistoryMatch, FindInHistoryResponse, GetWindowRequest,
    HistoryWindow, LiveBlock, LocateRowsResponse, RequestBody, ResponseBody, RowLocation,
    ServiceError, SnapshotResult,
};
use tantivy::{
    collector::TopDocs,
    query::QueryParser,
    schema::{Field, Schema, Value as _, STORED, STRING, TEXT},
    Index, IndexReader, IndexWriter, TantivyDocument, Term,
};

pub(super) struct LiveSearch {
    index: Index,
    writer: IndexWriter,
    reader: IndexReader,
    key: Field,
    text: Field,
    dirty: bool,
}

impl std::fmt::Debug for LiveSearch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LiveSearch")
            .field("dirty", &self.dirty)
            .finish_non_exhaustive()
    }
}

impl LiveSearch {
    pub(super) fn open(path: &std::path::Path, fresh: bool) -> Result<Self> {
        let mut schema = Schema::builder();
        let key = schema.add_text_field("key", STRING | STORED);
        let text = schema.add_text_field("text", TEXT);
        std::fs::create_dir_all(path)?;
        let index = if path.join("meta.json").try_exists()? {
            Index::open_in_dir(path)?
        } else {
            if !fresh {
                return Err("live search checkpoint is missing; close History, remove the derived live-v1 directory and run prepare-view".into());
            }
            Index::create_in_dir(path, schema.build())?
        };
        let writer = index.writer_with_num_threads(1, 20_000_000)?;
        if fresh {
            let _stamp = writer.delete_all_documents()?;
        }
        let reader = index.reader()?;
        Ok(Self {
            index,
            writer,
            reader,
            key,
            text,
            dirty: fresh,
        })
    }

    pub(super) fn put(&mut self, block: &LiveBlock) -> Result<()> {
        self.remove(&block.meta.key);
        let text = block
            .rows
            .iter()
            .map(|row| row.summary.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        let _stamp = self
            .writer
            .add_document(tantivy::doc!(self.key => block.meta.key.clone(), self.text => text))?;
        self.dirty = true;
        Ok(())
    }

    pub(super) fn remove(&mut self, key: &str) {
        let _stamp = self
            .writer
            .delete_term(Term::from_field_text(self.key, key));
        self.dirty = true;
    }

    pub(super) fn flush(&mut self) -> Result<()> {
        if self.dirty {
            let _stamp = self.writer.commit()?;
            self.reader.reload()?;
            self.dirty = false;
        }
        Ok(())
    }

    fn find(&mut self, query: &str, limit: usize) -> Result<Vec<String>> {
        self.flush()?;
        let parser = QueryParser::for_index(&self.index, vec![self.text]);
        let query = parser.parse_query(query)?;
        let searcher = self.reader.searcher();
        let hits = searcher.search(query.as_ref(), &TopDocs::with_limit(limit))?;
        let mut keys = Vec::new();
        for (_, address) in hits {
            let document: TantivyDocument = searcher.doc(address)?;
            if let Some(key) = document
                .get_first(self.key)
                .and_then(|value| value.as_str())
            {
                keys.push(key.to_owned());
            }
        }
        Ok(keys)
    }
}

impl LiveWorkspace {
    pub(crate) fn handle(&mut self, request: &RequestBody) -> Result<ResponseBody> {
        match editchain_index::boundary(|| {
            let result = self.handle_inner(request)?;
            if !matches!(request, RequestBody::SyncLive(_)) {
                self.unload()?;
            }
            Ok(result)
        }) {
            Ok(result) => result,
            Err(error) => {
                self.poisoned = true;
                Err(error.into())
            }
        }
    }

    fn handle_inner(&mut self, request: &RequestBody) -> Result<ResponseBody> {
        if self.poisoned {
            return Err(super::super::stale_snapshot().into());
        }
        if request
            .snapshot_id()
            .is_some_and(|id| id != &self.snapshot_id)
        {
            return Err(super::super::stale_snapshot().into());
        }
        let value = match request {
            RequestBody::ViewportLive(viewport) => {
                if !self.paged() {
                    return Err("native disclosure was not negotiated".into());
                }
                let revision = self.revision;
                self.observe_viewport(viewport)?;
                serde_json::to_value(editchain_protocol::LiveUpdate {
                    epoch: self.epoch.clone(),
                    revision: self.revision,
                    deltas: self
                        .journal
                        .iter()
                        .filter(|delta| delta.revision > revision)
                        .cloned()
                        .collect(),
                    work: editchain_protocol::LiveWork::default(),
                })?
            }
            RequestBody::ToggleLive(request) => {
                if !self.paged() {
                    return Err("native disclosure was not negotiated".into());
                }
                let revision = self.revision;
                self.toggle_disclosure(&request.key, request.task)?;
                serde_json::to_value(editchain_protocol::LiveUpdate {
                    epoch: self.epoch.clone(),
                    revision: self.revision,
                    deltas: self
                        .journal
                        .iter()
                        .filter(|delta| delta.revision > revision)
                        .cloned()
                        .collect(),
                    work: editchain_protocol::LiveWork::default(),
                })?
            }
            RequestBody::SyncLive(request) => serde_json::to_value(self.sync(request)?)?,
            RequestBody::GetWindow(request) => serde_json::to_value(self.window(request)?)?,
            RequestBody::LocateRows(request) => serde_json::to_value(self.locate(&request.keys)?)?,
            RequestBody::FindInHistory(request) => {
                serde_json::to_value(self.find(&request.query, request.top_k)?)?
            }
            RequestBody::GetNodeDetails(request) => {
                let workspace = self.details_workspace(&request.op_id)?;
                let details = workspace
                    .node_details(Some(request.op_id.clone()), None)
                    .ok_or("live node unavailable")?;
                serde_json::to_value(SnapshotResult {
                    snapshot_id: self.snapshot_id.clone(),
                    value: details,
                })?
            }
            RequestBody::GetFileDiff(request) => {
                let workspace =
                    if request.change.source == editchain_protocol::FileChangeSource::Git {
                        let repository = super::super::parse_repository_id(
                            request
                                .change
                                .repository
                                .as_deref()
                                .ok_or("missing Git repository")?,
                        )?;
                        let oid = super::super::parse_git_oid(
                            request
                                .change
                                .commit_oid
                                .as_deref()
                                .ok_or("missing Git commit")?,
                        )?;
                        self.git_workspace(
                            self.git
                                .commit(repository, oid)
                                .ok_or("Git commit is unavailable")?,
                        )
                    } else {
                        self.details_workspace(
                            request
                                .change
                                .op_id
                                .as_deref()
                                .ok_or("live file has no source operation")?,
                        )?
                    };
                let diff = workspace.file_diff(&request.change)?;
                serde_json::to_value(SnapshotResult {
                    snapshot_id: self.snapshot_id.clone(),
                    value: diff,
                })?
            }
            RequestBody::ResolveObject(request) => {
                let repository = super::super::parse_repository_id(&request.repository)?;
                let oid = super::super::parse_git_oid(&request.oid)?;
                let commit = self
                    .git
                    .commit(repository, oid)
                    .ok_or("Git object is unavailable")?;
                serde_json::to_value(SnapshotResult {
                    snapshot_id: self.snapshot_id.clone(),
                    value: super::super::resolved_object_from_commit(commit),
                })?
            }
            RequestBody::Open(_)
            | RequestBody::OpenLive(_)
            | RequestBody::OpenLivePaged(_)
            | RequestBody::Refresh(_) => {
                return Err(ServiceError::new(
                    ErrorCode::InvalidInput,
                    "open must establish a new live runtime",
                )
                .into())
            }
        };
        Ok(ResponseBody::Ok(value))
    }

    fn details_workspace(&self, id: &str) -> Result<super::super::Workspace> {
        let id = editchain_core::OpId::from_display_str(id).ok_or("invalid operation identity")?;
        if let Some(input) = self.owners.get(&id).and_then(|key| self.inputs.get(key)) {
            return Ok(self.local_workspace(input));
        }
        let op = self
            .tail
            .chain()
            .get(id)
            .ok_or("live operation unavailable")?
            .clone();
        Ok(self.local_workspace(&LiveRow {
            task: None,
            key: id.to_string(),
            anchor: id,
            incarnation: id,
            operations: vec![std::sync::Arc::new(op)],
        }))
    }

    fn window(&self, request: &GetWindowRequest) -> Result<HistoryWindow> {
        if self.paged() {
            return self.paged_window(request);
        }
        let mut offset = request.offset;
        let end = offset
            .saturating_add(request.limit)
            .min(self.blocks.measure().expanded);
        let mut rows = Vec::new();
        while offset < end {
            let Some((_, block, start)) = self.blocks.select(offset, Axis::Expanded) else {
                break;
            };
            let first =
                usize::try_from(offset.saturating_sub(start.expanded)).unwrap_or(usize::MAX);
            let count = usize::try_from(end.saturating_sub(offset)).unwrap_or(usize::MAX);
            let content = self.rows.rows(block)?;
            for source in content.iter().skip(first).take(count) {
                let mut row = source.clone();
                row.parent_row = row
                    .parent_row
                    .and_then(|parent| parent.checked_add(usize::try_from(start.expanded).ok()?));
                self.graph.decorate(
                    &block.meta.key,
                    offset.saturating_sub(start.expanded),
                    &mut row,
                );
                if offset == start.expanded {
                    row.task_group.clone_from(&block.meta.task_summary);
                }
                rows.push(row);
                offset = offset.saturating_add(1);
            }
        }
        Ok(HistoryWindow {
            snapshot_id: self.snapshot_id.clone(),
            rows,
            total: self.blocks.measure().expanded,
            chain_generation: u64::try_from(self.tail.chain().stats().accepted).unwrap_or(u64::MAX),
            max_lane: self.graph.max_lane(),
            sub_op_counts: None,
            expansion_spans: None,
            layout_ready: true,
        })
    }

    fn locate(&self, keys: &[String]) -> Result<LocateRowsResponse> {
        let mut rows = Vec::new();
        for key in keys {
            let (block_key, slot) = if self.paged() {
                let Some((block, slot)) = self.disclosure.position(key) else {
                    continue;
                };
                (block.as_str(), *slot)
            } else {
                (key.as_str(), 0)
            };
            let Some(order) = self.orders.get(block_key) else {
                continue;
            };
            let Some(block) = self.blocks.get(order) else {
                continue;
            };
            let Some(rank) = self.blocks.rank(order) else {
                continue;
            };
            let (row, node_key) = if self.paged() {
                let Ok(visible) = self.disclosure.slots(block_key).binary_search(&slot) else {
                    continue;
                };
                let content = self.rows.rows(block)?;
                let node_key = content
                    .get(usize::try_from(slot)?)
                    .ok_or("anchor slot exceeds block rows")?
                    .node_key
                    .clone();
                (
                    rank.visible.saturating_add(u64::try_from(visible)?),
                    node_key,
                )
            } else {
                (rank.expanded, block.meta.node_key.clone())
            };
            rows.push(RowLocation {
                key: key.clone(),
                node_key,
                row,
            });
        }
        Ok(LocateRowsResponse {
            snapshot_id: self.snapshot_id.clone(),
            rows,
        })
    }

    fn paged_window(&self, request: &GetWindowRequest) -> Result<HistoryWindow> {
        let mut offset = request.offset;
        let total = self.blocks.measure().visible;
        let end = offset.saturating_add(request.limit).min(total);
        let mut rows = Vec::new();
        while offset < end {
            let Some((_, block, start)) = self.blocks.select(offset, Axis::Visible) else {
                break;
            };
            let slots = self.disclosure.slots(&block.meta.key);
            let first = usize::try_from(offset.saturating_sub(start.visible))?;
            let content = self.rows.rows(block)?;
            for slot in slots
                .iter()
                .skip(first)
                .take(usize::try_from(end.saturating_sub(offset))?)
            {
                let mut row = content
                    .get(usize::try_from(*slot)?)
                    .cloned()
                    .ok_or("visible slot exceeds block rows")?;
                row.parent_row = row
                    .parent_row
                    .and_then(|parent| slots.binary_search(&u64::try_from(parent).ok()?).ok())
                    .and_then(|parent| usize::try_from(start.visible).ok()?.checked_add(parent));
                self.graph.decorate(&block.meta.key, *slot, &mut row);
                row.native_expanded = Some(self.disclosure.expanded(&block.meta.key, *slot));
                if *slot == 0 {
                    row.task_group = block.meta.task_summary.clone().map(|mut task| {
                        task.expanded = block
                            .meta
                            .task_group
                            .as_ref()
                            .map(|group| self.disclosure.group_expanded(group));
                        task.summarized = task.expanded == Some(false)
                            && self.disclosure.settled(&block.meta.key);
                        task
                    });
                }
                rows.push(row);
                offset = offset.saturating_add(1);
            }
        }
        Ok(HistoryWindow {
            snapshot_id: self.snapshot_id.clone(),
            rows,
            total,
            chain_generation: u64::try_from(self.tail.chain().stats().accepted)?,
            max_lane: self.graph.max_lane(),
            sub_op_counts: None,
            expansion_spans: (request.offset == 0).then(Vec::new),
            layout_ready: true,
        })
    }

    fn find(&mut self, query: &str, limit: usize) -> Result<FindInHistoryResponse> {
        let keys = self
            .search
            .as_mut()
            .ok_or("live search unavailable")?
            .find(query, limit.saturating_add(1))?;
        let more = keys.len() > limit;
        let before = self.revision;
        if self.paged()
            && self.reveal_matches(&keys.iter().take(limit).cloned().collect::<Vec<_>>())
        {
            self.poisoned = true;
            self.publish(
                Vec::new(),
                Vec::new(),
                editchain_protocol::LiveWork::default(),
            )?;
            self.checkpoint()?;
        }
        let matches = keys
            .iter()
            .take(limit)
            .filter_map(|key| {
                let order = self.orders.get(key)?;
                Some(FindInHistoryMatch {
                    node_key: self.blocks.get(order)?.meta.node_key.clone(),
                    row: if self.paged() {
                        self.blocks.rank(order)?.visible
                    } else {
                        self.blocks.rank(order)?.expanded
                    },
                })
            })
            .collect();
        Ok(FindInHistoryResponse {
            live: (self.revision != before).then(|| editchain_protocol::LiveUpdate {
                epoch: self.epoch.clone(),
                revision: self.revision,
                deltas: self
                    .journal
                    .iter()
                    .filter(|delta| delta.revision > before)
                    .cloned()
                    .collect(),
                work: editchain_protocol::LiveWork::default(),
            }),
            snapshot_id: self.snapshot_id.clone(),
            matches,
            more,
        })
    }
}
