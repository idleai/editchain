//! Portable capture of Codex's out-of-band session titles.
//!
//! Codex stores user-visible thread renames in `session_index.jsonl`, beside
//! the rollout tree. The rollout itself does not contain that title. Importing
//! a small derived metadata op keeps the chosen title with the `EditChain` and
//! makes later rendering independent of the live Codex home directory.

use std::collections::HashMap;
use std::io::BufRead;
use std::path::{Path, PathBuf};

use editchain_core::clock::Clock;
use editchain_core::op::{ImportOp, OpKind};
use editchain_core::parents::ParentSet;
use editchain_core::scope::ScopeRef;
use editchain_core::tags::Tags;
use editchain_core::{Op, OpId, SessionId};
use serde_json::Value;

use crate::claude_code::normalize::parse_source_time;
use crate::error::ImportError;
use crate::ids::{derive_actor_id, derive_external_entity_id, hash_raw};
use crate::sink::{payload_for, BlobSink};

/// Latest provider-owned title record for one Codex thread.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CodexSessionTitle {
    pub(crate) title: String,
    pub(crate) updated_at: Option<String>,
    pub(crate) source_hash: [u8; 32],
}

/// Thread and optional parent identity read from a rollout's raw session meta.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RawSessionIdentity {
    pub(crate) thread_id: String,
    pub(crate) parent_thread_id: Option<String>,
}

/// Load the last valid title record for every thread in Codex's append-only
/// session index. Missing indexes are normal (for example a rollout-only
/// archive) and produce an empty map.
pub(crate) fn load_session_titles(
    raw_root: &Path,
) -> Result<HashMap<String, CodexSessionTitle>, ImportError> {
    let Some(path) = session_index_path(raw_root) else {
        return Ok(HashMap::new());
    };
    let file = std::fs::File::open(path).map_err(ImportError::Io)?;
    let mut titles = HashMap::new();
    let mut reader = std::io::BufReader::new(file);
    let mut line = Vec::new();
    loop {
        line.clear();
        if reader
            .read_until(b'\n', &mut line)
            .map_err(ImportError::Io)?
            == 0
        {
            break;
        }
        let parse_bytes = line.strip_suffix(b"\n").unwrap_or(&line);
        let parse_bytes = parse_bytes.strip_suffix(b"\r").unwrap_or(parse_bytes);
        let Ok(value) = serde_json::from_slice::<Value>(parse_bytes) else {
            continue;
        };
        let Some(thread_id) = display_field(&value, "id") else {
            continue;
        };
        let Some(title) = display_field(&value, "thread_name") else {
            continue;
        };
        // Codex treats the physical last matching entry as authoritative.
        // Preserve that exact append-only behavior even when a timestamp is
        // absent or malformed.
        drop(titles.insert(
            thread_id,
            CodexSessionTitle {
                title,
                updated_at: display_field(&value, "updated_at"),
                source_hash: hash_raw(&line),
            },
        ));
    }
    Ok(titles)
}

/// Read only the exact session-identity fields needed to notice an out-of-band
/// title update before deciding that an unchanged rollout can be skipped.
pub(crate) fn raw_session_identity(
    rollout: &Path,
) -> Result<Option<RawSessionIdentity>, ImportError> {
    let file = std::fs::File::open(rollout).map_err(ImportError::Io)?;
    let mut reader = std::io::BufReader::new(file);
    let mut line = Vec::new();
    loop {
        line.clear();
        if reader
            .read_until(b'\n', &mut line)
            .map_err(ImportError::Io)?
            == 0
        {
            return Ok(None);
        }
        let Ok(value) = serde_json::from_slice::<Value>(&line) else {
            continue;
        };
        if value.get("type").and_then(Value::as_str) != Some("session_meta") {
            continue;
        }
        let Some(payload) = value.get("payload") else {
            return Ok(None);
        };
        let Some(thread_id) = display_field(payload, "id") else {
            return Ok(None);
        };
        let parent_thread_id = display_field(payload, "parent_thread_id")
            .or_else(|| display_field(payload, "parentThreadId"));
        return Ok(Some(RawSessionIdentity {
            thread_id,
            parent_thread_id,
        }));
    }
}

/// Build one deterministic metadata op carrying a title into the imported
/// session scope. `source_hash` and the raw-session parent are part of the id,
/// so a rename or rewritten rollout produces a new immutable fact.
pub(crate) fn session_title_op(
    title: &CodexSessionTitle,
    owning_thread: &str,
    session_id: SessionId,
    first_raw: OpId,
    blobs: &mut dyn BlobSink,
) -> Result<Op, ImportError> {
    let hash = blake3::Hash::from_bytes(title.source_hash).to_hex();
    let identity = format!("{owning_thread}:{first_raw}:{hash}");
    let value = serde_json::json!({
        "type": "session_title",
        "provider": "codex",
        "title": title.title,
        "updated_at": title.updated_at,
    });
    let encoded = serde_json::to_vec(&value).map_err(ImportError::Json)?;
    Ok(Op {
        id: derive_external_entity_id("codex:session-title:v1", &identity),
        parents: ParentSet::One(first_raw),
        actor: derive_actor_id(&format!("system:{owning_thread}")),
        clock: Clock::UnixMs(
            title
                .updated_at
                .as_deref()
                .and_then(parse_source_time)
                .unwrap_or(0),
        ),
        scope: ScopeRef::Session(session_id),
        tags: Tags::IMPORT | Tags::META,
        kind: OpKind::Import(ImportOp {
            raw_ref: payload_for(&encoded, blobs)?,
            raw_hash: Some(title.source_hash),
        }),
    })
}

fn display_field(value: &Value, name: &str) -> Option<String> {
    value
        .get(name)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
}

fn session_index_path(raw_root: &Path) -> Option<PathBuf> {
    let direct = raw_root.join("session_index.jsonl");
    if direct.is_file() {
        return Some(direct);
    }
    if raw_root.file_name().is_some_and(|name| name == "sessions") {
        let sibling = raw_root.parent()?.join("session_index.jsonl");
        if sibling.is_file() {
            return Some(sibling);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn last_session_index_entry_wins() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            dir.path().join("session_index.jsonl"),
            concat!(
                "{\"id\":\"thread-1\",\"thread_name\":\"First\",\"updated_at\":\"2026-01-01T00:00:00Z\"}\n",
                "not json\n",
                "{\"id\":\"thread-1\",\"thread_name\":\"Final\",\"updated_at\":\"2026-01-02T00:00:00Z\"}\n",
            ),
        )
        .expect("write index");
        let titles = load_session_titles(dir.path()).expect("load titles");
        assert_eq!(
            titles.get("thread-1").map(|title| title.title.as_str()),
            Some("Final")
        );
    }

    #[test]
    fn sessions_root_finds_sibling_index() {
        let dir = tempfile::tempdir().expect("tempdir");
        let sessions = dir.path().join("sessions");
        std::fs::create_dir(&sessions).expect("create sessions");
        std::fs::write(
            dir.path().join("session_index.jsonl"),
            "{\"id\":\"thread-1\",\"thread_name\":\"r8\"}\n",
        )
        .expect("write index");
        let titles = load_session_titles(&sessions).expect("load titles");
        assert_eq!(
            titles.get("thread-1").map(|title| title.title.as_str()),
            Some("r8")
        );
    }
}
