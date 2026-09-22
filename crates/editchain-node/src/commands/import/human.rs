//! Import locally archived human editor history (`--provider human`).
//!
//! The VS Code recorder can retain one append-only JSONL archive per
//! activation. Every line is a frozen envelope carrying one complete
//! [`EditorEvent`]:
//!
//! ```text
//! {"format":"editchain-human-history","schema":1,"workspace_path":"/abs/ws","event":{...}}
//! ```
//!
//! Import replays those events through the same canonical admission path the
//! live recorder uses ([`crate::editor::record`]), so source identities,
//! attribution, before/after buffer contents, Git context, and lifecycle
//! observations are preserved, and re-importing an unchanged archive is an
//! exact, idempotent replay.
//!
//! Each archive is preflighted and then replayed over the same captured byte
//! prefix: its exact length at discovery bounds both reads. An archive that is
//! still being appended to therefore cannot inject unvalidated tail records,
//! cannot be followed indefinitely, and a prefix that ends mid-record is
//! rejected with a clear error instead of being parsed. Preflight validates the
//! prefix in line order — envelope format and schema, per-event canonical
//! validity, per-session sequence continuity, and stable identity within a
//! recorder incarnation — before any operation is written, so a malformed,
//! truncated, or incomplete prefix never produces a partial chain.
//!
//! Cross-record admission checks that need the chain itself (an identity that
//! conflicts with already-retained content, for example) still run during the
//! replay and fail loudly, leaving earlier batches durable and the failure
//! reproducible on retry.
//!
//! # Workspace and relocation assumptions
//!
//! `workspace_path` is the workspace the recorder observed, not the location of
//! this invocation. Records whose recorded workspace does not match
//! `--workspace` are counted and skipped, so one archive directory can hold
//! history for several workspaces. Matching canonicalizes both paths when they
//! exist and otherwise compares them lexically, so a workspace that was moved
//! to a different absolute path no longer matches: rebuild from the original
//! path, or record again from the new location. The destination chain is
//! selected solely by `--chain` (relative paths resolve against the current
//! directory, like the Claude and Codex providers).

use std::collections::{BTreeMap, BTreeSet};
use std::fs::File;
use std::io::{self, BufRead, BufReader};
use std::path::{Path, PathBuf};

use editchain_core::human::HumanIdentity;
use editchain_import::ImportOptions;
use editchain_protocol::editor::{EditorEvent, RecordEditorEvents};
use editchain_protocol::RequestBody;

use crate::editor::Encoding;

/// Archive envelope discriminator.
const ARCHIVE_FORMAT: &str = "editchain-human-history";
/// Supported archive envelope schema.
const ARCHIVE_SCHEMA: u32 = 1;
/// Canonical editor batch bound shared with the protocol.
const MAX_BATCH_EVENTS: usize = 128;
/// Serialized source bytes retained in one replay batch.
///
/// Measured from the archive line itself, not estimated from the event kind, so
/// a record with bulk ranges or edit receipts is budgeted by what it really
/// costs to retain. One record larger than this travels in a batch of its own.
const MAX_BATCH_BYTES: usize = 32 * 1024 * 1024;
/// Bounded number of foreign workspace paths reported when none match.
const MAX_REPORTED_WORKSPACES: usize = 5;
/// Largest single JSONL record accepted; one event plus its envelope.
const MAX_LINE_BYTES: usize = editchain_protocol::MAX_REQUEST_FRAME_BYTES;

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

/// One frozen archive envelope: exactly one complete editor observation.
#[derive(Debug, serde::Deserialize)]
struct ArchiveLine {
    /// Envelope discriminator; must equal [`ARCHIVE_FORMAT`].
    format: String,
    /// Envelope schema; only [`ARCHIVE_SCHEMA`] is supported.
    schema: u32,
    /// Absolute workspace observed by the recorder.
    workspace_path: String,
    /// The complete, unmodified editor observation.
    event: EditorEvent,
}

/// Validation outcome for one archive file.
#[derive(Debug, Default)]
struct ArchivePlan {
    /// Records whose recorded workspace matched the requested one.
    records: u64,
    /// Records skipped because they belong to another workspace.
    skipped: u64,
    /// Recorder incarnations retained by this file.
    sessions: BTreeSet<String>,
    /// Bounded sample of recorded workspaces that did not match.
    foreign: Vec<String>,
}

/// One parsed archive record plus the exact source bytes it occupied.
#[derive(Debug)]
struct ArchiveRecord {
    /// One-based line number.
    line: usize,
    /// Serialized bytes of the JSONL record, excluding its newline.
    bytes: usize,
    /// Parsed envelope.
    envelope: ArchiveLine,
}

/// Durable outcome of replaying one archive.
#[derive(Debug, Default, Clone, Copy)]
struct ReplayOutcome {
    /// Source events accepted into the chain.
    written: u64,
    /// Source events already retained by the chain.
    replayed: u64,
    /// Canonical admission batches written for this archive.
    batches: u64,
}

/// Aggregate bounds for one replay batch.
#[derive(Debug, Clone, Copy)]
struct BatchBounds {
    /// Maximum source events retained in one batch.
    events: usize,
    /// Maximum serialized source bytes retained in one batch.
    bytes: usize,
}

impl Default for BatchBounds {
    fn default() -> Self {
        Self {
            events: MAX_BATCH_EVENTS,
            bytes: MAX_BATCH_BYTES,
        }
    }
}

impl BatchBounds {
    /// Whether a pending batch of these sizes must be admitted before more input.
    fn is_full(&self, pending_events: usize, pending_bytes: usize) -> bool {
        pending_events >= self.events || pending_bytes >= self.bytes
    }
}

/// One archive file plus the captured byte prefix both phases may read.
#[derive(Debug, Clone)]
struct ArchiveSource {
    /// Archive path.
    path: PathBuf,
    /// File length captured at discovery; bounds preflight and replay alike.
    prefix_len: u64,
}

/// Cross-record state for one recorder incarnation within one archive.
#[derive(Debug)]
struct SessionState {
    /// Persistent attribution, which may not change within an incarnation.
    identity: Option<HumanIdentity>,
    /// Next expected sequence, one-based and gap-free.
    next_sequence: u64,
}

/// Aggregated plan and durable outcomes for one import invocation.
#[derive(Debug, Default)]
struct Plan {
    /// Archive files read.
    files: u64,
    /// Matching records across all archives.
    records: u64,
    /// Records skipped for another workspace.
    skipped: u64,
    /// Source events accepted into the chain.
    written: u64,
    /// Source events already retained by the chain.
    replayed: u64,
    /// Canonical admission batches written across all archives.
    batches: u64,
    /// Distinct recorder incarnations across all archives.
    sessions: BTreeSet<String>,
    /// Bounded sample of recorded workspaces that did not match.
    foreign: Vec<String>,
}

impl Plan {
    fn merge(&mut self, plan: ArchivePlan) {
        self.records = self.records.saturating_add(plan.records);
        self.skipped = self.skipped.saturating_add(plan.skipped);
        self.sessions.extend(plan.sessions);
        for workspace in plan.foreign {
            if self.foreign.len() < MAX_REPORTED_WORKSPACES && !self.foreign.contains(&workspace) {
                self.foreign.push(workspace);
            }
        }
    }
}

/// Run the human history import for `--provider human`.
///
/// # Errors
///
/// Returns an error when no source is configured, no archive or no matching
/// record exists, an archive is malformed, truncated, incomplete, or carries an
/// unsupported format/schema, a canonical editor admission fails, or the
/// caller cancels preflight or replay.
pub(super) fn run(
    sessions_dir: &str,
    workspace: &str,
    chain: &str,
    dry_run: bool,
    options: &ImportOptions,
) -> Result<()> {
    if sessions_dir.trim().is_empty() {
        return Err("--provider human requires --sessions-dir <jsonl file or directory>".into());
    }
    let source = Path::new(sessions_dir);
    let archives = discover_archives(source)?;
    if archives.is_empty() {
        return Err(format!("no .jsonl human history archives in {}", source.display()).into());
    }
    // Validate every captured prefix before writing anything: a broken source
    // never yields a partial chain.
    let mut plan = Plan {
        files: u64::try_from(archives.len())?,
        ..Plan::default()
    };
    for archive in &archives {
        options.cancellation.check(&archive.path)?;
        plan.merge(plan_archive(archive, workspace, options)?);
    }
    if plan.records == 0 {
        let sample = if plan.foreign.is_empty() {
            String::new()
        } else {
            format!("; recorded workspaces include: {}", plan.foreign.join(", "))
        };
        return Err(format!(
            "no human history records for workspace {workspace} in {}{sample}",
            source.display()
        )
        .into());
    }
    if dry_run {
        print_preview(&plan);
        return Ok(());
    }
    let chain_root = resolve_chain_root(chain)?;
    for archive in &archives {
        options.cancellation.check(&archive.path)?;
        let outcome = import_archive(
            archive,
            workspace,
            &chain_root,
            options,
            BatchBounds::default(),
        )?;
        plan.written = plan.written.saturating_add(outcome.written);
        plan.replayed = plan.replayed.saturating_add(outcome.replayed);
        plan.batches = plan.batches.saturating_add(outcome.batches);
    }
    print_report(&plan);
    report_checkpoint(workspace, &chain_root);
    Ok(())
}

/// Prepare the derived live checkpoint exactly like the other providers.
#[expect(
    clippy::print_stdout,
    reason = "CLI command reports checkpoint readiness to stdout"
)]
fn report_checkpoint(workspace: &str, chain_root: &Path) {
    match crate::history::prepare_live_checkpoint(Path::new(workspace), chain_root) {
        Ok(snapshot) => println!(
            "Live checkpoint ready: {} visible rows at {}/live-v1",
            snapshot.nodes,
            snapshot.chain
        ),
        Err(error) => println!(
            "Live checkpoint preparation failed (import remains durable; run prepare-view to retry): {error}"
        ),
    }
}

#[expect(
    clippy::print_stdout,
    reason = "CLI command reports the validated human history preview to stdout"
)]
fn print_preview(plan: &Plan) {
    println!("Human history import preview:");
    println!("  Archives: {}", plan.files);
    println!("  Records: {}", plan.records);
    println!("  Recorder sessions: {}", plan.sessions.len());
    println!("  Skipped (other workspace): {}", plan.skipped);
    println!("  Dry run: chain not modified");
}

#[expect(
    clippy::print_stdout,
    reason = "CLI command reports durable human history import outcomes to stdout"
)]
fn print_report(plan: &Plan) {
    println!("Human history import complete:");
    println!("  Archives: {}", plan.files);
    println!("  Records: {}", plan.records);
    println!("  Recorder sessions: {}", plan.sessions.len());
    println!("  Skipped (other workspace): {}", plan.skipped);
    println!("  Written source events: {}", plan.written);
    println!("  Exact duplicates: {}", plan.replayed);
    println!("  Replay batches: {}", plan.batches);
}

/// Resolve `--chain` the way the other providers do: relative to the current
/// directory, absolute when already absolute.
fn resolve_chain_root(chain: &str) -> Result<PathBuf> {
    let path = PathBuf::from(chain);
    if path.is_absolute() {
        Ok(path)
    } else {
        Ok(std::env::current_dir()?.join(path))
    }
}

/// List the archives to read, in deterministic order, with their captured
/// prefix length.
///
/// A file argument is used directly. A directory contributes every `.jsonl`
/// file, ordered by date prefix and then by the session counter compared
/// numerically, so an unpadded `-session-9` still precedes `-session-10`.
/// Names that do not match the convention sort last by name.
fn discover_archives(source: &Path) -> Result<Vec<ArchiveSource>> {
    let metadata = std::fs::metadata(source).map_err(|error| {
        io::Error::new(
            error.kind(),
            format!(
                "cannot read human history source {}: {error}",
                source.display()
            ),
        )
    })?;
    if metadata.is_file() {
        return Ok(vec![ArchiveSource {
            path: source.to_path_buf(),
            prefix_len: metadata.len(),
        }]);
    }
    if !metadata.is_dir() {
        return Err(format!(
            "human history source is neither a file nor a directory: {}",
            source.display()
        )
        .into());
    }
    let mut archives = Vec::new();
    for entry in std::fs::read_dir(source)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_file()
            && path
                .extension()
                .is_some_and(|extension| extension == "jsonl")
        {
            archives.push(ArchiveSource {
                prefix_len: entry.metadata()?.len(),
                path,
            });
        }
    }
    archives.sort_by_key(|archive| archive_order_key(&archive.path));
    Ok(archives)
}

/// Deterministic directory ordering key: `(unmatched, date, counter, name)`.
fn archive_order_key(path: &Path) -> (u8, String, u64, String) {
    let name = path
        .file_name()
        .map_or_else(String::new, |name| name.to_string_lossy().into_owned());
    match parse_archive_name(&name) {
        Some((date, counter)) => (0, date, counter, name),
        None => (1, String::new(), 0, name),
    }
}

/// Parse `YYYY-MM-DD-session-<counter>.jsonl` into its ordering components.
fn parse_archive_name(name: &str) -> Option<(String, u64)> {
    let stem = name.strip_suffix(".jsonl")?;
    let (date, counter) = stem.rsplit_once("-session-")?;
    if !is_archive_date(date) || counter.is_empty() {
        return None;
    }
    if !counter.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    Some((date.to_string(), counter.parse::<u64>().ok()?))
}

/// Strict `YYYY-MM-DD` shape check.
fn is_archive_date(date: &str) -> bool {
    let bytes = date.as_bytes();
    bytes.len() == 10
        && bytes.iter().enumerate().all(|(index, byte)| {
            if index == 4 || index == 7 {
                *byte == b'-'
            } else {
                byte.is_ascii_digit()
            }
        })
}

/// Validate one captured archive prefix completely, without writing anything.
///
/// Cancellation is polled for every record, so a long full-buffer preflight
/// stops promptly instead of parsing to the end of the prefix.
fn plan_archive(
    source: &ArchiveSource,
    workspace: &str,
    options: &ImportOptions,
) -> Result<ArchivePlan> {
    let path = source.path.as_path();
    let mut reader = ArchiveReader::open(path, source.prefix_len)?;
    let mut plan = ArchivePlan::default();
    let mut sessions: BTreeMap<String, SessionState> = BTreeMap::new();
    while let Some(record) = reader.next_record()? {
        options.cancellation.check(path)?;
        if !same_workspace(&record.envelope.workspace_path, workspace) {
            plan.skipped = plan.skipped.saturating_add(1);
            if plan.foreign.len() < MAX_REPORTED_WORKSPACES
                && !plan.foreign.contains(&record.envelope.workspace_path)
            {
                plan.foreign.push(record.envelope.workspace_path.clone());
            }
            continue;
        }
        let session = record.envelope.event.session.clone();
        let sequence = record.envelope.event.sequence;
        let identity = record.envelope.event.identity.clone();
        let line = record.line;
        validate_event(path, line, record.envelope)?;
        let state = sessions
            .entry(session.clone())
            .or_insert_with(|| SessionState {
                identity: identity.clone(),
                next_sequence: 1,
            });
        if state.identity != identity {
            return Err(archive_error(
                path,
                line,
                format!("recorder identity changed within session {session}"),
            ));
        }
        if sequence != state.next_sequence {
            return Err(archive_error(
                path,
                line,
                format!(
                    "archive is incomplete for session {session}: expected sequence {}, found {sequence}",
                    state.next_sequence
                ),
            ));
        }
        state.next_sequence = state.next_sequence.saturating_add(1);
        plan.records = plan.records.saturating_add(1);
    }
    plan.sessions.extend(sessions.into_keys());
    Ok(plan)
}

/// Reject an event the canonical recorder would reject.
fn validate_event(path: &Path, line: usize, record: ArchiveLine) -> Result<()> {
    let batch = RecordEditorEvents {
        workspace_path: record.workspace_path,
        chain_dir: String::new(),
        events: vec![record.event],
    };
    RequestBody::RecordEditorEvents(batch)
        .validate()
        .map_err(|error| archive_error(path, line, format!("invalid editor event: {error}")))
}

/// Replay one validated archive through the canonical editor admission path.
fn import_archive(
    source: &ArchiveSource,
    workspace: &str,
    chain_root: &Path,
    options: &ImportOptions,
    bounds: BatchBounds,
) -> Result<ReplayOutcome> {
    let path = source.path.as_path();
    let mut reader = ArchiveReader::open(path, source.prefix_len)?;
    let mut context = ImportContext::new(workspace, chain_root)?;
    let mut pending: Vec<EditorEvent> = Vec::new();
    let mut pending_bytes = 0usize;
    let mut pending_session: Option<String> = None;
    while let Some(record) = reader.next_record()? {
        // Polled before the workspace filter so a prefix full of foreign
        // records cannot skip every batch-bound cancellation check.
        options.cancellation.check(path)?;
        if !same_workspace(&record.envelope.workspace_path, workspace) {
            continue;
        }
        // One batch never mixes recorder incarnations, so the batch bound also
        // bounds the encoder's single retained buffer.
        let session = record.envelope.event.session.clone();
        if pending_session.as_deref() != Some(session.as_str()) {
            if !pending.is_empty() {
                options.cancellation.check(path)?;
                context.flush(&mut pending)?;
                pending_bytes = 0;
            }
            context.reset_encoding();
            pending_session = Some(session);
        }
        // Never merge a record into a batch that would exceed the byte budget,
        // so one oversized record travels in a batch of its own.
        if !pending.is_empty() && pending_bytes.saturating_add(record.bytes) > bounds.bytes {
            options.cancellation.check(path)?;
            context.flush(&mut pending)?;
            pending_bytes = 0;
        }
        pending_bytes = pending_bytes.saturating_add(record.bytes);
        pending.push(record.envelope.event);
        if bounds.is_full(pending.len(), pending_bytes) {
            options.cancellation.check(path)?;
            context.flush(&mut pending)?;
            pending_bytes = 0;
        }
    }
    if !pending.is_empty() {
        options.cancellation.check(path)?;
        context.flush(&mut pending)?;
    }
    Ok(ReplayOutcome {
        written: context.written,
        replayed: context.replayed,
        batches: context.batches,
    })
}

/// Canonical admission state shared across every batch of one archive run.
struct ImportContext {
    /// Destination workspace; used by the canonical recorder for its index.
    workspace: String,
    /// Absolute chain root, passed as the recorder's chain directory.
    chain_dir: String,
    /// Canonical source encoder, retaining at most one previous buffer.
    encoding: Encoding,
    /// Source events accepted into the chain.
    written: u64,
    /// Source events already retained by the chain.
    replayed: u64,
    /// Canonical admission batches written.
    batches: u64,
}

impl ImportContext {
    fn new(workspace: &str, chain_root: &Path) -> Result<Self> {
        let chain_dir = chain_root
            .to_str()
            .ok_or("chain path is not valid UTF-8")?
            .to_string();
        Ok(Self {
            workspace: workspace.to_string(),
            chain_dir,
            encoding: Encoding::default(),
            written: 0,
            replayed: 0,
            batches: 0,
        })
    }

    /// Drop the retained buffer at a recorder-incarnation boundary.
    fn reset_encoding(&mut self) {
        self.encoding = Encoding::default();
    }

    /// Admit one bounded batch, preserving the archive's line order.
    fn flush(&mut self, events: &mut Vec<EditorEvent>) -> Result<()> {
        let batch = RecordEditorEvents {
            workspace_path: self.workspace.clone(),
            chain_dir: self.chain_dir.clone(),
            events: std::mem::take(events),
        };
        let outcome = crate::editor::record(&batch, &mut self.encoding)?;
        self.written = self.written.saturating_add(
            outcome
                .get("accepted")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(0),
        );
        self.replayed = self.replayed.saturating_add(
            outcome
                .get("replayed")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(0),
        );
        self.batches = self.batches.saturating_add(1);
        Ok(())
    }
}

/// Compare a recorded workspace with the requested one.
///
/// Both paths are canonicalized when they exist; otherwise trailing separators
/// are ignored and the text is compared directly.
fn same_workspace(recorded: &str, requested: &str) -> bool {
    if recorded == requested {
        return true;
    }
    let recorded = Path::new(recorded);
    let requested = Path::new(requested);
    match (recorded.canonicalize(), requested.canonicalize()) {
        (Ok(left), Ok(right)) => left == right,
        _ => lexical_workspace(recorded) == lexical_workspace(requested),
    }
}

/// Text form of a path without trailing separators.
fn lexical_workspace(path: &Path) -> String {
    path.to_string_lossy()
        .trim_end_matches(['/', '\\'])
        .to_string()
}

/// Bounded, streaming reader for one captured archive prefix.
struct ArchiveReader {
    /// Archive path, used only for diagnostics.
    path: PathBuf,
    /// Buffered file reader.
    reader: BufReader<File>,
    /// Bytes left in the captured prefix.
    remaining: u64,
    /// Last line number read (one-based).
    line: usize,
    /// Reused line buffer.
    buffer: Vec<u8>,
}

impl ArchiveReader {
    /// Open `path`, reading at most `prefix_len` bytes from its start.
    fn open(path: &Path, prefix_len: u64) -> Result<Self> {
        let file = File::open(path).map_err(|error| {
            io::Error::new(
                error.kind(),
                format!(
                    "cannot open human history archive {}: {error}",
                    path.display()
                ),
            )
        })?;
        Ok(Self {
            path: path.to_path_buf(),
            reader: BufReader::new(file),
            remaining: prefix_len,
            line: 0,
            buffer: Vec::new(),
        })
    }

    /// Read the next non-blank envelope within the captured prefix.
    fn next_record(&mut self) -> Result<Option<ArchiveRecord>> {
        loop {
            if self.remaining == 0 {
                return Ok(None);
            }
            let outcome = read_bounded_line(
                &mut self.reader,
                &mut self.buffer,
                MAX_LINE_BYTES,
                &mut self.remaining,
            )
            .map_err(|error| archive_error(&self.path, self.line.saturating_add(1), error))?;
            if outcome == LineRead::ShortFile {
                return Err(archive_error(
                    &self.path,
                    self.line.saturating_add(1),
                    "archive shrank below its captured length while importing",
                ));
            }
            self.line = self.line.checked_add(1).ok_or_else(|| {
                archive_error(&self.path, self.line, "archive line counter overflowed")
            })?;
            let text = std::str::from_utf8(&self.buffer)
                .map_err(|error| {
                    archive_error(
                        &self.path,
                        self.line,
                        format!("record is not valid UTF-8: {error}"),
                    )
                })?
                .trim();
            if text.is_empty() {
                continue;
            }
            let record: ArchiveLine = serde_json::from_str(text).map_err(|error| {
                let hint = if outcome == LineRead::PrefixEnd {
                    " (archive prefix ends mid-record; the file may have been captured mid-write)"
                } else {
                    ""
                };
                archive_error(
                    &self.path,
                    self.line,
                    format!("malformed human history record: {error}{hint}"),
                )
            })?;
            if record.format != ARCHIVE_FORMAT {
                return Err(archive_error(
                    &self.path,
                    self.line,
                    format!("unsupported archive format {:?}", record.format),
                ));
            }
            if record.schema != ARCHIVE_SCHEMA {
                return Err(archive_error(
                    &self.path,
                    self.line,
                    format!("unsupported human history schema {}", record.schema),
                ));
            }
            return Ok(Some(ArchiveRecord {
                line: self.line,
                bytes: self.buffer.len(),
                envelope: record,
            }));
        }
    }
}

/// How one bounded line read ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LineRead {
    /// A newline was consumed within the prefix.
    Line,
    /// The captured prefix ended before a newline; the buffer holds its tail.
    PrefixEnd,
    /// The file ended before the captured prefix length was reached.
    ShortFile,
}

/// Read one newline-terminated line within the remaining captured prefix.
///
/// Never exceeds `max_line` bytes in `buffer`, and never reads past the prefix.
fn read_bounded_line(
    reader: &mut impl BufRead,
    buffer: &mut Vec<u8>,
    max_line: usize,
    remaining: &mut u64,
) -> io::Result<LineRead> {
    buffer.clear();
    while *remaining > 0 {
        let available = reader.fill_buf()?;
        if available.is_empty() {
            return Ok(LineRead::ShortFile);
        }
        let budget = usize::try_from(*remaining)
            .unwrap_or(usize::MAX)
            .min(available.len());
        let window = available.get(..budget).unwrap_or(available);
        let newline = window.iter().position(|byte| *byte == b'\n');
        let end = newline.unwrap_or(window.len());
        let chunk = window.get(..end).unwrap_or_default();
        if buffer.len().saturating_add(chunk.len()) > max_line {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("human history record exceeds {max_line} bytes"),
            ));
        }
        buffer.extend_from_slice(chunk);
        let consumed = newline.map_or(window.len(), |index| index.saturating_add(1));
        let consumed = u64::try_from(consumed).map_err(io::Error::other)?;
        *remaining = remaining.saturating_sub(consumed);
        reader.consume(usize::try_from(consumed).unwrap_or(usize::MAX));
        if newline.is_some() {
            return Ok(LineRead::Line);
        }
    }
    Ok(LineRead::PrefixEnd)
}

/// Build a path-qualified archive error.
fn archive_error(
    path: &Path,
    line: usize,
    message: impl std::fmt::Display,
) -> Box<dyn std::error::Error> {
    io::Error::new(
        io::ErrorKind::InvalidData,
        format!("{}:{line}: {message}", path.display()),
    )
    .into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use editchain_core::human::{HumanWorkKind, HumanWorkRecord};
    use editchain_core::{Op, OpKind, Payload};
    use editchain_project::human::work_record;
    use editchain_store::format::decode_op;
    use editchain_store::{BlobReader, BlobStore, SegmentStore};
    use serde_json::{json, Value};

    /// Deterministic, schema-valid recorder incarnation for `n`.
    fn session(n: u64) -> String {
        format!("{n:08x}-0000-4000-8000-000000000000")
    }

    fn event(sequence: u64, session: &str, data: &Value) -> Value {
        json!({
            "schema": 1,
            "session": session,
            "sequence": sequence,
            "time_ms": 1_000_000_u64.saturating_add(sequence),
            "event": data,
        })
    }

    fn start(session: &str) -> Value {
        event(
            1,
            session,
            &json!({"type":"tracking_started","dwell_ms":2000,"vscode_version":"1.90.0"}),
        )
    }

    fn document(version: u64) -> Value {
        json!({"id":"buffer-1","uri":"file:///notes.txt","path":"notes.txt","version":version})
    }

    fn snapshot(sequence: u64, session: &str, version: u64, text: &str) -> Value {
        event(
            sequence,
            session,
            &json!({"type":"document_snapshot","document":document(version),"text":text}),
        )
    }

    fn change(sequence: u64, session: &str, version: u64, before: &str, after: &str) -> Value {
        event(
            sequence,
            session,
            &json!({
                "type":"document_changed",
                "document":document(version),
                "before_version":version.saturating_sub(1),
                "before":before,
                "after":after,
                "reason":null,
                "changes":[{"offset":0,"length":before.encode_utf16().count(),"text":after}],
            }),
        )
    }

    fn human_edit(sequence: u64, session: &str, change: u64) -> Value {
        event(
            sequence,
            session,
            &json!({"type":"human_edit","change":change,"signal":"editor_input"}),
        )
    }

    fn saved(sequence: u64, session: &str, version: u64) -> Value {
        event(
            sequence,
            session,
            &json!({"type":"document_saved","document":document(version)}),
        )
    }

    fn identity(n: u64) -> Value {
        json!({
            "kind": "unsigned",
            "guid": format!("{n:08x}-1111-4111-8111-111111111111"),
            "stream": format!("{n:024x}"),
        })
    }

    /// A legacy selection event with `ranges` bulk payload.
    fn selection(sequence: u64, session: &str, ranges: usize) -> Value {
        let range = json!({"start":[0,0],"end":[0,1]});
        let ranges: Vec<Value> = (0..ranges).map(|_| range.clone()).collect();
        event(
            sequence,
            session,
            &json!({
                "type":"selection_changed",
                "document":document(1),
                "editor":"editor-1",
                "ranges":ranges,
                "keyboard":true,
            }),
        )
    }

    fn stopped(sequence: u64, session: &str) -> Value {
        event(sequence, session, &json!({"type":"tracking_stopped"}))
    }

    /// One complete recorder incarnation: snapshot, human edit, save, stop.
    fn session_events(session: &str, before: &str, after: &str) -> Vec<Value> {
        vec![
            start(session),
            snapshot(2, session, 1, before),
            change(3, session, 2, before, after),
            human_edit(4, session, 3),
            saved(5, session, 2),
            stopped(6, session),
        ]
    }

    fn line(workspace: &Path, event: &Value) -> String {
        json!({
            "format": ARCHIVE_FORMAT,
            "schema": ARCHIVE_SCHEMA,
            "workspace_path": workspace.to_string_lossy(),
            "event": event,
        })
        .to_string()
    }

    fn archive_text(workspace: &Path, events: &[Value]) -> String {
        let mut body = String::new();
        for event in events {
            body.push_str(&line(workspace, event));
            body.push('\n');
        }
        body
    }

    fn write_archive(path: &Path, workspace: &Path, events: &[Value]) {
        std::fs::write(path, archive_text(workspace, events)).unwrap();
    }

    fn append_archive(path: &Path, workspace: &Path, events: &[Value]) {
        use std::io::Write as _;
        let mut file = std::fs::OpenOptions::new().append(true).open(path).unwrap();
        file.write_all(archive_text(workspace, events).as_bytes())
            .unwrap();
    }

    fn import(sessions: &Path, workspace: &Path, chain: &Path) -> Result<()> {
        let sessions = sessions.to_string_lossy().into_owned();
        let workspace = workspace.to_string_lossy().into_owned();
        let chain = chain.to_string_lossy().into_owned();
        run(
            &sessions,
            &workspace,
            &chain,
            false,
            &ImportOptions::default(),
        )
    }

    fn read_ops(chain: &Path) -> Vec<Op> {
        let store = SegmentStore::open(chain).unwrap();
        let mut ops = Vec::new();
        for page in store.read_all().unwrap() {
            for record in page.records {
                ops.push(decode_op(&record.data).unwrap());
            }
        }
        ops
    }

    /// Decode every retained `vscode.editor` source payload in append order.
    fn raw_events(chain: &Path) -> Vec<Value> {
        let reader = BlobReader::open(chain).unwrap();
        read_ops(chain)
            .iter()
            .filter_map(|op| {
                let OpKind::Import(import) = &op.kind else {
                    return None;
                };
                match &import.raw_ref {
                    Payload::Blob(blob) => reader.resolve_content(blob.id),
                    Payload::Inline(_) | Payload::Empty => None,
                }
            })
            .filter_map(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
            .filter(|value| value.get("source").and_then(Value::as_str) == Some("vscode.editor"))
            .collect()
    }

    fn first_source_index(chain: &Path, recorder: &str) -> usize {
        raw_events(chain)
            .iter()
            .position(|value| {
                value.pointer("/event/session").and_then(Value::as_str) == Some(recorder)
            })
            .unwrap()
    }

    fn fixture() -> (tempfile::TempDir, PathBuf, PathBuf, PathBuf) {
        let tmp = tempfile::tempdir().unwrap();
        let workspace = tmp.path().join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        let sessions = tmp.path().join("sessions");
        std::fs::create_dir_all(&sessions).unwrap();
        let chain = tmp.path().join("chain");
        (tmp, workspace, sessions, chain)
    }

    #[test]
    fn roundtrip_preserves_before_after_attribution_and_identity() {
        let (_tmp, workspace, sessions, chain) = fixture();
        let recorder = session(1);
        write_archive(
            &sessions.join("2026-09-21-session-0001.jsonl"),
            &workspace,
            &session_events(&recorder, "AI\n", "AIh\n"),
        );

        import(&sessions, &workspace, &chain).unwrap();

        let raw = raw_events(&chain);
        let change = raw
            .iter()
            .find(|value| {
                value.pointer("/event/event/type").and_then(Value::as_str)
                    == Some("document_changed")
            })
            .unwrap();
        assert_eq!(
            change
                .pointer("/event/event/before")
                .and_then(Value::as_str),
            Some("AI\n"),
            "the exact source buffer must survive import"
        );
        assert_eq!(
            change.pointer("/event/event/after").and_then(Value::as_str),
            Some("AIh\n"),
            "the exact destination buffer must survive import"
        );
        assert_eq!(
            change.pointer("/event/session").and_then(Value::as_str),
            Some(recorder.as_str()),
            "the recorder incarnation must survive import"
        );
        let receipt = raw
            .iter()
            .find(|value| {
                value.pointer("/event/event/type").and_then(Value::as_str) == Some("human_edit")
            })
            .unwrap();
        assert_eq!(
            receipt
                .pointer("/event/event/signal")
                .and_then(Value::as_str),
            Some("editor_input"),
            "human attribution evidence must survive import"
        );

        let records: Vec<HumanWorkRecord> =
            read_ops(&chain).iter().filter_map(work_record).collect();
        let edit = records
            .iter()
            .find(|record| record.kind == HumanWorkKind::Edit)
            .unwrap();
        assert_eq!(edit.path.as_deref(), Some("notes.txt"));
        assert_eq!(edit.session, recorder);
        let blobs = BlobStore::open_read_only(chain.join("blobs"))
            .unwrap()
            .unwrap();
        let before = edit.before.as_ref().map(|revision| revision.content);
        let after = edit.after.as_ref().map(|revision| revision.content);
        assert_eq!(
            before.and_then(|id| blobs.resolve_content(id)),
            Some(b"AI\n".to_vec()),
            "derived human work must reference the exact source content"
        );
        assert_eq!(
            after.and_then(|id| blobs.resolve_content(id)),
            Some(b"AIh\n".to_vec())
        );
    }

    #[test]
    fn repeated_import_writes_no_duplicates() {
        let (_tmp, workspace, sessions, chain) = fixture();
        let recorder = session(2);
        write_archive(
            &sessions.join("2026-09-21-session-0001.jsonl"),
            &workspace,
            &session_events(&recorder, "one\n", "one!\n"),
        );

        import(&sessions, &workspace, &chain).unwrap();
        let first = read_ops(&chain).len();
        assert!(first > 0, "the first import must write operations");

        import(&sessions, &workspace, &chain).unwrap();
        assert_eq!(
            read_ops(&chain).len(),
            first,
            "re-importing an unchanged archive must be an exact replay"
        );
    }

    #[test]
    fn append_only_growth_imports_only_new_events() {
        let (_tmp, workspace, sessions, chain) = fixture();
        let recorder = session(3);
        let path = sessions.join("2026-09-21-session-0001.jsonl");
        write_archive(
            &path,
            &workspace,
            &[
                start(&recorder),
                snapshot(2, &recorder, 1, "a"),
                change(3, &recorder, 2, "a", "ab"),
                human_edit(4, &recorder, 3),
                saved(5, &recorder, 2),
            ],
        );

        import(&sessions, &workspace, &chain).unwrap();
        let first = read_ops(&chain).len();

        append_archive(
            &path,
            &workspace,
            &[
                change(6, &recorder, 3, "ab", "abc"),
                human_edit(7, &recorder, 6),
                saved(8, &recorder, 3),
            ],
        );
        import(&sessions, &workspace, &chain).unwrap();
        let second = read_ops(&chain).len();
        assert!(
            second > first,
            "events appended to an archive must be imported"
        );

        import(&sessions, &workspace, &chain).unwrap();
        assert_eq!(
            read_ops(&chain).len(),
            second,
            "the appended archive must replay idempotently"
        );
    }

    #[test]
    fn directory_imports_in_date_then_numeric_counter_order() {
        let (_tmp, workspace, sessions, chain) = fixture();
        let ninth = session(9);
        let tenth = session(10);
        let next_day = session(11);
        write_archive(
            &sessions.join("2026-09-21-session-9.jsonl"),
            &workspace,
            &session_events(&ninth, "nine\n", "nine!\n"),
        );
        write_archive(
            &sessions.join("2026-09-21-session-10.jsonl"),
            &workspace,
            &session_events(&tenth, "ten\n", "ten!\n"),
        );
        write_archive(
            &sessions.join("2026-09-22-session-1.jsonl"),
            &workspace,
            &session_events(&next_day, "next\n", "next!\n"),
        );

        import(&sessions, &workspace, &chain).unwrap();

        let first = first_source_index(&chain, &ninth);
        let second = first_source_index(&chain, &tenth);
        let third = first_source_index(&chain, &next_day);
        assert!(
            first < second,
            "the session counter must compare numerically, not lexically"
        );
        assert!(
            second < third,
            "a later date must order after an earlier one"
        );
    }

    #[test]
    fn one_file_with_sequential_recorder_restarts_keeps_line_order() {
        let (_tmp, workspace, sessions, chain) = fixture();
        let first = session(50);
        let second = session(51);
        let mut events = session_events(&first, "first\n", "first!\n");
        events.extend(session_events(&second, "second\n", "second!\n"));
        write_archive(
            &sessions.join("2026-09-21-session-0001.jsonl"),
            &workspace,
            &events,
        );

        import(&sessions, &workspace, &chain).unwrap();

        assert!(
            first_source_index(&chain, &first) < first_source_index(&chain, &second),
            "a later incarnation in the same file must append after the earlier one"
        );
    }

    #[test]
    fn interleaved_sessions_keep_line_order() {
        let (_tmp, workspace, sessions, chain) = fixture();
        let a = session(60);
        let b = session(61);
        write_archive(
            &sessions.join("2026-09-21-session-0001.jsonl"),
            &workspace,
            &[
                start(&a),
                start(&b),
                snapshot(2, &a, 1, "a"),
                snapshot(2, &b, 1, "b"),
                change(3, &a, 2, "a", "a!"),
                change(3, &b, 2, "b", "b!"),
                human_edit(4, &a, 3),
                human_edit(4, &b, 3),
            ],
        );

        import(&sessions, &workspace, &chain).unwrap();

        let order: Vec<String> = raw_events(&chain)
            .iter()
            .filter_map(|value| {
                value
                    .pointer("/event/session")
                    .and_then(Value::as_str)
                    .map(str::to_string)
            })
            .collect();
        assert_eq!(
            order,
            vec![
                a.clone(),
                b.clone(),
                a.clone(),
                b.clone(),
                a.clone(),
                b.clone(),
                a,
                b
            ]
        );
    }

    #[test]
    fn identity_changes_within_a_session_are_rejected_before_writing() {
        let (_tmp, workspace, sessions, chain) = fixture();
        let recorder = session(70);
        let started = json!({
            "schema": 1,
            "session": recorder,
            "sequence": 1,
            "time_ms": 1_000_001_u64,
            "identity": identity(1),
            "event": {
                "type": "tracking_started",
                "dwell_ms": 2000,
                "vscode_version": "1.90.0",
                "activity_schema": 3,
            },
        });
        let later = json!({
            "schema": 1,
            "session": recorder,
            "sequence": 2,
            "time_ms": 1_000_002_u64,
            "identity": identity(2),
            "event": {"type": "document_snapshot", "document": document(1), "text": "a"},
        });
        write_archive(
            &sessions.join("2026-09-21-session-0001.jsonl"),
            &workspace,
            &[started, later],
        );

        let error = import(&sessions, &workspace, &chain).unwrap_err();
        assert!(
            error.to_string().contains("recorder identity changed"),
            "{error}"
        );
        assert!(
            !chain.exists(),
            "identity drift must be caught before writing"
        );
    }

    #[test]
    fn workspace_filter_skips_foreign_records() {
        let (tmp, workspace, sessions, chain) = fixture();
        let other = tmp.path().join("other");
        std::fs::create_dir_all(&other).unwrap();
        let mine = session(20);
        let theirs = session(21);
        write_archive(
            &sessions.join("2026-09-21-session-0001.jsonl"),
            &workspace,
            &session_events(&mine, "mine\n", "mine!\n"),
        );
        write_archive(
            &sessions.join("2026-09-21-session-0002.jsonl"),
            &other,
            &session_events(&theirs, "theirs\n", "theirs!\n"),
        );

        import(&sessions, &workspace, &chain).unwrap();

        let raw = raw_events(&chain);
        assert!(raw.iter().any(|value| {
            value.pointer("/event/session").and_then(Value::as_str) == Some(mine.as_str())
        }));
        assert!(!raw.iter().any(|value| {
            value.pointer("/event/session").and_then(Value::as_str) == Some(theirs.as_str())
        }));
    }

    #[test]
    fn import_requires_records_for_the_requested_workspace() {
        let (tmp, workspace, sessions, chain) = fixture();
        let other = tmp.path().join("other");
        std::fs::create_dir_all(&other).unwrap();
        write_archive(
            &sessions.join("2026-09-21-session-0001.jsonl"),
            &other,
            &session_events(&session(30), "other\n", "other!\n"),
        );

        let error = import(&sessions, &workspace, &chain).unwrap_err();
        assert!(
            error.to_string().contains("no human history records"),
            "{error}"
        );
        assert!(
            error
                .to_string()
                .contains(&other.to_string_lossy().into_owned()),
            "the error must sample the recorded workspace: {error}"
        );
        assert!(!chain.exists(), "a skipped import must not create a chain");
    }

    #[test]
    fn broken_archives_are_rejected_without_writing_any_file() {
        let (_tmp, workspace, sessions, chain) = fixture();
        let recorder = session(40);
        let valid = sessions.join("2026-09-21-session-0001.jsonl");
        write_archive(
            &valid,
            &workspace,
            &session_events(&recorder, "ok\n", "ok!\n"),
        );
        let malformed = sessions.join("2026-09-21-session-0002.jsonl");
        let mut body = archive_text(&workspace, &session_events(&session(41), "x\n", "xy\n"));
        body.push_str("{\"format\":\"editchain-human-history\"\n");
        std::fs::write(&malformed, body).unwrap();

        let error = import(&sessions, &workspace, &chain).unwrap_err();
        assert!(error.to_string().contains("malformed"), "{error}");
        assert!(
            !chain.exists(),
            "a malformed sibling must prevent importing the valid archive"
        );
    }

    #[test]
    fn truncated_records_are_rejected() {
        let (_tmp, workspace, sessions, chain) = fixture();
        let recorder = session(42);
        let path = sessions.join("2026-09-21-session-0001.jsonl");
        let mut body = archive_text(
            &workspace,
            &[start(&recorder), snapshot(2, &recorder, 1, "a")],
        );
        body.push_str("{\"format\":\"editchain-human-history\",\"schema\":1,\"work");
        std::fs::write(&path, body).unwrap();

        let error = import(&sessions, &workspace, &chain).unwrap_err();
        assert!(error.to_string().contains("malformed"), "{error}");
        assert!(
            error.to_string().contains("mid-record"),
            "a prefix cut mid-write must say so: {error}"
        );
        assert!(!chain.exists());
    }

    #[test]
    fn reader_reads_only_the_captured_prefix() {
        let (_tmp, workspace, sessions, _chain) = fixture();
        let recorder = session(80);
        let path = sessions.join("2026-09-21-session-0001.jsonl");
        write_archive(
            &path,
            &workspace,
            &[start(&recorder), snapshot(2, &recorder, 1, "a")],
        );
        let full = std::fs::read(&path).unwrap();
        let first_line = full.iter().position(|byte| *byte == b'\n').unwrap();
        let prefix_len = u64::try_from(first_line.saturating_add(1)).unwrap();

        let mut bounded = ArchiveReader::open(&path, prefix_len).unwrap();
        let record = bounded.next_record().unwrap().unwrap();
        assert_eq!(record.envelope.event.sequence, 1);
        assert!(
            bounded.next_record().unwrap().is_none(),
            "bytes past the captured prefix must never be read"
        );

        let mut mid_record = ArchiveReader::open(&path, prefix_len.saturating_add(8)).unwrap();
        assert!(mid_record.next_record().unwrap().is_some());
        let error = mid_record.next_record().unwrap_err();
        assert!(error.to_string().contains("mid-record"), "{error}");
    }

    #[test]
    fn preflight_honors_cancellation_between_records() {
        let (_tmp, workspace, sessions, chain) = fixture();
        write_archive(
            &sessions.join("2026-09-21-session-0001.jsonl"),
            &workspace,
            &session_events(&session(90), "cancel\n", "cancel!\n"),
        );
        let archives = discover_archives(&sessions).unwrap();
        let archive = archives.first().unwrap();
        let workspace_text = workspace.to_string_lossy().into_owned();
        let options = ImportOptions::default();
        options.cancellation.cancel();

        let error = plan_archive(archive, &workspace_text, &options).unwrap_err();
        assert!(error.to_string().contains("cancelled"), "{error}");
        assert!(!chain.exists());
    }

    #[test]
    fn replay_honors_cancellation_while_skipping_foreign_records() {
        let (tmp, workspace, sessions, chain) = fixture();
        let other = tmp.path().join("other");
        std::fs::create_dir_all(&other).unwrap();
        write_archive(
            &sessions.join("2026-09-21-session-0001.jsonl"),
            &other,
            &session_events(&session(91), "other\n", "other!\n"),
        );
        let archives = discover_archives(&sessions).unwrap();
        let archive = archives.first().unwrap();
        let workspace_text = workspace.to_string_lossy().into_owned();
        let options = ImportOptions::default();
        options.cancellation.cancel();

        let error = import_archive(
            archive,
            &workspace_text,
            &chain,
            &options,
            BatchBounds::default(),
        )
        .unwrap_err();
        assert!(error.to_string().contains("cancelled"), "{error}");
        assert!(!chain.exists());
    }

    #[test]
    fn replay_batches_follow_the_serialized_byte_bound() {
        let (_tmp, workspace, sessions, chain) = fixture();
        let recorder = session(95);
        let events = vec![
            start(&recorder),
            selection(2, &recorder, 20_000),
            selection(3, &recorder, 20_000),
            selection(4, &recorder, 20_000),
        ];
        let path = sessions.join("2026-09-21-session-0001.jsonl");
        write_archive(&path, &workspace, &events);
        let archives = discover_archives(&sessions).unwrap();
        let archive = archives.first().unwrap();
        let workspace_text = workspace.to_string_lossy().into_owned();

        // The budget input must be each record's real serialized length, not a
        // per-kind estimate: a bulk range record dwarfs any fixed hint.
        let prefix_len = std::fs::metadata(&path).unwrap().len();
        let mut reader = ArchiveReader::open(&path, prefix_len).unwrap();
        let mut measured = Vec::new();
        while let Some(record) = reader.next_record().unwrap() {
            measured.push(record.bytes);
        }
        assert_eq!(
            measured.first().copied(),
            Some(line(&workspace, &start(&recorder)).len())
        );
        let bulk_bytes = measured.iter().copied().max().unwrap();
        assert!(
            bulk_bytes > 262_144,
            "the fixture must carry bulk range payload, measured {bulk_bytes} bytes"
        );
        assert_eq!(measured.len(), events.len());

        // A bound smaller than one bulk record forces real canonical batches:
        // no batch can absorb two records, so every record is admitted alone.
        let options = ImportOptions::default();
        let small = import_archive(
            archive,
            &workspace_text,
            &chain,
            &options,
            BatchBounds {
                events: MAX_BATCH_EVENTS,
                bytes: 4_096,
            },
        )
        .unwrap();
        assert_eq!(
            small.batches,
            u64::try_from(events.len()).unwrap(),
            "serialized bytes must split one batch per record"
        );
        assert_eq!(small.written, u64::try_from(events.len()).unwrap());

        // A bound wider than the archive keeps it in a single batch.
        let wide = import_archive(
            archive,
            &workspace_text,
            &chain,
            &options,
            BatchBounds {
                events: MAX_BATCH_EVENTS,
                bytes: usize::MAX,
            },
        )
        .unwrap();
        assert_eq!(wide.batches, 1);
        assert!(!read_ops(&chain).is_empty());
    }

    #[test]
    fn sequence_gaps_are_rejected_as_incomplete() {
        let (_tmp, workspace, sessions, chain) = fixture();
        let recorder = session(43);
        write_archive(
            &sessions.join("2026-09-21-session-0001.jsonl"),
            &workspace,
            &[
                start(&recorder),
                snapshot(2, &recorder, 1, "a"),
                change(4, &recorder, 2, "a", "ab"),
            ],
        );

        let error = import(&sessions, &workspace, &chain).unwrap_err();
        assert!(error.to_string().contains("incomplete"), "{error}");
        assert!(!chain.exists());
    }

    #[test]
    fn unsupported_schema_and_format_are_rejected() {
        let (_tmp, workspace, sessions, chain) = fixture();
        let recorder = session(44);
        let path = sessions.join("2026-09-21-session-0001.jsonl");

        let future = json!({
            "format": ARCHIVE_FORMAT,
            "schema": 2,
            "workspace_path": workspace.to_string_lossy(),
            "event": start(&recorder),
        });
        std::fs::write(&path, format!("{future}\n")).unwrap();
        let error = import(&sessions, &workspace, &chain).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("unsupported human history schema"),
            "{error}"
        );

        let foreign = json!({
            "format": "editchain-something-else",
            "schema": ARCHIVE_SCHEMA,
            "workspace_path": workspace.to_string_lossy(),
            "event": start(&recorder),
        });
        std::fs::write(&path, format!("{foreign}\n")).unwrap();
        let error = import(&sessions, &workspace, &chain).unwrap_err();
        assert!(
            error.to_string().contains("unsupported archive format"),
            "{error}"
        );
        assert!(!chain.exists());
    }

    #[test]
    fn dry_run_validates_without_touching_the_chain() {
        let (_tmp, workspace, sessions, chain) = fixture();
        write_archive(
            &sessions.join("2026-09-21-session-0001.jsonl"),
            &workspace,
            &session_events(&session(45), "dry\n", "dry!\n"),
        );

        let sessions_text = sessions.to_string_lossy().into_owned();
        let workspace_text = workspace.to_string_lossy().into_owned();
        let chain_text = chain.to_string_lossy().into_owned();
        run(
            &sessions_text,
            &workspace_text,
            &chain_text,
            true,
            &ImportOptions::default(),
        )
        .unwrap();
        assert!(!chain.exists(), "a dry run must not create a chain");

        // A dry run is not a bypass: a broken archive still fails, unwritten.
        std::fs::write(
            sessions.join("2026-09-21-session-0002.jsonl"),
            "{ not an envelope\n",
        )
        .unwrap();
        let error = run(
            &sessions_text,
            &workspace_text,
            &chain_text,
            true,
            &ImportOptions::default(),
        )
        .unwrap_err();
        assert!(error.to_string().contains("malformed"), "{error}");
        assert!(!chain.exists());
    }

    #[test]
    fn human_requires_a_sessions_dir() {
        let error = run("", ".", ".editchain", false, &ImportOptions::default()).unwrap_err();
        assert!(error.to_string().contains("--sessions-dir"), "{error}");
    }

    #[test]
    fn missing_source_is_reported() {
        let (_tmp, workspace, _sessions, chain) = fixture();
        let error = import(&workspace.join("absent"), &workspace, &chain).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("cannot read human history source"),
            "{error}"
        );
        assert!(!chain.exists());
    }
}
