//! Incremental exact causal paths. Concurrent tasks do not split each other's
//! paths. Late edges and junctions split/join only the affected memberships.

use editchain_index::{Map, OrderedMap, OrderedSet};
use editchain_project::live::TaskIdentity;
use editchain_protocol::LiveOrder;
use std::collections::{BTreeMap, BTreeSet};

type Position = (String, LiveOrder);

#[derive(Debug, serde::Serialize, serde::Deserialize)]
struct Member {
    order: LiveOrder,
    task: TaskIdentity,
    parent: String,
    section: String,
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub(super) struct Section {
    pub(super) task: TaskIdentity,
    pub(super) members: OrderedSet<LiveOrder>,
}

#[derive(Debug, Default, serde::Serialize, serde::Deserialize)]
pub(super) struct Runs {
    order: OrderedMap<Position, String>,
    members: Map<String, Member>,
    pub(super) sections: Map<String, Section>,
    pub(super) dirty: BTreeSet<String>,
    pub(super) membership: BTreeMap<String, Option<String>>,
}

impl Runs {
    pub(super) fn section(&self, key: &str) -> Option<&str> {
        self.members.get(key).map(|member| member.section.as_str())
    }
    pub(super) fn remove(&mut self, key: &str) {
        let Some(member) = self.members.remove(key) else {
            return;
        };
        drop(self.order.remove(&(member.task.key, member.order.clone())));
        if let Some(run) = self.sections.get_mut(&member.section) {
            let _: bool = run.members.remove(&member.order);
        }
        self.split(&member.section, &member.order);
        let _: bool = self.dirty.insert(member.section);
        drop(self.membership.insert(key.into(), None));
    }

    pub(super) fn put(
        &mut self,
        key: String,
        order: LiveOrder,
        task: TaskIdentity,
        parent: String,
    ) {
        if let Some(old) = self
            .members
            .get(&key)
            .filter(|old| old.order == order && old.task == task && old.parent == parent)
        {
            drop(self.membership.insert(key, Some(old.section.clone())));
            return;
        }
        self.remove(&key);
        let at = (task.key.clone(), order.clone());
        let before = self
            .order
            .range(..at.clone())
            .next_back()
            .and_then(|(_, key)| self.members.get(key))
            .filter(|member| member.task == task);
        let after = self
            .order
            .range(at.clone()..)
            .next()
            .and_then(|(_, key)| self.members.get(key))
            .filter(|member| member.task == task);
        let split = before
            .zip(after)
            .filter(|(before, after)| {
                before.section == after.section && (before.parent != key || after.order.1 != parent)
            })
            .map(|(before, _)| before.section.clone());
        if let Some(section) = split {
            self.split(&section, &order);
        }
        let newer = self
            .order
            .range(..at.clone())
            .next_back()
            .and_then(|(_, key)| self.members.get(key))
            .filter(|member| member.task == task && member.parent == key)
            .map(|member| member.section.clone());
        let ancestor = self
            .order
            .range(at.clone()..)
            .next()
            .and_then(|(_, key)| self.members.get(key))
            .filter(|member| member.task == task && member.order.1 == parent)
            .map(|member| member.section.clone());
        let section = ancestor
            .clone()
            .or_else(|| newer.clone())
            .unwrap_or_else(|| format!("task:{}:path:{key}", task.key));
        let run = self
            .sections
            .entry(section.clone())
            .or_insert_with(|| Section {
                task: task.clone(),
                members: OrderedSet::new(),
            });
        let _: bool = run.members.insert(order.clone());
        let _: bool = self.dirty.insert(section.clone());
        drop(self.membership.insert(key.clone(), Some(section.clone())));
        drop(self.order.insert(at, key.clone()));
        drop(self.members.insert(
            key,
            Member {
                order,
                task,
                parent,
                section: section.clone(),
            },
        ));
        if let Some(newer) = newer.filter(|newer| newer != &section) {
            self.join(&section, &newer);
        }
    }

    fn split(&mut self, key: &str, at: &LiveOrder) {
        let Some(run) = self.sections.get_mut(key) else {
            return;
        };
        if run.members.range(..at.clone()).next().is_none()
            || run.members.range(at.clone()..).next().is_none()
        {
            return;
        }
        let older = run.members.split_off(at);
        let newer = std::mem::replace(&mut run.members, older);
        let Some(anchor) = newer.last() else { return };
        let next_key = format!("task:{}:path:{}:before:{}", run.task.key, anchor.1, at.1);
        let next = Section {
            task: run.task.clone(),
            members: newer,
        };
        for order in &next.members {
            if let Some(member) = self.members.get_mut(&order.1) {
                member.section.clone_from(&next_key);
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

    fn join(&mut self, into: &str, from: &str) {
        let Some(section) = self.sections.remove(from) else {
            return;
        };
        let Some(target) = self.sections.get_mut(into) else {
            return;
        };
        for order in &section.members {
            if let Some(member) = self.members.get_mut(&order.1) {
                into.clone_into(&mut member.section);
                drop(self.membership.insert(order.1.clone(), Some(into.into())));
            }
            let _: bool = target.members.insert(order.clone());
        }
        let _: bool = self.dirty.insert(from.into());
        let _: bool = self.dirty.insert(into.into());
    }
}
