//! Short writer transactions using retained canonical admission.

use super::{LiveWorkspace, Result};
use editchain_core::{Admission, Op, OpSet, RepositoryId};
use editchain_import::{
    batch::{DurableAdmission, DurableOpSink, ImportBatch},
    codex::{
        live::{LiveCodex, LiveSinks},
        CodexDiscoveryRequest, HelperCommand, RepositoryLookup,
    },
    FsBlobSink, FsCursorStore, ImportError, ImportOptions,
};
use editchain_protocol::{CodexLiveRequest, LiveWork};
use editchain_store::{
    format::{encode_op, Page},
    IndexedChain, SegmentStore,
};
use std::{io, path::PathBuf};

impl LiveWorkspace {
    pub(super) fn append_links(&mut self, links: &[Op]) -> Result<()> {
        let mut store = SegmentStore::open(&self.chain)?;
        self.queue_tail()?;
        let _admission = Writer {
            store: &mut store,
            canonical: self.tail.chain(),
        }
        .append_durable(links)?;
        Ok(())
    }
    pub(super) fn capture(
        &mut self,
        request: &CodexLiveRequest,
        work: &mut LiveWork,
    ) -> Result<()> {
        if request.paths.is_empty() {
            return Ok(());
        }
        if self
            .codex
            .as_ref()
            .is_none_or(|(helper, _)| helper != &request.helper)
        {
            self.codex = Some((
                request.helper.clone(),
                LiveCodex::start(&HelperCommand::new(&request.helper, Vec::new()))?,
            ));
        }
        let mut store = SegmentStore::open(&self.chain)?;
        // Recheck external appends after acquiring the writer lock. Keep their
        // view delta queued even if this provider transaction subsequently fails.
        self.queue_tail()?;
        let mut blobs = FsBlobSink::new(self.chain.join("blobs"))?;
        let mut cursors = FsCursorStore::new(self.chain.join("cursors"))?;
        let (_, provider) = self.codex.as_mut().ok_or("live provider unavailable")?;
        let options = ImportOptions::default();
        let repositories = Repositories(&self.catalog);
        let discovery = CodexDiscoveryRequest {
            workspace_path: self.root.clone(),
            raw_root: PathBuf::from(&request.sessions_root),
            selected_paths: request.paths.iter().map(PathBuf::from).collect(),
            repositories: &repositories,
        };
        let result =
            ImportBatch::capture_bounded(&cursors, options.batch_limits, |ops, pending| {
                provider.capture(
                    &discovery,
                    &options,
                    LiveSinks {
                        ops,
                        blobs: &mut blobs,
                        cursors: pending,
                    },
                )
            })
            .and_then(|batch| {
                batch.persist(
                    &mut Writer {
                        store: &mut store,
                        canonical: self.tail.chain(),
                    },
                    &mut cursors,
                )
            });
        if result.is_err() {
            provider.invalidate();
        }
        let _outcome = result?;
        work.source_bytes = provider.work.source_bytes;
        work.provider_records = provider.work.provider_records;
        work.provider_bootstraps = provider.work.bootstraps;
        work.provider_pending = provider.work.pending;
        Ok(())
    }
}

#[derive(Debug)]
struct Repositories<'a>(&'a editchain_git::RepositoryCatalog);
impl RepositoryLookup for Repositories<'_> {
    fn repository_for_cwd(
        &self,
        cwd: &std::path::Path,
    ) -> std::result::Result<Option<RepositoryId>, ImportError> {
        if !self.0.is_complete() {
            return Err(ImportError::OpSink(
                "incomplete live repository catalog".into(),
            ));
        }
        Ok(self
            .0
            .repository_for_path(cwd)
            .map(|repository| repository.id))
    }
}

struct Writer<'a> {
    store: &'a mut SegmentStore,
    canonical: &'a IndexedChain,
}

impl DurableOpSink for Writer<'_> {
    fn append_durable(
        &mut self,
        operations: &[Op],
    ) -> std::result::Result<DurableAdmission, ImportError> {
        let mut staged = OpSet::new();
        let mut result = DurableAdmission::default();
        let mut page = Page::new(0);
        let mut bytes = 0usize;
        for op in operations {
            let encoded = encode_op(op).map_err(io::Error::other)?;
            let known = self.canonical.classify(op.id, &encoded)?;
            if known == Admission::Duplicate {
                result.duplicates = result.duplicates.saturating_add(1);
                continue;
            }
            let incoming = staged.insert(op.id, encoded.clone());
            if incoming == Admission::Duplicate {
                result.duplicates = result.duplicates.saturating_add(1);
                continue;
            }
            if known == Admission::Conflict || incoming == Admission::Conflict {
                result.conflicts = result.conflicts.saturating_add(1);
            }
            if !page.records.is_empty() && bytes.saturating_add(encoded.len()) > 4 * 1024 * 1024 {
                self.store.append_page(&page)?;
                page = Page::new(
                    page.page_seq
                        .checked_add(1)
                        .ok_or_else(|| io::Error::other("live page sequence exhausted"))?,
                );
                bytes = 0;
            }
            bytes = bytes.saturating_add(encoded.len()).saturating_add(5);
            page.add_record(0, encoded);
            result.written = result.written.saturating_add(1);
        }
        if !page.records.is_empty() {
            self.store.append_page(&page)?;
        }
        Ok(result)
    }
}
