//! A burst is one activity only when every intervening buffer change is supported.

use super::{Change, HumanWorkKind, Session, Work};
use editchain_protocol::editor::EditorEditAttribution;

impl Session {
    pub(super) fn edit_batch(&mut self, edits: &[EditorEditAttribution]) -> Work {
        let Some(change) = self.batch_change(edits) else {
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

    fn batch_change(&self, edits: &[EditorEditAttribution]) -> Option<Change> {
        let start = edits.first()?.change;
        let mut combined = self.changes.get(&start)?.clone();
        let mut previous = start;
        for edit in edits.iter().skip(1) {
            let next = self.changes.get(&edit.change)?;
            // Include changes already consumed by other receipts, even in another file.
            if next.previous != Some(previous)
                || combined.after != next.before
                || combined.path != next.path
                || combined.context != next.context
                || combined.boundary != next.boundary
            {
                return None;
            }
            combined.after.clone_from(&next.after);
            previous = edit.change;
        }
        Some(combined)
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
