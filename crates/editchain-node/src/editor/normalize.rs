//! Deterministic source-order replay; no filesystem state is read here.

mod edits;
mod operations;
pub(super) use operations::observation;

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use editchain_core::{
    human::{HumanGitContext, HumanIdentity, HumanRevision, HumanWorkKind, HumanWorkRecord},
    ContentId, Op, OpId,
};
use editchain_protocol::editor::{EditorDocument, EditorEvent, EditorEventKind};
use editchain_store::BlobStore;

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

#[derive(Debug, Default, serde::Serialize, serde::Deserialize)]
pub(super) struct Normalizer {
    sessions: BTreeMap<String, Session>,
    streams: BTreeMap<HumanIdentity, Stream>,
}

#[derive(Debug, Default, serde::Serialize, serde::Deserialize)]
struct Stream {
    source: Option<OpId>,
    work: Option<OpId>,
    linked_context: Option<HumanGitContext>,
}

#[derive(Debug, Default, serde::Serialize, serde::Deserialize)]
struct Session {
    sequence: u64,
    dwell: u64,
    lifecycle_activity: bool,
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
    edit_boundary: u64,
    #[serde(default)]
    group_boundary: u64,
    #[serde(default)]
    saved: BTreeMap<String, u64>,
    last_change: Option<u64>,
    edit_group: Option<(u64, Change, u64)>,
    #[serde(default)]
    observed_group: Option<(u64, Change, u64)>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct Change {
    path: Option<String>,
    before: HumanRevision,
    after: HumanRevision,
    context: ObservedContext,
    boundary: u64,
    #[serde(default)]
    group_boundary: u64,
    previous: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
struct ObservedContext {
    git: Option<HumanGitContext>,
    time_ms: Option<u64>,
}

impl Normalizer {
    pub(super) fn frontier(&self, identity: &HumanIdentity) -> Option<OpId> {
        self.streams.get(identity).and_then(|stream| stream.source)
    }

    pub(super) fn admitted(&mut self, event: &EditorEvent, source: OpId) {
        if let Some(identity) = &event.identity {
            self.streams.entry(identity.clone()).or_default().source = Some(source);
        }
    }

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
        let mut stream = event
            .identity
            .as_ref()
            .map(|identity| self.streams.entry(identity.clone()).or_default());
        if let Some(stream) = stream.as_mut() {
            stream.source = Some(source);
        }
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
                identity: event.identity.clone(),
                user_name: event.user_name.clone(),
                turn: session.turn,
                edit_group: session.group(event, kind).map(|group| OpId {
                    seq: group,
                    ..source
                }),
                source_event: source,
                kind,
                path,
                before,
                after,
                git: git.clone(),
                context_observed_ms: context.time_ms,
                summary: summary.chars().take(240).collect(),
            };
            let link = stream
                .as_ref()
                .map_or(git != session.linked_context, |stream| {
                    git.is_some() && git != stream.linked_context
                });
            let previous = stream
                .as_ref()
                .map_or(session.previous, |stream| stream.work);
            let ops = operations::work(event, &record, previous, link)?;
            session.previous = ops.first().map(|op| op.id);
            if let Some(stream) = stream {
                stream.work = session.previous;
                if git.is_some() {
                    stream.linked_context.clone_from(&git);
                }
            }
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
        if matches!(
            event.event,
            EditorEventKind::WorkspaceContext { .. }
                | EditorEventKind::TrackingGap { .. }
                | EditorEventKind::TrackingStopped
                | EditorEventKind::DocumentSaved { .. }
                | EditorEventKind::DocumentRenamed { .. }
                | EditorEventKind::EditorOpened { .. }
                | EditorEventKind::EditorClosed { .. }
                | EditorEventKind::CodeRead { .. }
                | EditorEventKind::CodeExposure { .. }
        ) {
            self.edit_boundary = event.sequence;
            if let EditorEventKind::DocumentSaved { document } = &event.event {
                let _previous = self.saved.insert(document.id.clone(), event.sequence);
            } else if !matches!(
                event.event,
                EditorEventKind::CodeRead { group: Some(_), .. }
            ) {
                self.group_boundary = event.sequence;
            }
        }
        match &event.event {
            EditorEventKind::TrackingStarted {
                dwell_ms,
                activity_schema,
                ..
            } => {
                self.dwell = *dwell_ms;
                self.lifecycle_activity = matches!(activity_schema, Some(2 | 3));
            }
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
                drop(
                    self.changes.insert(
                        event.sequence,
                        Change {
                            path: document.path.clone(),
                            before,
                            after,
                            context: self.context(document.path.as_deref()),
                            boundary: self.edit_boundary,
                            group_boundary: self
                                .group_boundary
                                .max(self.saved.get(&document.id).copied().unwrap_or(0)),
                            previous: self.last_change,
                        },
                    ),
                );
                self.last_change = Some(event.sequence);
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
                return Ok(Some(Work::edit(change, HumanWorkKind::Edit)));
            }
            EditorEventKind::HumanEditBatch { edits, group } => {
                return Ok(Some(self.edit_batch(edits, *group, HumanWorkKind::Edit)))
            }
            EditorEventKind::ObservedEditBatch { changes, group } => {
                let edits: Vec<_> = changes
                    .iter()
                    .map(|change| editchain_protocol::editor::EditorEditAttribution {
                        change: *change,
                        signal: String::new(),
                    })
                    .collect();
                return Ok(Some(self.edit_batch(
                    &edits,
                    Some(*group),
                    HumanWorkKind::ObservedEdit,
                )));
            }
            EditorEventKind::CodeExposure {
                document,
                duration_ms,
                ranges,
                ..
            }
            | EditorEventKind::CodeRead {
                document,
                duration_ms,
                ranges,
                ..
            } => {
                // Preserve legacy exposure derivations. New read events must
                // satisfy the recorder's policy before they enter the graph.
                let qualified = *duration_ms >= self.dwell && self.dwell > 0;
                if *duration_ms < 250
                    || ranges.is_empty()
                    || (matches!(event.event, EditorEventKind::CodeRead { .. }) && !qualified)
                {
                    return Ok(None);
                }
                let known = self
                    .revisions
                    .get(&document.id)
                    .filter(|revision| revision.version == document.version)
                    .cloned();
                let kind = if qualified {
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
            EditorEventKind::EditorOpened { path, uri, .. }
            | EditorEventKind::EditorClosed { path, uri, .. } => {
                // Older sessions retain byte-identical parents and episode IDs.
                // New sessions explicitly include tab lifecycle in their series.
                if !self.lifecycle_activity
                    || matches!(
                        event.event,
                        EditorEventKind::EditorOpened {
                            restored: Some(true),
                            ..
                        }
                    )
                {
                    return Ok(None);
                }
                return Ok(Some(Work {
                    kind: if matches!(event.event, EditorEventKind::EditorOpened { .. }) {
                        HumanWorkKind::EditorOpened
                    } else {
                        HumanWorkKind::EditorClosed
                    },
                    path: path.clone(),
                    before: None,
                    after: None,
                    summary: path.as_ref().unwrap_or(uri).clone(),
                    context: None,
                }));
            }
            EditorEventKind::DocumentRenamed { .. } => self.turn = 0,
            EditorEventKind::DocumentSaved { .. }
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
