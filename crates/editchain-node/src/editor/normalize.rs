//! Deterministic source-order replay; no filesystem state is read here.

mod operations;
pub(super) use operations::observation;

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use editchain_core::{
    human::{HumanGitContext, HumanRevision, HumanWorkKind, HumanWorkRecord},
    ContentId, Op, OpId,
};
use editchain_protocol::editor::{EditorDocument, EditorEvent, EditorEventKind};
use editchain_store::BlobStore;

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

#[derive(Debug, Default)]
pub(super) struct Normalizer {
    sessions: BTreeMap<String, Session>,
}

#[derive(Debug, Default)]
struct Session {
    sequence: u64,
    dwell: u64,
    previous: Option<OpId>,
    turn: u64,
    last_work_ms: u64,
    contexts: Vec<HumanGitContext>,
    workspace: Option<std::path::PathBuf>,
    context_ms: Option<u64>,
    linked_context: Option<HumanGitContext>,
    revisions: BTreeMap<String, HumanRevision>,
    changes: BTreeMap<u64, Change>,
    confirmed: BTreeSet<u64>,
}

#[derive(Debug, Clone)]
struct Change {
    path: Option<String>,
    before: HumanRevision,
    after: HumanRevision,
    context: ObservedContext,
}

#[derive(Debug, Clone)]
struct ObservedContext {
    git: Option<HumanGitContext>,
    time_ms: Option<u64>,
}

impl Normalizer {
    pub(super) fn observe(
        &mut self,
        event: &EditorEvent,
        source: OpId,
        blobs: &mut BlobStore,
    ) -> Result<Vec<Op>> {
        let session = self.sessions.entry(event.session.clone()).or_default();
        if event.sequence <= session.sequence {
            return Ok(Vec::new());
        }
        session.sequence = event.sequence;
        let mut result = vec![observation(event, source)];
        let work = session.observe(event, source, blobs)?;
        if let Some(Work {
            kind,
            path,
            before,
            after,
            summary,
            context,
        }) = work
        {
            let context = context.unwrap_or_else(|| session.context(path.as_deref()));
            let git = context.git;
            if session.turn == 0
                || event.time_ms.saturating_sub(session.last_work_ms) > 30000
                || git != session.linked_context
            {
                session.turn = event.sequence;
            }
            let record = HumanWorkRecord {
                source: "vscode.work".into(),
                schema: 1,
                session: event.session.clone(),
                turn: session.turn,
                source_event: source,
                kind,
                path,
                before,
                after,
                git: git.clone(),
                context_observed_ms: context.time_ms,
                summary: summary.chars().take(240).collect(),
            };
            let link = git != session.linked_context;
            let ops = operations::work(event, &record, session.previous, link)?;
            session.previous = ops.first().map(|op| op.id);
            session.linked_context = git;
            session.last_work_ms = event.time_ms;
            if kind == HumanWorkKind::Gap {
                session.turn = 0;
            }
            result.extend(ops);
        }
        Ok(result)
    }
}

struct Work {
    kind: HumanWorkKind,
    path: Option<String>,
    before: Option<HumanRevision>,
    after: Option<HumanRevision>,
    summary: String,
    context: Option<ObservedContext>,
}

impl Session {
    fn context(&self, path: Option<&str>) -> ObservedContext {
        let git = path.and_then(|path| {
            let root = self.workspace.as_ref()?;
            let absolute = root.join(path);
            self.contexts
                .iter()
                .filter(|context| absolute.starts_with(&context.root))
                .max_by_key(|context| Path::new(&context.root).components().count())
                .cloned()
        });
        ObservedContext {
            git,
            time_ms: self.context_ms,
        }
    }

    fn observe(
        &mut self,
        event: &EditorEvent,
        source: OpId,
        blobs: &mut BlobStore,
    ) -> Result<Option<Work>> {
        match &event.event {
            EditorEventKind::TrackingStarted { dwell_ms, .. } => self.dwell = *dwell_ms,
            EditorEventKind::WorkspaceContext {
                observed_ms,
                repositories,
                workspace_path,
            } => {
                if self.contexts != *repositories {
                    self.turn = 0;
                }
                self.contexts.clone_from(repositories);
                self.workspace = workspace_path.as_ref().map(std::path::PathBuf::from);
                self.context_ms = Some(*observed_ms);
            }
            EditorEventKind::DocumentSnapshot { document, text } => {
                let revision = revision(document, document.version, text, Some(source), blobs)?;
                drop(self.revisions.insert(document.id.clone(), revision));
            }
            EditorEventKind::DocumentChanged {
                document,
                before_version,
                before,
                after,
                ..
            } => {
                let old = revision(document, *before_version, before, None, blobs)?;
                let before = self
                    .revisions
                    .get(&document.id)
                    .filter(|known| known.version == old.version && known.content == old.content)
                    .cloned()
                    .unwrap_or(old);
                let after = revision(document, document.version, after, Some(source), blobs)?;
                drop(self.revisions.insert(document.id.clone(), after.clone()));
                drop(self.changes.insert(
                    event.sequence,
                    Change {
                        path: document.path.clone(),
                        before,
                        after,
                        context: self.context(document.path.as_deref()),
                    },
                ));
            }
            EditorEventKind::HumanEdit { change, .. } => {
                if !self.confirmed.insert(*change) {
                    return Ok(None);
                }
                let Some(change) = self.changes.remove(change) else {
                    self.turn = 0;
                    return Ok(Some(Work {
                        kind: HumanWorkKind::Gap,
                        path: None,
                        before: None,
                        after: None,
                        summary: "Human edit: source revision unavailable".into(),
                        context: None,
                    }));
                };
                let summary = format!(
                    "Human edit · {}",
                    change.path.as_deref().unwrap_or("Untitled buffer")
                );
                return Ok(Some(Work {
                    kind: HumanWorkKind::Edit,
                    path: change.path,
                    before: Some(change.before),
                    after: Some(change.after),
                    summary,
                    context: Some(change.context),
                }));
            }
            EditorEventKind::CodeExposure {
                document,
                duration_ms,
                ranges,
                ..
            } => {
                // Keep micro-exposures between keystrokes in raw evidence and
                // coverage, without a separate activity dot for each interval.
                if *duration_ms < 250 || ranges.is_empty() {
                    return Ok(None);
                }
                let known = self
                    .revisions
                    .get(&document.id)
                    .filter(|revision| revision.version == document.version)
                    .cloned();
                let kind = if *duration_ms >= self.dwell && self.dwell > 0 {
                    HumanWorkKind::Read
                } else {
                    HumanWorkKind::Exposure
                };
                let label = if kind == HumanWorkKind::Read {
                    "Reading indicator"
                } else {
                    "Brief exposure"
                };
                let summary = format!(
                    "{label} · {} · {} ms",
                    document.path.as_deref().unwrap_or("Untitled buffer"),
                    duration_ms
                );
                return Ok(Some(Work {
                    kind,
                    path: document.path.clone(),
                    before: known.clone(),
                    after: known,
                    summary,
                    context: None,
                }));
            }
            EditorEventKind::TrackingGap { reason } => {
                self.turn = 0;
                self.revisions.clear();
                self.changes.clear();
                return Ok(Some(Work {
                    kind: HumanWorkKind::Gap,
                    path: None,
                    before: None,
                    after: None,
                    summary: format!("Capture gap · {reason}"),
                    context: None,
                }));
            }
            EditorEventKind::TrackingStopped => {
                self.turn = 0;
                self.revisions.clear();
                self.changes.clear();
            }
            EditorEventKind::DocumentRenamed { .. } => self.turn = 0,
            EditorEventKind::DocumentSaved { .. }
            | EditorEventKind::EditorOpened { .. }
            | EditorEventKind::EditorClosed { .. }
            | EditorEventKind::EditorActivated { .. }
            | EditorEventKind::SelectionChanged { .. }
            | EditorEventKind::VisibleRangesChanged { .. } => {}
        }
        Ok(None)
    }
}

fn revision(
    document: &EditorDocument,
    version: u64,
    text: &str,
    occurrence: Option<OpId>,
    blobs: &mut BlobStore,
) -> Result<HumanRevision> {
    blobs.write(text.as_bytes())?;
    Ok(HumanRevision {
        document: document.id.clone(),
        version,
        content: ContentId::Hash256(*blake3::hash(text.as_bytes()).as_bytes()),
        occurrence,
    })
}
