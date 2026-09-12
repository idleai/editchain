//! Native task membership and small section headers over independent item blocks.

#[cfg(test)]
mod lifecycle_tests;
mod metadata;
mod runs;
#[cfg(test)]
mod tests;

use editchain_project::live::{LiveChanges, LiveProjection, LiveRow, TaskIdentity};
use editchain_protocol::{LiveBlockMeta, TaskGroupDto};
use std::collections::{BTreeMap, HashMap};

#[derive(Debug, Default)]
pub(super) struct Tasks {
    runs: runs::Runs,
    metadata: metadata::Metadata,
    registered: HashMap<String, TaskIdentity>,
}

#[derive(Debug, Default)]
pub(super) struct Changes {
    pub(super) membership: BTreeMap<String, Option<String>>,
    pub(super) headers: Vec<LiveBlockMeta>,
    pub(super) removed: Vec<String>,
}

impl Tasks {
    pub(super) fn observe(&mut self, changes: &LiveChanges, projection: &LiveProjection) {
        for key in &changes.removed {
            self.metadata.remove(key, &mut self.runs.dirty);
        }
        for input in changes.upserts.values() {
            self.metadata
                .observe(input, projection, &mut self.runs.dirty);
        }
    }

    pub(super) fn metadata_only(input: &LiveRow, projection: &LiveProjection) -> bool {
        let observed = metadata::observations(input, projection);
        !observed.is_empty()
            && observed.iter().all(|(_, status)| {
                !matches!(
                    status,
                    editchain_protocol::TaskStatus::Failed
                        | editchain_protocol::TaskStatus::Interrupted
                )
            })
            && input.operations.len() == observed.len().saturating_add(1)
    }

    pub(super) fn update(
        &mut self,
        removed: &[String],
        upserts: &[LiveBlockMeta],
        inputs: &HashMap<String, LiveRow>,
    ) -> Changes {
        for key in removed {
            self.runs.remove(key);
        }
        for meta in upserts {
            let task = inputs.get(&meta.key).and_then(|row| row.task.clone());
            self.runs.put(meta.key.clone(), meta.order(), task);
        }
        let dirty = std::mem::take(&mut self.runs.dirty);
        let mut result = Changes {
            membership: std::mem::take(&mut self.runs.membership),
            ..Changes::default()
        };
        for key in dirty {
            let Some(section) = self
                .runs
                .sections
                .get(&key)
                .filter(|section| !section.members.is_empty())
            else {
                drop(self.runs.sections.remove(&key));
                if let Some(task) = self.registered.remove(&key) {
                    self.metadata.section(&task, &key, false);
                }
                result.removed.push(key);
                continue;
            };
            let Some(newest) = section.members.first() else {
                continue;
            };
            if !self.registered.contains_key(&key) {
                self.metadata.section(&section.task, &key, true);
                drop(self.registered.insert(key.clone(), section.task.clone()));
            }
            let task = TaskGroupDto {
                task_id: section.task.key.clone(),
                thread_id: section.task.thread.clone(),
                turn_id: section.task.turn.clone(),
                status: self.metadata.status(&section.task),
                title: self.metadata.title(&section.task),
                member_count: u64::try_from(section.members.len()).unwrap_or(u64::MAX),
                anchor: newest.1.clone(),
            };
            result.headers.push(LiveBlockMeta {
                key: key.clone(),
                sort_time: newest.0 .0,
                row_count: 1,
                spans: Vec::new(),
                node_key: key,
                parents: Vec::new(),
                chain_state: editchain_core::taxonomy::ChainState::Active,
                task_group: None,
                task_header: Some(task),
                task_protected: true,
            });
        }
        result
    }
}
