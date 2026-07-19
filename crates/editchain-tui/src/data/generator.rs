use std::collections::HashMap;
use editchain_core::*;
use crate::data::header::{OpHeader, OpOrdinal};
use crate::data::snapshot::{ChainStatistics, TuiSnapshot};

/// Generate a synthetic chain for testing the TUI without real storage.
///
/// Produces a realistic mix of operation kinds with branches and merges.
pub fn generate_synthetic_chain(count: usize) -> TuiSnapshot {
    let mut headers = Vec::with_capacity(count);
    let mut by_id = HashMap::new();
    let mut parents: Vec<Vec<OpOrdinal>> = Vec::with_capacity(count);
    let mut children: Vec<Vec<OpOrdinal>> = Vec::with_capacity(count);
    let mut by_kind: HashMap<u8, Vec<OpOrdinal>> = HashMap::new();
    let mut by_actor: HashMap<u64, Vec<OpOrdinal>> = HashMap::new();

    // Actors in the simulation
    let actors = [1001u64, 1002, 2001];

    // Track branch tips: (OpId, ordinal) per active branch
    let mut branch_tips: Vec<(OpId, OpOrdinal)> = Vec::new();

    // ── ChainStart at the beginning ──
    let start_id = OpId::new(NodeId(0), 0, 0);
    let start_header = OpHeader {
        id: start_id,
        actor: 0,
        clock_value: 1000,
        clock_sub: 0,
        scope_discriminant: 0,
        scope_value: 0,
        tags: 0,
        kind_code: 0,
        stage_code: None,
        parent_count: 0,
        parent0: None,
        parent1: None,
        preview: Some("editchain v2".into()),
    };
    let start_ordinal = headers.len() as OpOrdinal;
    headers.push(start_header);
    by_id.insert(start_id, start_ordinal);
    parents.push(Vec::new());
    children.push(Vec::new());
    by_kind.entry(0).or_default().push(start_ordinal);
    branch_tips.push((start_id, start_ordinal));

    // ── Operation kind distribution ──
    // (kind_code, weight)
    let kind_weights: &[(u8, u64)] = &[
        (2, 35),  // Message
        (3, 20),  // Tool
        (4, 15),  // Command
        (5, 15),  // File
        (6, 5),   // Reflection
        (8, 5),   // Note
        (9, 3),   // Error
        (1, 2),   // Actor
    ];

    // Tag bits per kind
    let kind_tags: &[(u8, u64)] = &[
        (2, Tags::MESSAGE.0 | Tags::AGENT.0),
        (3, Tags::TOOL.0 | Tags::AGENT.0),
        (4, Tags::COMMAND.0 | Tags::AGENT.0),
        (5, Tags::FILE.0),
        (6, Tags::REFLECTION.0 | Tags::AGENT.0),
        (8, Tags::NOTE.0 | Tags::HUMAN.0),
        (9, Tags::ERROR.0),
        (1, Tags::AGENT.0),
    ];

    // Preview templates per kind
    let previews: &[(u8, &[&str])] = &[
        (2, &[
            "Investigating the issue with file parsing...",
            "Found the root cause of the bug in the parser.",
            "Applying the fix to handle edge cases.",
            "Let me check the test suite for regressions.",
            "The error occurs when input exceeds buffer size.",
            "Refactoring the module to improve performance.",
            "Adding documentation for the new API surface.",
            "Reviewing the pull request changes.",
        ]),
        (3, &[
            "Read(src/parser.rs)",
            "Write(src/parser.rs)",
            "Bash(cargo test)",
            "Read(src/lib.rs)",
            "Bash(cargo build --release)",
            "Write(tests/integration.rs)",
            "Read(Cargo.toml)",
            "Bash(git diff)",
        ]),
        (4, &[
            "cargo test --lib",
            "cargo clippy --all-targets",
            "rustfmt src/main.rs",
            "cargo build 2>&1",
            "git status",
            "cargo test --test integration",
            "cat src/parser.rs | head -50",
            "cargo doc --no-deps",
        ]),
        (5, &[
            "src/parser.rs +12 -5",
            "src/lib.rs +42 -10",
            "tests/integration.rs +80 -0",
            "Cargo.toml +1 -1",
            "src/types.rs +25 -8",
            "README.md +15 -3",
            ".gitignore +2 -0",
            "src/main.rs +5 -5",
        ]),
        (6, &[
            "Covered operations up to seq=42 in session A.",
            "Window [100..200]: summarized tool calls.",
            "Frontier: node=1 boot=0 max_seq=85.",
            "Anchors: OpId(1:0:42), PathId(5).",
        ]),
        (8, &[
            "Supersedes previous analysis with corrected data.",
            "Explains rationale for the file restructuring.",
            "Corrects the earlier statement about buffer sizes.",
        ]),
        (9, &[
            "EC001: parent not found in OpSet",
            "EC002: duplicate operation id",
            "EC003: payload decode failure",
        ]),
        (1, &["Agent registered.", "Role updated."]),
    ];

    // Build weighted kind list
    let mut weighted_kinds: Vec<u8> = Vec::new();
    for &(kind, weight) in kind_weights {
        for _ in 0..weight {
            weighted_kinds.push(kind);
        }
    }

    // Simple PRNG for deterministic output
    let mut rng_state = 42u64;
    let mut rng = || -> u64 {
        rng_state = rng_state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        rng_state >> 33
    };

    let mut seq_counter = 1u64;
    let mut clock_base = 1000u64;

    for i in 0..count.saturating_sub(1) {
        clock_base += rng() % 50 + 1;

        // Pick a kind
        let kind_code = weighted_kinds[(rng() as usize) % weighted_kinds.len()];

        // Pick an actor
        let actor = actors[(rng() as usize) % actors.len()];

        // ── Determine parents ──
        // Decide whether to create a merge (two parents) or single-parent op.
        let is_merge = branch_tips.len() >= 2 && rng() % 100 < 8;

        if is_merge {
            // Merge two branch tips
            let idx_a = 0;
            let idx_b = ((rng() as usize) % (branch_tips.len() - 1)) + 1;
            let (id_a, ord_a) = branch_tips[idx_a];
            let (id_b, ord_b) = branch_tips[idx_b];

            let seq = seq_counter;
            seq_counter += 1;
            let node_id = NodeId((rng() % 3) + 1);
            let op_id = OpId::new(node_id, 0, seq);

            let tags = kind_tags.iter().find(|(k, _)| *k == kind_code).map(|(_, t)| *t).unwrap_or(0);
            let preview_text = previews.iter()
                .find(|(k, _)| *k == kind_code)
                .and_then(|(_, p)| p.get(rng() as usize % p.len()))
                .unwrap_or(&"");
            let preview = Some(Box::from(*preview_text));

            let header = OpHeader {
                id: op_id,
                actor,
                clock_value: clock_base,
                clock_sub: (rng() % 65536) as u16,
                scope_discriminant: if kind_code == 5 { 4 } else { (rng() % 4) as u8 },
                scope_value: rng() % 10000,
                tags,
                kind_code,
                stage_code: match kind_code {
                    3 => Some((rng() % 3) as u8),
                    4 => Some((rng() % 3) as u8),
                    _ => None,
                },
                parent_count: 2,
                parent0: Some(id_a),
                parent1: Some(id_b),
                preview,
            };

            let ordinal = headers.len() as OpOrdinal;
            headers.push(header);
            by_id.insert(op_id, ordinal);
            parents.push(vec![ord_a, ord_b]);
            children.push(Vec::new());
            children[ord_a as usize].push(ordinal);
            children[ord_b as usize].push(ordinal);
            by_kind.entry(kind_code).or_default().push(ordinal);
            by_actor.entry(actor).or_default().push(ordinal);

            // Replace both merged tips with the merge result
            if idx_a < idx_b {
                branch_tips.remove(idx_b);
                branch_tips[idx_a] = (op_id, ordinal);
            } else {
                branch_tips.remove(idx_a);
                branch_tips[idx_b] = (op_id, ordinal);
            }
            continue;
        }

        // Single parent — pick from a branch tip
        let tip_idx = if i < branch_tips.len() { i } else { rng() as usize % branch_tips.len() };
        let tip_idx = tip_idx.min(branch_tips.len().saturating_sub(1));
        let (parent_id, parent_ordinal) = branch_tips[tip_idx];

        let seq = seq_counter;
        seq_counter += 1;
        let node_id = NodeId((rng() % 3) + 1);
        let op_id = OpId::new(node_id, 0, seq);

        let tags = kind_tags.iter().find(|(k, _)| *k == kind_code).map(|(_, t)| *t).unwrap_or(0);
        let preview_text = previews.iter()
            .find(|(k, _)| *k == kind_code)
            .and_then(|(_, p)| p.get(rng() as usize % p.len()))
            .unwrap_or(&"");
        let preview = Some(Box::from(*preview_text));

        let header = OpHeader {
            id: op_id,
            actor,
            clock_value: clock_base,
            clock_sub: (rng() % 65536) as u16,
            scope_discriminant: if kind_code == 5 { 4 } else { (rng() % 4) as u8 },
            scope_value: rng() % 10000,
            tags,
            kind_code,
            stage_code: match kind_code {
                3 => Some((rng() % 3) as u8),
                4 => Some((rng() % 3) as u8),
                _ => None,
            },
            parent_count: if parent_id == start_id { 0 } else { 1 },
            parent0: if parent_id == start_id { None } else { Some(parent_id) },
            parent1: None,
            preview,
        };

        let ordinal = headers.len() as OpOrdinal;
        headers.push(header);
        by_id.insert(op_id, ordinal);
        parents.push(vec![parent_ordinal]);
        children.push(Vec::new());
        children[parent_ordinal as usize].push(ordinal);
        by_kind.entry(kind_code).or_default().push(ordinal);
        by_actor.entry(actor).or_default().push(ordinal);

        // Update or add branch tip
        if i < branch_tips.len() {
            branch_tips[i] = (op_id, ordinal);
        } else if branch_tips.len() < 5 && rng() % 100 < 15 {
            branch_tips.push((op_id, ordinal));
        } else {
            branch_tips[tip_idx] = (op_id, ordinal);
        }
    }

    // Statistics
    let mut statistics = ChainStatistics {
        total_ops: headers.len(),
        total_segments: 1,
        total_bytes: 0,
        by_kind: HashMap::new(),
    };
    for (&kind, ordinals) in &by_kind {
        statistics.by_kind.insert(kind, ordinals.len());
    }

    TuiSnapshot {
        headers,
        by_id,
        parents,
        children,
        by_kind,
        by_actor,
        statistics,
    }
}