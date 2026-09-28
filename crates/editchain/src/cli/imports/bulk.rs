//! File selection and bounded capture with one destination writer lifetime.

mod selection;

use std::{
    path::Path,
    time::{Duration, Instant},
};

use editchain_engine::{encode_op, Admission, OpSet};
use editchain_import::{
    batch::{DurableAdmission, DurableImport, ImportBatch},
    capture_import_file, BufferedBlobSink, ContentAddressedBlobSink, FsBlobSink, FsCursorStore,
    ImportOptions, ImportReport, MemoryCursorStore,
};
use editchain_store::{LogStore, SegmentStore};
use serde_json::{json, Value};

use super::{finish_report, report_value, Args, Output, Result};

#[derive(Default)]
struct Totals {
    report: ImportReport,
    admission: DurableAdmission,
    batches: usize,
    capture: Duration,
    persist: Duration,
}

#[derive(serde::Serialize)]
struct Preview<'a> {
    r#type: &'static str,
    source: &'a Path,
    report: serde_json::Map<String, Value>,
    operations: &'a [editchain_engine::Op],
}

impl Totals {
    fn add(&mut self, outcome: &DurableImport) {
        self.report.merge(&outcome.report);
        self.admission.written = self
            .admission
            .written
            .saturating_add(outcome.admission.written);
        self.admission.duplicates = self
            .admission
            .duplicates
            .saturating_add(outcome.admission.duplicates);
        self.admission.conflicts = self
            .admission
            .conflicts
            .saturating_add(outcome.admission.conflicts);
        self.batches = self.batches.saturating_add(1);
    }

    fn add_preview(&mut self, batch: &ImportBatch, evidence: &mut OpSet) -> Result<()> {
        self.report.merge(batch.report());
        // Captures retain distinct variants per file. Admit them across the
        // entire selection so local conflicts are counted once and repeats of
        // any variant remain duplicates, including already-conflicted IDs.
        for op in batch.operations() {
            let encoded = encode_op(op).map_err(std::io::Error::other)?;
            match evidence.insert(op.id, encoded) {
                Admission::Accepted => {}
                Admission::Duplicate => {
                    self.admission.duplicates = self.admission.duplicates.saturating_add(1);
                }
                Admission::Conflict => {
                    self.admission.conflicts = self.admission.conflicts.saturating_add(1);
                }
            }
        }
        self.batches = self.batches.saturating_add(1);
        Ok(())
    }

    fn value(&self) -> serde_json::Map<String, Value> {
        let mut value = report_value(&self.report);
        drop(value.insert("written".into(), self.admission.written.into()));
        drop(
            value.insert(
                "duplicates".into(),
                self.admission
                    .duplicates
                    .saturating_add(self.report.duplicates)
                    .into(),
            ),
        );
        drop(value.insert("conflicts".into(), self.admission.conflicts.into()));
        drop(value.insert("batches".into(), self.batches.into()));
        value
    }
}

pub(super) fn run(
    chain: &Path,
    args: &Args,
    options: &ImportOptions,
    output: &mut Output,
) -> Result<()> {
    let started = Instant::now();
    let sources = selection::prepare(chain, args, options)?;
    let selection_seconds = started.elapsed().as_secs_f64();
    if args.dry_run {
        return preview(&sources, args, options, output);
    }
    let helper = selection::helper(args);
    let opened = Instant::now();
    let mut writer = LogStore::new(SegmentStore::open(chain)?);
    let mut blobs = BufferedBlobSink::new(FsBlobSink::new(chain.join("blobs"))?);
    let mut cursors = FsCursorStore::new(chain.join("cursors"))?;
    let writer_open_seconds = opened.elapsed().as_secs_f64();
    let mut totals = Totals::default();
    let selected = sources
        .iter()
        .map(|source| source.files.len())
        .fold(0_usize, usize::saturating_add);
    let mut reports = Vec::new();
    for source in &sources {
        let mut source_totals = Totals::default();
        source.with_source(&helper, |request| {
            for file in &source.files {
                let capture = Instant::now();
                let batch = capture_import_file(request, file, options, &mut blobs, &cursors)?;
                options.cancellation.check(file.path())?;
                blobs.flush()?;
                source_totals.capture = source_totals.capture.saturating_add(capture.elapsed());
                let persist = Instant::now();
                let outcome = batch.persist(&mut writer, &mut cursors)?;
                source_totals.persist = source_totals.persist.saturating_add(persist.elapsed());
                source_totals.add(&outcome);
                totals.add(&outcome);
                if args.selection.progress {
                    super::super::output::diagnostic(&format!(
                        "import {}/{}: {} ({} written, {:.1}s elapsed)",
                        totals.batches,
                        selected,
                        file.path().display(),
                        outcome.admission.written,
                        started.elapsed().as_secs_f64(),
                    ))?;
                }
            }
            Ok(())
        })?;
        totals.capture = totals.capture.saturating_add(source_totals.capture);
        totals.persist = totals.persist.saturating_add(source_totals.persist);
        reports.push(json!({"provider":source.provider, "input":source.root,
            "workspace":source.workspace, "report":source_totals.value()}));
    }
    let mut report = totals.value();
    drop(report.insert("sources".into(), reports.into()));
    drop(report.insert(
        "timings".into(),
        json!({
            "selection_seconds":selection_seconds, "writer_open_seconds":writer_open_seconds,
            "capture_and_blobs_seconds":totals.capture.as_secs_f64(),
            "admission_and_cursors_seconds":totals.persist.as_secs_f64(),
            "total_seconds":started.elapsed().as_secs_f64(),
        }),
    ));
    output.emit(&report)?;
    finish_report(&Value::Object(report))
}

fn preview(
    sources: &[selection::Prepared],
    args: &Args,
    options: &ImportOptions,
    output: &mut Output,
) -> Result<()> {
    let helper = selection::helper(args);
    let mut blobs = ContentAddressedBlobSink::new();
    let cursors = MemoryCursorStore::new();
    let mut evidence = OpSet::new();
    let mut totals = Totals::default();
    output.begin_stream()?;
    for source in sources {
        source.with_source(&helper, |request| {
            for file in &source.files {
                let batch = capture_import_file(request, file, options, &mut blobs, &cursors)?;
                totals.add_preview(&batch, &mut evidence)?;
                output.emit(&Preview {
                    r#type: "capture",
                    source: file.path(),
                    report: report_value(batch.report()),
                    operations: batch.operations(),
                })?;
            }
            Ok(())
        })?;
    }
    let mut report = totals.value();
    drop(report.insert("type".into(), "summary".into()));
    drop(report.insert("dry_run".into(), true.into()));
    output.emit(&report)?;
    finish_report(&Value::Object(report))
}
