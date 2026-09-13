//! A burst is one activity only when every intervening buffer change is supported.

use super::{Change, HumanWorkKind, Session, Work};
use editchain_protocol::editor::EditorEditAttribution;

impl Session {
    pub(super) fn edit_batch(
        &mut self,
        edits: &[EditorEditAttribution],
        group: Option<u64>,
    ) -> Work {
        let change = self
            .batch_change(edits, group.is_some())
            .and_then(|change| self.continue_edit(change, edits, group));
        let Some(change) = change else {
            self.turn = 0;
            return Work {
                kind: HumanWorkKind::Gap,
                path: None,
                before: None,
                after: None,
                summary: "Human edit burst: source revisions are unavailable or discontinuous"
                    .into(),
                context: None,
            };
        };
        for edit in edits {
            let _new = self.confirmed.insert(edit.change);
            drop(self.changes.remove(&edit.change));
        }
        Work::edit(change)
    }

    fn continue_edit(
        &mut self,
        mut change: Change,
        edits: &[EditorEditAttribution],
        group: Option<u64>,
    ) -> Option<Change> {
        let Some(group) = group else {
            return Some(change);
        };
        let first = edits.first()?.change;
        if group != first {
            let (known, previous, _) = self.edit_group.as_ref()?;
            if *known != group
                || previous.after != change.before
                || previous.path != change.path
                || previous.context != change.context
                || previous.group_boundary != change.group_boundary
            {
                return None;
            }
            change.before.clone_from(&previous.before);
        }
        self.edit_group = Some((group, change.clone(), edits.last()?.change));
        Some(change)
    }

    fn batch_change(&self, edits: &[EditorEditAttribution], live: bool) -> Option<Change> {
        let start = edits.first()?.change;
        let mut combined = self.changes.get(&start)?.clone();
        let mut previous = start;
        for edit in edits.iter().skip(1) {
            let next = self.changes.get(&edit.change)?;
            // Include changes already consumed by other receipts, even in another file.
            if (!live && next.previous != Some(previous))
                || combined.after != next.before
                || combined.path != next.path
                || combined.context != next.context
                || if live {
                    combined.group_boundary != next.group_boundary
                } else {
                    combined.boundary != next.boundary
                }
            {
                return None;
            }
            combined.after.clone_from(&next.after);
            previous = edit.change;
        }
        Some(combined)
    }

    pub(super) fn group(
        &self,
        event: &editchain_protocol::editor::EditorEvent,
        kind: HumanWorkKind,
    ) -> Option<u64> {
        use editchain_protocol::editor::EditorEventKind;
        if let EditorEventKind::HumanEditBatch { group, .. } = &event.event {
            return group.filter(|_| kind == HumanWorkKind::Edit);
        }
        if let EditorEventKind::CodeRead {
            group: Some(group),
            document,
            ..
        } = &event.event
        {
            let (known, change, _) = self.edit_group.as_ref()?;
            return (*known == *group
                && kind == HumanWorkKind::Read
                && change.after.document == document.id
                && change.after.version == document.version
                && change.group_boundary
                    == self
                        .group_boundary
                        .max(self.saved.get(&document.id).copied().unwrap_or(0)))
            .then_some(*group);
        }
        None
    }
}

impl Work {
    pub(super) fn edit(change: Change) -> Self {
        let summary = format!(
            "Human edit · {}",
            change.path.as_deref().unwrap_or("Untitled buffer")
        );
        Self {
            kind: HumanWorkKind::Edit,
            path: change.path,
            before: Some(change.before),
            after: Some(change.after),
            summary,
            context: Some(change.context),
        }
    }
}
