//! Claude Code session profiler — analyze corpus structure.
//!
//! Usage: cargo run --bin cc_profile -- <sessions_dir>

use std::collections::{HashMap, HashSet};
use std::path::Path;

use editchain_import::claude_code::discover::discover_sessions;
use editchain_import::claude_code::envelope::parse_envelope;
use editchain_import::claude_code::reader::read_session_file;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 {
        eprintln!("Usage: {} <sessions_dir>", args[0]);
        std::process::exit(1);
    }

    let dir = Path::new(&args[1]);
    let sessions = match discover_sessions(dir) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("Error discovering sessions: {}", e);
            std::process::exit(1);
        }
    };

    println!("=== Claude Code Session Profile ===");
    println!("Directory: {}", dir.display());
    println!("Total files: {}", sessions.len());
    println!();

    let mut total_lines = 0;
    let mut total_bytes = 0;
    let mut type_counts: HashMap<String, usize> = HashMap::new();
    let mut subtype_counts: HashMap<String, usize> = HashMap::new();
    let mut session_ids: HashSet<String> = HashSet::new();
    let mut max_line_len: usize = 0;
    let mut malformed_lines = 0;
    let mut uuid_overlap: HashMap<String, usize> = HashMap::new();

    for session in &sessions {
        println!("  {} ({} bytes, subagent: {})",
            session.session_id,
            session.file_size,
            session.is_subagent
        );

        let (lines, bytes_read, _cursor) = match read_session_file(&session.path, None) {
            Ok(v) => v,
            Err(e) => {
                eprintln!("    Error: {}", e);
                continue;
            }
        };

        total_lines += lines.len();
        total_bytes += bytes_read;

        for line in &lines {
            if line.data.len() > max_line_len {
                max_line_len = line.data.len();
            }

            match parse_envelope(&line.data) {
                Some(env) => {
                    *type_counts.entry(env.record_type.clone()).or_insert(0) += 1;
                    if !env.subtype.is_empty() {
                        *subtype_counts.entry(env.subtype.clone()).or_insert(0) += 1;
                    }
                    if !env.session_id.is_empty() {
                        session_ids.insert(env.session_id.clone());
                    }
                    if !env.uuid.is_empty() {
                        *uuid_overlap.entry(env.uuid.clone()).or_insert(0) += 1;
                    }
                }
                None => {
                    malformed_lines += 1;
                }
            }
        }
    }

    println!();
    println!("=== Summary ===");
    println!("Total lines: {}", total_lines);
    println!("Total bytes: {}", total_bytes);
    println!("Max line length: {} bytes", max_line_len);
    println!("Malformed lines: {}", malformed_lines);
    println!("Unique session IDs: {}", session_ids.len());
    println!();

    println!("=== Record Types ===");
    let mut types: Vec<_> = type_counts.into_iter().collect();
    types.sort_by_key(|(_, c)| std::cmp::Reverse(*c));
    for (t, c) in &types {
        println!("  {:30}: {}", t, c);
    }
    println!();

    println!("=== Subtypes ===");
    let mut subtypes: Vec<_> = subtype_counts.into_iter().collect();
    subtypes.sort_by_key(|(_, c)| std::cmp::Reverse(*c));
    for (t, c) in &subtypes {
        println!("  {:30}: {}", t, c);
    }
    println!();

    // UUID overlap analysis
    let overlaps: Vec<_> = uuid_overlap.iter().filter(|(_, c)| **c > 1).collect();
    if !overlaps.is_empty() {
        println!("=== UUID Overlaps ({} total) ===", overlaps.len());
        for (uuid, count) in &overlaps {
            println!("  {} appears {} times", uuid, count);
        }
    } else {
        println!("No UUID overlaps detected across files.");
    }
}
