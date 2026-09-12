//! Conservative, revision-bound human-work coverage over retained AI evidence.

mod lines;

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path};

use editchain_core::{Op, OpKind, Payload, Tags};
use editchain_protocol::{
    editor::{EditorDocument, EditorEvent, EditorEventKind, EditorRange},
    OpenRequest,
};
use serde_json::{json, Value};

use super::Workspace;

#[derive(Clone, Default)]
struct Revision {
    text: String,
    origins: Vec<BTreeSet<usize>>,
}

struct Evidence {
    path: String,
    time: u64,
    lines: Vec<String>,
    origins: BTreeMap<usize, usize>,
    complete: bool,
}

#[derive(Default)]
struct Measurement {
    evidence: Vec<Evidence>,
    revisions: BTreeMap<(String, String), (u64, Revision)>,
    latest: BTreeMap<String, Revision>,
    read: BTreeSet<usize>,
    skimmed: BTreeSet<usize>,
    edited: BTreeSet<usize>,
    dwell: BTreeMap<String, u64>,
    generated: usize,
    generated_at: Vec<u64>,
    gaps: usize,
    unsupported_ai: usize,
    human_changes: usize,
    exposure_ms: u64,
}

pub(crate) fn report(request: &OpenRequest) -> Result<Value, Box<dyn std::error::Error>> {
    let mut workspace = Workspace::open(&request.workspace_path, &request.chain_dir)?;
    workspace.ensure_projection_loaded()?;
    let mut measurement = Measurement::default();
    measurement.collect_ai(&workspace);
    let mut operations: Vec<_> = workspace
        .source_ops
        .iter()
        .filter(|op| op.tags.matches_all(Tags::IMPORT | Tags::HUMAN))
        .collect();
    operations.sort_by_key(|op| (op.observed_unix_ms(), op.id));
    // Read the small indicator lane first, then hydrate one observation at a
    // time. A long typing session must not retain every full buffer in RAM.
    let human: BTreeSet<_> = operations
        .iter()
        .filter(|op| op.tags.matches_any(Tags::INFERRED))
        .filter_map(|op| editor_event(&workspace, op))
        .filter_map(|event| {
            if let EditorEventKind::HumanEdit { change, .. } = event.event {
                Some((event.session, change))
            } else {
                None
            }
        })
        .collect();
    let mut count = 0_usize;
    for op in operations {
        if let Some(event) = editor_event(&workspace, op) {
            measurement.observe(&event, &human);
            count = count.saturating_add(1);
        }
    }
    Ok(measurement.finish(Path::new(&request.workspace_path), count))
}

fn editor_event(workspace: &Workspace, op: &Op) -> Option<EditorEvent> {
    let OpKind::Import(import) = &op.kind else {
        return None;
    };
    let bytes = match &import.raw_ref {
        Payload::Inline(bytes) => Some(bytes.clone()),
        Payload::Blob(blob) => workspace
            .blob_resolver
            .as_ref()
            .and_then(|resolver| resolver.resolve_content(blob.id)),
        Payload::Empty => None,
    }?;
    let mut raw: Value = serde_json::from_slice(&bytes).ok()?;
    if raw.get("source").and_then(Value::as_str) != Some("vscode.editor") {
        return None;
    }
    serde_json::from_value(raw.get_mut("event")?.take()).ok()
}

impl Measurement {
    fn collect_ai(&mut self, workspace: &Workspace) {
        for (id, changes) in &workspace.agent_file_changes {
            let Some(time) = workspace
                .source_op(*id)
                .and_then(|op| op.observed_unix_ms())
            else {
                self.unsupported_ai = self.unsupported_ai.saturating_add(changes.len());
                continue;
            };
            for change in changes {
                if change.source != editchain_protocol::FileChangeSource::Agent {
                    continue;
                }
                let Ok(diff) = workspace.file_diff(change) else {
                    self.unsupported_ai = self.unsupported_ai.saturating_add(1);
                    continue;
                };
                if diff.binary || (diff.partial && diff.hunks.is_empty()) {
                    self.unsupported_ai = self.unsupported_ai.saturating_add(1);
                    continue;
                }
                if diff.hunks.is_empty() {
                    self.add_evidence(&diff.path, time, (&diff.before, &diff.after), true);
                } else {
                    for hunk in diff.hunks {
                        self.add_evidence(&diff.path, time, (&hunk.before, &hunk.after), false);
                    }
                }
            }
        }
        self.evidence.sort_by_key(|item| item.time);
    }

    fn add_evidence(&mut self, path: &str, time: u64, sides: (&str, &str), complete: bool) {
        let before = lines::split(sides.0);
        let after = lines::split(sides.1);
        let Some(pairs) = lines::unchanged(&before, &after) else {
            self.unsupported_ai = self.unsupported_ai.saturating_add(1);
            return;
        };
        let unchanged: BTreeSet<_> = pairs.iter().map(|(_, new)| *new).collect();
        let mut origins = BTreeMap::new();
        for (index, line) in after.iter().enumerate() {
            if !unchanged.contains(&index) && !line.trim().is_empty() {
                let _previous = origins.insert(index, self.generated);
                self.generated_at.push(time);
                self.generated = self.generated.saturating_add(1);
            }
        }
        self.evidence.push(Evidence {
            path: path.to_owned(),
            time,
            lines: after,
            origins,
            complete,
        });
    }

    fn seed(&self, path: &str, text: &str, time: u64) -> Revision {
        let content = lines::split(text);
        let mut revision = Revision {
            text: text.to_owned(),
            origins: vec![BTreeSet::new(); content.len()],
        };
        if let Some(old) = self.latest.get(path) {
            // Reopening or a capture gap preserves only unchanged lines; changed
            // lines receive no human attribution without a recorded indicator.
            if let Some(pairs) = lines::unchanged(&lines::split(&old.text), &content) {
                for (before, after) in pairs {
                    if let (Some(source), Some(target)) =
                        (old.origins.get(before), revision.origins.get_mut(after))
                    {
                        target.clone_from(source);
                    }
                }
            }
        }
        self.overlay(path, time, &mut revision);
        revision
    }

    fn overlay(&self, path: &str, time: u64, revision: &mut Revision) {
        let content = lines::split(&revision.text);
        for evidence in self
            .evidence
            .iter()
            .filter(|item| item.path == path && item.time <= time)
        {
            let pairs = if evidence.complete {
                lines::unchanged(&evidence.lines, &content).unwrap_or_default()
            } else {
                lines::unique_block(&content, &evidence.lines)
                    .map(|offset| {
                        (0..evidence.lines.len())
                            .map(|line| (line, offset.saturating_add(line)))
                            .collect()
                    })
                    .unwrap_or_default()
            };
            for (before, after) in pairs {
                if let Some(origin) = evidence.origins.get(&before) {
                    if let Some(target) = revision.origins.get_mut(after) {
                        *target = BTreeSet::from([*origin]);
                    }
                }
            }
        }
    }

    fn remember(&mut self, session: &str, document: &EditorDocument, revision: Revision) {
        if let Some(path) = &document.path {
            drop(self.latest.insert(path.clone(), revision.clone()));
        }
        drop(self.revisions.insert(
            (session.to_owned(), document.id.clone()),
            (document.version, revision),
        ));
    }

    fn fill_origins(&self, path: &str, time: u64, revision: &mut Revision) {
        // VS Code can announce the destination buffer before its rename event.
        // Recover newly available path evidence without replacing origins already
        // bound to this revision (including human-edited descendants).
        if revision.origins.iter().any(BTreeSet::is_empty) {
            let recovered = self.seed(path, &revision.text, time);
            for (target, source) in revision.origins.iter_mut().zip(recovered.origins) {
                if target.is_empty() {
                    *target = source;
                }
            }
        }
    }

    fn observe(&mut self, event: &EditorEvent, human: &BTreeSet<(String, u64)>) {
        match &event.event {
            EditorEventKind::TrackingStarted { dwell_ms, .. } => {
                let _previous = self.dwell.insert(event.session.clone(), *dwell_ms);
            }
            EditorEventKind::TrackingGap { .. } => self.gaps = self.gaps.saturating_add(1),
            EditorEventKind::DocumentSnapshot { document, text } => {
                let revision =
                    self.seed(document.path.as_deref().unwrap_or(""), text, event.time_ms);
                self.remember(&event.session, document, revision);
            }
            EditorEventKind::DocumentChanged {
                document,
                before_version,
                before,
                after,
                ..
            } => {
                let path = document.path.as_deref().unwrap_or("");
                let key = (event.session.clone(), document.id.clone());
                let mut old = self
                    .revisions
                    .get(&key)
                    .filter(|(version, revision)| {
                        *version == *before_version && revision.text == *before
                    })
                    .map_or_else(
                        || self.seed(path, before, event.time_ms),
                        |(_, revision)| revision.clone(),
                    );
                self.fill_origins(path, event.time_ms, &mut old);
                let is_human = human.contains(&(event.session.clone(), event.sequence));
                if is_human {
                    self.human_changes = self.human_changes.saturating_add(1);
                }
                let mut revision = self.evolve(&old, after, is_human);
                self.overlay(path, event.time_ms, &mut revision);
                self.remember(&event.session, document, revision);
            }
            EditorEventKind::CodeExposure {
                document,
                ranges,
                duration_ms,
                started_ms,
                ..
            } => {
                self.exposure_ms = self.exposure_ms.saturating_add(*duration_ms);
                let key = (event.session.clone(), document.id.clone());
                if let Some((_, revision)) = self
                    .revisions
                    .get(&key)
                    .filter(|(version, _)| *version == document.version)
                {
                    let mut revision = revision.clone();
                    self.fill_origins(
                        document.path.as_deref().unwrap_or(""),
                        *started_ms,
                        &mut revision,
                    );
                    let mut ids = exposed_origins(&revision, ranges);
                    ids.retain(|id| {
                        self.generated_at
                            .get(*id)
                            .is_some_and(|time| time <= started_ms)
                    });
                    self.skimmed.extend(ids.iter().copied());
                    if *duration_ms >= *self.dwell.get(&event.session).unwrap_or(&2000) {
                        self.read.extend(ids);
                    }
                } else {
                    self.gaps = self.gaps.saturating_add(1);
                }
            }
            EditorEventKind::DocumentRenamed { from, to } => {
                for evidence in &mut self.evidence {
                    if let Some(path) = renamed(&evidence.path, from, to) {
                        evidence.path = path;
                    }
                }
                let moves: Vec<_> = self
                    .latest
                    .keys()
                    .filter_map(|path| renamed(path, from, to).map(|new| (path.clone(), new)))
                    .collect();
                for (old, new) in moves {
                    if let Some(revision) = self.latest.remove(&old) {
                        drop(self.latest.insert(new, revision));
                    }
                }
            }
            EditorEventKind::TrackingStopped => {
                self.revisions
                    .retain(|(session, _), _| session != &event.session);
            }
            EditorEventKind::WorkspaceContext { .. }
            | EditorEventKind::HumanEdit { .. }
            | EditorEventKind::DocumentSaved { .. }
            | EditorEventKind::EditorOpened { .. }
            | EditorEventKind::EditorClosed { .. }
            | EditorEventKind::EditorActivated { .. }
            | EditorEventKind::SelectionChanged { .. }
            | EditorEventKind::VisibleRangesChanged { .. } => {}
        }
    }

    fn evolve(&mut self, old: &Revision, after: &str, human: bool) -> Revision {
        let before_lines = lines::split(&old.text);
        let after_lines = lines::split(after);
        let mut revision = Revision {
            text: after.to_owned(),
            origins: vec![BTreeSet::new(); after_lines.len()],
        };
        let Some(mut pairs) = lines::unchanged(&before_lines, &after_lines) else {
            self.gaps = self.gaps.saturating_add(1);
            return revision;
        };
        pairs.push((before_lines.len(), after_lines.len()));
        let (mut old_start, mut new_start) = (0, 0);
        for (old_end, new_end) in pairs {
            let touched: BTreeSet<_> = old
                .origins
                .get(old_start..old_end)
                .unwrap_or_default()
                .iter()
                .flatten()
                .copied()
                .collect();
            if human {
                self.edited.extend(touched.iter().copied());
                for target in revision
                    .origins
                    .get_mut(new_start..new_end)
                    .unwrap_or_default()
                {
                    target.clone_from(&touched);
                }
            }
            if let (Some(source), Some(target)) =
                (old.origins.get(old_end), revision.origins.get_mut(new_end))
            {
                target.clone_from(source);
            }
            old_start = old_end.saturating_add(1);
            new_start = new_end.saturating_add(1);
        }
        revision
    }

    fn finish(&self, root: &Path, event_count: usize) -> Value {
        let mut files = Vec::new();
        let paths: BTreeSet<_> = self.evidence.iter().map(|item| item.path.clone()).collect();
        let mut unavailable = 0_usize;
        for path in paths {
            let Some(text) = current_file(root, &path) else {
                unavailable = unavailable.saturating_add(1);
                continue;
            };
            let revision = self.seed(&path, &text, u64::MAX);
            let mut counts = [0_usize; 5];
            for (line, origins) in lines::split(&text).iter().zip(&revision.origins) {
                if line.trim().is_empty() || origins.is_empty() {
                    continue;
                }
                let read = !origins.is_disjoint(&self.read);
                let edited = !origins.is_disjoint(&self.edited);
                for (count, included) in counts.iter_mut().zip([
                    true,
                    read,
                    edited,
                    read && edited,
                    !origins.is_disjoint(&self.skimmed),
                ]) {
                    if included {
                        *count = count.saturating_add(1);
                    }
                }
            }
            files.push(json!({"path":path, "ai_lines":counts.first(), "read_lines":counts.get(1),
                "edited_lines":counts.get(2), "read_and_edited_lines":counts.get(3), "exposed_lines":counts.get(4),
                "nonblank_lines":lines::split(&text).iter().filter(|line| !line.trim().is_empty()).count()}));
        }
        let sum = |key| {
            files
                .iter()
                .filter_map(|file| file.get(key).and_then(Value::as_u64))
                .fold(0_u64, u64::saturating_add)
        };
        json!({"schema":1, "basis":"Current saved files with imported AI evidence; nonblank lines. Unsaved human work is retained in historical counts.",
            "ai_lines":sum("ai_lines"), "read_lines":sum("read_lines"), "edited_lines":sum("edited_lines"),
            "read_and_edited_lines":sum("read_and_edited_lines"), "exposed_lines":sum("exposed_lines"),
            "historical_ai_lines":self.generated, "historical_ai_lines_read":self.read.len(), "historical_ai_lines_edited":self.edited.len(),
            "human_changes":self.human_changes, "exposure_ms":self.exposure_ms, "events":event_count,
            "capture_gaps":self.gaps, "unsupported_ai_changes":self.unsupported_ai, "unavailable_files":unavailable,
            "files":files,
            "limitations":["Reading and skimming are visibility indicators, not proof of comprehension.",
                "Keyboard-correlated edits assume intentional human work; the stable VS Code API cannot verify authorship.",
                "AI origins follow unchanged lines from retained full snapshots or exact unique hunk matches. Unmatched code has unknown provenance.",
                "Only retained, dated AI changes are measurable. Missing imports and unsupported evidence are not zero human coverage."]})
    }
}

fn exposed_origins(revision: &Revision, ranges: &[EditorRange]) -> BTreeSet<usize> {
    let mut result = BTreeSet::new();
    for range in ranges {
        let [start, _] = range.start;
        let [end, column] = range.end;
        let end = end.saturating_add(u32::from(column > 0));
        for line in start..end {
            if let Some(origins) = usize::try_from(line)
                .ok()
                .and_then(|index| revision.origins.get(index))
            {
                result.extend(origins);
            }
        }
    }
    result
}

fn renamed(path: &str, from: &str, to: &str) -> Option<String> {
    if path == from {
        Some(to.to_owned())
    } else {
        path.strip_prefix(&format!("{from}/"))
            .map(|suffix| format!("{to}/{suffix}"))
    }
}

fn current_file(root: &Path, relative: &str) -> Option<String> {
    if Path::new(relative)
        .components()
        .any(|part| !matches!(part, Component::Normal(_) | Component::CurDir))
    {
        return None;
    }
    let path = root.join(relative).canonicalize().ok()?;
    if !path.starts_with(root.canonicalize().ok()?) || path.metadata().ok()?.len() > 1_048_576 {
        return None;
    }
    std::fs::read_to_string(path).ok()
}
