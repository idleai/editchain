//! Reuse existing presentation code on one changed logical block only.

use super::{
    super::{
        files::agent_file_change_index,
        payloads::{prepare_previews, PreviewContent},
        HistoryWindowOptions, Workspace,
    },
    LiveWorkspace, Result,
};
use editchain_project::{live::LiveRow, HistoryProjection};
use editchain_protocol::{LiveBlock, LiveBlockMeta};

pub(super) struct Presentation {
    pub(super) block: Option<LiveBlock>,
    pub(super) pending: Vec<editchain_core::BlobRef>,
}

impl LiveWorkspace {
    pub(super) fn local_workspace(&self, input: &LiveRow) -> Workspace {
        self.preview_workspace(input).0
    }

    fn preview_workspace(&self, input: &LiveRow) -> (Workspace, PreviewContent) {
        let resolver = self.blobs.clone();
        let operations: Vec<_> = input
            .operations
            .iter()
            .map(|op| op.as_ref().clone())
            .collect();
        let preview = prepare_previews(&operations, &resolver);
        let projection =
            HistoryProjection::from_source_previews(&operations, preview.ops, &preview.incomplete);
        let mut workspace = Workspace::from_projection(projection);
        workspace.source_ops.clone_from(&operations);
        workspace.source_op_index = input
            .operations
            .iter()
            .enumerate()
            .map(|(index, op)| (op.id, index))
            .collect();
        workspace.agent_file_changes = agent_file_change_index(
            &operations,
            &self.root,
            Some(&resolver),
            self.catalog.entries(),
        );
        workspace.blob_resolver = Some(resolver);
        workspace.repositories = self.catalog.clone();
        workspace.root_path.clone_from(&self.root);
        workspace.chain_path.clone_from(&self.chain);
        workspace
            .session_metadata
            .extend(super::super::sessions::session_metadata_index(&operations));
        let first =
            editchain_core::OpId::new(input.incarnation.node, input.incarnation.boot, 1 << 16);
        if let Some(meta) = self.projection.operation(first) {
            workspace
                .session_metadata
                .extend(super::super::sessions::session_metadata_index(
                    std::slice::from_ref(meta),
                ));
        }
        (workspace, preview.content)
    }

    pub(super) fn present(&self, input: &LiveRow) -> Result<Presentation> {
        if super::tasks::Tasks::metadata_only(input, &self.projection) {
            return Ok(Presentation {
                block: None,
                pending: Vec::new(),
            });
        }
        let (mut workspace, content) = self.preview_workspace(input);
        let mut presentation = Presentation {
            block: None,
            pending: content.pending,
        };
        workspace.prepare_live_item_view();
        let mut window = workspace.history_window(HistoryWindowOptions {
            offset: 0,
            limit: u64::MAX,
            include_layout: false,
        })?;
        if window.rows.is_empty() {
            return Ok(presentation);
        }
        let sort_time = self
            .projection
            .operation(input.incarnation)
            .and_then(|op| op.clock.observed_unix_ms())
            .or_else(|| window.rows.first().map(|row| row.timestamp_ms))
            .unwrap_or(0);
        for (index, row) in window.rows.iter_mut().enumerate() {
            row.continuity_key = if index == 0 {
                input.key.clone()
            } else if let Some(change) = &row.file_change {
                format!("{}:file:{}", input.key, change.path)
            } else {
                format!("{}:child:{index}", input.key)
            };
            row.session_summary = None;
            row.work_unit = None;
            row.group_end = false;
        }
        presentation.block = Some(LiveBlock {
            meta: LiveBlockMeta {
                task_group: None,
                task_summary: None,
                task_protected: unresolved(input)
                    || window.rows.iter().any(|row| {
                        matches!(
                            row.outcome,
                            editchain_core::taxonomy::Outcome::Failure
                                | editchain_core::taxonomy::Outcome::Warning
                                | editchain_core::taxonomy::Outcome::Cancelled
                        )
                    }),
                key: input.key.clone(),
                sort_time,
                row_count: window.total,
                spans: window.expansion_spans.unwrap_or_default(),
                node_key: window
                    .rows
                    .first()
                    .map(|row| row.node_key.clone())
                    .unwrap_or_default(),
                human_stream: human_stream(input),
                parents: Vec::new(),
                chain_state: window
                    .rows
                    .first()
                    .map(|row| row.chain_state)
                    .unwrap_or_default(),
            },
            rows: window.rows,
        });
        Ok(presentation)
    }
}

pub(super) fn human_stream(input: &LiveRow) -> Option<String> {
    input.operations.iter().find_map(|op| {
        // Legacy captures and provider messages keep their existing source rule.
        let _identity = editchain_project::human::work_record(op)?.identity?;
        let editchain_core::ScopeRef::Session(session) = op.scope else {
            return None;
        };
        Some(session.0.to_string())
    })
}

/// A completed task may still contain an explicitly unresolved tool/command.
fn unresolved(input: &LiveRow) -> bool {
    input
        .operations
        .iter()
        .rev()
        .find_map(|op| match &op.kind {
            editchain_core::OpKind::Tool(tool) => {
                Some(tool.stage != editchain_core::ToolStage::Finish)
            }
            editchain_core::OpKind::Command(command) => {
                Some(command.stage != editchain_core::CommandStage::Finish)
            }
            editchain_core::OpKind::File(file) => {
                Some(file.stage == editchain_core::FileStage::Proposed)
            }
            editchain_core::OpKind::ChainStart(_)
            | editchain_core::OpKind::Actor(_)
            | editchain_core::OpKind::Message(_)
            | editchain_core::OpKind::Reflection(_)
            | editchain_core::OpKind::Import(_)
            | editchain_core::OpKind::Note(_)
            | editchain_core::OpKind::Error(_)
            | editchain_core::OpKind::GitCommit(_)
            | editchain_core::OpKind::GitLink(_)
            | editchain_core::OpKind::Unknown(_) => None,
        })
        .unwrap_or(false)
}
