//! Preserve every logical transition while committing one final row window.

use super::{DomOp, Step, Viewport, ROW_H};

pub(crate) struct FrameBatch {
    step: Step,
    pub(crate) viewport: Viewport,
}

impl FrameBatch {
    pub(crate) fn new(viewport: Viewport) -> Self {
        Self {
            step: Step::new(),
            viewport,
        }
    }

    pub(crate) fn push(&mut self, mut step: Step) {
        for op in &step.ops {
            let scroll = if let DomOp::ReanchorLive { scroll_top, .. }
            | DomOp::SetScrollTop(scroll_top) = op
            {
                Some(*scroll_top)
            } else if let DomOp::RestoreScrollTop { row_index } = op {
                Some(row_index.saturating_mul(ROW_H))
            } else {
                None
            };
            if let Some(scroll) = scroll {
                self.viewport = Viewport::new(scroll, self.viewport.client_height.get());
            }
        }
        self.step.ops.append(&mut step.ops);
        self.step.sends.append(&mut step.sends);
        if step.save_state.is_some() {
            self.step.save_state = step.save_state;
        }
    }

    pub(crate) fn finish(mut self, top: i64, bottom: i64, window_pending: bool) -> Step {
        if window_pending {
            // A response can be superseded by another revision in this batch.
            // Its retired coordinates must never replace the retained DOM.
            self.step.ops.retain(|op| {
                !renders(op)
                    && !matches!(
                        op,
                        DomOp::RefreshHeader
                            | DomOp::SetScrollTop(_)
                            | DomOp::RestoreScrollTop { .. }
                            | DomOp::RevealRow { .. }
                            | DomOp::SetFindHighlight { .. }
                    )
            });
        }
        let reset = self.step.ops.iter().rposition(|op| {
            matches!(
                op,
                DomOp::ShowMessage { .. } | DomOp::ShowRequestError { .. }
            )
        });
        let first = self
            .step
            .ops
            .iter()
            .enumerate()
            .find(|(index, op)| reset.is_none_or(|reset| *index > reset) && renders(op))
            .map(|(index, _)| index);
        let live = self
            .step
            .ops
            .iter()
            .enumerate()
            .rev()
            .find_map(|(index, op)| {
                if reset.is_some_and(|reset| index <= reset) {
                    return None;
                }
                if let DomOp::ReanchorLive {
                    scroll_top,
                    animate_connections,
                    ..
                } = op
                {
                    Some((index, *scroll_top, *animate_connections))
                } else {
                    None
                }
            });
        self.step.ops = self
            .step
            .ops
            .into_iter()
            .enumerate()
            .filter_map(|(index, op)| {
                if renders(&op) {
                    return (Some(index) == first).then(|| {
                        live.map_or(
                            DomOp::PatchWindow { top, bottom },
                            |(_, scroll_top, animate_connections)| DomOp::ReanchorLive {
                                top,
                                bottom,
                                scroll_top,
                                animate_connections,
                            },
                        )
                    });
                }
                if first.is_some() && matches!(op, DomOp::RefreshHeader) {
                    return None;
                }
                if live.is_some_and(|(last, _, _)| index < last)
                    && matches!(op, DomOp::SetScrollTop(_) | DomOp::RestoreScrollTop { .. })
                {
                    return None;
                }
                Some(op)
            })
            .collect();
        self.step
    }
}

fn renders(op: &DomOp) -> bool {
    matches!(
        op,
        DomOp::ReanchorLive { .. }
            | DomOp::Reanchor { .. }
            | DomOp::PatchWindow { .. }
            | DomOp::AppendBelow { .. }
            | DomOp::PrependAbove { .. }
            | DomOp::TrimTop { .. }
            | DomOp::TrimBottom { .. }
            | DomOp::FillPlaceholders
    )
}

#[cfg(test)]
mod tests {
    use super::{DomOp, Step, Viewport};
    use crate::app::state::{FrameBatch, Send};

    #[test]
    fn a_burst_commits_one_window_and_acknowledges_every_revision() {
        let mut batch = FrameBatch::new(Viewport::new(3400, 340));
        for revision in 1_i64..=10 {
            let mut step = Step::new();
            step.ops.push(DomOp::ReanchorLive {
                top: 0,
                bottom: 20,
                scroll_top: 3400_i64.saturating_add(revision.saturating_mul(34)),
                animate_connections: true,
            });
            step.sends.push(Send::LiveSettled {
                snapshot_id: revision.to_string(),
                error: None,
            });
            batch.push(step);
        }
        assert_eq!(batch.viewport.scroll_top.get(), 3740);
        let step = batch.finish(94, 136, false);
        assert_eq!(
            step.ops,
            vec![DomOp::ReanchorLive {
                top: 94,
                bottom: 136,
                scroll_top: 3740,
                animate_connections: true
            }]
        );
        assert_eq!(step.sends.len(), 10);
    }

    #[test]
    fn find_effects_follow_the_final_window_and_errors_retire_old_render_plans() {
        let mut batch = FrameBatch::new(Viewport::new(0, 340));
        let mut step = Step::new();
        step.ops = vec![
            DomOp::Reanchor {
                top: 10,
                bottom: 30,
            },
            DomOp::SetFindHighlight { abs: 20 },
            DomOp::FillPlaceholders,
        ];
        batch.push(step);
        assert_eq!(
            batch.finish(10, 30, false).ops,
            vec![
                DomOp::PatchWindow {
                    top: 10,
                    bottom: 30
                },
                DomOp::SetFindHighlight { abs: 20 }
            ]
        );
        let mut batch = FrameBatch::new(Viewport::new(0, 340));
        let mut step = Step::new();
        step.ops = vec![
            DomOp::FillPlaceholders,
            DomOp::ShowMessage {
                text: "failed".into(),
                error: true,
            },
        ];
        batch.push(step);
        assert!(matches!(
            batch.finish(0, 0, false).ops.as_slice(),
            [DomOp::ShowMessage { error: true, .. }]
        ));
    }

    #[test]
    fn a_new_handoff_keeps_the_old_dom_until_current_coordinates_arrive() {
        let mut batch = FrameBatch::new(Viewport::new(0, 340));
        let mut step = Step::new();
        step.ops.push(DomOp::ReanchorLive {
            top: 0,
            bottom: 30,
            scroll_top: 0,
            animate_connections: true,
        });
        step.sends.push(Send::LiveSettled {
            snapshot_id: "previous".into(),
            error: None,
        });
        batch.push(step);
        let step = batch.finish(0, 30, true);
        assert!(step.ops.is_empty());
        assert_eq!(step.sends.len(), 1);
    }
}
