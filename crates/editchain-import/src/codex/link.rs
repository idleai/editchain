//! Exact Codex execution-topology facts.
//!
//! Codex rollouts expose execution identity and lifecycle evidence through
//! structured bridge fields. This module preserves only relationships whose
//! endpoints are identified by those fields. It never chooses an endpoint from
//! timestamps, file order, content, names, or proximity.
//!
//! - [`NoteRelationship::SpawnedBy`] requires one unambiguous exact spawn
//!   occurrence whose child thread exactly matches the child's
//!   `parentThreadId` relation. Current rollouts carry this on
//!   `collabToolCall.spawnAgent`; older rollouts used
//!   `subAgentActivity.started`.
//! - [`NoteRelationship::ReconnectsTo`] requires structured successful
//!   completion evidence and one unambiguous physical child terminal.
//! - [`NoteRelationship::ForkedFrom`] preserves `forkedFromId` as an
//!   execution-level fact. It deliberately does not fabricate a visible
//!   divergence row, because Codex supplies no exact row boundary for that
//!   field.
//!
//! Relationship IDs include the resolver, kind, every endpoint, and evidence.
//! Facts therefore remain deterministic without sharing reserved source lanes
//! with older heuristic notes.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use editchain_core::op::{NoteOp, NoteRelationship, OpKind};
use editchain_core::parents::ParentSet;
use editchain_core::payload::Payload;
use editchain_core::scope::ScopeRef;
use editchain_core::tags::Tags;
use editchain_core::{ActorId, Op, OpId, SessionId};

use crate::error::ImportError;
use crate::ids::{derive_external_entity_id, derive_session_id};

/// Cursor checkpoint for Codex normalized metadata. Version five retains one
/// path-specific `FileOp` for every entry in a multi-file change; version four
/// recognized exact `collabToolCall.spawnAgent` topology, version three added
/// portable capture of out-of-band session titles, and version two added exact
/// topology facts.
pub const CODEX_NORMALIZATION_VERSION: u32 = 5;

/// Resolver identifier retained in every emitted evidence payload.
pub const CODEX_TOPOLOGY_RESOLVER: &str = "codex-topology-v2";

const THREAD_NAMESPACE: &str = "codex:thread";
const RELATION_NAMESPACE: &str = "codex:relation:v2";

pub(crate) const SPAWN_SIGNAL_SUBAGENT_ACTIVITY: &str = "subAgentActivity.started";
pub(crate) const SPAWN_SIGNAL_COLLAB_TOOL: &str = "collabToolCall.spawnAgent";

/// A subagent lifecycle marker found in one thread's projection.
#[derive(Debug, Clone)]
pub struct ActivityMarker {
    /// The child thread named by `agentThreadId`.
    pub agent_thread_id: String,
    /// The marker's `agentPath`, used only for exact legacy completion
    /// correlation within this same parent thread.
    pub agent_path: Option<String>,
    /// Raw source occurrence carrying the marker.
    pub op_id: OpId,
    /// Whether the structured activity kind is exactly `started`.
    pub started: bool,
    /// Exact bridge signal that identified this activation.
    pub signal: &'static str,
}

/// Per-child completion evidence from a structured `agentsStates` map.
#[derive(Debug, Clone)]
pub struct CompletionEvidence {
    /// The child thread whose explicit status is `completed`.
    pub agent_thread_id: String,
    /// Raw source occurrence carrying the completed state.
    pub op_id: OpId,
}

/// Completion evidence from a legacy `collaboration.list_agents` result.
#[derive(Debug, Clone)]
pub struct LegacyCompletionEvidence {
    /// Completed `agent_name`, matched exactly to one `started.agentPath`.
    pub agent_path: String,
    /// Raw source occurrence carrying the result.
    pub op_id: OpId,
}

/// Per-thread topology captured from one physical rollout generation.
#[derive(Debug, Clone, Default)]
pub struct ThreadTopology {
    /// Owning Codex thread id.
    pub thread_id: String,
    /// Explicit `parentThreadId`.
    pub parent_thread_id: Option<String>,
    /// Explicit `forkedFromId`.
    pub forked_from_id: Option<String>,
    /// Explicit `agentPath`.
    pub agent_path: Option<String>,
    /// First physical occurrence in this rollout generation.
    pub first_raw: Option<OpId>,
    /// Last complete physical occurrence in this rollout generation.
    pub last_raw: Option<OpId>,
    /// Structured lifecycle markers in this thread.
    pub markers: Vec<ActivityMarker>,
    /// Structured completion states in this thread.
    pub completions: Vec<CompletionEvidence>,
    /// Legacy structured completion results in this thread.
    pub legacy_completions: Vec<LegacyCompletionEvidence>,
}

/// Emit exact Codex relationship facts.
///
/// The function is sink-independent: all required physical endpoints are
/// carried by [`ThreadTopology`]. A missing or ambiguous endpoint emits no
/// visible relationship instead of selecting a nearby row.
///
/// # Errors
///
/// Returns an error only if deterministic JSON evidence cannot be encoded.
pub fn emit_codex_relationship_notes(topology: &[ThreadTopology]) -> Result<Vec<Op>, ImportError> {
    if topology.is_empty() {
        return Ok(Vec::new());
    }

    let mut pending = Vec::new();
    emit_spawn_facts(topology, &mut pending);
    emit_reconnect_facts(topology, &mut pending);
    emit_fork_facts(topology, &mut pending);
    build_relationship_notes(pending)
}

/// Emit a spawn only when one exact `started` occurrence identifies the child.
fn emit_spawn_facts(topology: &[ThreadTopology], pending: &mut Vec<PendingNote>) {
    for child in topology {
        let (Some(parent_thread), Some(child_first)) =
            (child.parent_thread_id.as_deref(), child.first_raw)
        else {
            continue;
        };

        let candidates: Vec<&ActivityMarker> = topology
            .iter()
            .filter(|parent| parent.thread_id == parent_thread)
            .flat_map(|parent| parent.markers.iter())
            .filter(|marker| marker.started && marker.agent_thread_id == child.thread_id)
            .collect();
        // Keep the older exact signal when a rollout materializes both
        // representations of the same activation. That preserves the durable
        // relationship identity already emitted by normalization v2/v3 while
        // preventing the duplicate representation from looking ambiguous.
        // Current-only rollouts fall through to `collabToolCall.spawnAgent`.
        let legacy: Vec<&ActivityMarker> = candidates
            .iter()
            .copied()
            .filter(|marker| marker.signal == SPAWN_SIGNAL_SUBAGENT_ACTIVITY)
            .collect();
        let candidates = if legacy.is_empty() {
            candidates
        } else {
            legacy
        };
        let mut candidates_by_occurrence: BTreeMap<OpId, &ActivityMarker> = BTreeMap::new();
        for marker in candidates {
            let _: Option<&ActivityMarker> = candidates_by_occurrence.insert(marker.op_id, marker);
        }
        let mut candidates = candidates_by_occurrence.into_values();
        let Some(spawn) = candidates.next() else {
            continue;
        };
        if candidates.next().is_some() {
            // Several activations share the same thread identity. Without an
            // activation key, choosing one would manufacture provenance.
            continue;
        }

        pending.push(PendingNote::new(
            child_first,
            vec![spawn.op_id],
            NoteRelationship::SpawnedBy,
            ScopeRef::Session(derive_session_id(&child.thread_id)),
            BTreeMap::from([
                ("childThreadId".to_string(), child.thread_id.clone()),
                ("parentThreadId".to_string(), parent_thread.to_string()),
                ("signal".to_string(), spawn.signal.to_string()),
            ]),
        ));
    }
}

/// Emit every exactly correlated successful completion occurrence.
///
/// No chronological winner is selected across streams. Repeated explicit
/// completion occurrences remain repeated facts; downstream presentation may
/// coalesce identical visible edges while retaining each fact in the `OpSet`.
fn emit_reconnect_facts(topology: &[ThreadTopology], pending: &mut Vec<PendingNote>) {
    let mut terminals: HashMap<&str, BTreeSet<OpId>> = HashMap::new();
    for child in topology {
        if let Some(last_raw) = child.last_raw {
            let _: bool = terminals
                .entry(child.thread_id.as_str())
                .or_default()
                .insert(last_raw);
        }
    }

    let unique_terminal = |child: &str| -> Option<OpId> {
        let candidates = terminals.get(child)?;
        let mut candidates = candidates.iter().copied();
        let first = candidates.next()?;
        candidates.next().is_none().then_some(first)
    };

    // Group targets carried by one completion occurrence. This supports one
    // Wait/list result completing several children without assigning order.
    let mut groups: BTreeMap<(OpId, u64, &'static str), ReconnectGroup> = BTreeMap::new();
    for parent in topology {
        let session = derive_session_id(&parent.thread_id);
        for completion in &parent.completions {
            let Some(target) = unique_terminal(&completion.agent_thread_id) else {
                continue;
            };
            groups
                .entry((completion.op_id, session.0, "agentsStates.completed"))
                .or_default()
                .insert(target, completion.agent_thread_id.clone());
        }

        for completion in &parent.legacy_completions {
            let matches: BTreeSet<&str> = parent
                .markers
                .iter()
                .filter(|marker| {
                    marker.started
                        && marker.agent_path.as_deref() == Some(completion.agent_path.as_str())
                })
                .map(|marker| marker.agent_thread_id.as_str())
                .collect();
            let mut matches = matches.into_iter();
            let Some(child_thread) = matches.next() else {
                continue;
            };
            if matches.next().is_some() {
                continue;
            }
            let Some(target) = unique_terminal(child_thread) else {
                continue;
            };
            groups
                .entry((completion.op_id, session.0, "list_agents.completed"))
                .or_default()
                .insert(target, child_thread.to_string());
        }
    }

    for ((completion_op, session, signal), group) in groups {
        let child_ids = group.children.into_iter().collect::<Vec<_>>().join(",");
        pending.push(PendingNote::new(
            completion_op,
            group.targets.into_iter().collect(),
            NoteRelationship::ReconnectsTo,
            ScopeRef::Session(SessionId(session)),
            BTreeMap::from([
                ("childThreadIds".to_string(), child_ids),
                ("signal".to_string(), signal.to_string()),
            ]),
        ));
    }
}

/// Preserve `forkedFromId` without inventing a row-level divergence endpoint.
fn emit_fork_facts(topology: &[ThreadTopology], pending: &mut Vec<PendingNote>) {
    for fork in topology {
        let (Some(source_thread), Some(fork_first)) =
            (fork.forked_from_id.as_deref(), fork.first_raw)
        else {
            continue;
        };
        pending.push(PendingNote::new(
            fork_first,
            vec![derive_external_entity_id(THREAD_NAMESPACE, source_thread)],
            NoteRelationship::ForkedFrom,
            ScopeRef::Session(derive_session_id(&fork.thread_id)),
            BTreeMap::from([
                ("forkedFromId".to_string(), source_thread.to_string()),
                ("threadId".to_string(), fork.thread_id.clone()),
            ]),
        ));
    }
}

#[derive(Debug, Default)]
struct ReconnectGroup {
    targets: BTreeSet<OpId>,
    children: BTreeSet<String>,
}

impl ReconnectGroup {
    fn insert(&mut self, target: OpId, child: String) {
        let _: bool = self.targets.insert(target);
        let _: bool = self.children.insert(child);
    }
}

/// A relationship before deterministic evidence and ID materialization.
#[derive(Debug)]
struct PendingNote {
    parent: OpId,
    targets: Vec<OpId>,
    relationship: NoteRelationship,
    scope: ScopeRef,
    details: BTreeMap<String, String>,
}

impl PendingNote {
    fn new(
        parent: OpId,
        mut targets: Vec<OpId>,
        relationship: NoteRelationship,
        scope: ScopeRef,
        details: BTreeMap<String, String>,
    ) -> Self {
        targets.sort_unstable();
        targets.dedup();
        Self {
            parent,
            targets,
            relationship,
            scope,
            details,
        }
    }
}

fn build_relationship_notes(mut pending: Vec<PendingNote>) -> Result<Vec<Op>, ImportError> {
    pending.sort_by(compare_pending_notes);
    pending.dedup_by(|left, right| compare_pending_notes(left, right).is_eq());
    pending.into_iter().map(build_relationship_note).collect()
}

fn compare_pending_notes(left: &PendingNote, right: &PendingNote) -> std::cmp::Ordering {
    left.parent
        .cmp(&right.parent)
        .then_with(|| relationship_key(left.relationship).cmp(relationship_key(right.relationship)))
        .then_with(|| left.targets.cmp(&right.targets))
        .then_with(|| left.details.cmp(&right.details))
}

fn build_relationship_note(note: PendingNote) -> Result<Op, ImportError> {
    let relationship = relationship_key(note.relationship);
    let evidence = serde_json::to_vec(&serde_json::json!({
        "confidence": "exact",
        "details": note.details,
        "provider": "codex",
        "resolver": CODEX_TOPOLOGY_RESOLVER,
    }))?;
    let targets = note
        .targets
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(",");
    let identity = format!(
        "{CODEX_TOPOLOGY_RESOLVER}|{relationship}|{}|{targets}|{}",
        note.parent,
        String::from_utf8_lossy(&evidence)
    );
    let id = derive_external_entity_id(RELATION_NAMESPACE, &identity);
    Ok(Op {
        id,
        parents: ParentSet::One(note.parent),
        actor: ActorId(0),
        clock: editchain_core::clock::Clock::None,
        scope: note.scope,
        tags: Tags::META | Tags::IMPORT,
        kind: OpKind::Note(NoteOp {
            target_ids: note.targets,
            relationship: note.relationship,
            content: Payload::Inline(evidence),
        }),
    })
}

const fn relationship_key(relationship: NoteRelationship) -> &'static str {
    match relationship {
        NoteRelationship::SpawnedBy => "spawned-by",
        NoteRelationship::ReconnectsTo => "reconnects-to",
        NoteRelationship::ForkedFrom => "forked-from",
        NoteRelationship::Corrects
        | NoteRelationship::Supersedes
        | NoteRelationship::Rejects
        | NoteRelationship::Redacts
        | NoteRelationship::Explains
        | NoteRelationship::ForkOf
        | NoteRelationship::SubagentOf
        | NoteRelationship::OccurrenceOf
        | NoteRelationship::ProviderParent
        | NoteRelationship::LogicalParent
        | NoteRelationship::Contains
        | NoteRelationship::ToolResultOf => "unsupported",
    }
}

#[cfg(test)]
mod tests {
    #![expect(clippy::indexing_slicing, reason = "fixed-size unit-test fixtures")]

    use editchain_core::NodeId;

    use super::*;

    fn id(node: u64, seq: u64) -> OpId {
        OpId::new(NodeId(node), 0, seq)
    }

    fn topology(thread: &str, first: u64, last: u64) -> ThreadTopology {
        ThreadTopology {
            thread_id: thread.to_string(),
            first_raw: Some(id(first, 1)),
            last_raw: Some(id(last, 9)),
            ..ThreadTopology::default()
        }
    }

    #[test]
    fn spawn_requires_one_exact_started_marker() {
        let marker_id = id(1, 3);
        let mut parent = topology("parent", 1, 1);
        parent.markers.push(ActivityMarker {
            agent_thread_id: "child".to_string(),
            agent_path: Some("/root/child".to_string()),
            op_id: marker_id,
            started: true,
            signal: SPAWN_SIGNAL_SUBAGENT_ACTIVITY,
        });
        let mut child = topology("child", 2, 2);
        child.parent_thread_id = Some("parent".to_string());

        let notes = emit_codex_relationship_notes(&[parent.clone(), child.clone()]).unwrap();
        let spawn = notes
            .iter()
            .find(|op| matches!(&op.kind, OpKind::Note(note) if note.relationship == NoteRelationship::SpawnedBy))
            .expect("spawn fact");
        assert_eq!(spawn.parents, ParentSet::One(id(2, 1)));
        assert!(matches!(&spawn.kind, OpKind::Note(note) if note.target_ids == vec![marker_id]));

        parent.markers.clear();
        assert!(
            emit_codex_relationship_notes(&[parent.clone(), child.clone()])
                .unwrap()
                .is_empty()
        );

        parent.markers.extend([
            ActivityMarker {
                agent_thread_id: "child".to_string(),
                agent_path: None,
                op_id: id(1, 4),
                started: true,
                signal: SPAWN_SIGNAL_SUBAGENT_ACTIVITY,
            },
            ActivityMarker {
                agent_thread_id: "child".to_string(),
                agent_path: None,
                op_id: id(1, 5),
                started: true,
                signal: SPAWN_SIGNAL_SUBAGENT_ACTIVITY,
            },
        ]);
        assert!(emit_codex_relationship_notes(&[parent, child])
            .unwrap()
            .is_empty());
    }

    #[test]
    fn legacy_spawn_identity_survives_duplicate_current_representation() {
        let legacy_spawn = id(1, 3);
        let mut parent = topology("parent", 1, 1);
        parent.markers.extend([
            ActivityMarker {
                agent_thread_id: "child".to_string(),
                agent_path: Some("/root/child".to_string()),
                op_id: legacy_spawn,
                started: true,
                signal: SPAWN_SIGNAL_SUBAGENT_ACTIVITY,
            },
            ActivityMarker {
                agent_thread_id: "child".to_string(),
                agent_path: None,
                op_id: id(1, 4),
                started: true,
                signal: SPAWN_SIGNAL_COLLAB_TOOL,
            },
        ]);
        let mut child = topology("child", 2, 2);
        child.parent_thread_id = Some("parent".to_string());

        let notes = emit_codex_relationship_notes(&[parent, child]).unwrap();
        assert!(notes.iter().any(|op| matches!(&op.kind, OpKind::Note(note)
            if note.relationship == NoteRelationship::SpawnedBy
                && note.target_ids == vec![legacy_spawn]
                && matches!(&note.content, Payload::Inline(bytes)
                    if String::from_utf8_lossy(bytes)
                        .contains(SPAWN_SIGNAL_SUBAGENT_ACTIVITY)))));
    }

    #[test]
    fn fork_field_stays_execution_fact_without_visible_boundary_guess() {
        let mut fork = topology("fork", 2, 2);
        fork.forked_from_id = Some("trunk".to_string());

        let notes = emit_codex_relationship_notes(&[fork]).unwrap();
        assert_eq!(notes.len(), 1);
        assert!(matches!(&notes[0].kind, OpKind::Note(note)
            if note.relationship == NoteRelationship::ForkedFrom
                && note.target_ids == vec![derive_external_entity_id(THREAD_NAMESPACE, "trunk")]
                && matches!(note.content, Payload::Inline(_))));
        assert!(!notes.iter().any(|op| matches!(&op.kind, OpKind::Note(note)
            if note.relationship == NoteRelationship::ForkOf)));
    }

    #[test]
    fn reconnect_requires_one_unambiguous_child_terminal() {
        let completion = id(1, 7);
        let mut parent = topology("parent", 1, 1);
        parent.completions.push(CompletionEvidence {
            agent_thread_id: "child".to_string(),
            op_id: completion,
        });
        let child = topology("child", 2, 2);

        let notes = emit_codex_relationship_notes(&[parent.clone(), child.clone()]).unwrap();
        let reconnect = notes
            .iter()
            .find(|op| matches!(&op.kind, OpKind::Note(note) if note.relationship == NoteRelationship::ReconnectsTo))
            .expect("reconnect fact");
        assert_eq!(reconnect.parents, ParentSet::One(completion));
        assert!(matches!(&reconnect.kind, OpKind::Note(note) if note.target_ids == vec![id(2, 9)]));

        let second_generation = ThreadTopology {
            thread_id: "child".to_string(),
            first_raw: Some(id(3, 1)),
            last_raw: Some(id(3, 9)),
            ..ThreadTopology::default()
        };
        assert!(
            emit_codex_relationship_notes(&[parent, child, second_generation])
                .unwrap()
                .iter()
                .all(|op| !matches!(&op.kind, OpKind::Note(note)
                if note.relationship == NoteRelationship::ReconnectsTo))
        );
    }

    #[test]
    fn legacy_completion_needs_unique_exact_agent_path() {
        let marker_id = id(1, 3);
        let completion_id = id(1, 8);
        let mut parent = topology("parent", 1, 1);
        parent.markers.push(ActivityMarker {
            agent_thread_id: "child".to_string(),
            agent_path: Some("/root/child".to_string()),
            op_id: marker_id,
            started: true,
            signal: SPAWN_SIGNAL_SUBAGENT_ACTIVITY,
        });
        parent.legacy_completions.push(LegacyCompletionEvidence {
            agent_path: "/root/child".to_string(),
            op_id: completion_id,
        });
        let child = topology("child", 2, 2);

        let notes = emit_codex_relationship_notes(&[parent.clone(), child.clone()]).unwrap();
        assert!(notes.iter().any(|op| matches!(&op.kind, OpKind::Note(note)
            if note.relationship == NoteRelationship::ReconnectsTo
                && note.target_ids == vec![id(2, 9)])));

        parent.markers.push(ActivityMarker {
            agent_thread_id: "other".to_string(),
            agent_path: Some("/root/child".to_string()),
            op_id: id(1, 4),
            started: true,
            signal: SPAWN_SIGNAL_SUBAGENT_ACTIVITY,
        });
        assert!(emit_codex_relationship_notes(&[parent, child])
            .unwrap()
            .iter()
            .all(|op| !matches!(&op.kind, OpKind::Note(note)
                if note.relationship == NoteRelationship::ReconnectsTo)));
    }

    #[test]
    fn exact_fact_ids_and_evidence_are_deterministic() {
        let mut fork = topology("fork", 2, 2);
        fork.forked_from_id = Some("trunk".to_string());
        let first = emit_codex_relationship_notes(&[fork.clone()]).unwrap();
        let second = emit_codex_relationship_notes(&[fork]).unwrap();
        assert_eq!(first, second);
        assert!(matches!(&first[0].kind, OpKind::Note(note)
            if matches!(&note.content, Payload::Inline(bytes)
                if String::from_utf8_lossy(bytes).contains(CODEX_TOPOLOGY_RESOLVER))));
    }
}
