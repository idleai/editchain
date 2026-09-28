//! Provider-neutral capture through the shared import and persistence APIs.

mod bulk;
mod repositories;

use std::{
    io::Write,
    path::{Path, PathBuf},
};

use editchain_import::{
    batch::ImportBatch,
    capture_import,
    codex::{CodexDiscoveryRequest, HelperCommand},
    human::HumanImportRequest,
    BlobSink, CursorStore, DiscoveryRequest, ImportOptions, ImportSource,
};

use super::{
    error::{Failure, Result},
    input,
    output::Output,
};

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum, serde::Deserialize, serde::Serialize,
)]
#[serde(rename_all = "lowercase")]
enum Provider {
    Claude,
    Codex,
    Human,
}

#[derive(Debug, clap::Args)]
pub(super) struct Args {
    /// Native provider JSONL file/directory, or - for stdin. Uses shared capture APIs.
    #[arg(
        long,
        required_unless_present = "manifest",
        conflicts_with = "manifest"
    )]
    input: Option<PathBuf>,
    #[command(flatten)]
    selection: BulkSelection,
    /// Stable JSONL filename for stdin, preserving source identity across retries.
    #[arg(long, required_if_eq("input", "-"), requires = "input")]
    source_name: Option<String>,
    #[arg(long, value_enum, default_value = "claude")]
    provider: Provider,
    /// Source capture root used by provider filtering (not the destination chain).
    #[arg(long, default_value = ".")]
    workspace: String,
    /// Exact recorded root filter for shared human archive capture.
    #[arg(long, requires = "input")]
    recorded_root: Option<String>,
    #[arg(long)]
    codex_helper: Option<String>,
    #[arg(long, action = clap::ArgAction::Append, allow_hyphen_values = true)]
    codex_helper_arg: Vec<String>,
    #[arg(long, action = clap::ArgAction::Append)]
    codex_rollout: Vec<PathBuf>,
    /// Preview without creating or modifying the chain or cursors.
    #[arg(long)]
    dry_run: bool,
    /// Retain exact evidence without normalized operations.
    #[arg(long)]
    raw_only: bool,
    /// Include recorded private reasoning in normalization.
    #[arg(long)]
    include_thinking: bool,
}

#[derive(Debug, clap::Args)]
struct BulkSelection {
    /// JSON bulk-source manifest; all providers share one durable writer.
    #[arg(long, conflicts_with_all = ["glob", "bulk", "codex_rollout", "source_name", "recorded_root", "provider"])]
    manifest: Option<PathBuf>,
    /// Select discovered files by a quoted glob relative to --input; repeatable.
    #[arg(long, requires = "input", action = clap::ArgAction::Append)]
    glob: Vec<String>,
    /// Capture and commit one file at a time while retaining one writer.
    #[arg(long, requires = "input")]
    bulk: bool,
    /// Print completed bulk files and elapsed time to stderr.
    #[arg(long)]
    progress: bool,
}

impl BulkSelection {
    fn enabled(&self) -> bool {
        self.bulk || !self.glob.is_empty() || self.manifest.is_some()
    }
}

pub(super) fn run(
    chain: &Path,
    args: &Args,
    options: &ImportOptions,
    output: &mut Output,
) -> Result<()> {
    validate(args)?;
    let mut options = options.clone();
    options.normalize = !args.raw_only;
    options.include_thinking = args.include_thinking;
    if args.selection.enabled() {
        return bulk::run(chain, args, &options, output);
    }
    let report = capture(chain, args, &options)?;
    output.emit(&report)?;
    finish_report(&report)
}

fn finish_report(report: &serde_json::Value) -> Result<()> {
    if report
        .get("conflicts")
        .and_then(serde_json::Value::as_u64)
        .is_some_and(|n| n > 0)
    {
        return Err(Failure::new(4, "import retained conflicting evidence"));
    }
    if report
        .get("malformed")
        .and_then(serde_json::Value::as_u64)
        .is_some_and(|n| n > 0)
    {
        return Err(Failure::new(
            3,
            "import retained malformed source evidence; inspect the report",
        ));
    }
    Ok(())
}

fn validate(args: &Args) -> Result<()> {
    if args.selection.progress && !args.selection.enabled() {
        return Err(Failure::input(
            "--progress requires --bulk, --glob, or --manifest",
        ));
    }
    if args.selection.manifest.is_none()
        && args.provider != Provider::Codex
        && (args.codex_helper.is_some()
            || !args.codex_helper_arg.is_empty()
            || !args.codex_rollout.is_empty())
    {
        return Err(Failure::input(
            "Codex helper/rollout options require --provider codex",
        ));
    }
    if args.recorded_root.is_some() && args.provider != Provider::Human {
        return Err(Failure::input("--recorded-root requires --provider human"));
    }
    Ok(())
}

struct Source {
    path: PathBuf,
    _temporary: Option<tempfile::TempDir>,
}

impl Source {
    fn prepare(args: &Args, options: &ImportOptions) -> Result<Self> {
        let path = args
            .input
            .as_deref()
            .ok_or_else(|| Failure::input("import requires --input or --manifest"))?;
        if path != Path::new("-") && !(path.is_file() && args.provider == Provider::Claude) {
            return Ok(Self {
                path: path.to_owned(),
                _temporary: None,
            });
        }
        let name = if path == Path::new("-") {
            args.source_name
                .as_deref()
                .ok_or_else(|| Failure::input("stdin requires --source-name"))?
        } else {
            path.file_name()
                .and_then(|name| name.to_str())
                .ok_or_else(|| Failure::input("source filename must be UTF-8"))?
        };
        if Path::new(name).file_name().and_then(|part| part.to_str()) != Some(name)
            || Path::new(name)
                .extension()
                .is_none_or(|extension| extension != "jsonl")
        {
            return Err(Failure::input("source name must be one .jsonl filename"));
        }
        if args.provider == Provider::Codex && !name.starts_with("rollout-") {
            return Err(Failure::input("Codex source name must start with rollout-"));
        }
        let directory = tempfile::tempdir()?;
        let target = directory.path().join(name);
        let bytes = input::provider_bytes(path, &options.cancellation)?;
        std::fs::File::create(&target)?.write_all(&bytes)?;
        let path = if args.provider == Provider::Human {
            target
        } else {
            directory.path().to_owned()
        };
        Ok(Self {
            path,
            _temporary: Some(directory),
        })
    }
}

fn capture(chain: &Path, args: &Args, options: &ImportOptions) -> Result<serde_json::Value> {
    let source = Source::prepare(args, options)?;
    if args.dry_run {
        let mut blobs = editchain_import::ContentAddressedBlobSink::new();
        let cursors = editchain_import::MemoryCursorStore::new();
        let batch = capture_batch(
            chain,
            args,
            &source.path,
            options,
            &mut (&mut blobs, &cursors),
        )?;
        let mut result = report_value(batch.report());
        drop(result.insert("dry_run".into(), true.into()));
        drop(result.insert(
            "operations".into(),
            serde_json::to_value(batch.operations())?,
        ));
        return Ok(serde_json::Value::Object(result));
    }
    // Serialize cursor reservations with capture and durable operation admission.
    let store = editchain_store::SegmentStore::open(chain)?;
    let mut blobs = editchain_import::BufferedBlobSink::new(editchain_import::FsBlobSink::new(
        chain.join("blobs"),
    )?);
    let mut cursors = editchain_import::FsCursorStore::new(chain.join("cursors"))?;
    let batch = capture_batch(
        chain,
        args,
        &source.path,
        options,
        &mut (&mut blobs, &cursors),
    )?;
    options.cancellation.check(&source.path)?;
    blobs.flush()?;
    let outcome = batch.persist(&mut editchain_store::LogStore::new(store), &mut cursors)?;
    let mut result = report_value(&outcome.report);
    drop(result.insert("written".into(), outcome.admission.written.into()));
    drop(
        result.insert(
            "duplicates".into(),
            outcome
                .admission
                .duplicates
                .saturating_add(outcome.report.duplicates)
                .into(),
        ),
    );
    drop(result.insert("conflicts".into(), outcome.admission.conflicts.into()));
    Ok(serde_json::Value::Object(result))
}

fn capture_batch(
    chain: &Path,
    args: &Args,
    source: &Path,
    options: &ImportOptions,
    sinks: &mut (&mut dyn BlobSink, &dyn CursorStore),
) -> Result<ImportBatch> {
    let claude = DiscoveryRequest {
        workspace_path: args.workspace.clone().into(),
        sessions_dir: source.to_owned(),
        chain_dir: chain.to_owned(),
    };
    let human = HumanImportRequest {
        source: source.to_owned(),
        recorded_root: args.recorded_root.clone(),
    };
    match args.provider {
        Provider::Claude => {
            return Ok(capture_import(
                ImportSource::Claude(&claude),
                options,
                sinks.0,
                sinks.1,
            )?)
        }
        Provider::Human => {
            return Ok(capture_import(
                ImportSource::Human(&human),
                options,
                sinks.0,
                sinks.1,
            )?)
        }
        Provider::Codex => {}
    }
    let repositories = repositories::Repositories::discover(Path::new(&args.workspace))?;
    let codex = CodexDiscoveryRequest {
        workspace_path: args.workspace.clone().into(),
        raw_root: if source.is_file() {
            source.parent().unwrap_or(Path::new(".")).to_owned()
        } else {
            source.to_owned()
        },
        selected_paths: if source.is_file() {
            vec![std::fs::canonicalize(source)?]
        } else {
            args.codex_rollout.clone()
        },
        repositories: &repositories,
    };
    let helper = HelperCommand::new(
        args.codex_helper
            .clone()
            .unwrap_or_else(|| "codex-session-exporter".into()),
        args.codex_helper_arg.clone(),
    );
    Ok(capture_import(
        ImportSource::Codex {
            request: &codex,
            helper: &helper,
        },
        options,
        sinks.0,
        sinks.1,
    )?)
}

fn report_value(
    report: &editchain_import::ImportReport,
) -> serde_json::Map<String, serde_json::Value> {
    serde_json::Map::from_iter([
        ("files_discovered".into(), report.files_discovered.into()),
        ("files_processed".into(), report.files_processed.into()),
        ("raw_ops".into(), report.raw_ops.into()),
        ("normalized_ops".into(), report.normalized_ops.into()),
        ("evidence_ops".into(), report.evidence_ops.into()),
        ("duplicates".into(), report.duplicates.into()),
        ("malformed".into(), report.malformed.into()),
        ("conflicts".into(), report.op_conflicts.into()),
    ])
}
