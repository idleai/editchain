//! One explicit prepare-view migration from synthetic headers to physical
//! task paths. Reuse canonical reducers, row pages and graph lanes; never
//! replay the source history just to change disclosure semantics.

use super::{LiveWorkspace, Result};
use editchain_protocol::rank::Measure;

impl LiveWorkspace {
    pub(super) fn restore_partial_items(&mut self) -> Result<()> {
        self.poisoned = true;
        let changes = self.projection.refresh_partial_items();
        let (removed, blocks) = self.apply_blocks(changes)?;
        drop(self.connect(&removed, blocks)?);
        Ok(())
    }

    pub(super) fn restore_human_streams(&mut self) -> Result<()> {
        self.poisoned = true;
        let mut blocks: Vec<_> = self
            .inputs
            .iter()
            .filter_map(|(key, input)| {
                let stream = super::rows::human_stream(input)?;
                let order = self.orders.get(key)?;
                let mut block = self.blocks.get(order)?.clone();
                block.meta.human_stream = Some(stream);
                Some(block)
            })
            .collect();
        // Start from tips so linked recorder runs reserve their path before an
        // unrelated human root reuses a free column. Other lanes stay in place.
        blocks.sort_by_key(|block| block.meta.order());
        let keys: Vec<_> = blocks.iter().map(|block| block.meta.key.clone()).collect();
        let metas: Vec<_> = blocks.iter().map(|block| block.meta.clone()).collect();
        self.graph.edit(&keys, &metas);
        // These are metadata repairs, not removed rows: retain disclosure state.
        drop(self.connect(&[], blocks)?);
        Ok(())
    }

    pub(super) fn refresh_edit_rows(&mut self, include_exposure: bool) -> Result<()> {
        self.poisoned = true;
        let upserts = self
            .inputs
            .iter()
            .filter(|(_, input)| {
                input.operations.iter().any(|op| {
                    matches!(op.kind, editchain_core::OpKind::File(_))
                        || editchain_project::human::work_record(op).is_some_and(|work| {
                            work.kind == editchain_core::human::HumanWorkKind::Edit
                                || (include_exposure
                                    && work.kind == editchain_core::human::HumanWorkKind::Exposure)
                        })
                })
            })
            .map(|(key, input)| (key.clone(), input.clone()))
            .collect();
        let (removed, blocks) = self.apply_blocks(editchain_project::live::LiveChanges {
            upserts,
            ..Default::default()
        })?;
        drop(self.connect(&removed, blocks)?);
        if include_exposure {
            self.regroup_disclosure();
        }
        Ok(())
    }

    pub(super) fn regroup(&mut self) {
        self.poisoned = true;
        self.tasks.reset_paths();
        let keys: Vec<_> = self.orders.keys().cloned().collect();
        let mut metas = Vec::new();
        for key in keys {
            let Some(order) = self.orders.get(&key).cloned() else {
                continue;
            };
            let Some(mut block) = self.blocks.get(&order).cloned() else {
                continue;
            };
            if block.meta.task_summary.is_some() {
                let _removed = self.remove_block(&key);
                self.disclosure.remove(&key);
                continue;
            }
            block.meta.task_group = None;
            metas.push(block.meta.clone());
            drop(self.blocks.insert(
                order,
                block.clone(),
                Measure {
                    expanded: block.meta.row_count,
                    visible: block.meta.row_count,
                },
            ));
        }
        let groups = self.tasks.update(&[], &metas, &self.inputs, &self.graph);
        for meta in metas {
            let order = meta.order();
            let Some(mut block) = self.blocks.get(&order).cloned() else {
                continue;
            };
            block.meta.task_group = groups.membership.get(&meta.key).cloned().flatten();
            block.meta.task_summary = groups.summaries.get(&meta.key).cloned().flatten();
            drop(self.blocks.insert(
                order,
                block.clone(),
                Measure {
                    expanded: block.meta.row_count,
                    visible: block.meta.row_count,
                },
            ));
        }
        self.regroup_disclosure();
    }

    pub(super) fn repair_graph(&mut self) -> Result<()> {
        self.poisoned = true;
        self.ancestry
            .observe_relationships(self.projection.prepare_relationships());
        self.tasks.reset_paths();
        let mut blocks = Vec::new();
        for order in self.orders.values() {
            if let Some(mut block) = self.blocks.get(order).cloned() {
                block.meta.task_group = None;
                block.meta.task_summary = None;
                blocks.push(block);
            }
        }
        drop(self.connect(&[], blocks)?);
        self.regroup_disclosure();
        Ok(())
    }
}
