//! Codex structural relationship linking — `SubagentOf` / `ReconnectsTo` /
//! `ForkOf` notes over the ops of one import run.
//!
//! Codex threads carry explicit structural metadata in `session_meta`:
//! `parentThreadId` (this thread is a subagent of that thread), `forkedFromId`
//! (this thread forked from that thread), and `agentPath` (subagent identity).
//! Unlike Claude Code, subagent threads are separate rollout files with their
//! own session scopes, so linking resolves ops across threads (files).
//!
//! The post-pass emits the same typed relationship notes the shared projection
//! already renders as virtual edges (SPEC §1.1) — stored causality is never
//! mutated:
//!
//! - **SubagentOf** — causal parent is the subagent thread's first op; target
//!   is the parent thread's *earliest real `started`* subagent-activity marker
//!   for that child when the parent file was imported in the same run, else
//!   the parent thread's first op. `interacted`/`interrupted` markers are
//!   never spawn targets.
//! - **ReconnectsTo** — emitted only from *explicit per-child completion
//!   evidence*: a `collabToolCall` item's `agentsStates` entry whose status is
//!   `completed`, or a legacy `collaboration.list_agents` tool result whose
//!   completed `agent_name` matches exactly one `started` marker's `agentPath`
//!   in the same thread. The subAgentActivity kinds (`started`/`interacted`/
//!   `interrupted`) and the collab tool's own status/tool (CloseAgent,
//!   SendInput, …) are never completion signals. Causal parent is the
//!   completion-carrying tool op; target is the child thread's last op. Each
//!   child reconnects at its *earliest* explicit completion, and completion
//!   targets are grouped per marker op (one note, sorted unique targets).
//! - **ForkOf** — causal parent is the fork thread's first op; target is the
//!   source thread's newest raw op at or before the fork thread's start
//!   (clock-bounded divergence anchor), when the source thread is present in
//!   this run's ops. Forking is suppressed for threads with explicit subagent
//!   provenance (`parentThreadId` or `agentPath`) — the copied
//!   `forkedFromId` in subagent session metadata is not a standalone fork —
//!   and skipped entirely when the fork thread's start clock is unknown (no
//!   reliable divergence boundary) or the source anchor would be a guess.
//!
//! Relationship notes are session-scoped (the owning thread's session), tagged
//! [`Tags::META`] (the shared projection folds structural notes out of rendered
//! rows and reads them as virtual edges), and use collision-free deterministic
//! ids: one reserved derived ordinal per relationship, plus a per-ordinal index
//! when several same-type notes anchor at the same source ordinal.
//!
//! Cross-run incremental imports cannot resolve threads from earlier runs (the
//! op sink only holds this run's ops) — the same boundary as the Claude
//! subagent/fork post-passes. Threads whose referenced counterpart is absent
//! from this run's ops are skipped.

use std::collections::HashMap;

use editchain_core::op::{NoteOp, NoteRelationship, OpKind};
use editchain_core::payload::Payload;
use editchain_core::scope::ScopeRef;
use editchain_core::tags::Tags;
use editchain_core::{ActorId, Op, OpId, SessionId};

use crate::ids::{derive_session_id, SourcePosition, SourceStream};

/// Reserved high derived-ordinals for Codex relationship note ids.
///
/// Normalization allocates derived ordinals sequentially starting at 1 per
/// source record; these reserved values sit far above any realistic
/// content-block count, and one distinct value per relationship keeps notes
/// with the same causal parent collision-free. When several notes of the same
/// relationship anchor at the same source ordinal, a deterministic per-ordinal
/// index is added to the reserved base (bounded by the number of completion
/// markers on one physical line in practice).
const SUBAGENT_NOTE_DISC: u16 = 0xFFFC;
const RECONNECT_NOTE_DISC: u16 = 0xFFFB;
const FORK_NOTE_DISC: u16 = 0xFFFA;

/// A subagent lifecycle marker found in one thread's ops.
#[derive(Debug, Clone)]
pub struct ActivityMarker {
    /// The subagent thread id this marker refers to.
    pub agent_thread_id: String,
    /// The marker's `agentPath`, when the bridge exposed it (legacy
    /// `list_agents` completion matching uses this as the child identity).
    pub agent_path: Option<String>,
    /// Op id of the normalized `subAgentActivity` note in the parent thread.
    pub op_id: OpId,
    /// Whether the activity kind is the real `started` kind. Only `started`
    /// markers are valid `SubagentOf` targets and legacy completion matches.
    pub started: bool,
}

/// Per-child completion evidence from a `collabToolCall` item's `agentsStates`.
#[derive(Debug, Clone)]
pub struct CompletionEvidence {
    /// The child thread id whose explicit status is `completed`.
    pub agent_thread_id: String,
    /// Op id of the normalized collab tool-call op carrying the completion.
    pub op_id: OpId,
}

/// Per-child completion evidence from a legacy `collaboration.list_agents`
/// tool result (pre-R2 corpora).
#[derive(Debug, Clone)]
pub struct LegacyCompletionEvidence {
    /// The completed agent's `agent_name` (matched against a `started`
    /// marker's `agentPath` in the same thread).
    pub agent_path: String,
    /// Op id of the normalized `list_agents` tool op carrying the output.
    pub op_id: OpId,
}

/// Per-thread topology captured during import, from explicit session metadata.
#[derive(Debug, Clone, Default)]
pub struct ThreadTopology {
    /// The owning thread id of the physical rollout file.
    pub thread_id: String,
    /// Explicit `parentThreadId` — this thread is a subagent of that thread.
    pub parent_thread_id: Option<String>,
    /// Explicit `forkedFromId` — this thread forked from that thread.
    pub forked_from_id: Option<String>,
    /// Explicit `agentPath` — subagent provenance (suppresses standalone fork
    /// geometry).
    pub agent_path: Option<String>,
    /// Subagent lifecycle markers found in this thread's own ops, in first-seen
    /// order.
    pub markers: Vec<ActivityMarker>,
    /// Explicit per-child completions from collab tool-call `agentsStates`.
    pub completions: Vec<CompletionEvidence>,
    /// Explicit per-child completions from legacy `list_agents` results.
    pub legacy_completions: Vec<LegacyCompletionEvidence>,
}

/// Emit structural relationship notes over the ops of one import run.
///
/// Does not mutate `ops` and never infers topology: relationship evidence comes
/// exclusively from explicit session metadata and explicit per-child completion signals. Returns
/// new [`Op`]s tagged [`Tags::META`] (the shared projection folds structural
/// notes out of rendered rows and reads them as virtual edges). Threads whose
/// referenced counterpart is absent from `ops` (e.g. imported in an earlier
/// run) are skipped — linking is best-effort within one run.
#[must_use]
pub fn emit_codex_relationship_notes(ops: &[Op], topology: &[ThreadTopology]) -> Vec<Op> {
    if topology.is_empty() {
        return Vec::new();
    }

    // Index ops by session scope (thread id), preserving input order.
    let mut ops_by_thread: HashMap<u64, Vec<usize>> = HashMap::new();
    for (i, op) in ops.iter().enumerate() {
        if let ScopeRef::Session(sid) = op.scope {
            ops_by_thread.entry(sid.0).or_default().push(i);
        }
    }

    // First/last op per thread id, resolved lazily from the session index.
    // Session-scoped derived lanes (inter-agent/compaction notes) exist too,
    // so the first op is the minimum seq and the last op the maximum seq.
    let first_op =
        |ops_by_thread: &HashMap<u64, Vec<usize>>, thread: &str, ops: &[Op]| -> Option<Op> {
            let sid = SessionId(derive_session_id(thread).0);
            let indices = ops_by_thread.get(&sid.0)?;
            indices
                .iter()
                .filter_map(|&i| ops.get(i))
                .min_by_key(|op| op.id.seq)
                .cloned()
        };
    let last_op =
        |ops_by_thread: &HashMap<u64, Vec<usize>>, thread: &str, ops: &[Op]| -> Option<Op> {
            let sid = SessionId(derive_session_id(thread).0);
            let indices = ops_by_thread.get(&sid.0)?;
            indices
                .iter()
                .filter_map(|&i| ops.get(i))
                .max_by_key(|op| op.id.seq)
                .cloned()
        };
    let session_id_for = |thread: &str| SessionId(derive_session_id(thread).0);
    let session_for = |thread: &str| ScopeRef::Session(session_id_for(thread));

    let mut pending: Vec<PendingNote> = Vec::new();

    // SubagentOf: this thread declares a parent thread. Target is the parent's
    // earliest real `started` marker for this child; fall back to the parent
    // thread's first op when the marker is missing or the parent file was not
    // imported in this run.
    for topo in topology {
        let Some(parent_thread) = topo.parent_thread_id.as_deref() else {
            continue;
        };
        let Some(sub_first) = first_op(&ops_by_thread, &topo.thread_id, ops) else {
            continue;
        };
        let target = topology
            .iter()
            .find(|p| p.thread_id == parent_thread)
            .and_then(|p| {
                p.markers
                    .iter()
                    .find(|m| m.agent_thread_id == topo.thread_id && m.started)
                    .map(|m| m.op_id)
            })
            .or_else(|| first_op(&ops_by_thread, parent_thread, ops).map(|op| op.id));
        if let Some(target) = target {
            pending.push(PendingNote {
                parent: sub_first.id,
                targets: vec![target],
                relationship: NoteRelationship::SubagentOf,
                disc: SUBAGENT_NOTE_DISC,
                scope: session_for(&topo.thread_id),
            });
        }
    }

    // ReconnectsTo: resolve every child's earliest explicit completion, then
    // group per completion marker into one note with sorted unique targets.
    let mut per_child: HashMap<String, (OpId, u64, SessionId)> = HashMap::new();
    for topo in topology {
        for completion in &topo.completions {
            register_completion(
                &mut per_child,
                completion.agent_thread_id.clone(),
                completion.op_id,
                session_id_for(&topo.thread_id),
            );
        }
        for legacy in &topo.legacy_completions {
            let started_matches = topo.markers.iter().filter(|m| {
                m.started && m.agent_path.as_deref() == Some(legacy.agent_path.as_str())
            });
            let mut matches = started_matches;
            // Missing or ambiguous evidence (no started marker, or more than
            // one with the same agentPath) means no edge.
            if matches.clone().count() != 1 {
                continue;
            }
            let Some(marker) = matches.next() else {
                continue;
            };
            register_completion(
                &mut per_child,
                marker.agent_thread_id.clone(),
                legacy.op_id,
                session_id_for(&topo.thread_id),
            );
        }
    }
    // (marker op, session, target) triples, sorted so grouping is deterministic
    // and per-ordinal indices are stable.
    let mut reconnect_groups: Vec<(OpId, SessionId, OpId)> = Vec::new();
    for (child, (marker_op, _seq, session)) in per_child {
        let Some(sub_last) = last_op(&ops_by_thread, &child, ops) else {
            continue;
        };
        reconnect_groups.push((marker_op, session, sub_last.id));
    }
    reconnect_groups.sort_unstable_by_key(|(marker, session, target)| {
        (
            marker.node.0,
            marker.boot,
            marker.seq,
            session.0,
            target.node.0,
            target.boot,
            target.seq,
        )
    });
    // Group per completion marker op into one note with sorted unique targets.
    // The triples are sorted first so grouping stays deterministic.
    let mut per_marker: Vec<(OpId, SessionId, Vec<OpId>)> = Vec::new();
    for (marker_op, session, target) in reconnect_groups {
        match per_marker.last_mut() {
            Some((last_marker, last_session, targets))
                if *last_marker == marker_op && *last_session == session =>
            {
                targets.push(target);
            }
            _ => per_marker.push((marker_op, session, vec![target])),
        }
    }
    for (marker_op, session, mut targets) in per_marker {
        targets.sort_unstable();
        targets.dedup();
        pending.push(PendingNote {
            parent: marker_op,
            targets,
            relationship: NoteRelationship::ReconnectsTo,
            disc: RECONNECT_NOTE_DISC,
            scope: ScopeRef::Session(session),
        });
    }
    // ForkOf: this thread declares a fork source thread. Explicit subagent
    // provenance suppresses fork geometry, and an unknown fork start clock
    // (or unknown source clocks) skips the divergence anchor entirely rather
    // than guessing.
    for topo in topology {
        let Some(source_thread) = topo.forked_from_id.as_deref() else {
            continue;
        };
        if topo.parent_thread_id.is_some() || topo.agent_path.is_some() {
            continue;
        }
        let Some(fork_first) = first_op(&ops_by_thread, &topo.thread_id, ops) else {
            continue;
        };
        if fork_first.tags.matches_any(Tags::SOURCE_TIME_UNKNOWN) {
            continue;
        }
        let fork_clock = fork_first.clock.as_u64();
        let source_indices = {
            let sid = SessionId(derive_session_id(source_thread).0);
            ops_by_thread.get(&sid.0)
        };
        let target = source_indices.and_then(|indices| {
            indices
                .iter()
                .filter_map(|&i| ops.get(i))
                // Raw source records only: normalized lanes carry `Clock::None`
                // and would otherwise win the seq max at the same ordinal.
                .filter(|op| {
                    matches!(op.kind, OpKind::Import(_))
                        && !op.tags.matches_any(Tags::SOURCE_TIME_UNKNOWN)
                        && op.clock.as_u64() <= fork_clock
                })
                .max_by_key(|op| op.id.seq)
                .map(|op| op.id)
        });
        if let Some(target) = target {
            pending.push(PendingNote {
                parent: fork_first.id,
                targets: vec![target],
                relationship: NoteRelationship::ForkOf,
                disc: FORK_NOTE_DISC,
                scope: session_for(&topo.thread_id),
            });
        }
    }

    build_relationship_notes(pending)
}

/// A relationship note to emit, before its collision-free derived id is
/// assigned. Only the causal parent's [`OpId`] is needed — the note inherits
/// the parent's stream, source ordinal, and session scope.
struct PendingNote {
    parent: OpId,
    targets: Vec<OpId>,
    relationship: NoteRelationship,
    disc: u16,
    scope: ScopeRef,
}

/// Assign collision-free derived ids to the pending notes and build the final
/// [`Op`]s.
///
/// Notes anchored at the same source ordinal in the same stream would
/// otherwise share an id (same reserved derived ordinal). Such groups are
/// bounded by the number of relationship notes one physical line can produce;
/// within a group every note gets a deterministic `0xFF00 + index` derived
/// ordinal, while a group of one keeps its relationship's reserved disc.
#[expect(
    clippy::arithmetic_side_effects,
    reason = "per-ordinal note index is bounded by the number of relationship notes one source line can produce (far below u16::MAX)"
)]
fn build_relationship_notes(pending: Vec<PendingNote>) -> Vec<Op> {
    let mut pending = pending;
    pending.sort_by(|a, b| {
        (
            a.parent.node.0,
            a.parent.boot,
            a.parent.seq >> 16,
            a.disc,
            a.parent.seq,
        )
            .cmp(&(
                b.parent.node.0,
                b.parent.boot,
                b.parent.seq >> 16,
                b.disc,
                b.parent.seq,
            ))
    });
    // Group notes by (stream, source ordinal) in deterministic order.
    let mut groups: Vec<Vec<PendingNote>> = Vec::new();
    for note in pending {
        let anchor = (note.parent.node.0, note.parent.boot, note.parent.seq >> 16);
        match groups.last_mut() {
            Some(group)
                if group.first().is_some_and(|first| {
                    (
                        first.parent.node.0,
                        first.parent.boot,
                        first.parent.seq >> 16,
                    ) == anchor
                }) =>
            {
                group.push(note);
            }
            _ => groups.push(vec![note]),
        }
    }
    let mut notes = Vec::new();
    for group in groups {
        match group.as_slice() {
            [single] => notes.push(single.take_note(single.disc)),
            many => {
                for (index, note) in many.iter().enumerate() {
                    let disc = 0xFF00 + u16::try_from(index).unwrap_or(0);
                    notes.push(note.take_note(disc));
                }
            }
        }
    }
    notes
}

impl PendingNote {
    fn take_note(&self, disc: u16) -> Op {
        relationship_note(
            self.parent,
            self.targets.clone(),
            self.relationship,
            disc,
            self.scope,
        )
    }
}

/// Register a child completion, keeping the earliest explicit evidence op.
fn register_completion(
    per_child: &mut HashMap<String, (OpId, u64, SessionId)>,
    child: String,
    marker_op: OpId,
    session: SessionId,
) {
    let seq = marker_op.seq;
    match per_child.entry(child) {
        std::collections::hash_map::Entry::Occupied(mut entry) => {
            if seq < entry.get().1 {
                let _replaced: (OpId, u64, SessionId) = entry.insert((marker_op, seq, session));
            }
        }
        std::collections::hash_map::Entry::Vacant(entry) => {
            let _slot: &mut (OpId, u64, SessionId) = entry.insert((marker_op, seq, session));
        }
    }
}

/// Build a structural relationship note whose causal parent is `parent_id`
/// and whose targets are `target_ids`, mirroring the Claude subagent/fork note
/// shape. The note is session-scoped (the owning thread's session), tagged
/// [`Tags::META`], and anchored at the parent op's source ordinal with a
/// reserved derived ordinal.
fn relationship_note(
    parent_id: OpId,
    target_ids: Vec<OpId>,
    relationship: NoteRelationship,
    disc: u16,
    scope: ScopeRef,
) -> Op {
    let stream = SourceStream::new(parent_id.node, parent_id.boot);
    let note_id = stream
        .op_from_position(SourcePosition::derived(parent_id.seq >> 16, disc))
        .unwrap_or(parent_id);
    Op {
        id: note_id,
        parents: editchain_core::parents::ParentSet::One(parent_id),
        actor: ActorId(0),
        clock: editchain_core::clock::Clock::None,
        scope,
        tags: Tags::META | Tags::IMPORT,
        kind: OpKind::Note(NoteOp {
            target_ids,
            relationship,
            content: Payload::Empty,
        }),
    }
}

#[cfg(test)]
mod tests {
    #![expect(
        clippy::indexing_slicing,
        clippy::panic,
        clippy::wildcard_enum_match_arm,
        reason = "Test fixtures use fixed small vectors; asserts panic on unexpected variants and match a Note kind with a wildcard fallback"
    )]
    use super::*;
    use editchain_core::op::ImportOp;
    use editchain_core::parents::ParentSet;
    use editchain_core::NodeId;

    use crate::ids::derive_actor_id;

    /// Build a raw import op on `stream` at `seq` with a Unix-ms clock.
    fn raw(stream: &SourceStream, seq: u64, clock_ms: u64) -> Op {
        Op {
            id: stream.op_from_position(SourcePosition::raw(seq)).unwrap(),
            parents: if seq == 1 {
                ParentSet::None
            } else {
                ParentSet::One(
                    stream
                        .op_from_position(SourcePosition::raw(seq.saturating_sub(1)))
                        .unwrap(),
                )
            },
            actor: ActorId(0),
            clock: editchain_core::clock::Clock::UnixMs(clock_ms),
            scope: ScopeRef::None,
            tags: Tags::IMPORT,
            kind: OpKind::Import(ImportOp {
                raw_ref: Payload::Empty,
                raw_hash: None,
            }),
        }
    }

    /// Build a raw import op with an unknown source clock (the raw lane's
    /// `SOURCE_TIME_UNKNOWN` tag + `UnixMs(0)`).
    fn raw_unknown_clock(stream: &SourceStream, seq: u64) -> Op {
        let mut op = raw(stream, seq, 0);
        op.tags |= Tags::SOURCE_TIME_UNKNOWN;
        op
    }

    /// Build a subagent activity marker note op on `stream` at `seq`, scoped to
    /// the given session scope (like the normalized Codex note ops).
    fn marker(stream: &SourceStream, seq: u64, session: SessionId) -> Op {
        Op {
            id: stream
                .op_from_position(SourcePosition::derived(seq, 1))
                .unwrap(),
            parents: ParentSet::One(stream.op_from_position(SourcePosition::raw(seq)).unwrap()),
            actor: derive_actor_id("system:parent"),
            clock: editchain_core::clock::Clock::None,
            scope: ScopeRef::Session(session),
            tags: Tags::NOTE | Tags::IMPORT,
            kind: OpKind::Note(NoteOp {
                target_ids: Vec::new(),
                relationship: NoteRelationship::Explains,
                content: Payload::Empty,
            }),
        }
    }

    fn scope(thread: &str) -> ScopeRef {
        ScopeRef::Session(SessionId(derive_session_id(thread).0))
    }

    #[test]
    fn subagent_of_targets_earliest_started_marker() {
        let parent_stream = SourceStream::new(NodeId(1), 0);
        let sub_stream = SourceStream::new(NodeId(2), 0);
        let parent_session = SessionId(derive_session_id("parent-1").0);
        let interacted = marker(&parent_stream, 2, parent_session);
        let spawn = marker(&parent_stream, 3, parent_session);
        let mut ops = vec![
            raw(&parent_stream, 1, 1000),
            raw(&parent_stream, 2, 2000),
            interacted.clone(),
            raw(&parent_stream, 3, 3000),
            spawn.clone(),
            raw(&sub_stream, 1, 4000),
        ];
        ops[0].scope = scope("parent-1");
        ops[1].scope = scope("parent-1");
        ops[3].scope = scope("parent-1");
        ops[5].scope = scope("sub-1");
        let topology = vec![
            ThreadTopology {
                thread_id: "parent-1".to_string(),
                parent_thread_id: None,
                forked_from_id: None,
                agent_path: None,
                markers: vec![
                    ActivityMarker {
                        agent_thread_id: "sub-1".to_string(),
                        agent_path: Some("/root/sub".to_string()),
                        op_id: interacted.id,
                        started: false,
                    },
                    ActivityMarker {
                        agent_thread_id: "sub-1".to_string(),
                        agent_path: Some("/root/sub".to_string()),
                        op_id: spawn.id,
                        started: true,
                    },
                ],
                completions: Vec::new(),
                legacy_completions: Vec::new(),
            },
            ThreadTopology {
                thread_id: "sub-1".to_string(),
                parent_thread_id: Some("parent-1".to_string()),
                forked_from_id: None,
                agent_path: Some("/root/sub".to_string()),
                markers: Vec::new(),
                completions: Vec::new(),
                legacy_completions: Vec::new(),
            },
        ];
        let notes = emit_codex_relationship_notes(&ops, &topology);
        let subagent = notes
            .iter()
            .find(|n| matches!(&n.kind, OpKind::Note(note) if note.relationship == NoteRelationship::SubagentOf))
            .expect("SubagentOf note");
        assert_eq!(
            subagent.parents,
            ParentSet::One(sub_stream.op_from_position(SourcePosition::raw(1)).unwrap())
        );
        match &subagent.kind {
            OpKind::Note(note) => assert_eq!(note.target_ids, vec![spawn.id]),
            _ => panic!("expected note op"),
        }
        assert_eq!(
            subagent.scope,
            scope("sub-1"),
            "relationship notes are session-scoped"
        );
    }

    #[test]
    fn subagent_of_falls_back_to_parent_first_op_without_marker() {
        let parent_stream = SourceStream::new(NodeId(1), 0);
        let sub_stream = SourceStream::new(NodeId(2), 0);
        let mut ops = vec![raw(&parent_stream, 1, 1000), raw(&sub_stream, 1, 2000)];
        ops[0].scope = scope("parent-1");
        ops[1].scope = scope("sub-1");
        let topology = vec![ThreadTopology {
            thread_id: "sub-1".to_string(),
            parent_thread_id: Some("parent-1".to_string()),
            forked_from_id: None,
            agent_path: None,
            markers: Vec::new(),
            completions: Vec::new(),
            legacy_completions: Vec::new(),
        }];
        let notes = emit_codex_relationship_notes(&ops, &topology);
        assert_eq!(notes.len(), 1);
        match &notes[0].kind {
            OpKind::Note(note) => {
                assert_eq!(note.relationship, NoteRelationship::SubagentOf);
                assert_eq!(
                    note.target_ids,
                    vec![parent_stream
                        .op_from_position(SourcePosition::raw(1))
                        .unwrap()]
                );
            }
            _ => panic!("expected note op"),
        }
    }

    #[test]
    fn reconnects_to_groups_per_marker_and_targets_subagent_last_op() {
        let parent_stream = SourceStream::new(NodeId(1), 0);
        let sub_stream = SourceStream::new(NodeId(2), 0);
        let second_stream = SourceStream::new(NodeId(3), 0);
        let third_stream = SourceStream::new(NodeId(4), 0);
        let mut ops = vec![
            raw(&parent_stream, 1, 1000),
            raw(&parent_stream, 2, 2000),
            raw(&parent_stream, 3, 3000),
            raw(&sub_stream, 1, 4000),
            raw(&sub_stream, 2, 5000),
            raw(&second_stream, 1, 6000),
            raw(&third_stream, 1, 7000),
            raw(&third_stream, 2, 8000),
        ];
        ops[0].scope = scope("parent-1");
        ops[1].scope = scope("parent-1");
        ops[2].scope = scope("parent-1");
        ops[3].scope = scope("sub-1");
        ops[4].scope = scope("sub-1");
        ops[5].scope = scope("second-1");
        ops[6].scope = scope("third-1");
        ops[7].scope = scope("third-1");
        // One Wait-style collab call completes two children on the same marker
        // op, and a second collab op (same physical line, different derived
        // lane) completes a third child: one grouped note per marker op, and
        // the two notes share a source ordinal so their ids must not collide.
        let wait_lane_1 = parent_stream
            .op_from_position(SourcePosition::derived(3, 1))
            .unwrap();
        let wait_lane_2 = parent_stream
            .op_from_position(SourcePosition::derived(3, 2))
            .unwrap();
        let topology = vec![ThreadTopology {
            thread_id: "parent-1".to_string(),
            parent_thread_id: None,
            forked_from_id: None,
            agent_path: None,
            markers: Vec::new(),
            completions: vec![
                CompletionEvidence {
                    agent_thread_id: "sub-1".to_string(),
                    op_id: wait_lane_1,
                },
                CompletionEvidence {
                    agent_thread_id: "second-1".to_string(),
                    op_id: wait_lane_1,
                },
                CompletionEvidence {
                    agent_thread_id: "third-1".to_string(),
                    op_id: wait_lane_2,
                },
            ],
            legacy_completions: Vec::new(),
        }];
        let notes = emit_codex_relationship_notes(&ops, &topology);
        let reconnects: Vec<_> = notes
            .iter()
            .filter(|n| matches!(&n.kind, OpKind::Note(note) if note.relationship == NoteRelationship::ReconnectsTo))
            .collect();
        assert_eq!(
            reconnects.len(),
            2,
            "one grouped note per completion marker"
        );
        let first = reconnects
            .iter()
            .find(|n| n.parents == ParentSet::One(wait_lane_1))
            .expect("first marker note");
        match &first.kind {
            OpKind::Note(note) => {
                let mut expected = vec![
                    sub_stream.op_from_position(SourcePosition::raw(2)).unwrap(),
                    second_stream
                        .op_from_position(SourcePosition::raw(1))
                        .unwrap(),
                ];
                expected.sort_unstable();
                assert_eq!(
                    note.target_ids, expected,
                    "sorted unique targets per marker"
                );
            }
            _ => panic!("expected note op"),
        }
        let second = reconnects
            .iter()
            .find(|n| n.parents == ParentSet::One(wait_lane_2))
            .expect("second marker note");
        match &second.kind {
            OpKind::Note(note) => assert_eq!(
                note.target_ids,
                vec![third_stream
                    .op_from_position(SourcePosition::raw(2))
                    .unwrap()]
            ),
            _ => panic!("expected note op"),
        }
        assert!(
            reconnects.iter().all(|n| n.scope == scope("parent-1")),
            "relationship notes are session-scoped"
        );
        // Notes anchored at the same source ordinal get distinct ids.
        let ids: std::collections::HashSet<OpId> = reconnects.iter().map(|n| n.id).collect();
        assert_eq!(
            ids.len(),
            reconnects.len(),
            "same-ordinal note ids must not collide"
        );
        // The grouped note's id is deterministic: re-running produces the same
        // note ids.
        let again = emit_codex_relationship_notes(&ops, &topology);
        let again_ids: std::collections::HashSet<OpId> = again
            .iter()
            .filter(|n| matches!(&n.kind, OpKind::Note(note) if note.relationship == NoteRelationship::ReconnectsTo))
            .map(|n| n.id)
            .collect();
        assert_eq!(again_ids, ids, "relationship note ids are deterministic");
    }

    #[test]
    fn reconnects_to_uses_earliest_explicit_completion_per_child() {
        let parent_stream = SourceStream::new(NodeId(1), 0);
        let sub_stream = SourceStream::new(NodeId(2), 0);
        let early = parent_stream
            .op_from_position(SourcePosition::derived(2, 1))
            .unwrap();
        let late = parent_stream
            .op_from_position(SourcePosition::derived(3, 1))
            .unwrap();
        let mut ops = vec![
            raw(&parent_stream, 1, 1000),
            raw(&parent_stream, 2, 2000),
            raw(&parent_stream, 3, 3000),
            raw(&sub_stream, 1, 4000),
            raw(&sub_stream, 2, 5000),
        ];
        ops[0].scope = scope("parent-1");
        ops[1].scope = scope("parent-1");
        ops[2].scope = scope("parent-1");
        ops[3].scope = scope("sub-1");
        ops[4].scope = scope("sub-1");
        let topology = vec![ThreadTopology {
            thread_id: "parent-1".to_string(),
            parent_thread_id: None,
            forked_from_id: None,
            agent_path: None,
            markers: Vec::new(),
            completions: vec![
                CompletionEvidence {
                    agent_thread_id: "sub-1".to_string(),
                    op_id: late,
                },
                CompletionEvidence {
                    agent_thread_id: "sub-1".to_string(),
                    op_id: early,
                },
            ],
            legacy_completions: Vec::new(),
        }];
        let notes = emit_codex_relationship_notes(&ops, &topology);
        let reconnects: Vec<_> = notes
            .iter()
            .filter(|n| matches!(&n.kind, OpKind::Note(note) if note.relationship == NoteRelationship::ReconnectsTo))
            .collect();
        assert_eq!(reconnects.len(), 1);
        assert_eq!(reconnects[0].parents, ParentSet::One(early));
        match &reconnects[0].kind {
            OpKind::Note(note) => assert_eq!(
                note.target_ids,
                vec![sub_stream.op_from_position(SourcePosition::raw(2)).unwrap()]
            ),
            _ => panic!("expected note op"),
        }
    }

    #[test]
    fn legacy_completion_maps_completed_agent_path_to_started_marker() {
        let parent_stream = SourceStream::new(NodeId(1), 0);
        let sub_stream = SourceStream::new(NodeId(2), 0);
        let spawn = marker(
            &parent_stream,
            2,
            SessionId(derive_session_id("parent-1").0),
        );
        let list_op = parent_stream
            .op_from_position(SourcePosition::derived(3, 1))
            .unwrap();
        let mut ops = vec![
            raw(&parent_stream, 1, 1000),
            raw(&parent_stream, 2, 2000),
            spawn.clone(),
            raw(&parent_stream, 3, 3000),
            raw(&sub_stream, 1, 4000),
            raw(&sub_stream, 2, 5000),
        ];
        ops[0].scope = scope("parent-1");
        ops[1].scope = scope("parent-1");
        ops[3].scope = scope("parent-1");
        ops[4].scope = scope("sub-1");
        ops[5].scope = scope("sub-1");
        let topology = vec![ThreadTopology {
            thread_id: "parent-1".to_string(),
            parent_thread_id: None,
            forked_from_id: None,
            agent_path: None,
            markers: vec![ActivityMarker {
                agent_thread_id: "sub-1".to_string(),
                agent_path: Some("/root/sub".to_string()),
                op_id: spawn.id,
                started: true,
            }],
            completions: Vec::new(),
            legacy_completions: vec![LegacyCompletionEvidence {
                agent_path: "/root/sub".to_string(),
                op_id: list_op,
            }],
        }];
        let notes = emit_codex_relationship_notes(&ops, &topology);
        let reconnect = notes
            .iter()
            .find(|n| matches!(&n.kind, OpKind::Note(note) if note.relationship == NoteRelationship::ReconnectsTo))
            .expect("ReconnectsTo note");
        assert_eq!(reconnect.parents, ParentSet::One(list_op));
        match &reconnect.kind {
            OpKind::Note(note) => assert_eq!(
                note.target_ids,
                vec![sub_stream.op_from_position(SourcePosition::raw(2)).unwrap()]
            ),
            _ => panic!("expected note op"),
        }
    }

    #[test]
    fn legacy_completion_without_exact_started_match_skips_edge() {
        let parent_stream = SourceStream::new(NodeId(1), 0);
        let sub_stream = SourceStream::new(NodeId(2), 0);
        let list_op = parent_stream
            .op_from_position(SourcePosition::derived(2, 1))
            .unwrap();
        let mut ops = vec![
            raw(&parent_stream, 1, 1000),
            raw(&parent_stream, 2, 2000),
            raw(&sub_stream, 1, 3000),
        ];
        ops[0].scope = scope("parent-1");
        ops[1].scope = scope("parent-1");
        ops[2].scope = scope("sub-1");
        // Two started markers share the same agentPath: ambiguous, no edge.
        let ambiguous = vec![
            ActivityMarker {
                agent_thread_id: "sub-1".to_string(),
                agent_path: Some("/root/sub".to_string()),
                op_id: list_op,
                started: true,
            },
            ActivityMarker {
                agent_thread_id: "sub-2".to_string(),
                agent_path: Some("/root/sub".to_string()),
                op_id: list_op,
                started: true,
            },
        ];
        let topology = vec![ThreadTopology {
            thread_id: "parent-1".to_string(),
            parent_thread_id: None,
            forked_from_id: None,
            agent_path: None,
            markers: ambiguous,
            completions: Vec::new(),
            legacy_completions: vec![LegacyCompletionEvidence {
                agent_path: "/root/sub".to_string(),
                op_id: list_op,
            }],
        }];
        assert!(
            emit_codex_relationship_notes(&ops, &topology).is_empty(),
            "ambiguous agentPath evidence must not fabricate an edge"
        );
        // Missing started marker: no edge either.
        let topology = vec![ThreadTopology {
            thread_id: "parent-1".to_string(),
            parent_thread_id: None,
            forked_from_id: None,
            agent_path: None,
            markers: Vec::new(),
            completions: Vec::new(),
            legacy_completions: vec![LegacyCompletionEvidence {
                agent_path: "/root/other".to_string(),
                op_id: list_op,
            }],
        }];
        assert!(
            emit_codex_relationship_notes(&ops, &topology).is_empty(),
            "missing agentPath evidence must not fabricate an edge"
        );
    }

    #[test]
    fn fork_of_targets_clock_bounded_source_anchor() {
        let trunk_stream = SourceStream::new(NodeId(1), 0);
        let fork_stream = SourceStream::new(NodeId(2), 0);
        // Trunk has ops at 1000 and 5000; the fork starts at 3000, so the
        // divergence anchor is the trunk's first op (the newest op at or before
        // the fork's start).
        let mut ops = vec![
            raw(&trunk_stream, 1, 1000),
            raw(&trunk_stream, 2, 5000),
            raw(&fork_stream, 1, 3000),
        ];
        ops[0].scope = scope("trunk-1");
        ops[1].scope = scope("trunk-1");
        ops[2].scope = scope("fork-1");
        let topology = vec![ThreadTopology {
            thread_id: "fork-1".to_string(),
            parent_thread_id: None,
            forked_from_id: Some("trunk-1".to_string()),
            agent_path: None,
            markers: Vec::new(),
            completions: Vec::new(),
            legacy_completions: Vec::new(),
        }];
        let notes = emit_codex_relationship_notes(&ops, &topology);
        assert_eq!(notes.len(), 1);
        match &notes[0].kind {
            OpKind::Note(note) => {
                assert_eq!(note.relationship, NoteRelationship::ForkOf);
                assert_eq!(
                    note.target_ids,
                    vec![trunk_stream
                        .op_from_position(SourcePosition::raw(1))
                        .unwrap()]
                );
                assert_eq!(
                    notes[0].parents,
                    ParentSet::One(
                        fork_stream
                            .op_from_position(SourcePosition::raw(1))
                            .unwrap()
                    )
                );
            }
            _ => panic!("expected note op"),
        }
        assert_eq!(
            notes[0].scope,
            scope("fork-1"),
            "relationship notes are session-scoped"
        );
    }

    #[test]
    fn fork_of_skips_unknown_clock_and_ignores_derived_source_lanes() {
        let trunk_stream = SourceStream::new(NodeId(1), 0);
        let fork_stream = SourceStream::new(NodeId(2), 0);
        let mut ops = vec![
            raw(&trunk_stream, 1, 1000),
            raw(&trunk_stream, 2, 2000),
            raw_unknown_clock(&trunk_stream, 3),
            raw(&fork_stream, 1, 3000),
        ];
        ops[0].scope = scope("trunk-1");
        ops[1].scope = scope("trunk-1");
        ops[2].scope = scope("trunk-1");
        ops[3].scope = scope("fork-1");
        // A derived lane at trunk ordinal 2 (Clock::None) must not win the
        // anchor over the raw op at ordinal 2.
        let derived = Op {
            id: trunk_stream
                .op_from_position(SourcePosition::derived(2, 1))
                .unwrap(),
            parents: ParentSet::One(ops[1].id),
            actor: ActorId(0),
            clock: editchain_core::clock::Clock::None,
            scope: scope("trunk-1"),
            tags: Tags::NOTE | Tags::IMPORT,
            kind: OpKind::Note(NoteOp {
                target_ids: Vec::new(),
                relationship: NoteRelationship::Explains,
                content: Payload::Empty,
            }),
        };
        ops.push(derived);
        let topology = vec![ThreadTopology {
            thread_id: "fork-1".to_string(),
            parent_thread_id: None,
            forked_from_id: Some("trunk-1".to_string()),
            agent_path: None,
            markers: Vec::new(),
            completions: Vec::new(),
            legacy_completions: Vec::new(),
        }];
        let notes = emit_codex_relationship_notes(&ops, &topology);
        match &notes[0].kind {
            OpKind::Note(note) => {
                assert_eq!(
                    note.target_ids,
                    vec![trunk_stream
                        .op_from_position(SourcePosition::raw(2))
                        .unwrap()],
                    "raw source anchor wins over derived lane at the same ordinal"
                );
            }
            _ => panic!("expected note op"),
        }

        // Unknown fork start clock: skipped entirely rather than guessed.
        let unknown_fork = raw_unknown_clock(&fork_stream, 1);
        let topology = vec![ThreadTopology {
            thread_id: "fork-2".to_string(),
            parent_thread_id: None,
            forked_from_id: Some("trunk-1".to_string()),
            agent_path: None,
            markers: Vec::new(),
            completions: Vec::new(),
            legacy_completions: Vec::new(),
        }];
        let mut ops2 = vec![unknown_fork.clone(), raw(&trunk_stream, 1, 1000)];
        ops2[0].scope = scope("fork-2");
        ops2[1].scope = scope("trunk-1");
        assert!(
            emit_codex_relationship_notes(&ops2, &topology).is_empty(),
            "unknown fork clock must skip fork anchoring"
        );
    }

    #[test]
    fn fork_of_suppressed_by_explicit_subagent_provenance() {
        let trunk_stream = SourceStream::new(NodeId(1), 0);
        let fork_stream = SourceStream::new(NodeId(2), 0);
        let mut ops = vec![raw(&trunk_stream, 1, 1000), raw(&fork_stream, 1, 3000)];
        ops[0].scope = scope("trunk-1");
        ops[1].scope = scope("fork-1");
        for (parent, agent_path) in [
            (Some("parent-1".to_string()), None),
            (None, Some("/root/sub".to_string())),
        ] {
            let topology = vec![ThreadTopology {
                thread_id: "fork-1".to_string(),
                parent_thread_id: parent,
                forked_from_id: Some("trunk-1".to_string()),
                agent_path,
                markers: Vec::new(),
                completions: Vec::new(),
                legacy_completions: Vec::new(),
            }];
            assert!(
                emit_codex_relationship_notes(&ops, &topology).is_empty(),
                "explicit subagent provenance suppresses ForkOf"
            );
        }
    }

    #[test]
    fn missing_counterpart_thread_skips_linking() {
        let sub_stream = SourceStream::new(NodeId(1), 0);
        let mut ops = vec![raw(&sub_stream, 1, 1000)];
        ops[0].scope = scope("sub-1");
        let topology = vec![ThreadTopology {
            thread_id: "sub-1".to_string(),
            parent_thread_id: Some("parent-missing".to_string()),
            forked_from_id: None,
            agent_path: None,
            markers: Vec::new(),
            completions: Vec::new(),
            legacy_completions: Vec::new(),
        }];
        assert!(
            emit_codex_relationship_notes(&ops, &topology).is_empty(),
            "a parent thread absent from this run's ops is skipped"
        );
    }
}
