//! A final archive snapshot can contain arguments and output in one event.
//! Independent source observations remain independent persisted events.

use editchain_core::activity::{Kind, Operation, Terminal};
use editchain_core::{CommandStage, Op, OpId, OpKind, Payload, ToolStage};
use std::collections::BTreeMap;

#[derive(Debug, Default)]
pub(super) struct Calls {
    groups: BTreeMap<(OpId, Vec<u8>), Parts>,
    owners: BTreeMap<OpId, (OpId, Vec<u8>)>,
}

#[derive(Debug, Default)]
struct Parts {
    starts: Vec<OpId>,
    finished: Option<OpId>,
    arguments: Option<Payload>,
    name: Option<Payload>,
    terminal: Option<Terminal>,
}

impl Calls {
    pub(super) fn observe(&mut self, op: &Op) {
        let Some(original) = op.parents.iter().next().copied() else {
            return;
        };
        let (native, started, finished) = match &op.kind {
            OpKind::Tool(tool) => (
                &tool.tool_call_id,
                tool.stage == ToolStage::Start,
                tool.stage == ToolStage::Finish,
            ),
            OpKind::Command(command) => (
                &command.command_id,
                command.stage == CommandStage::Start,
                command.stage == CommandStage::Finish,
            ),
            OpKind::ChainStart(_)
            | OpKind::Actor(_)
            | OpKind::Session(_)
            | OpKind::Message(_)
            | OpKind::File(_)
            | OpKind::Reflection(_)
            | OpKind::Import(_)
            | OpKind::Note(_)
            | OpKind::Error(_)
            | OpKind::GitCommit(_)
            | OpKind::GitLink(_)
            | OpKind::Unknown(_)
            | OpKind::Activity(_) => return,
        };
        let Payload::Inline(native) = native else {
            return;
        };
        if native.is_empty() {
            return;
        }
        let native = serde_json::from_slice::<String>(native)
            .map_or_else(|_| native.clone(), String::into_bytes);
        let key = (original, native);
        let parts = self.groups.entry(key.clone()).or_default();
        if started {
            parts.starts.push(op.id);
        }
        if finished {
            parts.finished = Some(op.id);
        }
        if let OpKind::Tool(tool) = &op.kind {
            if started {
                parts.arguments = Some(tool.content.clone());
                parts.name = Some(tool.tool_name.clone());
            }
        }
        if let OpKind::Command(command) = &op.kind {
            if started {
                parts.terminal = Some(Terminal {
                    command: command.content.clone(),
                    cwd: Payload::Empty,
                    exit_code: None,
                });
                if parts.arguments.is_none() {
                    parts.arguments = Some(command.content.clone());
                }
            }
        }
        let _old = self.owners.insert(op.id, key);
    }

    pub(super) fn folded(&self) -> Vec<(OpId, OpId)> {
        self.groups
            .values()
            .flat_map(|parts| {
                let target = parts.finished.or_else(|| parts.starts.first().copied());
                parts.starts.iter().filter_map(move |start| {
                    target
                        .filter(|target| target != start)
                        .map(|target| (*start, target))
                })
            })
            .collect()
    }

    pub(super) fn apply(&self, id: OpId, record: &mut Operation) {
        let Some(parts) = self.owners.get(&id).and_then(|key| self.groups.get(key)) else {
            return;
        };
        let Kind::Tool(tool) = &mut record.kind else {
            return;
        };
        if let Some(arguments) = &parts.arguments {
            tool.arguments.clone_from(arguments);
        }
        if let Some(name) = &parts.name {
            tool.name.clone_from(name);
        }
        if let Some(terminal) = &parts.terminal {
            let exit_code = tool.terminal.as_ref().and_then(|value| value.exit_code);
            tool.terminal = Some(terminal.clone());
            if let Some(terminal) = &mut tool.terminal {
                terminal.exit_code = exit_code;
            }
        }
    }
}
