//! Retained source cursors and provider reducers; durable admission stays host-owned.

use super::{
    helper::{LiveHelper, LiveHelperInput},
    import::rollout_in_workspace,
    normalize::{build_raw_op, NormalizeContext},
    projection::{LiveProjection, Projection},
    records::RecordBatch,
    CodexDiscoveryRequest, HelperCommand, RolloutFile, CODEX_NORMALIZATION_VERSION,
};
use crate::{
    cursor::{resolve_source_cursor, ResolvedSourceCursor},
    ids::{derive_session_id, SourcePosition, SourceStream},
    sink::{
        emit_op, BlobSink, CursorStore, CursorValue, EmissionKind, MaterializationCheckpoint,
        OpSink,
    },
    source_read::{LineWithHash, LiveRead, SourceReadPlan},
    ImportError, ImportOptions, ImportReport,
};
use editchain_core::NodeId;
use std::{collections::HashMap, io, path::PathBuf};

#[derive(Debug)]
struct Session {
    read: LiveRead,
    fold: LiveProjection,
    accepted: CursorValue,
    node: NodeId,
    touched: u64,
}

struct SourceJob<'a> {
    source: &'a RolloutFile,
    request: &'a CodexDiscoveryRequest<'a>,
    options: &'a ImportOptions,
}

struct Capture {
    session: Session,
    lines: Vec<LineWithHash>,
    start: u64,
    projection: Projection,
}

/// Host-owned, staged sinks; checkpoint persistence must follow durable append.
pub struct LiveSinks<'a> {
    /// Staged operations, typically owned by `ImportBatch`.
    pub ops: &'a mut dyn OpSink,
    /// Durable blob sink.
    pub blobs: &'a mut dyn BlobSink,
    /// Staged cursor overlay, typically owned by `ImportBatch`.
    pub cursors: &'a mut dyn CursorStore,
}

impl std::fmt::Debug for LiveSinks<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LiveSinks").finish_non_exhaustive()
    }
}

/// Observable work performed by one live capture.
#[derive(Debug, Default, Clone, Copy, serde::Serialize)]
pub struct LiveCaptureWork {
    /// Raw provider bytes read during this call.
    pub source_bytes: u64,
    /// Complete lines reduced by the retained Codex helper.
    pub provider_records: u64,
    /// Explicit cold source bootstraps in this call.
    pub bootstraps: u64,
    /// Another bounded capture is needed even if the file's stamp is unchanged.
    pub pending: bool,
}

/// A resident provider adapter. A failed durable transaction must call
/// [`Self::invalidate`] before retrying; its checkpoint never advances on disk.
#[derive(Debug)]
pub struct LiveCodex {
    helper: LiveHelper,
    sessions: HashMap<PathBuf, Session>,
    clock: u64,
    /// Counters for the last capture, surfaced in live diagnostics.
    pub work: LiveCaptureWork,
}

impl LiveCodex {
    /// Start one owned, multiplexed exporter process.
    ///
    /// # Errors
    /// Returns helper startup errors.
    pub fn start(command: &HelperCommand) -> Result<Self, ImportError> {
        Ok(Self {
            helper: LiveHelper::start(command)?,
            sessions: HashMap::new(),
            clock: 0,
            work: LiveCaptureWork::default(),
        })
    }

    /// Discard speculative source state after a failed durable transaction.
    pub fn invalidate(&mut self) {
        self.sessions.clear();
    }

    /// Capture selected rollouts. The host holds the chain writer lock and
    /// persists these sinks before the next call.
    ///
    /// # Errors
    /// Returns source, helper, protocol, normalization or sink errors.
    pub fn capture(
        &mut self,
        request: &CodexDiscoveryRequest<'_>,
        options: &ImportOptions,
        mut sinks: LiveSinks<'_>,
    ) -> Result<ImportReport, ImportError> {
        self.work = LiveCaptureWork::default();
        let rollouts =
            super::discover::selected_rollouts(&request.raw_root, &request.selected_paths)
                .map_err(ImportError::OpSink)?;
        let mut report = ImportReport::new();
        report.files_discovered = rollouts.len();
        for source in &rollouts {
            let job = SourceJob {
                source,
                request,
                options,
            };
            let emitted = self.capture_one(&job, &mut sinks)?;
            report.merge_emissions(&emitted);
            report.files_processed = report
                .files_processed
                .saturating_add(emitted.files_processed);
        }
        Ok(report)
    }

    fn capture_one(
        &mut self,
        job: &SourceJob<'_>,
        sinks: &mut LiveSinks<'_>,
    ) -> Result<ImportReport, ImportError> {
        let resolved = resolve_source_cursor(
            sinks.cursors,
            "codex",
            &job.request.raw_root,
            &job.source.path,
            job.request.workspace_path.to_str().unwrap_or("/workspace"),
        )?;
        let Some(capture) = self.read(job, &resolved, sinks.cursors)? else {
            return Ok(ImportReport::new());
        };
        if capture
            .projection
            .session_meta
            .as_ref()
            .and_then(|meta| meta.cwd.as_deref())
            .is_some_and(|cwd| !rollout_in_workspace(&job.request.workspace_path, cwd))
        {
            return Ok(ImportReport::new());
        }
        let report = stage(&capture, job, &resolved.canonical_key, sinks)?;
        self.work.pending |= capture.session.read.has_more();
        let mut session = capture.session;
        session.accepted = sinks
            .cursors
            .get_cursor(&resolved.canonical_key)?
            .ok_or_else(|| io::Error::other("live checkpoint was not staged"))?;
        self.clock = self.clock.saturating_add(1);
        session.touched = self.clock;
        self.retain(job.source.path.clone(), session);
        Ok(report)
    }

    fn read(
        &mut self,
        job: &SourceJob<'_>,
        resolved: &ResolvedSourceCursor,
        cursors: &dyn CursorStore,
    ) -> Result<Option<Capture>, ImportError> {
        let retained = self.sessions.remove(&job.source.path).filter(|session| {
            resolved.cursor.as_ref() == Some(&session.accepted)
                && resolved.source_node == session.node
        });
        if let Some(mut session) = retained {
            if let Ok(batch) = session
                .read
                .poll(&job.source.path, &job.options.source_control())
            {
                self.work.source_bytes = self.work.source_bytes.saturating_add(batch.bytes_read);
                let start = session.accepted.ops_emitted;
                let projection = self.project(
                    &mut session.fold,
                    &LiveHelperInput {
                        source: &job.source.path,
                        generation: session.accepted.accepted_generation.unwrap_or(0),
                        after: start,
                        lines: &batch.lines,
                    },
                    job.options,
                )?;
                session.read = batch.next;
                return Ok(Some(Capture {
                    session,
                    lines: batch.lines,
                    start,
                    projection,
                }));
            }
        }
        let plan = SourceReadPlan::capture_controlled(
            &job.source.path,
            resolved.cursor.as_ref(),
            cursors.get_generation(&resolved.state_key)?,
            cursors.get_reservation(&resolved.canonical_key)?.as_ref(),
            &job.options.source_control(),
        )?;
        let replay = MaterializationCheckpoint::needs_replay(
            plan.checkpoint().materialization.as_ref(),
            super::materialize::CONTRACT,
            job.options.include_thinking,
            plan.start_seq(),
        )?;
        if plan.lines().is_empty() && !replay {
            return Ok(None);
        }
        self.bootstrap(job, &plan, resolved, replay).map(Some)
    }

    fn retain(&mut self, source: PathBuf, session: Session) {
        if self.sessions.len() >= 63 {
            if let Some(oldest) = self
                .sessions
                .iter()
                .min_by_key(|(_, value)| value.touched)
                .map(|(key, _)| key.clone())
            {
                drop(self.sessions.remove(&oldest));
            }
        }
        drop(self.sessions.insert(source, session));
    }

    fn project(
        &mut self,
        fold: &mut LiveProjection,
        input: &LiveHelperInput<'_>,
        options: &ImportOptions,
    ) -> Result<Projection, ImportError> {
        let reply = self.helper.project(input, &options.cancellation)?;
        self.work.provider_records = self
            .work
            .provider_records
            .saturating_add(reply.records_projected);
        let ordinals = input
            .lines
            .iter()
            .enumerate()
            .filter(|(_, line)| !line.data.iter().all(u8::is_ascii_whitespace))
            .map(|(index, _)| {
                input
                    .after
                    .saturating_add(u64::try_from(index).unwrap_or(u64::MAX))
                    .saturating_add(1)
            })
            .collect::<Vec<_>>();
        fold.apply(
            &reply.records,
            &ordinals,
            input
                .after
                .saturating_add(u64::try_from(input.lines.len()).unwrap_or(u64::MAX)),
        )
        .map_err(|error| ImportError::OpSink(error.to_string()))
    }

    fn bootstrap(
        &mut self,
        job: &SourceJob<'_>,
        plan: &SourceReadPlan,
        resolved: &ResolvedSourceCursor,
        replay: bool,
    ) -> Result<Capture, ImportError> {
        self.work.bootstraps = self.work.bootstraps.saturating_add(1);
        self.work.source_bytes = self
            .work
            .source_bytes
            .saturating_add(plan.checkpoint().file_size);
        let (read, lines) = LiveRead::bootstrap(&job.source.path, plan)?;
        let mut fold = LiveProjection::default();
        let start = if replay { 0 } else { plan.start_seq() };
        let mut projection = Projection::default();
        for (index, chunk) in lines.chunks(128).enumerate() {
            let after = u64::try_from(index)
                .map_err(io::Error::other)?
                .saturating_mul(128);
            let delta = self.project(
                &mut fold,
                &LiveHelperInput {
                    source: &job.source.path,
                    generation: plan.generation(),
                    after,
                    lines: chunk,
                },
                job.options,
            )?;
            merge_suffix(&mut projection, delta, start);
        }
        let lines = lines
            .into_iter()
            .skip(usize::try_from(start).map_err(io::Error::other)?)
            .collect();
        Ok(Capture {
            session: Session {
                read,
                fold,
                accepted: plan.checkpoint().clone(),
                node: resolved.source_node,
                touched: 0,
            },
            lines,
            start,
            projection,
        })
    }
}

fn merge_suffix(output: &mut Projection, delta: Projection, start: u64) {
    output.owning_thread = delta.owning_thread;
    output.session_meta = delta.session_meta;
    output.session_meta_source_ordinal = delta.session_meta_source_ordinal;
    output.item_occurrences.extend(
        delta
            .item_occurrences
            .into_iter()
            .filter(|item| item.last_seen > start),
    );
    output.turn_occurrences.extend(
        delta
            .turn_occurrences
            .into_iter()
            .filter(|(ordinal, _, _)| *ordinal > start),
    );
    output.removed_turns.extend(
        delta
            .removed_turns
            .into_iter()
            .filter(|(ordinal, _)| *ordinal > start),
    );
    output.inter_agent_lines.extend(
        delta
            .inter_agent_lines
            .into_iter()
            .filter(|line| line.source_ordinal > start),
    );
    output.compacted_lines.extend(
        delta
            .compacted_lines
            .into_iter()
            .filter(|line| line.source_ordinal > start),
    );
}

fn stage(
    capture: &Capture,
    job: &SourceJob<'_>,
    key: &str,
    sinks: &mut LiveSinks<'_>,
) -> Result<ImportReport, ImportError> {
    let mut checkpoint = capture.session.read.checkpoint().clone();
    let thread = capture
        .projection
        .owning_thread
        .as_deref()
        .unwrap_or(&job.source.session_id);
    let stream = SourceStream::new(
        capture.session.node,
        checkpoint.accepted_generation.unwrap_or(0),
    );
    let batch = RecordBatch {
        lines: &capture.lines,
        start: capture.start,
        checkpoint: &checkpoint,
        check: &|| job.options.cancellation.check(&job.source.path),
    };
    let context = NormalizeContext {
        stream: &stream,
        thread,
        session_id: derive_session_id(thread),
        lanes: HashMap::new(),
        batch_end: checkpoint.ops_emitted,
        include_thinking: job.options.include_thinking,
        blobs: sinks.blobs,
    };
    let mut report = emit_live(&capture.projection, &batch, context, sinks.ops)?;
    if capture.start == 0 {
        if let (Some(meta), Some(ordinal)) = (
            &capture.projection.session_meta,
            capture.projection.session_meta_source_ordinal,
        ) {
            if let Some(op) = super::session_git::session_git_link_op(
                job.request.repositories,
                meta,
                ordinal,
                &stream,
                derive_session_id(thread),
            )? {
                emit_op(&op, sinks.ops, &mut report, EmissionKind::Derived)?;
            }
        }
    }
    checkpoint.source_node = Some(capture.session.node);
    checkpoint.normalization_version =
        CODEX_NORMALIZATION_VERSION.max(checkpoint.normalization_version);
    checkpoint.materialization = Some(MaterializationCheckpoint {
        contract: super::materialize::CONTRACT.into(),
        through: checkpoint.ops_emitted,
        includes_thinking: job.options.include_thinking
            || checkpoint
                .materialization
                .as_ref()
                .is_some_and(|previous| previous.includes_thinking),
    });
    sinks
        .cursors
        .set_generation(key, checkpoint.accepted_generation.unwrap_or(0))?;
    sinks.cursors.set_cursor(key, &checkpoint)?;
    report.files_processed = 1;
    Ok(report)
}

fn emit_live(
    projection: &Projection,
    batch: &RecordBatch<'_>,
    mut context: NormalizeContext<'_>,
    ops: &mut dyn OpSink,
) -> Result<ImportReport, ImportError> {
    let mut report = ImportReport::new();
    let mut previous = (batch.start > 0)
        .then(|| {
            context
                .stream
                .op_from_position(SourcePosition::raw(batch.start))
        })
        .transpose()?;
    for (index, line) in batch.lines.iter().enumerate() {
        batch.check_cancellation()?;
        let ordinal = batch
            .start
            .saturating_add(u64::try_from(index).map_err(io::Error::other)?)
            .saturating_add(1);
        let op = build_raw_op(
            &line.data,
            line.hash,
            context.stream,
            ordinal,
            context.thread,
            context.session_id,
            previous,
            context.blobs,
        )?;
        previous = Some(op.id);
        emit_op(&op, ops, &mut report, EmissionKind::Raw)?;
    }
    report.merge_emissions(&super::materialize::emit_batch(
        projection,
        batch,
        &mut context,
        ops,
    )?);
    for op in super::evidence::batch_evidence(projection, batch, context.stream, context.thread)? {
        emit_op(&op, ops, &mut report, EmissionKind::Derived)?;
    }
    Ok(report)
}
