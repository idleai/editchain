//! Fork linking — link sessions that are forks of one original session.
//!
//! Claude Code can resume or continue a single session in several ways,
//! producing multiple session files that share the same message ancestry (the
//! `parentUuid` chain) but diverge into separate continuations. Each is
//! imported as an independent linear chain, so they render as parallel
//! identical chains rather than a fork. This module detects sessions sharing a
//! common `parentUuid` prefix and links them so they share a root and render as
//! a fork (one trunk, several branches) instead of duplicated content.
//!
//! Linking emits typed [`NoteRelationship::ForkOf`] relationship notes rather
//! than mutating causal parents — stored causality stays immutable (SPEC §1.1).
//! The layout reads these notes as virtual edges when projecting branches.
//!
//! ## Geometry note
//!
//! Ops are displayed newest-first, so chronological order maps to *descending*
//! rows. To make a fork render as branches off a shared trunk we emit one
//! `ForkOf` note per fork session whose causal parent is that session's first
//! op and whose target is the representative session's first op (the shared
//! root). The layout treats each such note as an edge from its parent down to
//! its target — i.e. from each fork branch down to the shared root.

use std::collections::HashMap;

use editchain_core::{
    clock::Clock,
    op::{NoteOp, NoteRelationship, OpKind},
    payload::Payload,
    scope::ScopeRef,
    tags::Tags,
    ActorId, Op, OpId,
};

use crate::ids::{SourcePosition, SourceStream};

/// Reserved high derived-ordinal used for `ForkOf` note IDs.
///
/// Normalization allocates derived ordinals sequentially starting at 1 per
/// source record; these reserved values sit far above any realistic content-block
/// count so they never collide with normalized children on the same stream.
const FORK_NOTE_DISC: u16 = 0xFFFE;

/// Emit `ForkOf` relationship notes linking sessions that are forks of one original.
///
/// Does not mutate `ops`. Returns new [`Op`]s (one per non-trunk fork session),
/// each tagged [`Tags::META`] so projection treats them as structural metadata
/// rather than graph rows.
///
/// ## Tree-correct trunk selection
///
/// A fork group shares a leading `parentUuid` ancestry prefix. The **trunk** is
/// the session that *authors* the shared fork-point message (the first
/// `parentUuid` of the signature resolves to that session's own records); the
/// other members are its branches. This is deterministic and matches how Claude
/// Code records forks (a resumed session's first record points at the message it
/// continues). We deliberately do NOT pick the longest chain as the trunk — that
/// demoted the genuine root and mis-grouped near-copies (see memory
/// `seed-hub-fork-bug`).
///
/// ## Branch at the divergence point
///
/// Each branch's `ForkOf` note anchors at the exact split: the causal parent is
/// the branch's first op *after* its shared prefix with the trunk, and the target
/// is the trunk's message at that divergence boundary. The projection reads the
/// note as a virtual edge, so the branch renders branching off the trunk at the
/// split rather than re-drawing the duplicated prologue on its own lane. The
/// renderer additionally folds the branch's pre-boundary prologue into the trunk
/// (see project `collapsed_ops`), so shared messages never render twice.
#[must_use]
pub fn emit_fork_notes(ops: &[Op]) -> Vec<Op> {
    // The number of leading parentUuids that identify a fork group. Sessions
    // sharing this many leading ancestry records are treated as forks of one
    // original session. Bounded so we don't over-group sessions that merely
    // share a couple of early records by coincidence.
    const SIGNATURE_LEN: usize = 4;

    // Group raw ImportOps by session scope so we can build each session's
    // parentUuid chain (aligned to raw op ids) and find its records.
    let mut ops_by_session: HashMap<u64, Vec<usize>> = HashMap::new();
    for (i, op) in ops.iter().enumerate() {
        if let ScopeRef::Session(sid) = op.scope {
            ops_by_session.entry(sid.0).or_default().push(i);
        }
    }

    // For each session, build an ordered `(parentUuid, raw_op_id)` vector aligned
    // to the record chain (record order). Non-Import records are skipped for the
    // chain but their raw op id is still usable as branch anchors.
    let mut session_chain: HashMap<u64, Vec<(String, OpId)>> = HashMap::new();
    for (&sid, indices) in &ops_by_session {
        let mut chain: Vec<(String, OpId)> = Vec::new();
        for &i in indices {
            let Some(op) = ops.get(i) else {
                continue;
            };
            if matches!(op.kind, OpKind::Import(_)) {
                if let Some(pu) = parent_uuid(op) {
                    // Place the import at its chain position; keep the op id so we
                    // can anchor a divergence on the trunk later.
                    chain.push((pu, op.id));
                }
            }
        }
        if !chain.is_empty() {
            drop(session_chain.insert(sid, chain));
        }
    }

    // Group sessions by their fork signature: the first few parentUuids of the
    // chain. Sessions sharing a signature are fork *candidates*.
    let mut groups: HashMap<Vec<String>, Vec<u64>> = HashMap::new();
    for (&sid, chain) in &session_chain {
        let sig: Vec<String> = chain
            .iter()
            .take(SIGNATURE_LEN)
            .map(|(pu, _)| pu.clone())
            .collect();
        if sig.len() >= SIGNATURE_LEN {
            let slot = groups.entry(sig.clone()).or_default();
            if !slot.contains(&sid) {
                slot.push(sid);
            }
        }
    }

    // A session A "owns" a message uuid if that uuid appears in A's own imported
    // records (i.e. A authored the record whose uuid == the parentUuid). We scan
    // all raw import payloads once to build uuid -> session scope.
    let mut uuid_owner: HashMap<String, u64> = HashMap::new();
    for op in ops {
        if let ScopeRef::Session(sid) = op.scope {
            if let Some(u) = record_uuid(op) {
                let _unused = uuid_owner.entry(u).or_insert(sid.0);
            }
        }
    }

    let mut notes = Vec::new();
    for (_sig, sids) in groups {
        if sids.len() < 2 {
            continue; // not a fork — single session
        }
        let Some(trunk) = pick_trunk(&sids, &session_chain, &uuid_owner) else {
            continue;
        };
        let Some(trunk_chain) = session_chain.get(&trunk) else {
            continue;
        };

        // Compute the group-wide fork point: the newest record that EVERY member
        // shares with the trunk (the latest-common-ancestor). This is the minimum
        // pairwise shared-prefix length over all branches, because a session that
        // diverges earliest bounds how far the common ancestry extends. Anchoring
        // every branch at THIS one index makes a multi-branch hub render as a
        // single clean fork off the trunk, rather than per-branch splits at
        // arbitrary depths (which made different branches attach near the root vs
        // near the tip of the same trunk).
        let mut min_shared: Option<usize> = None;
        for &sid in &sids {
            if sid == trunk {
                continue;
            }
            let Some(fork_chain) = session_chain.get(&sid) else {
                continue;
            };
            // Sessions that are a strict prefix of the trunk (pure continue/
            // duplicate, no divergent tail) contribute no fork boundary.
            if let Some(shared) = diverge_index(trunk_chain, fork_chain) {
                min_shared = Some(min_shared.map_or(shared, |m| m.min(shared)));
            }
        }
        let Some(shared) = min_shared else {
            continue; // no branch has a genuine divergence — not a fork
        };

        let Some((_, trunk_boundary_id)) = trunk_chain.get(shared) else {
            continue;
        };
        for &sid in &sids {
            if sid == trunk {
                continue;
            }
            let Some(fork_chain) = session_chain.get(&sid) else {
                continue;
            };
            let Some((_, fork_boundary_id)) = fork_chain.get(shared) else {
                continue;
            };
            // ForkOf: parent = branch's op at the single shared fork point, target
            // = the trunk's op at that same point. Scope follows the branch so the
            // note is grouped with the branch session.
            notes.push(fork_note(*fork_boundary_id, *trunk_boundary_id, sid));
        }
    }

    notes
}

/// Pick the fork group's trunk: the session that owns the shared fork-point
/// message (the first `parentUuid` of the signature). Falls back to the session
/// whose chain is longest when ownership cannot be resolved (e.g. orphaned
/// fork-point), so we never leave a fork group unlinked.
fn pick_trunk(
    sids: &[u64],
    session_chain: &HashMap<u64, Vec<(String, OpId)>>,
    uuid_owner: &HashMap<String, u64>,
) -> Option<u64> {
    // The shared fork-point is the first parentUuid of the (common) signature.
    let sig_first = session_chain
        .get(sids.first()?)
        .and_then(|c| c.first())
        .map(|(pu, _)| pu.clone());
    if let Some(sig_first) = sig_first {
        if let Some(&owner) = uuid_owner.get(&sig_first) {
            if sids.contains(&owner) {
                return Some(owner);
            }
        }
    }
    // Ownership unresolved — fall back to the longest chain.
    sids.iter()
        .max_by_key(|&&s| session_chain.get(&s).map_or(0, Vec::len))
        .copied()
}

/// Index of the first position where two (parentUuid-aligned) chains differ,
/// i.e. the number of leading entries they share. `None` if one chain is a
/// strict prefix of the other (pure continued/duplicate transcript — no fork
/// boundary), or if they share fewer than the minimum fork prefix.
fn diverge_index(a: &[(String, OpId)], b: &[(String, OpId)]) -> Option<usize> {
    // Minimum shared ancestry (in parentUuid records) required to consider two
    // sessions related forks at all. Fewer than this and they only share a
    // trivial opening by coincidence, not a real fork point.
    const FORK_MIN_SHARED: usize = 3;

    let shared = a
        .iter()
        .zip(b.iter())
        .take_while(|((x, _), (y, _))| x == y)
        .count();
    if shared < FORK_MIN_SHARED {
        // Trivially shared opening — not a fork.
        return None;
    }
    // If b (the fork) ends within the shared region (strict prefix of a) — a
    // common ancestor but no divergent tail — there is no fork boundary.
    if shared >= b.len() {
        return None;
    }
    // Ensure the fork has at least one divergent trailing record beyond the
    // boundary. A pair that diverges only on its very last message is a resume
    // artifact (the fork's content is otherwise identical to the trunk), not a
    // meaningful branch — treat it as a duplicate and leave it unlinked. This
    // still admits genuine mid/late forks (e.g. the seed hub's `d81f9fe2`, which
    // diverges from `3f7db8b8` at record 544 of 949).
    if shared >= b.len().saturating_sub(1) {
        return None;
    }
    // The fork chain diverges somewhere after the shared prefix. `b[shared]` is
    // the first divergent record.
    Some(shared)
}

/// Build a `ForkOf` note anchored at the branch's divergence boundary: the causal
/// parent is that boundary op on the fork session's chain, and the target is the
/// trunk's message at the same boundary. Scope is the branch session's id, so the
/// note groups with the branch it links.
fn fork_note(fork_boundary_id: OpId, trunk_boundary_id: OpId, branch_sid: u64) -> Op {
    // Use the boundary op's id/node/boot to derive a deterministic note id on the
    // fork session's own stream.
    let stream = SourceStream::new(fork_boundary_id.node, fork_boundary_id.boot);
    let note_id = stream.op_from_position(SourcePosition::derived(
        fork_boundary_id.seq >> 16,
        FORK_NOTE_DISC,
    ));
    let note_id = note_id.unwrap_or(fork_boundary_id);

    Op {
        id: note_id,
        parents: editchain_core::parents::ParentSet::One(fork_boundary_id),
        actor: ActorId(0),
        clock: Clock::None,
        scope: ScopeRef::Session(editchain_core::SessionId(branch_sid)),
        tags: Tags::META | Tags::IMPORT,
        kind: OpKind::Note(NoteOp {
            target_ids: vec![trunk_boundary_id],
            relationship: NoteRelationship::ForkOf,
            content: Payload::Empty,
        }),
    }
}

/// Extract the record's `uuid` from a raw import op's JSONL (the message id this
/// record authors). Returns `None` if the record has no uuid.
fn record_uuid(op: &Op) -> Option<String> {
    let OpKind::Import(import) = &op.kind else {
        return None;
    };
    let Payload::Inline(bytes) = &import.raw_ref else {
        return None;
    };
    let value: serde_json::Value = serde_json::from_slice(bytes).ok()?;
    value
        .get("uuid")
        .and_then(serde_json::Value::as_str)
        .map(ToString::to_string)
}

/// Extract the `parentUuid` from a raw import op's JSONL record.
fn parent_uuid(op: &Op) -> Option<String> {
    let OpKind::Import(import) = &op.kind else {
        return None;
    };
    let Payload::Inline(bytes) = &import.raw_ref else {
        return None;
    };
    // The raw record is a JSONL line; parse it to read the `parentUuid` field.
    // The envelope parser already validated it as JSON, so this is best-effort.
    let value: serde_json::Value = serde_json::from_slice(bytes).ok()?;
    value
        .get("parentUuid")
        .and_then(serde_json::Value::as_str)
        .map(ToString::to_string)
}

#[cfg(test)]
mod tests {
    #![expect(
        clippy::indexing_slicing,
        clippy::redundant_pattern_matching,
        clippy::wildcard_enum_match_arm,
        reason = "Test fixtures use fixed small vectors and a single fixed note index; some asserts match a Note kind with a wildcard fallback"
    )]
    use super::*;
    use editchain_core::{ImportOp, NodeId, SessionId};

    /// Build a raw import op for a session with the given parentUuid.
    fn import_op(node: u64, seq: u64, sid: u64, parent_uuid: &str) -> Op {
        Op {
            id: OpId::new(NodeId(node), 0, seq),
            parents: editchain_core::parents::ParentSet::None,
            actor: ActorId(1),
            clock: Clock::UnixMs(seq),
            scope: ScopeRef::Session(SessionId(sid)),
            tags: Tags::IMPORT,
            kind: OpKind::Import(ImportOp {
                raw_ref: Payload::Inline(
                    format!(r#"{{"parentUuid":"{parent_uuid}"}}"#).into_bytes(),
                ),
                raw_hash: None,
            }),
        }
    }

    /// Build a session's ops as a sequence of parentUuid-bearing imports.
    fn session_ops(node: u64, sid: u64, parent_uuids: &[&str]) -> Vec<Op> {
        (1..=parent_uuids.len())
            .zip(parent_uuids)
            .map(|(seq, pu)| import_op(node, u64::try_from(seq).unwrap_or_default(), sid, pu))
            .collect()
    }

    #[test]
    fn emits_fork_note_for_genuine_early_divergence() {
        // Two sessions that share a real ancestry prefix (a fork point) and then
        // diverge within the first half of the shorter chain — a genuine fork.
        // They share the first 4 parentUuids (so they'd group) and diverge at
        // record 5, well before the shorter chain (10) is half consumed.
        let mut ops = session_ops(
            1,
            1,
            &["s1", "s2", "s3", "s4", "s5", "a6", "a7", "a8", "a9", "a10"],
        );
        ops.extend(session_ops(
            2,
            2,
            &["s1", "s2", "s3", "s4", "s5", "b6", "b7", "b8", "b9", "b10"],
        ));
        let notes = emit_fork_notes(&ops);

        // One ForkOf note emitted; input ops untouched (10 ops per session × 2).
        assert_eq!(notes.len(), 1);
        assert_eq!(ops.len(), 20);
        let note = &notes[0];
        assert!(matches!(note.kind, OpKind::Note(_)));
        assert!(matches!(note.tags.matches_any(Tags::META), true));
    }

    #[test]
    fn does_not_emit_for_near_copy_transcript() {
        // Two sessions that are near-identical (share almost the whole shorter
        // chain, diverging only at the very end) are duplicated transcripts, NOT
        // a fork. Linking them would make one re-render the other's content.
        let mut ops = session_ops(
            1,
            1,
            &["a", "b", "c", "d", "e", "f", "g", "h", "i", "j", "k", "x"],
        );
        ops.extend(session_ops(
            2,
            2,
            &["a", "b", "c", "d", "e", "f", "g", "h", "i", "j", "k", "y"],
        ));
        let notes = emit_fork_notes(&ops);
        assert!(notes.is_empty(), "near-copy transcript must not be forked");
    }

    #[test]
    fn does_not_emit_for_identical_transcripts() {
        // Two sessions with byte-identical parentUuid chains (one is a strict
        // prefix of the other) — a pure duplicate, no divergence. Not a fork.
        let mut ops = session_ops(1, 1, &["root-a", "root-b", "root-c", "root-d"]);
        ops.extend(session_ops(2, 2, &["root-a", "root-b", "root-c", "root-d"]));
        let notes = emit_fork_notes(&ops);
        assert!(notes.is_empty());
    }

    #[test]
    fn does_not_emit_for_distinct_sessions() {
        // Two sessions with different parentUuid chains — not forks.
        let mut ops = session_ops(1, 1, &["chain-a-1", "chain-a-2"]);
        ops.extend(session_ops(2, 2, &["chain-b-1", "chain-b-2"]));
        let notes = emit_fork_notes(&ops);
        assert!(notes.is_empty());
    }

    /// Build an import op carrying both a `parentUuid` and the `uuid` it authors.
    /// Used to model the seed-hub topology where one session is the *owner* of the
    /// shared fork-point message (so it becomes the trunk even if it is not the
    /// longest session).
    fn import_op_with_uuid(node: u64, seq: u64, sid: u64, parent_uuid: &str, uuid: &str) -> Op {
        Op {
            id: OpId::new(NodeId(node), 0, seq),
            parents: editchain_core::parents::ParentSet::None,
            actor: ActorId(1),
            clock: Clock::UnixMs(seq),
            scope: ScopeRef::Session(SessionId(sid)),
            tags: Tags::IMPORT,
            kind: OpKind::Import(ImportOp {
                raw_ref: Payload::Inline(
                    format!(r#"{{"parentUuid":"{parent_uuid}","uuid":"{uuid}"}}"#).into_bytes(),
                ),
                raw_hash: None,
            }),
        }
    }

    #[test]
    fn seed_hub_picks_owner_as_trunk_not_longest_and_branches_at_divergence() {
        // Each chain is a list of (parentUuid, uuid); the FIRST uuid on trunk A
        // must equal `p1` so A owns the fork-point message.
        fn build(node: u64, sid: u64, records: &[(&str, &str)]) -> Vec<Op> {
            records
                .iter()
                .enumerate()
                .map(|(seq, &(pu, uu))| {
                    import_op_with_uuid(node, u64::try_from(seq + 1).unwrap(), sid, pu, uu)
                })
                .collect()
        }

        // Mirrors the real seed hub: three sessions share the fork signature
        // `[p1,p2,p3,p4]`. Session A authors `p1` (the fork-point message) and is
        // the SHORTER trunk; sessions B (long) and C (short) both share the same
        // prefix and then diverge. Using longest-session-as-trunk (the old bug)
        // would demote A and suppress the real branches.
        let mut ops = Vec::new();

        // Trunk A (SHORTER, 7 records): owns the fork-point message `p1`.
        ops.extend(build(
            1,
            1,
            &[
                ("p1", "p1"),
                ("p2", "p2"),
                ("p3", "p3"),
                ("p4", "p4"),
                ("ta5", "ta5"),
                ("ta6", "ta6"),
                ("ta7", "ta7"),
            ],
        ));
        // Branch B (LONGER, 9 records): shares the prefix, then diverges with a
        // real tail.
        ops.extend(build(
            2,
            2,
            &[
                ("p1", "b-p1"),
                ("p2", "b-p2"),
                ("p3", "b-p3"),
                ("p4", "b-p4"),
                ("tb5", "tb5"),
                ("tb6", "tb6"),
                ("tb7", "tb7"),
                ("tb8", "tb8"),
                ("tb9", "tb9"),
            ],
        ));
        // Branch C (SHORT, 6 records): shares the prefix, then diverges with a
        // two-message tail (admitted as a genuine branch, not a resume artifact).
        ops.extend(build(
            3,
            3,
            &[
                ("p1", "c-p1"),
                ("p2", "c-p2"),
                ("p3", "c-p3"),
                ("p4", "c-p4"),
                ("tc5", "tc5"),
                ("tc6", "tc6"),
            ],
        ));

        let notes = emit_fork_notes(&ops);
        // Two branches (B and C) fork off trunk A — not the longest session (B).
        assert_eq!(
            notes.len(),
            2,
            "expected B and C to fork off A; got {}",
            notes.len()
        );
        // Each note's target must be on trunk A's stream (node 1), i.e. anchored
        // at trunk A's divergence message, not B's.
        for n in &notes {
            let t = if let OpKind::Note(note) = &n.kind {
                note.target_ids.first().copied().unwrap()
            } else {
                OpId::new(NodeId(0), 0, 0)
            };
            assert_eq!(
                t.node.0, 1,
                "fork target must anchor on the trunk (node 1), got {t}"
            );
        }
    }

    #[test]
    fn single_fork_point_at_min_shared_prev_boundary_rather_than_per_branch_depth() {
        // Two branches of one trunk that diverge at DIFFERENT depths. The single
        // shared fork point must be the newest record both branches' ancestors share
        // with the trunk (the earliest-diverging branch bounds it), NOT each branch's
        // individual split depth. Otherwise branches attach to the same trunk at
        // unrelated rows (one near the root, one near the tip).
        fn build(node: u64, sid: u64, records: &[(&str, &str)]) -> Vec<Op> {
            records
                .iter()
                .enumerate()
                .map(|(seq, &(pu, uu))| {
                    import_op_with_uuid(node, u64::try_from(seq + 1).unwrap(), sid, pu, uu)
                })
                .collect()
        }

        let mut ops = Vec::new();
        // Trunk A (node 1) owns `p1`; chain p1..p10 (10 records).
        let trunk = ["p1", "p2", "p3", "p4", "p5", "t6", "t7", "t8", "t9", "t10"];
        ops.extend(build(
            1,
            1,
            &trunk.iter().map(|&p| (p, p)).collect::<Vec<_>>(),
        ));
        // Branch B (node 2): shares p1..p5 then diverges at record 6 (depth 5).
        let b = ["p1", "p2", "p3", "p4", "p5", "b6", "b7", "b8"];
        ops.extend(build(2, 2, &b.iter().map(|&p| (p, p)).collect::<Vec<_>>()));
        // Branch C (node 3): shares p1..p3 then diverges at record 4 (depth 3).
        let c = ["p1", "p2", "p3", "p4", "c5", "c6", "c7"];
        ops.extend(build(3, 3, &c.iter().map(|&p| (p, p)).collect::<Vec<_>>()));

        let notes = emit_fork_notes(&ops);
        assert_eq!(notes.len(), 2, "both B and C fork off trunk A");

        // The group-wide fork point is the EARLIEST divergence = depth 3 (record 4,
        // where C splits). Both notes must target trunk A at record 4 (seq 4), NOT
        // B's own depth 5 (seq 6).
        let trunk_anchor_seq = 5u64; // trunk[4] = p5; build uses seq = record_index + 1
        for n in &notes {
            let t = match &n.kind {
                OpKind::Note(note) => note.target_ids.first().copied().unwrap(),
                _ => OpId::new(NodeId(0), 0, 0),
            };
            assert_eq!(
                t.node.0, 1,
                "fork target must be on trunk A (node 1), got {t}"
            );
            assert_eq!(
                t.seq, trunk_anchor_seq,
                "all branches must anchor at the single shared fork point (record {trunk_anchor_seq}), got {t}"
            );
        }
    }
}
