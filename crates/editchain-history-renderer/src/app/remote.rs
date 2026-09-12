//! Native paging keeps the renderer's work proportional to its viewport.

use super::{ExpansionIndex, HistoryAppState, Send, SnapshotPhase, Step, Viewport};
use editchain_protocol::{ErrorCode, LiveBaseline, LiveUpdate, ServiceError, SnapshotId};

#[derive(Debug, Clone)]
pub(in crate::app) struct Remote {
    epoch: SnapshotId,
    revision: u64,
    pending_find: Option<editchain_protocol::FindInHistoryResponse>,
}

impl HistoryAppState {
    pub(super) fn open_remote(&mut self, baseline: &LiveBaseline) -> Result<(), ServiceError> {
        if !baseline.blocks.is_empty() {
            return Err(invalid("Native paging sent a global topology."));
        }
        let total =
            i64::try_from(baseline.total).map_err(|_overflow| invalid("Live total overflow."))?;
        self.expansion = Some(ExpansionIndex::from_metadata(total, None, Some(&[]))?);
        self.remote = Some(Remote {
            epoch: baseline.epoch.clone(),
            revision: baseline.revision,
            pending_find: None,
        });
        Ok(())
    }

    pub(super) fn apply_remote_find(
        &mut self,
        mut found: editchain_protocol::FindInHistoryResponse,
        viewport: &Viewport,
        step: &mut Step,
    ) -> Result<(), ServiceError> {
        let update = found
            .live
            .take()
            .ok_or_else(|| invalid("Native find disclosure is missing."))?;
        if update
            .deltas
            .last()
            .is_none_or(|delta| delta.snapshot_id != found.snapshot_id)
        {
            return Err(invalid("Find disclosure revision mismatch."));
        }
        self.apply_remote_update(&update, viewport, step)?;
        if self.live.is_some() {
            if let Some(remote) = &mut self.remote {
                remote.pending_find = Some(found);
            }
        } else {
            self.apply_find_response(found, viewport, step);
        }
        Ok(())
    }

    pub(super) fn finish_remote_find(&mut self, viewport: &Viewport, step: &mut Step) {
        if self.remote.is_none() {
            return;
        }
        if let Some(found) = self
            .remote
            .as_mut()
            .and_then(|remote| remote.pending_find.take())
        {
            self.apply_find_response(found, viewport, step);
        } else {
            let query = self.find.query().to_owned();
            if !query.is_empty() {
                self.submit_find(&query, step);
            }
        }
    }

    pub(super) fn apply_remote_update(
        &mut self,
        update: &LiveUpdate,
        viewport: &Viewport,
        step: &mut Step,
    ) -> Result<(), ServiceError> {
        let remote = self
            .remote
            .as_ref()
            .ok_or_else(|| invalid("Native baseline is missing."))?;
        if remote.epoch != update.epoch {
            return Err(invalid("Native live epoch changed."));
        }
        let mut revision = remote.revision;
        let mut latest = None;
        for delta in &update.deltas {
            if delta.revision <= revision {
                continue;
            }
            if delta.base_revision != revision || delta.revision != revision.saturating_add(1) {
                return Err(invalid("Native live revision gap."));
            }
            revision = delta.revision;
            latest = Some(delta);
        }
        if revision != update.revision {
            return Err(invalid("Native live replay is incomplete."));
        }
        let Some(delta) = latest else {
            if self.live.is_some() {
                return Ok(());
            }
            step.sends.push(Send::LiveSettled {
                snapshot_id: self.snapshot_id.as_str().to_owned(),
                error: None,
            });
            return Ok(());
        };
        let total = delta
            .visible_total
            .and_then(|total| i64::try_from(total).ok())
            .ok_or_else(|| invalid("Missing native visible total."))?;
        let index = ExpansionIndex::from_metadata(total, None, Some(&[]))?;
        // Preserve the keyed DOM until the replacement viewport has arrived.
        self.pause_live(viewport, step);
        if let Some(remote) = &mut self.remote {
            remote.revision = revision;
            remote.pending_find = None;
        }
        self.snapshot_id.clone_from(&delta.snapshot_id);
        self.view_gen = self.view_gen.saturating_add(1);
        self.requests.clear();
        self.cache.clear();
        self.total = Some(total);
        self.expansion = Some(index);
        self.phase = SnapshotPhase::Opening;
        self.max_lane =
            u32::try_from(delta.max_lane).map_err(|_overflow| invalid("Live lane overflow."))?;
        self.locate_remote_anchors(step);
        step.sends.push(Send::Log(format!(
            "live delta: revision {revision}, {} blocks, {} chain records",
            update.work.blocks, update.work.chain_records
        )));
        Ok(())
    }
}

fn invalid(message: &str) -> ServiceError {
    ServiceError::new(ErrorCode::InvalidInput, message)
}
