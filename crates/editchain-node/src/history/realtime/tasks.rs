//! Native task membership and summaries attached to real causal path members.

#[cfg(test)]
mod lifecycle_tests;
mod metadata;
mod runs;
#[cfg(test)]
mod tests;

use editchain_index::Map as HashMap;
use editchain_project::live::{LiveChanges, LiveProjection, LiveRow, TaskIdentity};
use editchain_protocol::{LiveBlockMeta, TaskGroupDto};
use std::collections::BTreeMap;

#[derive(Debug, Default, serde::Serialize, serde::Deserialize)]
pub(super) struct Tasks {
    runs: runs::Runs,
    metadata: metadata::Metadata,
    registered: HashMap<String, TaskIdentity>,
    #[serde(default)]
    anchors: HashMap<String, String>,
}

#[derive(Debug, Default, serde::Serialize, serde::Deserialize)]
pub(super) struct Changes {
    pub(super) membership: BTreeMap<String, Option<String>>,
    pub(super) summaries: BTreeMap<String, Option<TaskGroupDto>>,
    pub(super) removed: Vec<String>,
}

impl Tasks {
    pub(super) fn reset_paths(&mut self) {
        self.runs = runs::Runs::default();
        self.registered.clear();
        self.anchors.clear();
        self.metadata.reset_sections();
    }
    pub(super) fn member_keys(&self, section: &str) -> Vec<String> {
        self.runs
            .sections
            .get(section)
            .map(|section| {
                section
                    .members
                    .iter()
                    .map(|order| order.1.clone())
                    .collect()
            })
            .unwrap_or_default()
    }
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
        graph: &editchain_protocol::live_graph::LiveGraph,
    ) -> Changes {
        for key in removed {
            self.runs.remove(key);
        }
        let keys: std::collections::BTreeSet<_> = upserts
            .iter()
            .map(|meta| &meta.key)
            .chain(graph.changed_boundaries())
            .collect();
        let mut eligible = Vec::new();
        for key in keys {
            let Some(meta) = graph.task_member(key) else {
                self.runs.remove(key);
                continue;
            };
            if let Some(task) = inputs.get(key).and_then(|row| row.task.clone()) {
                eligible.push((meta, task));
            } else {
                self.runs.remove(key);
            }
        }
        // Oldest first joins ordinary bootstrap/appends directly to their parent.
        eligible.sort_by_key(|(meta, _)| std::cmp::Reverse(meta.order()));
        for (meta, task) in eligible {
            self.runs.put(
                meta.key.clone(),
                meta.order(),
                task,
                meta.parents.first().cloned().unwrap_or_default(),
            );
        }
        for meta in upserts {
            if let Some(section) = self.runs.section(&meta.key) {
                if self.anchors.get(section) == Some(&meta.key) {
                    let _inserted = self.runs.dirty.insert(section.to_owned());
                }
            }
        }
        let dirty = std::mem::take(&mut self.runs.dirty);
        let mut result = Changes {
            membership: std::mem::take(&mut self.runs.membership),
            ..Changes::default()
        };
        for key in &dirty {
            if let Some(anchor) = self.anchors.remove(key) {
                drop(result.summaries.insert(anchor, None));
            }
        }
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
            if section.members.len() < 2 {
                continue;
            }
            let task = TaskGroupDto {
                expanded: None,
                summarized: false,
                task_id: section.task.key.clone(),
                thread_id: section.task.thread.clone(),
                turn_id: section.task.turn.clone(),
                status: self.metadata.status(&section.task),
                title: self.metadata.title(&section.task),
                member_count: u64::try_from(section.members.len()).unwrap_or(u64::MAX),
                anchor: newest.1.clone(),
            };
            drop(self.anchors.insert(key, newest.1.clone()));
            drop(result.summaries.insert(newest.1.clone(), Some(task)));
        }
        result
    }
}
