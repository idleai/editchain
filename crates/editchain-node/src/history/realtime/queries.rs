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
    pub(super) fn new() -> Result<Self> {
        let mut schema = Schema::builder();
        let key = schema.add_text_field("key", STRING | STORED);
        let text = schema.add_text_field("text", TEXT);
        let index = Index::create_in_ram(schema.build());
        let writer = index.writer_with_num_threads(1, 20_000_000)?;
        let reader = index.reader()?;
        Ok(Self {
            index,
            writer,
            reader,
            key,
            text,
            dirty: false,
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

    fn find(&mut self, query: &str, limit: usize) -> Result<Vec<String>> {
        if self.dirty {
            let _stamp = self.writer.commit()?;
            self.reader.reload()?;
            self.dirty = false;
        }
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
            RequestBody::SyncLive(request) => serde_json::to_value(self.sync(request)?)?,
            RequestBody::GetWindow(request) => serde_json::to_value(self.window(request))?,
            RequestBody::LocateRows(request) => serde_json::to_value(self.locate(&request.keys))?,
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
            RequestBody::Open(_) | RequestBody::OpenLive(_) | RequestBody::Refresh(_) => {
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
            return self.local_workspace(input);
        }
        let op = self
            .tail
            .chain()
            .get(id)
            .ok_or("live operation unavailable")?
            .clone();
        self.local_workspace(&LiveRow {
            task: None,
            key: id.to_string(),
            anchor: id,
            incarnation: id,
            operations: vec![op],
        })
    }

    fn window(&self, request: &GetWindowRequest) -> HistoryWindow {
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
            for source in block.rows.iter().skip(first).take(count) {
                let mut row = source.clone();
                row.parent_row = row
                    .parent_row
                    .and_then(|parent| parent.checked_add(usize::try_from(start.expanded).ok()?));
                self.graph.decorate(
                    &block.meta.key,
                    offset.saturating_sub(start.expanded),
                    &mut row,
                );
                rows.push(row);
                offset = offset.saturating_add(1);
            }
        }
        HistoryWindow {
            snapshot_id: self.snapshot_id.clone(),
            rows,
            total: self.blocks.measure().expanded,
            chain_generation: u64::try_from(self.tail.chain().stats().accepted).unwrap_or(u64::MAX),
            max_lane: self.graph.max_lane(),
            sub_op_counts: None,
            expansion_spans: None,
            layout_ready: true,
        }
    }

    fn locate(&self, keys: &[String]) -> LocateRowsResponse {
        let rows = keys
            .iter()
            .filter_map(|key| {
                let order = self.orders.get(key)?;
                let block = self.blocks.get(order)?;
                Some(RowLocation {
                    key: key.clone(),
                    node_key: block.rows.first()?.node_key.clone(),
                    row: self.blocks.rank(order)?.expanded,
                })
            })
            .collect();
        LocateRowsResponse {
            snapshot_id: self.snapshot_id.clone(),
            rows,
        }
    }

    fn find(&mut self, query: &str, limit: usize) -> Result<FindInHistoryResponse> {
        let keys = self.search.find(query, limit.saturating_add(1))?;
        let more = keys.len() > limit;
        let matches = keys
            .iter()
            .take(limit)
            .filter_map(|key| {
                let order = self.orders.get(key)?;
                Some(FindInHistoryMatch {
                    node_key: self.blocks.get(order)?.rows.first()?.node_key.clone(),
                    row: self.blocks.rank(order)?.expanded,
                })
            })
            .collect();
        Ok(FindInHistoryResponse {
            snapshot_id: self.snapshot_id.clone(),
            matches,
            more,
        })
    }
}
