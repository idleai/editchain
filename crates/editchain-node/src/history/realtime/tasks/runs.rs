//! Incremental contiguous task sections. Interleaving splits only the affected run.

use editchain_project::live::TaskIdentity;
use editchain_protocol::LiveOrder;
use std::collections::{BTreeMap, BTreeSet, HashMap};

#[derive(Debug)]
struct Member {
    order: LiveOrder,
    task: Option<TaskIdentity>,
    section: Option<String>,
}

#[derive(Debug)]
pub(super) struct Section {
    pub(super) task: TaskIdentity,
    pub(super) members: BTreeSet<LiveOrder>,
}

#[derive(Debug, Default)]
pub(super) struct Runs {
    order: BTreeMap<LiveOrder, String>,
    members: HashMap<String, Member>,
    pub(super) sections: HashMap<String, Section>,
    pub(super) dirty: BTreeSet<String>,
    pub(super) membership: BTreeMap<String, Option<String>>,
}

impl Runs {
    pub(super) fn remove(&mut self, key: &str) {
        let Some(member) = self.members.remove(key) else {
            return;
        };
        drop(self.order.remove(&member.order));
        if let Some(section) = member.section {
            if let Some(run) = self.sections.get_mut(&section) {
                let _: bool = run.members.remove(&member.order);
            }
            let _: bool = self.dirty.insert(section);
        }
    }

    pub(super) fn put(&mut self, key: String, order: LiveOrder, task: Option<TaskIdentity>) {
        if let Some(old) = self
            .members
            .get(&key)
            .filter(|old| old.order == order && old.task == task)
        {
            drop(self.membership.insert(key, old.section.clone()));
            return;
        }
        self.remove(&key);
        let before = self
            .order
            .range(..order.clone())
            .next_back()
            .and_then(|(_, key)| self.members.get(key))
            .and_then(|member| member.section.clone());
        let after = self
            .order
            .range(order.clone()..)
            .next()
            .and_then(|(_, key)| self.members.get(key))
            .and_then(|member| member.section.clone());
        if let (Some(before), Some(after)) = (&before, &after) {
            if before == after
                && self
                    .sections
                    .get(before)
                    .is_some_and(|run| Some(&run.task) != task.as_ref())
            {
                self.split(before, &order);
            }
        }
        let section = task.as_ref().map(|task| {
            // Re-read after splitting. Prefer the older section's stable identity.
            let neighbour = self
                .order
                .range(order.clone()..)
                .next()
                .into_iter()
                .chain(self.order.range(..order.clone()).next_back())
                .filter_map(|(_, key)| self.members.get(key))
                .filter(|member| member.task.as_ref() == Some(task))
                .find_map(|member| member.section.clone());
            let section = neighbour.unwrap_or_else(|| format!("task:{}:section:{key}", task.key));
            let run = self
                .sections
                .entry(section.clone())
                .or_insert_with(|| Section {
                    task: task.clone(),
                    members: BTreeSet::new(),
                });
            let _: bool = run.members.insert(order.clone());
            let _: bool = self.dirty.insert(section.clone());
            section
        });
        drop(self.membership.insert(key.clone(), section.clone()));
        drop(self.order.insert(order.clone(), key.clone()));
        drop(self.members.insert(
            key,
            Member {
                order,
                task,
                section,
            },
        ));
    }

    fn split(&mut self, key: &str, at: &LiveOrder) {
        let Some(run) = self.sections.get_mut(key) else {
            return;
        };
        let older = run.members.split_off(at);
        let newer = std::mem::replace(&mut run.members, older);
        let Some(anchor) = newer.last() else {
            return;
        };
        let next_key = format!("task:{}:section:{}:before:{}", run.task.key, anchor.1, at.1);
        let next = Section {
            task: run.task.clone(),
            members: newer,
        };
        for order in &next.members {
            if let Some(member) = self.members.get_mut(&order.1) {
                member.section = Some(next_key.clone());
                drop(
                    self.membership
                        .insert(order.1.clone(), Some(next_key.clone())),
                );
            }
        }
        let _: bool = self.dirty.insert(key.into());
        let _: bool = self.dirty.insert(next_key.clone());
        drop(self.sections.insert(next_key, next));
    }
}
