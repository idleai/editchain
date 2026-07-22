use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::Path;

use editchain_codec::frame::decode_op;
use editchain_codec::page::decode_page;
use editchain_core::{CommandStage, Op, OpId, OpKind, Payload, ScopeRef, ToolStage};

use crate::data::header::{OpHeader, OpOrdinal};
use crate::data::snapshot::{ChainStatistics, TuiSnapshot};

/// Load a chain from disk into a `TuiSnapshot`.
///
/// Reads all `.eclog` segment files in the given directory,
/// decodes each record into an Op, and builds compact headers + indexes.
#[expect(
    clippy::arithmetic_side_effects,
    clippy::as_conversions,
    clippy::cast_possible_truncation,
    clippy::indexing_slicing,
    clippy::manual_let_else,
    clippy::print_stderr,
    reason = "TUI chain loader; arithmetic bounded by file sizes; let-else is clearer for early continue"
)]
pub(crate) fn load_chain(path: &Path) -> io::Result<TuiSnapshot> {
    if !path.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("chain directory not found: {}", path.display()),
        ));
    }

    let mut headers = Vec::new();
    let mut by_id = HashMap::new();
    let mut parents: Vec<Vec<OpOrdinal>> = Vec::new();
    let mut children: Vec<Vec<OpOrdinal>> = Vec::new();
    let mut by_kind: HashMap<u8, Vec<OpOrdinal>> = HashMap::new();
    let mut by_actor: HashMap<u64, Vec<OpOrdinal>> = HashMap::new();

    // Discover segment files sorted by sequence number
    let mut segments: Vec<(u32, std::path::PathBuf)> = Vec::new();
    for entry in fs::read_dir(path)? {
        let entry = entry?;
        let name = entry.file_name();
        let name_str = match name.to_str() {
            Some(s) => s,
            None => continue,
        };
        if let Some(seq_str) = name_str.strip_suffix(".eclog") {
            if let Ok(seq) = seq_str.parse::<u32>() {
                segments.push((seq, entry.path()));
            }
        }
    }
    segments.sort_by_key(|(seq, _)| *seq);

    let mut total_bytes = 0u64;
    let total_segments = segments.len();

    for (_seq, seg_path) in &segments {
        let bytes = fs::read(seg_path)?;
        total_bytes += bytes.len() as u64;

        // A segment file may contain multiple concatenated pages.
        let mut offset = 0;
        while offset < bytes.len() {
            if let Some(page) = decode_page(&bytes[offset..]) {
                let encoded_len = encoded_page_len(&bytes[offset..]);
                for record in &page.records {
                    match decode_op(&record.data) {
                        Ok(op) => {
                            let ordinal = headers.len() as OpOrdinal;

                            // Build OpHeader
                            let kind_code = op_kind_code(&op.kind);
                            let stage_code = op_stage_code(&op.kind);
                            let parent_ids: Vec<OpId> = op.parents.iter().copied().collect();
                            let parent_count = parent_ids.len() as u8;

                            // Preview text from message content or tool name
                            let preview = extract_preview(&op);

                            // Resolve parent ordinals (may be missing if parent is in a later segment)
                            let mut parent_ords = Vec::new();
                            for pid in &parent_ids {
                                if let Some(&pord) = by_id.get(pid) {
                                    parent_ords.push(pord);
                                }
                            }

                            let header = OpHeader {
                                id: op.id,
                                actor: op.actor.0,
                                clock_value: op.clock.as_u64(),
                                clock_sub: op.clock.sub(),
                                scope_discriminant: op.scope.discriminant(),
                                scope_value: match &op.scope {
                                    ScopeRef::None => 0,
                                    ScopeRef::Chain(id) => id.0,
                                    ScopeRef::Session(id) => id.0,
                                    ScopeRef::Turn(id) => id.0,
                                    ScopeRef::File(id) => id.0,
                                },
                                tags: op.tags.0,
                                kind_code,
                                stage_code,
                                parent_count,
                                parent0: parent_ids.first().copied(),
                                parent1: parent_ids.get(1).copied(),
                                preview,
                            };

                            headers.push(header);
                            let _: Option<OpOrdinal> = by_id.insert(op.id, ordinal);
                            parents.push(parent_ords.clone());
                            children.push(Vec::new());

                            // Register as child of each resolved parent
                            for &pord in &parent_ords {
                                if (pord as usize) < children.len() {
                                    children[pord as usize].push(ordinal);
                                }
                            }

                            by_kind.entry(kind_code).or_default().push(ordinal);
                            by_actor.entry(op.actor.0).or_default().push(ordinal);
                        }
                        Err(e) => {
                            // Corrupt record — skip with warning (future: diagnostics popup)
                            eprintln!("Warning: failed to decode record at offset {offset}: {e}");
                        }
                    }
                }
                offset += encoded_len;
            } else {
                break; // partial trailing page (power-loss tolerance)
            }
        }
    }

    // Build statistics
    let mut statistics = ChainStatistics {
        total_ops: headers.len(),
        total_segments,
        total_bytes,
        by_kind: HashMap::new(),
    };
    for (&kind, ordinals) in &by_kind {
        let _: Option<usize> = statistics.by_kind.insert(kind, ordinals.len());
    }

    Ok(TuiSnapshot {
        headers,
        by_id,
        parents,
        children,
        by_kind,
        by_actor,
        statistics,
    })
}

/// Compute the encoded length of a page from its bytes.
#[expect(
    clippy::arithmetic_side_effects,
    clippy::as_conversions,
    clippy::indexing_slicing,
    reason = "Binary page parsing; offsets are bounded by buffer length checks"
)]
fn encoded_page_len(bytes: &[u8]) -> usize {
    if bytes.len() < 8 {
        return bytes.len();
    }
    let mut offset = 8;
    while offset + 4 <= bytes.len() {
        let len =
            u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap_or([0; 4])) as usize;
        offset += 4;
        if offset + 1 + len > bytes.len() {
            break;
        }
        offset += 1 + len;
    }
    offset
}

/// Map an `OpKind` to a numeric code.
const fn op_kind_code(kind: &OpKind) -> u8 {
    match kind {
        OpKind::ChainStart(_) => 0,
        OpKind::Actor(_) => 1,
        OpKind::Message(_) => 2,
        OpKind::Tool(_) => 3,
        OpKind::Command(_) => 4,
        OpKind::File(_) => 5,
        OpKind::Reflection(_) => 6,
        OpKind::Import(_) => 7,
        OpKind::Note(_) => 8,
        OpKind::Error(_) => 9,
        OpKind::Unknown(_) => 10,
    }
}

/// Extract stage code from an `OpKind`.
const fn op_stage_code(kind: &OpKind) -> Option<u8> {
    match kind {
        OpKind::Tool(t) => Some(match t.stage {
            ToolStage::Start => 0,
            ToolStage::Delta => 1,
            ToolStage::Finish => 2,
        }),
        OpKind::Command(c) => Some(match c.stage {
            CommandStage::Start => 0,
            CommandStage::Output => 1,
            CommandStage::Finish => 2,
        }),
        OpKind::ChainStart(_)
        | OpKind::Actor(_)
        | OpKind::Message(_)
        | OpKind::File(_)
        | OpKind::Reflection(_)
        | OpKind::Import(_)
        | OpKind::Note(_)
        | OpKind::Error(_)
        | OpKind::Unknown(_) => None,
    }
}

/// Extract a short preview string from an operation.
#[expect(
    clippy::string_slice,
    reason = "Preview truncation at byte boundary is acceptable for display"
)]
fn extract_preview(op: &Op) -> Option<Box<str>> {
    let text = match &op.kind {
        OpKind::Message(m) => match &m.content {
            Payload::Inline(b) => Some(bytes_to_preview(b)),
            Payload::Empty | Payload::Blob(_) => None,
        },
        OpKind::Tool(t) => match &t.tool_name {
            Payload::Inline(b) => Some(bytes_to_preview(b)),
            Payload::Empty | Payload::Blob(_) => None,
        },
        OpKind::Command(c) => match &c.content {
            Payload::Inline(b) => Some(bytes_to_preview(b)),
            Payload::Empty | Payload::Blob(_) => None,
        },
        OpKind::File(f) => Some(format!("{}", f.path.0).into_boxed_str()),
        OpKind::Reflection(r) => match &r.summary {
            Payload::Inline(b) => Some(bytes_to_preview(b)),
            Payload::Empty | Payload::Blob(_) => None,
        },
        OpKind::Note(n) => match &n.content {
            Payload::Inline(b) => Some(bytes_to_preview(b)),
            Payload::Empty | Payload::Blob(_) => None,
        },
        OpKind::Error(e) => match &e.message {
            Payload::Inline(b) => Some(bytes_to_preview(b)),
            Payload::Empty | Payload::Blob(_) => None,
        },
        OpKind::ChainStart(cs) => Some(bytes_to_preview(&cs.name)),
        OpKind::Actor(a) => match &a.label {
            Payload::Inline(b) => Some(bytes_to_preview(b)),
            Payload::Empty | Payload::Blob(_) => None,
        },
        OpKind::Import(i) => match &i.raw_ref {
            Payload::Inline(b) => Some(bytes_to_preview(b)),
            Payload::Empty | Payload::Blob(_) => None,
        },
        OpKind::Unknown(u) => {
            Some(format!("unknown kind={}", u.kind_discriminant).into_boxed_str())
        }
    };
    text.map(|s| {
        if s.len() > 80 {
            s[..80].to_string().into_boxed_str()
        } else {
            s
        }
    })
}

/// Convert bytes to a preview string, handling non-UTF-8 gracefully.
fn bytes_to_preview(bytes: &[u8]) -> Box<str> {
    String::from_utf8_lossy(bytes)
        .lines()
        .next()
        .unwrap_or("")
        .to_string()
        .into_boxed_str()
}
