use std::collections::HashMap;

use serde_json::Value;

/// The `editchain-v1` projection schema version required by this importer.
pub const SCHEMA_VERSION: &str = "editchain-v1";

/// Source-neutral semantic kind of a projected item.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProjectionKind {
    /// A message (user or assistant).
    Message,
    /// A tool call lifecycle op.
    Tool,
    /// A shell command lifecycle op.
    Command,
    /// A file observation or change.
    File,
    /// An agent context reflection (reasoning summary, plan, compaction).
    Reflection,
    /// A relationship/content note (e.g. subagent activity).
    Note,
    /// Unknown kind (forward-compatible; raw-only lane).
    Unknown,
}

impl ProjectionKind {
    /// Map a bridge item `kind` tag to a neutral kind. Unrecognized tags are
    /// `Unknown` (non-fatal, forward compatible).
    #[must_use]
    pub fn parse(s: &str) -> Self {
        match s {
            "userMessage" | "agentMessage" => Self::Message,
            "toolCall" | "collabToolCall" => Self::Tool,
            "commandExecution" => Self::Command,
            "fileChange" | "imageView" => Self::File,
            "reasoning" | "plan" | "contextCompaction" => Self::Reflection,
            "subAgentActivity" => Self::Note,
            _ => Self::Unknown,
        }
    }
}

/// A projected item from one `changedItems` entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectionItem {
    /// Stable logical-item identity within the physical file.
    pub item_id: String,
    /// Owning turn id (bridge identity lane; removal is turn-scoped).
    pub turn_id: String,
    /// Neutral semantic kind of the item.
    pub kind: ProjectionKind,
    /// Source-neutral actor label derived from the item kind.
    pub actor: String,
    /// The bridge item projection object as received (camelCase fields).
    pub payload: Value,
}

/// One parsed `line` record from the bridge.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectionRecord {
    /// 1-based physical line ordinal the record describes.
    pub source_ordinal: u64,
    /// Whether the bridge reported a decode error for this line.
    pub decode_error: bool,
    /// Bridge metadata: owning thread id from `projection.sessionMeta.threadId`.
    pub thread_id: Option<String>,
    /// Bridge session metadata carried by this line, when present.
    pub session_meta: Option<SessionMeta>,
    /// Upsert lane: changed items on this line.
    pub changed_items: Vec<ProjectionItem>,
    /// Turn lifecycle metadata reported on this line.
    pub changed_turns: Vec<TurnMeta>,
    /// Remove lane: turn ids rolled back on this line.
    pub removed_turn_ids: Vec<String>,
    /// Non-fatal item-level problems dropped from `changed_items` on this
    /// line (the line still counts toward the record count; the raw lane
    /// preserves the physical line byte-exact).
    pub malformed_items: usize,
    /// Inter-agent communication content carried by this line, when present.
    pub inter_agent: Option<InterAgentLine>,
    /// Context-compaction summary carried by this line, when present.
    pub compacted: Option<CompactedLine>,
}

/// A folded final logical item for one physical file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FinalItem {
    /// Stable logical-item identity within the physical file.
    pub item_id: String,
    /// Owning turn id (bridge identity lane; removal is turn-scoped).
    pub turn_id: String,
    /// First-seen physical line ordinal (stable anchor, never updated).
    pub first_seen: u64,
    /// Last upsert physical line ordinal for this item (>= `first_seen`).
    /// Incremental imports anchor deterministic update ops here when an item
    /// first seen before the cursor is changed after it.
    pub last_seen: u64,
    /// Final semantic kind of the item.
    pub kind: ProjectionKind,
    /// Final source-neutral actor label of the item.
    pub actor: String,
    /// Final item projection object (bridge camelCase fields).
    pub payload: Value,
}

/// Source-neutral inter-agent message content on one physical line.
///
/// The author/recipient labels are diagnostic only — scope and identity are
/// never derived from them (the owning thread id rules in [`super`] govern).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InterAgentLine {
    /// Physical line ordinal the content was carried on.
    pub source_ordinal: u64,
    /// Newline-joined plaintext content.
    pub content: String,
    /// Author label (diagnostic only).
    pub author: Option<String>,
    /// Recipient label (diagnostic only).
    pub recipient: Option<String>,
}

/// Source-neutral context-compaction summary on one physical line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompactedLine {
    /// Physical line ordinal the summary was carried on.
    pub source_ordinal: u64,
    /// Compaction summary message text.
    pub message: String,
}

/// Provider-neutral turn metadata from one `changedTurns` entry.
///
/// Turn identity is the bridge's stable per-thread turn id (`turnId`); the
/// status/timing fields are the bridge's normalized view of the source turn
/// lifecycle. The importer persists the identity on normalized ops (via
/// [`editchain_core::ScopeRef::Turn`]) and records this metadata explicitly so
/// it is never transient.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TurnMeta {
    /// Stable turn id within the owning thread (bridge identity lane).
    pub turn_id: String,
    /// Bridge-normalized turn status (e.g. `completed`, `inProgress`).
    pub status: Option<String>,
    /// Turn error message, when the bridge reported one.
    pub error_message: Option<String>,
    /// Turn start time (Unix seconds), when the bridge reported one.
    pub started_at: Option<i64>,
    /// Turn completion time (Unix seconds), when the bridge reported one.
    pub completed_at: Option<i64>,
    /// Turn duration in milliseconds, when the bridge reported one.
    pub duration_ms: Option<i64>,
}

/// Provider-neutral session metadata from the bridge `sessionMeta` object.
///
/// The owning thread id ([`Self::thread_id`]) is the session scope identity.
/// `parent_thread_id` / `forked_from_id` are explicit execution pointers. The
/// importer emits a visible `SpawnedBy` relation only with
/// an exact matching start occurrence and retains `forked_from_id` as a hidden
/// `ForkedFrom` fact. `agent_path` and source fields are
/// explicit provenance (never inferred by sniffing raw records).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionMeta {
    /// Owning thread identity (`session_meta.payload.id`).
    pub thread_id: String,
    /// Codex session id (payload `session_id`; never used for scope).
    pub session_id: Option<String>,
    /// Parent thread id — this thread is a subagent of that thread.
    pub parent_thread_id: Option<String>,
    /// Thread id this thread was forked from.
    pub forked_from_id: Option<String>,
    /// Subagent display name, when present.
    pub agent_nickname: Option<String>,
    /// Subagent spawn path (identity/provenance).
    pub agent_path: Option<String>,
    /// Bridge `threadSource` passthrough (provenance only).
    pub thread_source: Option<Value>,
    /// Bridge `source` passthrough (provenance only; may be a string or an
    /// object such as `{"subagent":{"thread_spawn":{...}}}`).
    pub source: Option<Value>,
    /// Originator label (e.g. `codex`, `test`) — provenance only.
    pub originator: Option<String>,
    /// Model provider label (e.g. `openai`) — provenance only.
    pub model_provider: Option<String>,
    /// Working directory recorded by the provider (projection `cwd`); used by
    /// the workspace project filter. Absent in projections from older bridges.
    pub cwd: Option<String>,
    /// Git state captured by Codex when the session started.
    pub git: Option<SessionGitMeta>,
}

/// Provider-neutral Git state captured at session start.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionGitMeta {
    /// Exact commit checked out when the session started.
    pub commit_hash: Option<String>,
    /// Branch name observed at session start (provenance only).
    pub branch: Option<String>,
    /// Repository remote URL observed at session start (provenance only).
    pub repository_url: Option<String>,
}

/// The folded result of one physical file's projection stream.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Projection {
    /// Physical source ordinals represented by `line` records, in strictly
    /// increasing order. The importer uses this to require every newly read
    /// non-blank line even when an incremental cursor makes the old blank-line
    /// count unavailable.
    pub line_ordinals: Vec<u64>,
    /// Final logical items, ordered by `(first_seen, item_id, turn_id)`.
    pub final_items: Vec<FinalItem>,
    /// Inter-agent communication lines in physical order (deterministic
    /// per-line note lanes).
    pub inter_agent_lines: Vec<InterAgentLine>,
    /// Context-compaction summary lines in physical order (deterministic
    /// per-line reflection lanes).
    pub compacted_lines: Vec<CompactedLine>,
    /// First non-empty bridge owning-thread metadata (`sessionMeta.threadId`
    /// or the `final` record's `threadId`).
    pub owning_thread: Option<String>,
    /// First bridge `sessionMeta` object (structural/provenance metadata).
    pub session_meta: Option<SessionMeta>,
    /// Physical source ordinal carrying [`Self::session_meta`].
    pub session_meta_source_ordinal: Option<u64>,
    /// Final per-turn metadata, first-appearance order, merged by turn id
    /// (the last reported status/timing wins, mirroring item upserts).
    pub turns: Vec<TurnMeta>,
    /// Number of non-fatal problems: bridge decode-error lines and item-level
    /// malformed records (the raw lane preserves those lines byte-exact).
    pub malformed: usize,
}

/// Errors from parsing a helper's projection output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProjectionError {
    /// Schema-level violation (fatal for the file; cursor not advanced).
    Protocol(String),
}

impl std::fmt::Display for ProjectionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Protocol(detail) => write!(f, "{detail}"),
        }
    }
}

/// Parse a complete helper stdout buffer into a folded [`Projection`].
///
/// The bridge emits exactly one `line` record per non-blank physical JSONL
/// line, in order, with `sourceOrdinal` equal to the 1-based physical line
/// number (blank lines produce gaps), followed by an optional single `final`
/// record whose `sourceOrdinal` is the EOF anchor (`expected_total`).
///
/// `expected_total` is the physical line count of the rollout file as the
/// bridge counts it (complete lines plus one for a trailing partial line).
/// `expected_records` is the exact number of `line` records expected (non-blank
/// complete lines plus a non-blank partial line) and is enforced only when
/// known — i.e. on full-file imports; incremental imports pass `None`.
///
/// # Errors
///
/// Returns [`ProjectionError::Protocol`] for schema violations: bad schema
/// version, invalid JSON, missing/out-of-range/out-of-sequence ordinals, more
/// than one `final` record, a `final` record before the end, or a line-record
/// count mismatch when `expected_records` is provided.
#[expect(
    clippy::arithmetic_side_effects,
    reason = "counter increments are bounded by the projection line count"
)]
pub fn parse_projection(
    stdout: &[u8],
    expected_total: u64,
    expected_records: Option<u64>,
) -> Result<Projection, ProjectionError> {
    let mut projection = Projection::default();
    let mut items: HashMap<(String, String), FinalItem> = HashMap::new();
    let mut last_ordinal: u64 = 0;
    let mut line_records: u64 = 0;
    let mut record_index: u64 = 0;
    let mut saw_final: bool = false;

    for line in stdout.split(|b| *b == b'\n') {
        if line.is_empty() {
            continue;
        }
        record_index += 1;
        let record = match parse_record(line, record_index, expected_total) {
            Ok(record) => record,
            Err(detail) => return Err(ProjectionError::Protocol(detail)),
        };
        match record {
            ParsedRecord::Line(record) => {
                if saw_final {
                    return Err(ProjectionError::Protocol(format!(
                        "record {record_index}: line record after final record"
                    )));
                }
                if record.source_ordinal <= last_ordinal {
                    return Err(ProjectionError::Protocol(format!(
                        "record {record_index}: ordinal {} out of sequence (last {last_ordinal})",
                        record.source_ordinal
                    )));
                }
                last_ordinal = record.source_ordinal;
                line_records += 1;
                projection.line_ordinals.push(record.source_ordinal);
                projection.malformed += record.malformed_items;
                if record.decode_error {
                    projection.malformed += 1;
                    continue;
                }
                if projection.owning_thread.is_none() {
                    projection.owning_thread.clone_from(&record.thread_id);
                }
                if projection.session_meta.is_none() {
                    projection.session_meta_source_ordinal =
                        record.session_meta.as_ref().map(|_| record.source_ordinal);
                    projection.session_meta.clone_from(&record.session_meta);
                }
                for turn in &record.changed_turns {
                    merge_turn(&mut projection.turns, turn);
                }
                for removed in &record.removed_turn_ids {
                    items.retain(|(turn_id, _), _item| turn_id != removed);
                }
                for change in record.changed_items {
                    let candidate = FinalItem {
                        item_id: change.item_id,
                        turn_id: change.turn_id,
                        first_seen: record.source_ordinal,
                        last_seen: record.source_ordinal,
                        kind: change.kind,
                        actor: change.actor,
                        payload: change.payload,
                    };
                    // Fold key order is (turn_id, item_id) so turn-scoped
                    // removals retain by the first tuple element.
                    let key = (candidate.turn_id.clone(), candidate.item_id.clone());
                    let _unused: &mut FinalItem = items
                        .entry(key)
                        .and_modify(|existing| {
                            existing.kind = candidate.kind;
                            existing.actor.clone_from(&candidate.actor);
                            existing.payload.clone_from(&candidate.payload);
                            existing.last_seen = candidate.first_seen;
                        })
                        .or_insert(candidate);
                }
                if let Some(inter_agent) = record.inter_agent {
                    projection.inter_agent_lines.push(inter_agent);
                }
                if let Some(compacted) = record.compacted {
                    projection.compacted_lines.push(compacted);
                }
            }
            ParsedRecord::Final(thread_id) => {
                if saw_final {
                    return Err(ProjectionError::Protocol(format!(
                        "record {record_index}: second final record"
                    )));
                }
                saw_final = true;
                if projection.owning_thread.is_none() {
                    projection.owning_thread = thread_id;
                }
            }
        }
    }

    if let Some(expected) = expected_records {
        if line_records != expected {
            return Err(ProjectionError::Protocol(format!(
                "expected {expected} line records for {expected} non-blank physical lines, got {line_records}"
            )));
        }
    }

    // Unknown item kinds are forward-compatible raw-only lanes: they never
    // materialize normalized ops and must not leak into `final_items`.
    items.retain(|_key, item| item.kind != ProjectionKind::Unknown);

    let mut final_items: Vec<FinalItem> = items.into_values().collect();
    final_items.sort_by(|a, b| {
        (a.first_seen, &a.item_id, &a.turn_id).cmp(&(b.first_seen, &b.item_id, &b.turn_id))
    });
    projection.final_items = final_items;
    Ok(projection)
}

/// One parsed bridge record before folding.
#[derive(Debug, Clone, PartialEq, Eq)]
enum ParsedRecord {
    /// A `line` record.
    Line(Box<ProjectionRecord>),
    /// A `final` record (only its owning-thread metadata is consumed).
    Final(Option<String>),
}

/// Parse and validate one bridge record.
fn parse_record(
    line: &[u8],
    record_index: u64,
    expected_total: u64,
) -> Result<ParsedRecord, String> {
    let value: Value = serde_json::from_slice(line)
        .map_err(|e| format!("record {record_index}: invalid JSON on stdout: {e}"))?;
    let obj = value
        .as_object()
        .ok_or_else(|| format!("record {record_index}: not a JSON object"))?;

    let version = obj
        .get("schemaVersion")
        .and_then(Value::as_str)
        .ok_or_else(|| format!("record {record_index}: missing schema version `schemaVersion`"))?;
    if version != SCHEMA_VERSION {
        return Err(format!(
            "record {record_index}: schema version {version:?} does not match {SCHEMA_VERSION:?}"
        ));
    }

    let record_type = obj
        .get("recordType")
        .and_then(Value::as_str)
        .ok_or_else(|| format!("record {record_index}: missing `recordType`"))?;
    let ordinal = obj
        .get("sourceOrdinal")
        .and_then(Value::as_u64)
        .ok_or_else(|| format!("record {record_index}: missing ordinal `sourceOrdinal`"))?;

    match record_type {
        "line" => parse_line_record(obj, record_index, ordinal, expected_total)
            .map(|record| ParsedRecord::Line(Box::new(record))),
        "final" => {
            parse_final_record(obj, record_index, ordinal, expected_total).map(ParsedRecord::Final)
        }
        other => Err(format!(
            "record {record_index}: unknown recordType {other:?}"
        )),
    }
}

/// Parse a `line` record: decode status, session metadata, and the change set.
#[expect(
    clippy::arithmetic_side_effects,
    reason = "malformed-entry counter is bounded by changedItems entries on a single line"
)]
fn parse_line_record(
    obj: &serde_json::Map<String, Value>,
    record_index: u64,
    ordinal: u64,
    expected_total: u64,
) -> Result<ProjectionRecord, String> {
    if ordinal == 0 || ordinal > expected_total {
        return Err(format!(
            "record {record_index}: ordinal {ordinal} out of range 1..={expected_total}"
        ));
    }

    let decode = obj
        .get("decode")
        .and_then(Value::as_object)
        .ok_or_else(|| format!("record {record_index}: missing `decode` object"))?;
    let status = decode
        .get("status")
        .and_then(Value::as_str)
        .ok_or_else(|| format!("record {record_index}: missing `decode.status`"))?;
    let decode_error = match status {
        "ok" => false,
        "error" => true,
        other => {
            return Err(format!(
                "record {record_index}: unknown decode.status {other:?}"
            ));
        }
    };

    let mut thread_id = None;
    let mut session_meta = None;
    let mut changed_items = Vec::new();
    let mut changed_turns = Vec::new();
    let mut removed_turn_ids = Vec::new();
    let mut malformed_items = 0;
    let mut inter_agent = None;
    let mut compacted = None;

    if let Some(projection) = obj.get("projection").and_then(Value::as_object) {
        if let Some(session_meta_obj) = projection.get("sessionMeta").and_then(Value::as_object) {
            thread_id = session_meta_obj
                .get("threadId")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .map(ToString::to_string);
            let parsed = parse_session_meta(session_meta_obj);
            if !parsed.thread_id.is_empty() || parsed.session_id.is_some() {
                session_meta = Some(parsed);
            }
        }
        if let Some(changes) = projection.get("changedItems").and_then(Value::as_array) {
            for change in changes {
                match parse_item_change(change) {
                    Some(item) => changed_items.push(item),
                    // Item-level problems are non-fatal: drop only the
                    // malformed entry, keep the line's record count intact,
                    // and carry the ordinal validation it already passed.
                    None => malformed_items += 1,
                }
            }
        }
        if let Some(turns) = projection.get("changedTurns").and_then(Value::as_array) {
            for turn in turns {
                if let Some(meta) = parse_turn_change(turn) {
                    changed_turns.push(meta);
                }
            }
        }
        if let Some(removed) = projection.get("removedTurnIds").and_then(Value::as_array) {
            for id in removed {
                if let Some(id) = id.as_str() {
                    if !id.is_empty() {
                        removed_turn_ids.push(id.to_string());
                    }
                }
            }
        }
        inter_agent = projection
            .get("interAgent")
            .and_then(Value::as_object)
            .map(|ia| InterAgentLine {
                source_ordinal: ordinal,
                content: ia
                    .get("content")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
                author: ia
                    .get("author")
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
                    .map(ToString::to_string),
                recipient: ia
                    .get("recipient")
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
                    .map(ToString::to_string),
            });
        compacted = projection
            .get("compacted")
            .and_then(Value::as_object)
            .map(|c| CompactedLine {
                source_ordinal: ordinal,
                message: c
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
            });
    }

    Ok(ProjectionRecord {
        source_ordinal: ordinal,
        decode_error,
        thread_id,
        session_meta,
        changed_items,
        changed_turns,
        removed_turn_ids,
        malformed_items,
        inter_agent,
        compacted,
    })
}

/// Parse one `changedItems` entry into a projection item.
fn parse_item_change(change: &Value) -> Option<ProjectionItem> {
    let change_obj = change.as_object()?;
    let turn_id = change_obj
        .get("turnId")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())?;
    let item = change_obj.get("item").and_then(Value::as_object)?;
    let kind = item
        .get("kind")
        .and_then(Value::as_str)
        .map_or(ProjectionKind::Unknown, ProjectionKind::parse);
    let item_id = item
        .get("id")
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())?;
    let actor = match item.get("kind").and_then(Value::as_str) {
        Some("userMessage") => "user",
        Some("agentMessage") => "assistant",
        _ => "system",
    }
    .to_string();
    Some(ProjectionItem {
        item_id: item_id.to_string(),
        turn_id: turn_id.to_string(),
        kind,
        actor,
        payload: Value::Object(item.clone()),
    })
}

/// Parse one `changedTurns` entry into turn metadata.
///
/// Entries missing a non-empty `turnId` are dropped (they carry no identity).
fn parse_turn_change(change: &Value) -> Option<TurnMeta> {
    let obj = change.as_object()?;
    let turn_id = obj
        .get("turnId")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())?
        .to_string();
    Some(TurnMeta {
        turn_id,
        status: obj
            .get("status")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(ToString::to_string),
        error_message: obj
            .get("errorMessage")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(ToString::to_string),
        started_at: obj.get("startedAt").and_then(Value::as_i64),
        completed_at: obj.get("completedAt").and_then(Value::as_i64),
        duration_ms: obj.get("durationMs").and_then(Value::as_i64),
    })
}

/// Parse a bridge `sessionMeta` object into provider-neutral session metadata.
///
/// The owning thread id is the first-priority identity; the Codex `session_id`
/// is carried as provenance only. All structural fields (`parentThreadId`,
/// `forkedFromId`, `agentPath`) and the source passthroughs are explicit —
/// nothing is inferred by parsing raw records.
fn parse_session_meta(meta: &serde_json::Map<String, Value>) -> SessionMeta {
    let str_field = |name: &str| {
        meta.get(name)
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(ToString::to_string)
    };
    SessionMeta {
        thread_id: str_field("threadId").unwrap_or_default(),
        session_id: str_field("sessionId"),
        parent_thread_id: str_field("parentThreadId"),
        forked_from_id: str_field("forkedFromId"),
        agent_nickname: str_field("agentNickname"),
        agent_path: str_field("agentPath"),
        thread_source: meta.get("threadSource").cloned(),
        source: meta.get("source").cloned(),
        originator: str_field("originator"),
        model_provider: str_field("modelProvider"),
        cwd: str_field("cwd"),
        git: meta
            .get("git")
            .and_then(Value::as_object)
            .map(|git| SessionGitMeta {
                commit_hash: git
                    .get("commitHash")
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
                    .map(ToString::to_string),
                branch: git
                    .get("branch")
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
                    .map(ToString::to_string),
                repository_url: git
                    .get("repositoryUrl")
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
                    .map(ToString::to_string),
            }),
    }
}

/// Merge one turn-metadata entry into a first-appearance-ordered list.
///
/// The last reported status/timing for a turn id wins (the fold mirror of item
/// upserts); the first appearance keeps the turn's position in the list.
fn merge_turn(turns: &mut Vec<TurnMeta>, incoming: &TurnMeta) {
    if let Some(existing) = turns.iter_mut().find(|t| t.turn_id == incoming.turn_id) {
        existing.status.clone_from(&incoming.status);
        existing.error_message.clone_from(&incoming.error_message);
        existing.started_at = incoming.started_at;
        existing.completed_at = incoming.completed_at;
        existing.duration_ms = incoming.duration_ms;
    } else {
        turns.push(incoming.clone());
    }
}

/// Parse a `final` record: validate the EOF anchor and return its owning-thread
/// metadata.
fn parse_final_record(
    obj: &serde_json::Map<String, Value>,
    record_index: u64,
    ordinal: u64,
    expected_total: u64,
) -> Result<Option<String>, String> {
    if ordinal != expected_total {
        return Err(format!(
            "record {record_index}: final record ordinal {ordinal} does not match EOF anchor {expected_total}"
        ));
    }
    let thread_id = obj
        .get("threadId")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(ToString::to_string);
    Ok(thread_id)
}
