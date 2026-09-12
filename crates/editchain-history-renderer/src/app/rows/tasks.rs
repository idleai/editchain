//! Task controls annotate real rows. Only a settled folded anchor substitutes
//! the task path summary for its own content; incoming records stay readable.

use super::{BundleKind, RowContext, RowInput};
use std::borrow::Cow;

fn title(task: &editchain_protocol::TaskGroupDto) -> String {
    let title = task.title.as_deref().unwrap_or("Codex task");
    if task.task_id.starts_with("human:") && task.status == editchain_protocol::TaskStatus::Unknown
    {
        title.to_owned()
    } else {
        format!("{title} · {}", task.status.label())
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct TaskView {
    pub(crate) expanded: bool,
    pub(crate) folded: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TaskDisclosure {
    pub(crate) expanded: bool,
    pub(crate) folded: bool,
    pub(crate) text: String,
    pub(crate) label: String,
}

pub(super) fn disclosure(row: &RowInput, context: &RowContext) -> Option<TaskDisclosure> {
    let task = row
        .source
        .task_group
        .as_ref()
        .filter(|task| task.member_count > 1)?;
    let expanded = task.expanded.unwrap_or(context.task.expanded);
    let folded = if task.expanded.is_some() {
        task.summarized
    } else {
        context.task.folded
    };
    let action = if expanded { "Collapse" } else { "Expand" };
    let glyph = if expanded { "▾" } else { "▸" };
    Some(TaskDisclosure {
        expanded,
        folded,
        text: format!("{glyph} {} activities", task.member_count),
        label: format!(
            "{action} {} activities · {}",
            task.member_count,
            title(task)
        ),
    })
}

pub(super) fn presentation<'a>(
    row: &'a RowInput,
    disclosure: Option<&TaskDisclosure>,
) -> Cow<'a, RowInput> {
    if !disclosure.is_some_and(|task| task.folded) {
        return Cow::Borrowed(row);
    }
    let Some(task) = &row.source.task_group else {
        return Cow::Borrowed(row);
    };
    let mut folded = row.clone();
    folded.bundle_kind = Some(BundleKind::WorkGroup);
    folded.bundle_count = Some(task.member_count);
    folded.display_summary = title(task);
    Cow::Owned(folded)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::rows::RowSpec;

    fn row(state: &str, count: u64) -> RowInput {
        let expanded = state == "open";
        let summarized = state == "folded";
        RowInput::from_legacy(
            &serde_json::json!({"node_key":"physical", "continuity_key":"item",
            "timestamp_ms":1, "group":"session", "parents":["parent"], "is_submodule":false,
            "summary":"Newest actual output", "kind":"message", "sub_ops":[{"kind":"note"}],
            "task_group":{"task_id":"t", "thread_id":"s", "turn_id":"turn", "status":"inProgress",
                "title":"Implement grouping", "member_count":count, "anchor":"item", "expanded":expanded, "summarized":summarized}}),
        )
    }

    #[test]
    fn folding_changes_presentation_without_replacing_physical_identity_or_detail_control() {
        let open = RowSpec::from_row(&row("open", 3), &RowContext::default());
        let folded = RowSpec::from_row(&row("folded", 3), &RowContext::default());
        assert_eq!(open.identity, folded.identity);
        assert_eq!(open.identity.node_key, "physical");
        assert_eq!(open.display_summary, "Newest actual output");
        assert!(
            open.disclosure.is_some(),
            "original item details have their own control"
        );
        assert!(open.task_disclosure.as_ref().unwrap().expanded);
        assert!(!open.graph.is_bundle);
        assert!(folded.display_summary.contains("Implement grouping"));
        assert!(folded.graph.is_bundle);
        assert!(folded.disclosure.is_none());
    }

    #[test]
    fn a_fresh_anchor_is_readable_inside_a_folded_task_and_singletons_have_no_task_control() {
        let fresh = RowSpec::from_row(&row("fresh", 3), &RowContext::default());
        assert_eq!(fresh.display_summary, "Newest actual output");
        assert!(!fresh.graph.is_bundle);
        assert!(!fresh.task_disclosure.as_ref().unwrap().expanded);
        let single = RowSpec::from_row(&row("folded", 1), &RowContext::default());
        assert!(single.task_disclosure.is_none());
        assert_eq!(single.display_summary, "Newest actual output");
    }
}
