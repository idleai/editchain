//! Apply revisioned block edits to the existing bounded cache and animated DOM.

use super::{
    host, live, ExpandedRow, HistoryAppState, PageCache, Send, Step, Unwrapped, Viewport, ROW_H,
};
use editchain_protocol::{ErrorCode, LiveDelta, LiveUpdate, ServiceError};
use serde_json::Value;
use std::collections::{BTreeMap, HashSet};

impl HistoryAppState {
    /// Frame the retained window at ordinary lane pitch. Distant historical
    /// branches must not compress the current tracks or reserve empty columns.
    pub(crate) fn graph_max_lane(&self) -> u32 {
        if self.remote.is_none()
            && self
                .expansion
                .as_ref()
                .is_none_or(|index| index.live.is_none())
        {
            return self.max_lane;
        }
        self.cache
            .indices()
            .filter(|abs| {
                self.visible_index_for_abs(abs.get())
                    .is_some_and(|visible| {
                        (self.render_top..=self.render_bottom).contains(&visible)
                    })
            })
            .filter_map(|abs| self.cache.get(abs))
            .map(crate::app::row_input::RowInput::max_graph_lane)
            .max()
            .and_then(|lane| u32::try_from(lane).ok())
            .unwrap_or(0)
    }

    pub(super) fn handle_delta(
        &mut self,
        body: Option<Value>,
        viewport: &Viewport,
        step: &mut Step,
    ) {
        let decoded = match host::unwrap(body) {
            Unwrapped::Ok(value) => host::decode::<LiveUpdate>(value),
            Unwrapped::Err(error) => Err(error),
        };
        let target = decoded
            .as_ref()
            .ok()
            .and_then(|update| update.deltas.last())
            .map(|delta| delta.snapshot_id.as_str().to_owned());
        let result = decoded.and_then(|update| self.apply_update(&update, viewport, step));
        if let Err(error) = result {
            self.fail_snapshot(step, &error.message);
            if let Some(snapshot_id) = target {
                step.sends.push(Send::LiveSettled {
                    snapshot_id,
                    error: Some(error.message),
                });
            }
        }
    }

    fn apply_update(
        &mut self,
        update: &LiveUpdate,
        viewport: &Viewport,
        step: &mut Step,
    ) -> Result<(), ServiceError> {
        if self.remote.is_some() {
            return self.apply_remote_update(update, viewport, step);
        }
        let index = self
            .expansion
            .as_ref()
            .and_then(|index| index.live.as_ref())
            .ok_or_else(|| invalid("Live baseline is missing."))?;
        if index.epoch != update.epoch {
            return Err(invalid("Live epoch changed; bootstrap required."));
        }
        let mut effective = *viewport;
        let previous_revision = index.revision;
        for delta in &update.deltas {
            let revision = self
                .expansion
                .as_ref()
                .and_then(|index| index.live.as_ref())
                .map_or(0, |index| index.revision);
            if delta.revision <= revision {
                continue;
            }
            effective = self.apply_delta(delta, &effective)?;
        }
        let revision = self
            .expansion
            .as_ref()
            .and_then(|index| index.live.as_ref())
            .map_or(0, |index| index.revision);
        if revision != update.revision {
            return Err(invalid("Live replay has a revision gap."));
        }
        if previous_revision == revision {
            step.sends.push(Send::LiveSettled {
                snapshot_id: self.snapshot_id.as_str().to_owned(),
                error: None,
            });
            return Ok(());
        }
        step.ops.push(super::DomOp::RefreshHeader);
        let query = self.find.query().to_owned();
        if !query.is_empty() {
            self.submit_find(&query, step);
        }
        self.live = Some(live::LiveUpdate::for_delta(effective));
        self.apply_live_window(step);
        step.sends.push(Send::Log(format!(
            "live delta: revision {revision}, {} blocks, {} chain records",
            update.work.blocks, update.work.chain_records
        )));
        Ok(())
    }

    fn apply_delta(
        &mut self,
        delta: &LiveDelta,
        viewport: &Viewport,
    ) -> Result<Viewport, ServiceError> {
        let index = self
            .expansion
            .as_ref()
            .and_then(|index| index.live.as_ref())
            .ok_or_else(|| invalid("Live topology is missing."))?;
        let touched: HashSet<_> = delta
            .removed
            .iter()
            .map(String::as_str)
            .chain(delta.upserts.iter().map(|block| block.meta.key.as_str()))
            .collect();
        let positions: BTreeMap<_, _> = self
            .cache
            .indices()
            .filter_map(|abs| Some((abs, index.position(abs.get())?)))
            .collect();
        let top = Self::viewport_visible_top(viewport);
        let viewed: Vec<_> = (top..=self.viewport_visible_bottom(viewport))
            .filter_map(|visible| self.abs_index_for_visible(visible))
            .filter_map(|abs| index.position(abs))
            .collect();
        let mut anchor = self
            .abs_index_for_visible(top)
            .and_then(|abs| index.position(abs));
        let mut focused = self
            .selection
            .roving()
            .and_then(|abs| index.position(abs.get()));
        let continuity_at = |position: &Option<(String, u64)>| {
            position
                .as_ref()
                .and_then(|position| index.absolute(position))
                .and_then(|abs| self.cache.get_by_index(abs))
                .map(|row| row.continuity_key().to_owned())
        };
        let anchor_key = continuity_at(&anchor);
        let focus_key = continuity_at(&focused);
        remap_child(&mut anchor, anchor_key.as_deref(), delta);
        remap_child(&mut focused, focus_key.as_deref(), delta);
        if self
            .selection
            .continuity_key()
            .is_some_and(|key| retired(key, delta))
        {
            self.selection.clear();
        }
        // Validate incoming row budgets before the topology changes.
        let prepared = delta
            .upserts
            .iter()
            .map(|block| {
                PageCache::prepare(block.rows.clone()).map(|rows| (block.meta.key.clone(), rows))
            })
            .collect::<Result<Vec<_>, _>>()?;
        let index = self
            .expansion
            .as_mut()
            .and_then(|index| index.live.as_mut())
            .ok_or_else(|| invalid("Live topology is missing."))?;
        index.apply(delta, &self.expanded_keys)?;
        // A former endpoint can become foldable after an append. Keep the
        // reader's visible rows exposed until an explicit disclosure change.
        for position in viewed {
            if let Some(abs) = index.absolute(&position).and_then(ExpandedRow::new) {
                index.reveal(abs);
            }
        }
        for (old, entry) in self.cache.take_entries() {
            let Some(position) = positions.get(&old) else {
                continue;
            };
            if touched.contains(position.0.as_str()) {
                continue;
            }
            if let Some(new) = index.absolute(position).and_then(ExpandedRow::new) {
                drop(self.cache.insert(new, entry.relocate(old, new)?));
            }
        }
        for (key, rows) in prepared {
            for (slot, entry) in rows.into_iter().enumerate() {
                let position = (key.clone(), u64::try_from(slot).unwrap_or(u64::MAX));
                if let Some(abs) = index.absolute(&position).and_then(ExpandedRow::new) {
                    let local = ExpandedRow::new(i64::try_from(slot).unwrap_or(0))
                        .ok_or_else(|| invalid("Live child coordinate overflow."))?;
                    drop(self.cache.insert(abs, entry.relocate(local, abs)?));
                }
            }
        }
        // The cache is bounded independently of history length. Recompute only
        // retained row geometry; unchanged content and disclosure stay resident.
        for (abs, entry) in self.cache.take_entries() {
            if let Some(position) = index.position(abs.get()) {
                drop(
                    self.cache
                        .insert(abs, entry.decorate(&index.graph, &position)?),
                );
            }
        }
        let focus = focused
            .as_ref()
            .and_then(|position| index.absolute(position))
            .and_then(ExpandedRow::new);
        self.selection.set_roving(focus);
        for row in delta.upserts.iter().flat_map(|block| &block.rows) {
            if self.selection.continuity_key() == Some(row.continuity_key.as_str()) {
                self.selection
                    .select_with_continuity(&row.node_key, &row.continuity_key);
            }
        }
        let anchor_abs = anchor
            .as_ref()
            .and_then(|position| index.absolute(position));
        self.snapshot_id.clone_from(&delta.snapshot_id);
        self.view_gen = self.view_gen.saturating_add(1);
        self.requests.clear();
        self.total =
            Some(i64::try_from(delta.total).map_err(|_overflow| invalid("Live total overflow."))?);
        self.max_lane =
            u32::try_from(delta.max_lane).map_err(|_overflow| invalid("Live lane overflow."))?;
        let visible = anchor_abs
            .and_then(|abs| self.visible_index_for_abs(abs))
            .unwrap_or(top)
            .min(self.visible_total().saturating_sub(1))
            .max(0);
        let scroll = if viewport.scroll_top.get() == 0 {
            0
        } else {
            visible
                .saturating_mul(ROW_H)
                .saturating_add(viewport.scroll_top.get().rem_euclid(ROW_H))
        };
        Ok(Viewport::new(scroll, viewport.client_height.get()))
    }
}

fn remap_child(position: &mut Option<(String, u64)>, continuity: Option<&str>, delta: &LiveDelta) {
    let Some((key, slot)) = position.as_mut() else {
        return;
    };
    if let Some(block) = delta.upserts.iter().find(|block| block.meta.key == *key) {
        *slot = block
            .rows
            .iter()
            .position(|row| Some(row.continuity_key.as_str()) == continuity)
            .and_then(|slot| u64::try_from(slot).ok())
            .unwrap_or(0);
    }
}

fn retired(continuity: &str, delta: &LiveDelta) -> bool {
    let owns = |key: &str| continuity == key || continuity.starts_with(&format!("{key}:"));
    delta.removed.iter().any(|key| owns(key))
        || delta.upserts.iter().any(|block| {
            owns(&block.meta.key)
                && !block
                    .rows
                    .iter()
                    .any(|row| row.continuity_key == continuity)
        })
}

fn invalid(message: &str) -> ServiceError {
    ServiceError::new(ErrorCode::InvalidInput, message)
}
