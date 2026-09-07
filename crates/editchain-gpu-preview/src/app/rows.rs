//! Pure row presentation model for one `HistoryRow` JSON value.
//!
//! This module ports the row-presentation half of the legacy JS controller
//! into a deterministic, target-independent layer. Given a raw cached row
//! ([`RowSpec::from_value`]) it resolves every presentation input the DOM
//! shell needs:
//!
//! - stable identity/`data-key` semantics for top-level rows and bundled
//!   sub-ops ([`RowSpec::node_key`], [`RowSpec::op_id`], [`RowSpec::git_oid`],
//!   ...),
//! - the exact CSS classes and ARIA attributes `buildRowHtml` emits (`.row`
//!   with `data-row`/`data-key`, `row-selected`, `row-find-current`,
//!   `row-placeholder`, `row-expandable`, `row-subop`, roving tabindex,
//!   `aria-selected`, `aria-expanded`, disclosure labels),
//! - content/summary formatting inputs (`summary`, `displaySummaryForRow`,
//!   `plainRowSummary`, Git prefix chips, deterministic Markdown summary
//!   structure),
//! - date/author/commit labels, kind classes, row tags, work-unit headers,
//!   typed Activity bundles, promotion rails, session provenance and disclosure
//!   metadata,
//! - graph data consumed by the wgpu frame contract (`lane`, `above`, `below`,
//!   `transitions`, `is_subop`, `is_bundle`), and
//! - the exact `openJson` identity envelope for eligible rows and nothing
//!   else. Ineligible rows yield `None` (production announces "No raw record
//!   is available for this row").
//!
//! The summary is represented as a structured, HTML-safe token tree
//! ([`MdInline`]/[`MdLine`]/[`Summary`]) instead of markup: no HTML string is
//! ever generated here, and raw HTML from imported content is stripped during
//! plain-text normalization the same way `main.js` does. Escaping and DOM
//! assembly remain the DOM layer's responsibility.
//!
//! Deliberate DOM-layer omissions (documented, not silently dropped):
//!
//! - The graph cell SVG, column layout `style`, sticky header, and column
//!   resize plumbing are layout concerns; this layer only exposes the frame
//!   graph fields.
//! - `formatDate` renders deterministically in UTC (the legacy JS renderer
//!   used the host locale/timezone via `toLocaleDateString`).
//! - Selection/find/expansion/roving-tabindex state is passed in via
//!   [`RowContext`] because the pure state machine does not own those flags;
//!   the shell supplies them.
//! - Fractional `member_count`/`work_unit.count` JSON numbers normalize to
//!   `u64` (the wire contract ships integer counts; JS `Number.isFinite`
//!   would accept fractions).
//! - `esc`/`codicon` glyph rendering, `data-*` attribute writing, and the
//!   `fillPlaceholders` DOM pass are shell concerns built on the values here.

use std::collections::VecDeque;

use serde_json::Value;

use super::host::row as wire;
use crate::ChainState;

/// Row presentation mode. Production always constructs [`Self::Activity`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ViewMode {
    /// The shipped semantic work-unit/bundle/promotion presentation.
    Activity,
    /// Retained only for pure compatibility tests of the former raw UI.
    #[cfg(test)]
    Raw,
}

/// Shell-owned state a row build depends on (selection, find highlight,
/// expansion, roving tabindex, and group boundary).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RowContext {
    pub(crate) view: ViewMode,
    /// Absolute expanded-history index (`data-row`).
    pub(crate) abs_index: i64,
    /// Whether this row opens a new `group` run (`row-group-start` divider).
    pub(crate) is_group_start: bool,
    /// The selected row's `node_key` (`selectedRowKey`), if any.
    pub(crate) selected_key: Option<String>,
    /// Whether this absolute index is the current find-in-chain match.
    pub(crate) find_current: bool,
    /// Whether this row's descendant span is revealed.
    pub(crate) expanded: bool,
    /// The roving-tabindex row (`rovingAbs`); exactly one per window is 0.
    pub(crate) roving_abs: Option<i64>,
}

impl Default for RowContext {
    fn default() -> Self {
        RowContext {
            view: ViewMode::Activity,
            abs_index: 0,
            is_group_start: false,
            selected_key: None,
            find_current: false,
            expanded: false,
            roving_abs: None,
        }
    }
}

impl RowContext {
    /// The common per-row context with defaulted stateful flags. Production
    /// contexts come from the state machine (`HistoryAppState::row_context`);
    /// this helper exists for the native spec goldens.
    #[cfg(test)]
    pub(crate) fn for_row(view: ViewMode, abs_index: i64, is_group_start: bool) -> RowContext {
        RowContext {
            view,
            abs_index,
            is_group_start,
            ..RowContext::default()
        }
    }
}

/// One row's identity + graph frame data, the inputs the later DOM/wgpu
/// renderers consume. `placeholder` rows carry identity only (production
/// renders `<div class="row row-placeholder" data-row="…">`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct RowIdentity {
    /// Absolute expanded-history index (`data-row`).
    pub(crate) abs_index: i64,
    /// Stable wire identity (`data-key`; selection compares on it).
    pub(crate) node_key: String,
    pub(crate) is_subop: bool,
    /// Presentation hierarchy depth (`0` top-level, `1` work member, `2`
    /// existing detail/bundle member nested beneath that work member).
    pub(crate) hierarchy_depth: u8,
    /// `subop_kind` wire value (Codicon selector; `meta`/unknown -> `info`).
    pub(crate) subop_kind: String,
    pub(crate) op_id: String,
    pub(crate) git_oid: String,
    pub(crate) repository: String,
    pub(crate) commit_id: String,
    pub(crate) turn_id: String,
}

/// Graph fields the row-local SVG renderer and retained frame contract consume,
/// verbatim from the wire row.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct GraphData {
    pub(crate) lane: u32,
    pub(crate) above: Vec<u32>,
    pub(crate) below: Vec<u32>,
    pub(crate) transitions: Vec<(u32, u32)>,
    pub(crate) muted_above: Vec<u32>,
    pub(crate) muted_below: Vec<u32>,
    pub(crate) muted_transitions: Vec<(u32, u32)>,
    /// Presentation state for this row's own dot/capsule.
    pub(crate) chain_state: ChainState,
    pub(crate) is_subop: bool,
    pub(crate) is_bundle: bool,
    /// Whether this bundle's members are currently revealed.
    pub(crate) expanded: bool,
}

/// The frozen outcome-badge visibility switch (default off).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct BadgeOptions {
    pub(crate) show_success_outcome: bool,
}

/// The production whitelisted record-role classes (`RECORD_ROLE_CLASSES`).
const RECORD_ROLE_CLASSES: [&str; 7] = [
    "narrative",
    "action",
    "result",
    "artifact",
    "lifecycle",
    "echo",
    "unknown",
];

/// `TOOL_PAYLOAD_TEXT_KEYS` — BFS visit order for tool envelopes.
const TOOL_PAYLOAD_TEXT_KEYS: [&str; 15] = [
    "text",
    "output_text",
    "input_text",
    "message",
    "summary",
    "output",
    "content",
    "status",
    "completed",
    "failed",
    "error",
    "cmd",
    "command",
    "query",
    "path",
];

/// The `esc`-style HTML-escape map used by the DOM layer's renderer; the
/// escape implementation itself lives in the DOM shell.
///
/// Provided as a constant so tests and the shell share the exact production
/// entities (`'` -> `&#39;`, matching `main.js`).
#[cfg(test)]
pub(crate) fn html_escape(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for c in input.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            other => out.push(other),
        }
    }
    out
}

/// JS truthiness for wire values (`||`/`?:` semantics in `main.js`).
fn js_truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(flag) => *flag,
        Value::Number(number) => number.as_f64().is_some_and(|f| f != 0.0 && !f.is_nan()),
        Value::String(text) => !text.is_empty(),
        Value::Array(_) | Value::Object(_) => true,
    }
}

/// JS `String(value)` coercion (used where `main.js` stringifies a row field).
fn js_string(value: &Value) -> String {
    match value {
        Value::Null => "null".to_owned(),
        Value::Bool(flag) => flag.to_string(),
        Value::Number(number) => number.to_string(),
        Value::String(text) => text.clone(),
        Value::Array(items) => items
            .iter()
            .map(js_array_element)
            .collect::<Vec<String>>()
            .join(","),
        Value::Object(_) => "[object Object]".to_owned(),
    }
}

/// `Array.prototype.toString` element rule: `null`/`undefined` become empty.
fn js_array_element(value: &Value) -> String {
    if value.is_null() {
        String::new()
    } else {
        js_string(value)
    }
}

/// Read a row string field as JS would (missing/undefined -> empty).
fn row_str(value: &Value, key: &str) -> String {
    wire::owned_str(value, key)
}

/// Read a row numeric field with JS `||` semantics (0/missing -> 0).
fn row_ms(value: &Value, key: &str) -> i64 {
    value.get(key).and_then(Value::as_i64).unwrap_or(0)
}

/// Shorten a raw 64-bit identifier for display (`shortId`: keep the tail).
pub(crate) fn short_id(id: &str) -> String {
    if id.is_empty() {
        return String::new();
    }
    let chars: Vec<char> = id.chars().collect();
    let keep_from = chars.len().saturating_sub(12);
    chars.into_iter().skip(keep_from).collect()
}

/// Human subtitle for either endpoint of a connected graph chain.
/// Provider titles replace opaque session ids; a distinct named subagent is
/// appended without repeating providers that store the session title in both
/// fields.
pub(crate) fn group_label_text(
    group: &str,
    session_title: Option<&str>,
    agent_nickname: Option<&str>,
) -> String {
    if let Some(rest) = group.strip_prefix("repo:") {
        return format!("Git · repo {}", short_id(rest));
    }
    let title = session_title
        .map(str::trim)
        .filter(|title| !title.is_empty());
    let mut label = title.map_or_else(
        || {
            group.strip_prefix("session:").map_or_else(
                || "EditChain ops".to_owned(),
                |rest| format!("Session {}", short_id(rest)),
            )
        },
        str::to_owned,
    );
    if let Some(agent) = agent_nickname
        .map(str::trim)
        .filter(|agent| !agent.is_empty())
        .filter(|agent| !agent.eq_ignore_ascii_case(&label))
    {
        label.push_str(" · ");
        label.push_str(agent);
    }
    label
}

/// Whether this row's own graph node has an open side.
///
/// `above` and `below` carry same-lane half-segments. Cross-lane bends carry
/// the remaining connection at the node: parent-anchored bends end at the dot
/// from above, while child-anchored bends start at the dot toward below. A
/// graph subtitle belongs only on a true tip or root, never merely on a group
/// boundary that happens to lie inside a continuing chain.
fn is_graph_endpoint(row: &Value) -> bool {
    let lane = wire::lane(row);
    let above = wire::above(row);
    let below = wire::below(row);
    let transitions = wire::transitions(row);
    let connected_above =
        above.contains(&lane) || transitions.iter().any(|&(_, to_lane)| to_lane == lane);
    let connected_below =
        below.contains(&lane) || transitions.iter().any(|&(from_lane, _)| from_lane == lane);

    !connected_above || !connected_below
}

fn session_meta_field<'a>(row: &'a Value, field: &str) -> Option<&'a str> {
    row.get("session_meta")
        .and_then(|meta| meta.get(field))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

/// `shortCommitId` — display value for the Commit/ID column.
pub(crate) fn short_commit_id(row: &Value) -> String {
    let git_oid = row_str(row, "git_oid");
    let commit_id = row_str(row, "commit_id");
    let op_id = row_str(row, "op_id");
    let turn_id = row_str(row, "turn_id");
    let is_subop = wire::bool(row, "is_subop");
    if !git_oid.is_empty() {
        let preferred = if commit_id.is_empty() {
            git_oid
        } else {
            commit_id
        };
        return short_id(&preferred);
    }
    if is_subop {
        return short_id(&op_id);
    }
    if !turn_id.is_empty() {
        return short_id(&turn_id);
    }
    let preferred = if commit_id.is_empty() {
        op_id
    } else {
        commit_id
    };
    short_id(&preferred)
}

/// Commit/ID column hover title (`row.commit_id || row.op_id || ''`).
pub(crate) fn commit_cell_title(row: &Value) -> String {
    let commit_id = row_str(row, "commit_id");
    if commit_id.is_empty() {
        row_str(row, "op_id")
    } else {
        commit_id
    }
}

// --- Date -------------------------------------------------------------------

const MONTH_NAMES: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

/// Deterministic UTC `formatDate` (see module docs for the locale divergence).
pub(crate) fn format_date(ms: i64) -> String {
    if ms == 0 {
        return String::new();
    }
    let days = ms.div_euclid(86_400_000);
    let seconds = ms.rem_euclid(86_400_000).div_euclid(1_000);
    let hour = seconds.div_euclid(3_600);
    let minute = seconds.rem_euclid(3_600).div_euclid(60);
    let (year, month, day) = civil_from_days(days);
    let (hour12, meridiem) = if hour == 0 {
        (12, "AM")
    } else if hour < 12 {
        (hour, "AM")
    } else if hour == 12 {
        (12, "PM")
    } else {
        (hour.wrapping_sub(12), "PM")
    };
    let month_index = usize::try_from(month.wrapping_sub(1)).unwrap_or(0);
    let month_name = MONTH_NAMES.get(month_index).copied().unwrap_or("Jan");
    format!("{month_name} {day}, {year} {hour12:02}:{minute:02} {meridiem}")
}

/// Days since 1970-01-01 to civil (y, m, d); Howard Hinnant's algorithm.
fn civil_from_days(days_since_epoch: i64) -> (i64, i64, i64) {
    let z = days_since_epoch.wrapping_add(719_468);
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe
        .wrapping_sub(doe.div_euclid(1_460))
        .wrapping_add(doe.div_euclid(36_524))
        .wrapping_sub(doe.div_euclid(146_096)))
    .div_euclid(365);
    let y = yoe.wrapping_add(era.wrapping_mul(400));
    let doy = doe.wrapping_sub(
        365_i64
            .wrapping_mul(yoe)
            .wrapping_add(yoe.div_euclid(4))
            .wrapping_sub(yoe.div_euclid(100)),
    );
    let mp = doy.wrapping_mul(5).wrapping_add(2).div_euclid(153);
    let day = doy
        .wrapping_sub(mp.wrapping_mul(153).wrapping_add(2).div_euclid(5))
        .wrapping_add(1);
    let month = if mp < 10 {
        mp.wrapping_add(3)
    } else {
        mp.wrapping_sub(9)
    };
    let year = if month <= 2 { y.wrapping_add(1) } else { y };
    (year, month, day)
}

// --- Markdown plain-text toolchain -----------------------------------------

/// Escapable punctuation in Markdown escapes (``[\\`*_[\]{}()#+\-.!~>]``).
fn is_escapable(c: char) -> bool {
    matches!(
        c,
        '\\' | '`'
            | '*'
            | '_'
            | '['
            | ']'
            | '{'
            | '}'
            | '('
            | ')'
            | '#'
            | '+'
            | '-'
            | '.'
            | '!'
            | '~'
            | '>'
    )
}

/// Single-emphasis boundary set used by `markdownPlainInline` (JS
/// `[\s([{<:;,.!?-]` and `[\s)\]}>:;,.!?-]`).
fn is_boundary_before(c: char) -> bool {
    c.is_whitespace()
        || matches!(
            c,
            '(' | '[' | '{' | '<' | ':' | ';' | ',' | '.' | '!' | '?' | '-'
        )
}

fn is_boundary_after(c: char) -> bool {
    c.is_whitespace()
        || matches!(
            c,
            ')' | ']' | '}' | '>' | ':' | ';' | ',' | '.' | '!' | '?' | '-'
        )
}

/// First char index where `needle` occurs at or after `start`, char-based.
fn find_from(chars: &[char], needle: &[char], start: usize) -> Option<usize> {
    if needle.is_empty() {
        return Some(start.min(chars.len()));
    }
    let width = needle.len();
    let last = chars.len().saturating_sub(width);
    if start > last {
        return None;
    }
    chars
        .windows(width)
        .skip(start)
        .position(|window| window == needle)
        .map(|found| start.saturating_add(found))
}

/// Whether `needle` occurs exactly at char index `at`.
fn starts_with_at(chars: &[char], needle: &[char], at: usize) -> bool {
    needle
        .iter()
        .enumerate()
        .all(|(k, c)| chars.get(at.saturating_add(k)) == Some(c))
}

/// Bounds-checked char read (the strip scanners never index out of range).
fn char_at(chars: &[char], index: usize) -> char {
    chars.get(index).copied().unwrap_or('\0')
}

/// `markdownClosing` — unescaped closing delimiter search, char-based.
fn markdown_closing(chars: &[char], delimiter: &[char], start: usize) -> Option<usize> {
    let mut at = find_from(chars, delimiter, start)?;
    loop {
        let escaped = at
            .checked_sub(1)
            .and_then(|p| chars.get(p))
            .is_some_and(|c| *c == '\\');
        if !escaped && at > start {
            return Some(at);
        }
        at = find_from(chars, delimiter, at.saturating_add(delimiter.len()))?;
    }
}

/// `markdownPlainInline` — plain text of one Markdown fragment (12-step port).
pub(crate) fn markdown_plain_inline(value: &str) -> String {
    let mut chars: Vec<char> = value.chars().collect();
    // \\([\\`*_[\]{}()#+\-.!~>]) -> '$1'
    chars = unescape_punctuation(&chars);
    // !\[([^\]\n]*)\]\([^\n)]*\) -> label
    chars = strip_bracket_links(&chars, true);
    // \[([^\]\n]+)\]\([^\n)]*\) -> label
    chars = strip_bracket_links(&chars, false);
    // `+([^`\n]*?)`+ -> '$1'
    chars = strip_code_spans(&chars);
    // \*\*([^*\n]+)\*\* / __([^_\n]+)__ / ~~([^~\n]+)~~ -> '$1'
    chars = strip_paired('*', &chars);
    chars = strip_paired('_', &chars);
    chars = strip_paired('~', &chars);
    // (^|[\s([{<:;,.!?-])\*([^*\n]+)\*(?=$|[\s)\]}>:;,.!?-]) -> '$1$2'
    chars = strip_single_emphasis('*', &chars);
    chars = strip_single_emphasis('_', &chars);
    // <\/?[A-Za-z][^>\n]*> -> ''
    chars = strip_html_tags(&chars);
    // \*{2,}|~{2,}|`+ -> ''
    chars = strip_delimiter_runs(&chars);
    // \s+ -> ' ', then trim.
    chars
        .into_iter()
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<&str>>()
        .join(" ")
}

fn unescape_punctuation(chars: &[char]) -> Vec<char> {
    let mut out = Vec::with_capacity(chars.len());
    let mut index = 0;
    while index < chars.len() {
        if char_at(chars, index) == '\\' {
            if let Some(next) = chars.get(index.saturating_add(1)) {
                if is_escapable(*next) {
                    out.push(*next);
                    index = index.saturating_add(2);
                    continue;
                }
            }
        }
        out.push(char_at(chars, index));
        index = index.saturating_add(1);
    }
    out
}

/// `!\[([^\]\n]*)\]\([^\n)]*\)` (image, empty label allowed) and
/// `\[([^\]\n]+)\]\([^\n)]*\)` (text link, non-empty label) -> the label.
fn strip_bracket_links(chars: &[char], image: bool) -> Vec<char> {
    let mut out = Vec::with_capacity(chars.len());
    let mut index = 0;
    while index < chars.len() {
        let mut cursor = index;
        let is_image = image && chars.get(cursor) == Some(&'!');
        if is_image {
            cursor = cursor.saturating_add(1);
        }
        if chars.get(cursor) == Some(&'[') {
            let label_start = cursor.saturating_add(1);
            let mut label_end = None;
            for (offset, c) in chars.iter().enumerate().skip(label_start) {
                if *c == ']' {
                    label_end = Some(offset);
                    break;
                }
                if *c == '\n' {
                    break;
                }
            }
            if let Some(label_end) = label_end {
                let label_len = label_end.saturating_sub(label_start);
                if image || label_len > 0 {
                    let paren = label_end.saturating_add(1);
                    if chars.get(paren) == Some(&'(') {
                        let target_start = paren.saturating_add(1);
                        let mut target_end = None;
                        for (offset, c) in chars.iter().enumerate().skip(target_start) {
                            if *c == ')' || *c == '\n' {
                                target_end = Some(offset);
                                break;
                            }
                        }
                        if let Some(target_end) = target_end {
                            if chars.get(target_end) == Some(&')') {
                                for c in chars.iter().skip(label_start).take(label_len) {
                                    out.push(*c);
                                }
                                index = target_end.saturating_add(1);
                                continue;
                            }
                        }
                    }
                }
            }
        }
        out.push(char_at(chars, index));
        index = index.saturating_add(1);
    }
    out
}

/// `` `+([^`\n]*?)`+ `` -> the inner text (opening/closing runs independent).
fn strip_code_spans(chars: &[char]) -> Vec<char> {
    let mut out = Vec::with_capacity(chars.len());
    let mut index = 0;
    while index < chars.len() {
        if char_at(chars, index) == '`' {
            let open_run = chars.iter().skip(index).take_while(|c| **c == '`').count();
            let content_start = index.saturating_add(open_run);
            let mut close_start = None;
            for (offset, c) in chars.iter().enumerate().skip(content_start) {
                if *c == '\n' {
                    break;
                }
                if *c == '`' {
                    close_start = Some(offset);
                    break;
                }
            }
            if let Some(close_start) = close_start {
                let close_run = chars
                    .iter()
                    .skip(close_start)
                    .take_while(|c| **c == '`')
                    .count();
                for c in chars
                    .iter()
                    .skip(content_start)
                    .take(close_start.saturating_sub(content_start))
                {
                    out.push(*c);
                }
                index = close_start.saturating_add(close_run);
                continue;
            }
        }
        out.push(char_at(chars, index));
        index = index.saturating_add(1);
    }
    out
}

/// `**([^*\n]+)**` / `__([^_\n]+)__` / `~~([^~\n]+)~~` -> the inner text.
fn strip_paired(delimiter: char, chars: &[char]) -> Vec<char> {
    let mut out = Vec::with_capacity(chars.len());
    let mut index = 0;
    while index < chars.len() {
        let pair_here = chars.get(index) == Some(&delimiter)
            && chars.get(index.saturating_add(1)) == Some(&delimiter);
        if !pair_here {
            out.push(char_at(chars, index));
            index = index.saturating_add(1);
            continue;
        }
        let mut inner_start = index.saturating_add(2);
        let mut inner_end = None;
        while inner_start < chars.len() {
            let c = char_at(chars, inner_start);
            if c == '\n' || c == delimiter {
                if c == delimiter && chars.get(inner_start.saturating_add(1)) == Some(&delimiter) {
                    inner_end = Some(inner_start);
                }
                break;
            }
            inner_start = inner_start.saturating_add(1);
        }
        if let Some(inner_end) = inner_end {
            let content_len = inner_end.saturating_sub(index.saturating_add(2));
            if content_len > 0 {
                for c in chars.iter().skip(index.saturating_add(2)).take(content_len) {
                    out.push(*c);
                }
                index = inner_end.saturating_add(2);
                continue;
            }
        }
        out.push(char_at(chars, index));
        index = index.saturating_add(1);
    }
    out
}

/// Single-emphasis strip with boundary lookarounds; the boundary char before
/// the opening mark was already emitted, so only the marks are dropped.
fn strip_single_emphasis(delimiter: char, chars: &[char]) -> Vec<char> {
    let mut out = Vec::with_capacity(chars.len());
    let mut index = 0;
    while index < chars.len() {
        if char_at(chars, index) == delimiter {
            let boundary_before = index == 0
                || chars
                    .get(index.saturating_sub(1))
                    .is_some_and(|c| is_boundary_before(*c));
            let mut inner_start = index.saturating_add(1);
            let mut inner_end = None;
            while inner_start < chars.len() {
                let c = char_at(chars, inner_start);
                if c == '\n' || c == delimiter {
                    if c == delimiter {
                        let after = chars.get(inner_start.saturating_add(1)).copied();
                        if after.is_none_or(is_boundary_after) {
                            inner_end = Some(inner_start);
                        }
                    }
                    break;
                }
                inner_start = inner_start.saturating_add(1);
            }
            if boundary_before {
                let next_is_text = chars
                    .get(index.saturating_add(1))
                    .is_some_and(|c| !c.is_whitespace());
                if let Some(inner_end) = inner_end {
                    let content_len = inner_end.saturating_sub(index.saturating_add(1));
                    if next_is_text && content_len > 0 {
                        for c in chars.iter().skip(index.saturating_add(1)).take(content_len) {
                            out.push(*c);
                        }
                        index = inner_end.saturating_add(1);
                        continue;
                    }
                }
            }
        }
        out.push(char_at(chars, index));
        index = index.saturating_add(1);
    }
    out
}

/// `<\/?[A-Za-z][^>\n]*>` -> '' (greedy to the last `>` before the newline:
/// same-line text between tags is removed exactly like production, never
/// interpreted as markup inside the privileged webview).
fn strip_html_tags(chars: &[char]) -> Vec<char> {
    let mut out = Vec::with_capacity(chars.len());
    let mut index = 0;
    while index < chars.len() {
        if char_at(chars, index) == '<' {
            let mut cursor = index.saturating_add(1);
            if chars.get(cursor) == Some(&'/') {
                cursor = cursor.saturating_add(1);
            }
            let letter = chars.get(cursor).copied();
            if letter.is_some_and(|c| c.is_ascii_alphabetic()) {
                cursor = cursor.saturating_add(1);
                let mut gt = None;
                for (offset, c) in chars.iter().enumerate().skip(cursor) {
                    if *c == '>' {
                        gt = Some(offset);
                        break;
                    }
                    if *c == '\n' {
                        break;
                    }
                }
                if let Some(gt) = gt {
                    index = gt.saturating_add(1);
                    continue;
                }
            }
        }
        out.push(char_at(chars, index));
        index = index.saturating_add(1);
    }
    out
}

/// `\*{2,}|~{2,}|`+ -> '' (unmatched decoration runs are removed).
fn strip_delimiter_runs(chars: &[char]) -> Vec<char> {
    let mut out = Vec::with_capacity(chars.len());
    let mut index = 0;
    while index < chars.len() {
        let c = char_at(chars, index);
        let run = chars.iter().skip(index).take_while(|x| **x == c).count();
        let remove_run = run >= 2 && (c == '*' || c == '~');
        let remove_run = remove_run || c == '`';
        if remove_run {
            index = index.saturating_add(run);
            continue;
        }
        out.push(c);
        index = index.saturating_add(1);
    }
    out
}

/// `markdownPlainLine` — plain-text form of one Markdown source line. Block
/// prefixes lose punctuation while retaining the actual sentence.
pub(crate) fn markdown_plain_line(value: &str) -> String {
    let mut line = value.trim().to_owned();
    line = strip_heading_prefix(&line);
    line = strip_task_marker_prefix(&line);
    line = strip_list_marker_prefix(&line);
    line = strip_ordered_marker_prefix(&line);
    line = strip_quote_marker_prefix(&line);
    line = strip_callout_marker_prefix(&line);
    line = strip_fence_marker_prefix(&line);
    markdown_plain_inline(&line)
}

/// Strip one leading prefix with its following whitespace run (JS `\s+`).
fn strip_heading_prefix(line: &str) -> String {
    let chars: Vec<char> = line.chars().collect();
    let hashes = chars.iter().take_while(|c| **c == '#').count();
    if !(1..=6).contains(&hashes) || !chars.get(hashes).is_some_and(|c| c.is_whitespace()) {
        return line.to_owned();
    }
    let mut cursor = hashes;
    while chars.get(cursor).is_some_and(|c| c.is_whitespace()) {
        cursor = cursor.saturating_add(1);
    }
    chars.iter().skip(cursor).copied().collect()
}

/// `^[-+*]\s+\[[ xX]\]\s+` strip.
fn strip_task_marker_prefix(line: &str) -> String {
    let chars: Vec<char> = line.chars().collect();
    if !matches!(chars.first(), Some('-' | '+' | '*')) {
        return line.to_owned();
    }
    let mut cursor = 1;
    while chars.get(cursor).is_some_and(|c| c.is_whitespace()) {
        cursor = cursor.saturating_add(1);
    }
    if chars.get(cursor) != Some(&'[') {
        return line.to_owned();
    }
    let state = chars.get(cursor.saturating_add(1)).copied();
    if !matches!(state, Some(' ' | 'x' | 'X')) || chars.get(cursor.saturating_add(2)) != Some(&']')
    {
        return line.to_owned();
    }
    let mut after = cursor.saturating_add(3);
    if !chars.get(after).is_some_and(|c| c.is_whitespace()) {
        return line.to_owned();
    }
    while chars.get(after).is_some_and(|c| c.is_whitespace()) {
        after = after.saturating_add(1);
    }
    chars.iter().skip(after).copied().collect()
}

/// `^[-+*]\s+` strip.
fn strip_list_marker_prefix(line: &str) -> String {
    let chars: Vec<char> = line.chars().collect();
    if !matches!(chars.first(), Some('-' | '+' | '*')) {
        return line.to_owned();
    }
    let mut cursor = 1;
    if !chars.get(cursor).is_some_and(|c| c.is_whitespace()) {
        return line.to_owned();
    }
    while chars.get(cursor).is_some_and(|c| c.is_whitespace()) {
        cursor = cursor.saturating_add(1);
    }
    chars.iter().skip(cursor).copied().collect()
}

/// `^\d+[.)]\s+` strip.
fn strip_ordered_marker_prefix(line: &str) -> String {
    let chars: Vec<char> = line.chars().collect();
    let digits = chars.iter().take_while(|c| c.is_ascii_digit()).count();
    let sep = chars.get(digits).copied();
    if digits == 0 || !matches!(sep, Some('.' | ')')) {
        return line.to_owned();
    }
    let mut cursor = digits.saturating_add(1);
    if !chars.get(cursor).is_some_and(|c| c.is_whitespace()) {
        return line.to_owned();
    }
    while chars.get(cursor).is_some_and(|c| c.is_whitespace()) {
        cursor = cursor.saturating_add(1);
    }
    chars.iter().skip(cursor).copied().collect()
}

/// `^>\s*` strip.
fn strip_quote_marker_prefix(line: &str) -> String {
    let chars: Vec<char> = line.chars().collect();
    if chars.first() != Some(&'>') {
        return line.to_owned();
    }
    let mut cursor = 1;
    while chars.get(cursor).is_some_and(|c| c.is_whitespace()) {
        cursor = cursor.saturating_add(1);
    }
    chars.iter().skip(cursor).copied().collect()
}

/// `^\[![A-Za-z]+\]\s*` strip.
fn strip_callout_marker_prefix(line: &str) -> String {
    let chars: Vec<char> = line.chars().collect();
    if chars.first() != Some(&'[') || chars.get(1) != Some(&'!') {
        return line.to_owned();
    }
    let letters = chars
        .iter()
        .skip(2)
        .take_while(|c| c.is_ascii_alphabetic())
        .count();
    if letters == 0 || chars.get(2usize.saturating_add(letters)) != Some(&']') {
        return line.to_owned();
    }
    let mut cursor = 2usize.saturating_add(letters).saturating_add(1);
    while chars.get(cursor).is_some_and(|c| c.is_whitespace()) {
        cursor = cursor.saturating_add(1);
    }
    chars.iter().skip(cursor).copied().collect()
}

/// ``^`{3,}\s*[A-Za-z0-9_+.-]*\s*`` strip.
fn strip_fence_marker_prefix(line: &str) -> String {
    let chars: Vec<char> = line.chars().collect();
    let ticks = chars.iter().take_while(|c| **c == '`').count();
    if ticks < 3 {
        return line.to_owned();
    }
    let mut cursor = ticks;
    while chars.get(cursor).is_some_and(|c| c.is_whitespace()) {
        cursor = cursor.saturating_add(1);
    }
    while chars
        .get(cursor)
        .is_some_and(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '+' | '-' | '.'))
    {
        cursor = cursor.saturating_add(1);
    }
    while chars.get(cursor).is_some_and(|c| c.is_whitespace()) {
        cursor = cursor.saturating_add(1);
    }
    chars.iter().skip(cursor).copied().collect()
}

/// `markdownPlainSummary` — every meaningful line joined with ` · `.
pub(crate) fn markdown_plain_summary(value: &str) -> String {
    let normalized = value.replace("\r\n", "\n").replace('\r', "\n");
    normalized
        .split('\n')
        .map(markdown_plain_line)
        .filter(|line| !line.is_empty())
        .collect::<Vec<String>>()
        .join(" · ")
}

/// The line filter used by `renderMarkdownSummary` (non-empty trim AND non-empty
/// plain form).
fn line_kept(line: &str) -> bool {
    !line.trim().is_empty() && !markdown_plain_line(line).is_empty()
}

/// `markdownPlainSummary` used as the detail/tooltip text.
pub(crate) fn markdown_detail_summary(value: &str) -> String {
    markdown_plain_summary(value)
}

// --- Structured Markdown summary (HTML-safe token tree) ---------------------

/// One inline Markdown token. Text is HTML-safe by construction (raw HTML is
/// stripped during plain-text normalization); the DOM layer escapes when
/// rendering. `Space` is the whitespace-only fragment span (nbsp, aria-hidden).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum MdInline {
    Text(String),
    Space,
    Code(String),
    Strong(Vec<MdInline>),
    Em(Vec<MdInline>),
    Strike(Vec<MdInline>),
    Link {
        label: Vec<MdInline>,
        target: String,
    },
    Image {
        label: Vec<MdInline>,
        target: String,
    },
}

/// One rendered summary line's semantic kind (the `md-line` class family).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum MdLineKind {
    Heading { level: u8 },
    Task { done: bool },
    Unordered,
    Ordered { marker: String },
    Quote { callout: Option<String> },
    Fence { language: String },
    Plain,
    Empty,
}

/// One summary line: kind + inline tokens.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct MdLine {
    pub(crate) kind: MdLineKind,
    pub(crate) inline: Vec<MdInline>,
}

/// The structured result of `renderMarkdownSummary`: the first meaningful
/// line plus the quiet `+N lines` tail count (aria-hidden in the DOM layer).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Summary {
    pub(crate) line: MdLine,
    pub(crate) more: usize,
}

impl Summary {
    /// The label `renderMarkdownSummary` uses when no meaningful line exists.
    #[cfg(test)]
    pub(crate) const EMPTY_LABEL: &'static str = "Structured content";

    fn empty() -> Summary {
        Summary {
            line: MdLine {
                kind: MdLineKind::Empty,
                inline: Vec::new(),
            },
            more: 0,
        }
    }

    /// Port of `renderMarkdownSummary`: first meaningful line + hidden count.
    pub(crate) fn parse(value: &str) -> Summary {
        let normalized = value.replace("\r\n", "\n").replace('\r', "\n");
        let lines: Vec<&str> = normalized
            .split('\n')
            .map(str::trim)
            .filter(|line| line_kept(line))
            .collect();
        let Some(first) = lines.first() else {
            return Summary::empty();
        };
        let line = render_markdown_line(first);
        Summary {
            line,
            more: lines.len().saturating_sub(1),
        }
    }
}

/// The summary row content after any Git prefix has moved to the Tags column.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RowSummary {
    /// `gitSummaryParts` prefix used to build the separate Tags-column chip.
    pub(crate) git_prefix: Option<String>,
    /// Rendered markdown of the content after the colon; `None` when a Git
    /// prefix leaves no content span (production renders no content then).
    pub(crate) content: Option<Summary>,
    /// Plain DOM text for `content`, kept separate from the row's full plain
    /// summary so a rendered Git prefix is not repeated beside its chip.
    pub(crate) plain_content: Option<String>,
}

/// Split a Git conventional prefix from the first colon (`gitSummaryParts`).
pub(crate) fn git_summary_parts(row: &Value, value: &str) -> Option<(String, String)> {
    if row_str(row, "git_oid").is_empty() {
        return None;
    }
    let chars: Vec<char> = value.chars().collect();
    let colon = chars.iter().position(|c| *c == ':')?;
    if colon == 0 {
        return None;
    }
    let prefix: String = chars.iter().take(colon).collect();
    let prefix = prefix.trim();
    if prefix.is_empty() {
        return None;
    }
    let content: String = chars
        .iter()
        .skip(colon.saturating_add(1))
        .collect::<String>()
        .trim_start()
        .to_owned();
    Some((prefix.to_owned(), content))
}

impl RowSummary {
    /// Parse the content summary and its separate Tags-column Git prefix.
    pub(crate) fn parse(row: &Value, display_summary: &str) -> RowSummary {
        if let Some((prefix, content)) = git_summary_parts(row, display_summary) {
            let plain_content = if content.is_empty() {
                None
            } else {
                let plain = markdown_plain_summary(&content);
                Some(if plain.is_empty() {
                    "(no summary)".to_owned()
                } else {
                    plain
                })
            };
            RowSummary {
                git_prefix: Some(prefix),
                content: if content.is_empty() {
                    None
                } else {
                    Some(Summary::parse(&content))
                },
                plain_content,
            }
        } else {
            let plain = markdown_plain_summary(display_summary);
            RowSummary {
                git_prefix: None,
                content: Some(Summary::parse(display_summary)),
                plain_content: Some(if plain.is_empty() {
                    "(no summary)".to_owned()
                } else {
                    plain
                }),
            }
        }
    }
}

/// `plainRowSummary` — plain text of the display summary (colon absent too).
pub(crate) fn plain_row_summary(row: &Value, value: &str) -> String {
    if let Some((prefix, content)) = git_summary_parts(row, value) {
        let combined = if content.is_empty() {
            prefix
        } else {
            format!("{prefix} {content}")
        };
        markdown_plain_summary(&combined)
    } else {
        markdown_plain_summary(value)
    }
}

/// Recursive depth cap for the inline renderer (`level > 4` in main.js).
const MAX_INLINE_DEPTH: u8 = 4;

/// `renderMarkdownInline` — structured tokens, main.js span semantics.
pub(crate) fn render_markdown_inline(value: &str, depth: u8) -> Vec<MdInline> {
    if depth > MAX_INLINE_DEPTH {
        return vec![MdInline::Text(markdown_plain_inline(value))];
    }
    let chars: Vec<char> = value.chars().collect();
    let mut out: Vec<MdInline> = Vec::new();
    let mut plain: String = String::new();
    let mut index = 0usize;
    while index < chars.len() {
        // Markdown escapes: show the escaped punctuation without the backslash.
        if char_at(&chars, index) == '\\' {
            if let Some(&next) = chars.get(index.saturating_add(1)) {
                if is_escapable(next) {
                    plain.push(next);
                    index = index.saturating_add(2);
                    continue;
                }
            }
        }
        // Images and links stay non-navigable; labels keep inline emphasis.
        if let Some((image, label, target, len)) = parse_link(&chars, index) {
            flush_plain(&mut out, &mut plain);
            let label_chars: Vec<char> = label.clone();
            let label_string: String = label_chars.into_iter().collect();
            let inner = render_markdown_inline(&label_string, depth.saturating_add(1));
            if image {
                out.push(MdInline::Image {
                    label: inner,
                    target,
                });
            } else {
                out.push(MdInline::Link {
                    label: inner,
                    target,
                });
            }
            index = index.saturating_add(len);
            continue;
        }
        // Code spans are literal.
        if char_at(&chars, index) == '`' {
            let open_run = chars.iter().skip(index).take_while(|c| **c == '`').count();
            let delimiter: Vec<char> = std::iter::repeat_n('`', open_run).collect();
            let content_start = index.saturating_add(open_run);
            if let Some(close) = markdown_closing(&chars, &delimiter, content_start) {
                flush_plain(&mut out, &mut plain);
                let raw: String = chars
                    .iter()
                    .copied()
                    .skip(content_start)
                    .take(close.saturating_sub(content_start))
                    .collect();
                let code = strip_code_edges(&raw);
                out.push(MdInline::Code(code));
                index = close.saturating_add(open_run);
                continue;
            }
        }
        // Paired strong / underline-bold / strike (checked in that order).
        let mut paired = None;
        for (delim, kind) in [
            ("**", PairKind::Strong),
            ("__", PairKind::Strong),
            ("~~", PairKind::Strike),
        ] {
            let needle: Vec<char> = delim.chars().collect();
            if starts_with_at(&chars, &needle, index) {
                paired = Some((needle, kind));
                break;
            }
        }
        if let Some((needle, kind)) = paired {
            if let Some(close) =
                markdown_closing(&chars, &needle, index.saturating_add(needle.len()))
            {
                flush_plain(&mut out, &mut plain);
                let inner_text: String = chars
                    .iter()
                    .copied()
                    .skip(index.saturating_add(needle.len()))
                    .take(close.saturating_sub(index.saturating_add(needle.len())))
                    .collect();
                let inner = render_markdown_inline(&inner_text, depth.saturating_add(1));
                match kind {
                    PairKind::Strong => out.push(MdInline::Strong(inner)),
                    PairKind::Strike => out.push(MdInline::Strike(inner)),
                }
                index = close.saturating_add(needle.len());
                continue;
            }
        }
        // Conservative single emphasis with boundary checks.
        let delim = char_at(&chars, index);
        if delim == '*' || delim == '_' {
            let previous = if index == 0 {
                None
            } else {
                chars.get(index.saturating_sub(1)).copied()
            };
            let next = chars.get(index.saturating_add(1)).copied();
            let boundary_before = previous.is_none_or(is_boundary_before);
            let close = markdown_closing(&chars, &[delim], index.saturating_add(1));
            let after = close.and_then(|c| chars.get(c.saturating_add(1)).copied());
            let boundary_after = after.is_none_or(is_boundary_after);
            if boundary_before && next.is_some_and(|c| !c.is_whitespace()) && boundary_after {
                if let Some(close) = close {
                    flush_plain(&mut out, &mut plain);
                    let inner_text: String = chars
                        .iter()
                        .copied()
                        .skip(index.saturating_add(1))
                        .take(close.saturating_sub(index.saturating_add(1)))
                        .collect();
                    let inner = render_markdown_inline(&inner_text, depth.saturating_add(1));
                    out.push(MdInline::Em(inner));
                    index = close.saturating_add(1);
                    continue;
                }
            }
        }
        plain.push(char_at(&chars, index));
        index = index.saturating_add(1);
    }
    flush_plain(&mut out, &mut plain);
    out
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PairKind {
    Strong,
    Strike,
}

/// `flushPlain` — emit accumulated plain text as Text/Space tokens with the
/// non-breaking-space edge restoration (flex-item whitespace survival).
fn flush_plain(out: &mut Vec<MdInline>, plain: &mut String) {
    if plain.is_empty() {
        return;
    }
    let cleaned = markdown_plain_inline(plain);
    if !cleaned.is_empty() {
        let leading = plain.chars().next().is_some_and(char::is_whitespace);
        let trailing = plain.chars().last().is_some_and(char::is_whitespace);
        let mut text = String::new();
        if leading {
            text.push('\u{00a0}');
        }
        text.push_str(&cleaned);
        if trailing {
            text.push('\u{00a0}');
        }
        out.push(MdInline::Text(text));
    } else if plain.chars().any(char::is_whitespace) {
        out.push(MdInline::Space);
    }
    plain.clear();
}

/// The anchored link/image parse at `index` (label + target + total length).
fn parse_link(chars: &[char], index: usize) -> Option<(bool, Vec<char>, String, usize)> {
    let mut cursor = index;
    let image = chars.get(cursor) == Some(&'!');
    if image {
        cursor = cursor.saturating_add(1);
    }
    if chars.get(cursor) != Some(&'[') {
        return None;
    }
    let label_start = cursor.saturating_add(1);
    let mut label_end = None;
    for (offset, c) in chars.iter().enumerate().skip(label_start) {
        if *c == ']' {
            label_end = Some(offset);
            break;
        }
        if *c == '\n' {
            break;
        }
    }
    let label_end = label_end?;
    let label_len = label_end.saturating_sub(label_start);
    if label_len == 0 {
        return None;
    }
    if chars.get(label_end.saturating_add(1)) != Some(&'(') {
        return None;
    }
    let target_start = label_end.saturating_add(2);
    let mut target_end = None;
    for (offset, c) in chars.iter().enumerate().skip(target_start) {
        if *c == ')' || *c == '\n' {
            target_end = Some(offset);
            break;
        }
    }
    let target_end = target_end?;
    if target_end == target_start {
        return None;
    }
    if chars.get(target_end) != Some(&')') {
        return None;
    }
    let target: String = chars
        .iter()
        .copied()
        .skip(target_start)
        .take(target_end.saturating_sub(target_start))
        .collect();
    let label: Vec<char> = chars
        .iter()
        .copied()
        .skip(label_start)
        .take(label_len)
        .collect();
    let total_len = target_end.saturating_add(1).saturating_sub(index);
    Some((image, label, target.trim().to_owned(), total_len))
}

/// JS code-span edge cleanup: strip ONE leading space or ONE trailing space.
fn strip_code_edges(code: &str) -> String {
    let mut chars: Vec<char> = code.chars().collect();
    let leading_space = chars.first() == Some(&' ');
    if leading_space && chars.len() > 1 {
        let _removed: char = chars.remove(0);
    } else if chars.first() == Some(&' ') {
        return String::new();
    }
    if chars.last() == Some(&' ') {
        let _popped: Option<char> = chars.pop();
    }
    chars.into_iter().collect()
}

/// `renderMarkdownLine` — one source line with its compact semantic prefix.
pub(crate) fn render_markdown_line(value: &str) -> MdLine {
    let line = value.trim();
    if line.is_empty() {
        return MdLine {
            kind: MdLineKind::Plain,
            inline: Vec::new(),
        };
    }
    // Heading: /^(#{1,6})\s+(.+?)\s*#*$/
    if let Some((level, content)) = parse_heading(line) {
        return MdLine {
            kind: MdLineKind::Heading { level },
            inline: render_markdown_inline(&content, 0),
        };
    }
    // Task: /^[-+*]\s+\[([ xX])\]\s+(.+)$/
    if let Some((done, content)) = parse_task(line) {
        return MdLine {
            kind: MdLineKind::Task { done },
            inline: render_markdown_inline(&content, 0),
        };
    }
    // Unordered: /^[-+*]\s+(.+)$/
    if let Some(content) = parse_list(line) {
        return MdLine {
            kind: MdLineKind::Unordered,
            inline: render_markdown_inline(&content, 0),
        };
    }
    // Ordered: /^(\d+[.)])\s+(.+)$/
    if let Some((marker, content)) = parse_ordered(line) {
        return MdLine {
            kind: MdLineKind::Ordered { marker },
            inline: render_markdown_inline(&content, 0),
        };
    }
    // Quote: /^>\s*(.+)$/ with callout /^\[!([A-Za-z]+)\]\s*(.*)$/
    if let Some(content) = parse_quote(line) {
        if let Some((callout, rest)) = parse_callout(&content) {
            return MdLine {
                kind: MdLineKind::Quote {
                    callout: Some(callout),
                },
                inline: render_markdown_inline(&rest, 0),
            };
        }
        return MdLine {
            kind: MdLineKind::Quote { callout: None },
            inline: render_markdown_inline(&content, 0),
        };
    }
    // Fence: /^`{3,}\s*([A-Za-z0-9_+.-]*)\s*(.*)$/
    if let Some((language, content)) = parse_fence(line) {
        return MdLine {
            kind: MdLineKind::Fence { language },
            inline: render_markdown_inline(&content, 0),
        };
    }
    MdLine {
        kind: MdLineKind::Plain,
        inline: render_markdown_inline(line, 0),
    }
}

/// Heading parse with the exact JS backtracking-free semantics: 1..6 hashes,
/// whitespace, non-empty content, trailing whitespace+hashes stripped.
fn parse_heading(line: &str) -> Option<(u8, String)> {
    let chars: Vec<char> = line.chars().collect();
    let hashes = chars.iter().take_while(|c| **c == '#').count();
    if !(1..=6).contains(&hashes) || !chars.get(hashes).is_some_and(|c| c.is_whitespace()) {
        return None;
    }
    let mut cursor = hashes;
    while chars.get(cursor).is_some_and(|c| c.is_whitespace()) {
        cursor = cursor.saturating_add(1);
    }
    let mut content: Vec<char> = chars.iter().copied().skip(cursor).collect();
    // \s*#*$ — strip trailing whitespace and trailing hashes from the end.
    while content
        .last()
        .is_some_and(|c| c.is_whitespace() || *c == '#')
    {
        let _popped: Option<char> = content.pop();
    }
    if content.is_empty() {
        return None;
    }
    Some((u8::try_from(hashes).ok()?, content.into_iter().collect()))
}

fn parse_task(line: &str) -> Option<(bool, String)> {
    let chars: Vec<char> = line.chars().collect();
    if !matches!(chars.first(), Some('-' | '+' | '*')) {
        return None;
    }
    let mut cursor = 1;
    while chars.get(cursor).is_some_and(|c| c.is_whitespace()) {
        cursor = cursor.saturating_add(1);
    }
    if chars.get(cursor) != Some(&'[') {
        return None;
    }
    let state = chars.get(cursor.saturating_add(1)).copied();
    if !matches!(state, Some(' ' | 'x' | 'X')) || chars.get(cursor.saturating_add(2)) != Some(&']')
    {
        return None;
    }
    let mut after = cursor.saturating_add(3);
    if !chars.get(after).is_some_and(|c| c.is_whitespace()) {
        return None;
    }
    while chars.get(after).is_some_and(|c| c.is_whitespace()) {
        after = after.saturating_add(1);
    }
    let content: String = chars.iter().copied().skip(after).collect();
    if content.is_empty() {
        return None;
    }
    Some((matches!(state, Some('x' | 'X')), content))
}

fn parse_list(line: &str) -> Option<String> {
    let chars: Vec<char> = line.chars().collect();
    if !matches!(chars.first(), Some('-' | '+' | '*')) {
        return None;
    }
    let mut cursor = 1;
    if !chars.get(cursor).is_some_and(|c| c.is_whitespace()) {
        return None;
    }
    while chars.get(cursor).is_some_and(|c| c.is_whitespace()) {
        cursor = cursor.saturating_add(1);
    }
    let content: String = chars.iter().copied().skip(cursor).collect();
    if content.is_empty() {
        return None;
    }
    Some(content)
}

fn parse_ordered(line: &str) -> Option<(String, String)> {
    let chars: Vec<char> = line.chars().collect();
    let digits = chars.iter().take_while(|c| c.is_ascii_digit()).count();
    let sep = chars.get(digits).copied();
    if digits == 0 || !matches!(sep, Some('.' | ')')) {
        return None;
    }
    let mut cursor = digits.saturating_add(1);
    if !chars.get(cursor).is_some_and(|c| c.is_whitespace()) {
        return None;
    }
    while chars.get(cursor).is_some_and(|c| c.is_whitespace()) {
        cursor = cursor.saturating_add(1);
    }
    let content: String = chars.iter().copied().skip(cursor).collect();
    if content.is_empty() {
        return None;
    }
    let marker: String = chars
        .iter()
        .copied()
        .take(digits.saturating_add(1))
        .collect();
    Some((marker, content))
}

fn parse_quote(line: &str) -> Option<String> {
    let chars: Vec<char> = line.chars().collect();
    if chars.first() != Some(&'>') {
        return None;
    }
    let mut cursor = 1;
    while chars.get(cursor).is_some_and(|c| c.is_whitespace()) {
        cursor = cursor.saturating_add(1);
    }
    let content: String = chars.iter().copied().skip(cursor).collect();
    if content.is_empty() {
        return None;
    }
    Some(content)
}

/// `/^\[!([A-Za-z]+)\]\s*(.*)$/` on the quote content.
fn parse_callout(quote: &str) -> Option<(String, String)> {
    let chars: Vec<char> = quote.chars().collect();
    if chars.first() != Some(&'[') || chars.get(1) != Some(&'!') {
        return None;
    }
    let letters = chars
        .iter()
        .skip(2)
        .take_while(|c| c.is_ascii_alphabetic())
        .count();
    if letters == 0 || chars.get(2usize.saturating_add(letters)) != Some(&']') {
        return None;
    }
    let mut cursor = 2usize.saturating_add(letters).saturating_add(1);
    while chars.get(cursor).is_some_and(|c| c.is_whitespace()) {
        cursor = cursor.saturating_add(1);
    }
    let callout: String = chars.iter().copied().skip(2).take(letters).collect();
    let rest: String = chars.iter().copied().skip(cursor).collect();
    Some((callout, rest))
}

/// ``/^`{3,}\s*([A-Za-z0-9_+.-]*)\s*(.*)$/`` — language may be empty.
fn parse_fence(line: &str) -> Option<(String, String)> {
    let chars: Vec<char> = line.chars().collect();
    let ticks = chars.iter().take_while(|c| **c == '`').count();
    if ticks < 3 {
        return None;
    }
    let mut cursor = ticks;
    while chars.get(cursor).is_some_and(|c| c.is_whitespace()) {
        cursor = cursor.saturating_add(1);
    }
    let language_start = cursor;
    while chars
        .get(cursor)
        .is_some_and(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '+' | '-' | '.'))
    {
        cursor = cursor.saturating_add(1);
    }
    let language: String = chars
        .iter()
        .copied()
        .skip(language_start)
        .take(cursor.saturating_sub(language_start))
        .collect();
    while chars.get(cursor).is_some_and(|c| c.is_whitespace()) {
        cursor = cursor.saturating_add(1);
    }
    let content: String = chars.iter().copied().skip(cursor).collect();
    Some((language, content))
}

// --- Tool-payload display compaction (displaySummaryForRow) -----------------

/// The JS truthiness check for a summary source (`row.summary || …`).
fn summary_source(row: &Value) -> String {
    match row.get("summary") {
        Some(value) if js_truthy(value) => js_string(value),
        _ => "(no summary)".to_owned(),
    }
}

/// `toolishRow` — the row shapes whose payloads get compacted.
fn toolish_row(row: &Value) -> bool {
    let record_role = row_str(row, "record_role");
    let kind = row_str(row, "kind");
    let is_system = wire::bool(row, "is_system");
    let activity_kind = row_str(row, "activity_kind");
    let is_action_or_tool =
        record_role == "action" || record_role == "result" || kind == "tool" || kind == "command";
    let operational =
        activity_kind == "execute" || is_system || kind == "tool" || kind == "command";
    is_action_or_tool && operational
}

/// JSON-ish start test (`jsonish` regex) on a payload.
fn jsonish(payload: &str) -> bool {
    let mut chars = payload.chars().filter(|c| !c.is_whitespace());
    match chars.next() {
        Some('[') => matches!(chars.next(), Some('{' | '"' | ']')),
        Some('{') => matches!(chars.next(), Some('"' | '}')),
        Some('"') => true,
        _ => false,
    }
}

/// The nested-envelope JSON-ish start test (no bare-string alternative).
fn nested_jsonish(payload: &str) -> bool {
    let mut chars = payload.chars().filter(|c| !c.is_whitespace());
    match chars.next() {
        Some('[') => matches!(chars.next(), Some('{' | '"' | ']')),
        Some('{') => matches!(chars.next(), Some('"' | '}')),
        _ => false,
    }
}

/// `decodedToolPayloadText` — decode a JSON tool payload with the narrow
/// truncated-summary recovery path.
fn decoded_tool_payload_text(value: &str) -> String {
    let source = value.trim();
    match serde_json::from_str::<Value>(source) {
        Ok(parsed) => {
            // Some adapters serialize a JSON envelope as a JSON string; unwrap
            // at most once (the JS `try` also covers this parse).
            if let Value::String(inner) = &parsed {
                let trimmed = inner.trim();
                if nested_jsonish(trimmed) {
                    match serde_json::from_str::<Value>(trimmed) {
                        Ok(inner_value) => return first_tool_payload_text(&inner_value),
                        Err(_) => return recovery_text(source),
                    }
                }
            }
            first_tool_payload_text(&parsed)
        }
        Err(_) => recovery_text(source),
    }
}

/// BFS over the tool envelope's text-bearing keys with the visit budget.
fn first_tool_payload_text(value: &Value) -> String {
    let mut pending: VecDeque<&Value> = VecDeque::new();
    pending.push_back(value);
    let mut visits = 0usize;
    while let Some(current) = pending.pop_front() {
        visits = visits.saturating_add(1);
        if visits >= 48 {
            break;
        }
        if let Value::String(text) = current {
            if !text.trim().is_empty() {
                return text.clone();
            }
        }
        match current {
            Value::Array(items) => {
                for item in items {
                    pending.push_back(item);
                }
            }
            Value::Object(map) => {
                for key in TOOL_PAYLOAD_TEXT_KEYS {
                    if let Some(item) = map.get(key) {
                        pending.push_back(item);
                    }
                }
            }
            Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
        }
    }
    String::new()
}

/// The truncated-summary recovery path (regex + escape recovery in main.js).
fn recovery_text(source: &str) -> String {
    let chars: Vec<char> = source.chars().collect();
    let mut index = 0usize;
    while index < chars.len() {
        if chars.get(index) == Some(&'"') {
            let mut cursor = index.saturating_add(1);
            // Keyword match: one of TOOL_PAYLOAD_TEXT_KEYS followed by `"`.
            let mut keyword = None;
            for key in TOOL_PAYLOAD_TEXT_KEYS {
                let key_chars: Vec<char> = key.chars().collect();
                let matches = key_chars
                    .iter()
                    .enumerate()
                    .all(|(k, c)| chars.get(cursor.saturating_add(k)) == Some(c));
                if matches && chars.get(cursor.saturating_add(key_chars.len())) == Some(&'"') {
                    keyword = Some(key);
                    break;
                }
            }
            if let Some(keyword) = keyword {
                cursor = cursor
                    .saturating_add(keyword.chars().count())
                    .saturating_add(1);
                while chars.get(cursor).is_some_and(|c| c.is_whitespace()) {
                    cursor = cursor.saturating_add(1);
                }
                if chars.get(cursor) == Some(&':') {
                    cursor = cursor.saturating_add(1);
                    while chars.get(cursor).is_some_and(|c| c.is_whitespace()) {
                        cursor = cursor.saturating_add(1);
                    }
                    if chars.get(cursor) == Some(&'"') {
                        cursor = cursor.saturating_add(1);
                        let mut encoded = String::new();
                        let mut done = false;
                        while cursor < chars.len() {
                            let c = char_at(&chars, cursor);
                            if c == '\\' {
                                if let Some(&next) = chars.get(cursor.saturating_add(1)) {
                                    encoded.push(c);
                                    encoded.push(next);
                                    cursor = cursor.saturating_add(2);
                                    continue;
                                }
                            } else if c == '"' {
                                done = true;
                                break;
                            }
                            encoded.push(c);
                            cursor = cursor.saturating_add(1);
                        }
                        if done {
                            return unescape_recovered(&encoded);
                        }
                    }
                }
            }
        }
        index = index.saturating_add(1);
    }
    String::new()
}

/// The three ordered escape recoveries (JS `replace` chain).
fn unescape_recovered(encoded: &str) -> String {
    let mut out = encoded
        .replace("\\r\\n", "\n")
        .replace("\\n", "\n")
        .replace("\\r", "\n");
    out = out.replace("\\\"", "\"");
    out.replace("\\\\", "\\")
}

/// `conciseToolText` — reduce multi-line execution envelopes to their first
/// meaningful status or output line.
fn concise_tool_text(value: &str) -> String {
    let normalized = value.replace("\r\n", "\n").replace('\r', "\n");
    let lines: Vec<&str> = normalized
        .split('\n')
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect();
    let status = lines.iter().find(|line| is_script_status(line)).copied();
    if status.is_some_and(|line| script_status_kind(line) == Some("completed")) {
        return "Script completed".to_owned();
    }
    if status.is_some_and(|line| script_status_kind(line) == Some("failed")) {
        return "Script failed".to_owned();
    }
    if status.is_some_and(|line| script_status_kind(line) == Some("running")) {
        return "Script running".to_owned();
    }
    match lines.first() {
        Some(first) => first.split_whitespace().collect::<Vec<&str>>().join(" "),
        None => String::new(),
    }
}

/// `/^Script\s+(?:completed|failed|running)\b/i` test.
fn is_script_status(line: &str) -> bool {
    script_status_kind(line).is_some()
}

fn script_status_kind(line: &str) -> Option<&'static str> {
    let lower = line.to_ascii_lowercase();
    let rest = lower.strip_prefix("script")?;
    let rest = rest.trim_start();
    for (word, kind) in [
        ("completed", "completed"),
        ("failed", "failed"),
        ("running", "running"),
    ] {
        if let Some(after) = rest.strip_prefix(word) {
            // \b — the following char must not be a word char.
            if after.chars().next().is_none_or(|c| !is_word_char(c)) {
                return Some(kind);
            }
        }
    }
    None
}

fn is_word_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

/// `displaySummaryForRow` — presentation-only compaction of action/result
/// payloads. Human narrative is left untouched for Markdown rendering.
pub(crate) fn display_summary_for_row(row: &Value, value: &str) -> String {
    let source = value.to_owned();
    let toolish = toolish_row(row);
    let trimmed = source.trim();
    // Leading inert container tag (`<…>`), as in main.js.
    let container_len = leading_container_tag_len(trimmed);
    let candidate = if let Some(len) = container_len {
        trimmed.get(len..).unwrap_or("").trim().to_owned()
    } else {
        trimmed.to_owned()
    };
    // `tool:` wrapper prefix.
    let wrapper_len = tool_wrapper_len(&candidate);
    let payload = if let Some(len) = wrapper_len {
        candidate.get(len..).unwrap_or("").trim().to_owned()
    } else {
        candidate.clone()
    };
    let jsonish = jsonish(&payload);
    if !toolish && !jsonish {
        return source;
    }
    if toolish && wrapper_len.is_none() && !jsonish {
        return source;
    }
    let decoded = if jsonish {
        decoded_tool_payload_text(&payload)
    } else {
        payload
    };
    let concise = concise_tool_text(&decoded);
    if !concise.is_empty() {
        return concise;
    }
    // A narrative JSON sample with no recognized envelope stays authored
    // content; generic labels are reserved for operational rows.
    if !toolish {
        return source;
    }
    match row_str(row, "outcome").as_str() {
        "success" => "Completed".to_owned(),
        "failure" => "Failed".to_owned(),
        "warning" => "Completed with warnings".to_owned(),
        "cancelled" => "Cancelled".to_owned(),
        _ => {
            let record_role = row_str(row, "record_role");
            let kind = row_str(row, "kind");
            if record_role == "action" || kind == "command" {
                "Tool request".to_owned()
            } else {
                "Tool result".to_owned()
            }
        }
    }
}

/// `/^<[A-Za-z][A-Za-z0-9_.:-]*>\s*/` — one leading inert container tag.
fn leading_container_tag_len(trimmed: &str) -> Option<usize> {
    let chars: Vec<char> = trimmed.chars().collect();
    if chars.first() != Some(&'<') {
        return None;
    }
    let first = chars.get(1).copied()?;
    if !first.is_ascii_alphabetic() {
        return None;
    }
    let mut cursor = 2usize;
    while let Some(c) = chars.get(cursor).copied() {
        if c == '>' {
            let mut after = cursor.saturating_add(1);
            while chars.get(after).is_some_and(|c| c.is_whitespace()) {
                after = after.saturating_add(1);
            }
            return Some(after);
        }
        if !(c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | ':' | '-')) {
            return None;
        }
        cursor = cursor.saturating_add(1);
    }
    None
}

/// `/^tool:\s*[A-Za-z0-9_.:-]+(?:\s+|$)/i` — consumed-prefix length.
fn tool_wrapper_len(candidate: &str) -> Option<usize> {
    let chars: Vec<char> = candidate.chars().collect();
    let mut lower = chars.iter().map(char::to_ascii_lowercase);
    if !lower.by_ref().take(4).eq("tool".chars()) {
        return None;
    }
    if chars.get(4) != Some(&':') {
        return None;
    }
    let mut cursor = 5usize;
    let mut name_len = 0usize;
    while let Some(c) = chars.get(cursor).copied() {
        if !(c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | ':' | '-')) {
            break;
        }
        name_len = name_len.saturating_add(1);
        cursor = cursor.saturating_add(1);
    }
    if name_len == 0 {
        return None;
    }
    let after = chars.get(cursor).copied();
    let at_end = cursor == chars.len();
    let has_space = after.is_some_and(char::is_whitespace);
    if !at_end && !has_space {
        return None;
    }
    let mut consumed = cursor;
    if has_space {
        while chars.get(consumed).is_some_and(|c| c.is_whitespace()) {
            consumed = consumed.saturating_add(1);
        }
    }
    Some(consumed)
}

// --- Badge / chrome layer ---------------------------------------------------

/// One assembled row tag (DOM-ready exact class list, text, and labels).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ChromeItem {
    /// Exact production class list (e.g. `"rel-badge rel-subagent"`).
    pub(crate) classes: String,
    /// Display text (relation glyph + label, outcome, prefix, or count).
    pub(crate) text: String,
    /// Hover title.
    pub(crate) title: String,
    /// `aria-label` where production supplies one.
    pub(crate) aria_label: Option<String>,
}

impl ChromeItem {
    fn new(classes: &str, text: &str, title: &str, aria_label: Option<&str>) -> ChromeItem {
        ChromeItem {
            classes: classes.to_owned(),
            text: text.to_owned(),
            title: title.to_owned(),
            aria_label: aria_label.map(str::to_owned),
        }
    }
}

/// The `REL_LABELS` table (structural parent-relation kinds).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RelationKind {
    Subagent,
    Reconnect,
    Fork,
}

impl RelationKind {
    pub(crate) fn from_wire(kind: &str) -> Option<RelationKind> {
        match kind {
            "subagent" => Some(RelationKind::Subagent),
            "reconnect" => Some(RelationKind::Reconnect),
            "fork" => Some(RelationKind::Fork),
            _ => None,
        }
    }

    pub(crate) fn class(self) -> &'static str {
        match self {
            RelationKind::Subagent => "rel-subagent",
            RelationKind::Reconnect => "rel-reconnect",
            RelationKind::Fork => "rel-fork",
        }
    }

    pub(crate) fn glyph(self) -> &'static str {
        match self {
            RelationKind::Subagent => "↳",
            RelationKind::Reconnect => "↩",
            RelationKind::Fork => "⇉",
        }
    }

    pub(crate) fn text(self) -> &'static str {
        match self {
            RelationKind::Subagent => "subagent",
            RelationKind::Reconnect => "return",
            RelationKind::Fork => "fork",
        }
    }

    pub(crate) fn title(self) -> &'static str {
        match self {
            RelationKind::Subagent => "Starts a subagent branch",
            RelationKind::Reconnect => "Completion returns into the subagent branch",
            RelationKind::Fork => "Branches off the target row at a fork boundary",
        }
    }
}

/// `relationBadges` — compact badges for a row's parent relations, de-duped.
pub(crate) fn relation_badges(row: &Value) -> Vec<ChromeItem> {
    let Some(relations) = row.get("parent_relations").and_then(Value::as_array) else {
        return Vec::new();
    };
    let mut seen: Vec<RelationKind> = Vec::new();
    let mut out = Vec::new();
    for relation in relations {
        let kind = relation.get("kind").and_then(Value::as_str).unwrap_or("");
        let Some(kind) = RelationKind::from_wire(kind) else {
            continue;
        };
        if seen.contains(&kind) {
            continue;
        }
        seen.push(kind);
        out.push(ChromeItem::new(
            &format!("rel-badge {}", kind.class()),
            &format!("{} {}", kind.glyph(), kind.text()),
            kind.title(),
            Some(kind.title()),
        ));
    }
    out
}

/// Provider-neutral activities with concise labels for the dedicated Activity
/// column. Conversation rows are refined to `agent` or `user` from their
/// author; the remaining values are presentation labels rather than badges.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ActivityKind {
    Work,
    Conversation,
    Plan,
    Explore,
    Execute,
    Change,
    Verify,
    Diagnose,
    Coordinate,
    SourceControl,
    External,
    System,
}

impl ActivityKind {
    pub(crate) fn from_wire(kind: &str) -> Option<ActivityKind> {
        match kind {
            "work" => Some(ActivityKind::Work),
            "conversation" => Some(ActivityKind::Conversation),
            "plan" => Some(ActivityKind::Plan),
            "explore" => Some(ActivityKind::Explore),
            "execute" => Some(ActivityKind::Execute),
            "change" => Some(ActivityKind::Change),
            "verify" => Some(ActivityKind::Verify),
            "diagnose" => Some(ActivityKind::Diagnose),
            "coordinate" => Some(ActivityKind::Coordinate),
            "source_control" => Some(ActivityKind::SourceControl),
            "external" => Some(ActivityKind::External),
            "system" => Some(ActivityKind::System),
            _ => None,
        }
    }

    pub(crate) fn text(self) -> &'static str {
        match self {
            ActivityKind::Work => "work",
            ActivityKind::Conversation => "agent",
            ActivityKind::Plan => "plan",
            ActivityKind::Explore => "explore",
            ActivityKind::Execute => "tooluse",
            ActivityKind::Change => "change",
            ActivityKind::Verify => "verify",
            ActivityKind::Diagnose => "diagnose",
            ActivityKind::Coordinate => "coordinate",
            ActivityKind::SourceControl => "git",
            ActivityKind::External => "external",
            ActivityKind::System => "system",
        }
    }
}

/// One resolved value for the dedicated Activity column. Every real row gets
/// a non-empty label; placeholders intentionally retain the empty default.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct RowClassification {
    /// Compact visual label (`tooluse`, `git`, `agent`, `user`, or a wire fallback).
    pub(crate) label: String,
    /// Field used to resolve the label (`activity_kind`, `kind`, etc.).
    pub(crate) source: String,
    /// Hover/accessibility description retaining the unshortened wire value.
    pub(crate) title: String,
}

impl RowClassification {
    fn new(label: &str, source: &str, wire_value: &str) -> RowClassification {
        let source_title = match source {
            "activity_kind" => "Activity",
            "kind" => "Kind",
            "record_role" => "Record role",
            "is_system" => "System classification",
            "git_oid" => "Git identity",
            _ => "Classification",
        };
        RowClassification {
            label: label.to_owned(),
            source: source.to_owned(),
            title: format!("{source_title}: {wire_value}"),
        }
    }

    fn session_summary() -> RowClassification {
        RowClassification {
            label: "session".to_owned(),
            source: "session_summary".to_owned(),
            title: "Session summary".to_owned(),
        }
    }
}

/// Whether a wire token is useful as a visible classification fallback.
fn classification_token(value: &str) -> Option<&str> {
    let trimmed = value.trim();
    (!trimmed.is_empty() && trimmed != "unknown").then_some(trimmed)
}

/// Turn a provider-neutral wire token into a compact display fallback.
fn classification_fallback(value: &str) -> String {
    value.trim().replace('_', "-")
}

/// Resolve the stable Activity-column value. Semantic activity wins, except
/// that Git identity is authoritative even for sparse/older rows; then kind,
/// record role, and a final `other` label guarantee a populated real row.
pub(crate) fn row_classification(row: &Value) -> RowClassification {
    let kind = row_str(row, "kind");
    let activity = row_str(row, "activity_kind");
    let git_oid = row_str(row, "git_oid");
    if !git_oid.is_empty() || kind == "git" {
        return if activity == "source_control" {
            RowClassification::new("git", "activity_kind", "source_control")
        } else if !git_oid.is_empty() {
            RowClassification::new("git", "git_oid", &git_oid)
        } else {
            RowClassification::new("git", "kind", "git")
        };
    }

    if let Some(activity_kind) = ActivityKind::from_wire(activity.trim()) {
        let label = if activity_kind == ActivityKind::Conversation
            && matches!(row_str(row, "author").trim(), "human" | "user")
        {
            "user"
        } else {
            activity_kind.text()
        };
        return RowClassification::new(label, "activity_kind", activity.trim());
    }

    if wire::bool(row, "is_system") {
        return RowClassification::new("system", "is_system", "true");
    }
    if let Some(kind) = classification_token(&kind) {
        return RowClassification::new(&classification_fallback(kind), "kind", kind);
    }

    let role = row_str(row, "record_role");
    if let Some(role) = classification_token(&role) {
        return RowClassification::new(&classification_fallback(role), "record_role", role);
    }

    RowClassification::new("other", "fallback", "other")
}

/// The `OUTCOME_LABELS` whitelist.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OutcomeKind {
    Success,
    Warning,
    Failure,
    Cancelled,
}

impl OutcomeKind {
    pub(crate) fn from_wire(kind: &str) -> Option<OutcomeKind> {
        match kind {
            "success" => Some(OutcomeKind::Success),
            "warning" => Some(OutcomeKind::Warning),
            "failure" => Some(OutcomeKind::Failure),
            "cancelled" => Some(OutcomeKind::Cancelled),
            _ => None,
        }
    }

    pub(crate) fn class(self) -> &'static str {
        match self {
            OutcomeKind::Success => "outcome-success",
            OutcomeKind::Warning => "outcome-warning",
            OutcomeKind::Failure => "outcome-failure",
            OutcomeKind::Cancelled => "outcome-neutral",
        }
    }

    pub(crate) fn text(self) -> &'static str {
        match self {
            OutcomeKind::Success => "ok",
            OutcomeKind::Warning => "warn",
            OutcomeKind::Failure => "✕",
            OutcomeKind::Cancelled => "cancelled",
        }
    }

    pub(crate) fn aria(self) -> &'static str {
        match self {
            OutcomeKind::Failure => "failed",
            other @ (OutcomeKind::Success | OutcomeKind::Warning | OutcomeKind::Cancelled) => {
                other.text()
            }
        }
    }
}

/// `outcomeBadge` with the frozen visibility switches.
pub(crate) fn outcome_badge(row: &Value, options: BadgeOptions) -> Option<ChromeItem> {
    let wire_outcome = row_str(row, "outcome");
    if wire_outcome == "success" && !options.show_success_outcome {
        return None;
    }
    let outcome = OutcomeKind::from_wire(&wire_outcome)?;
    Some(ChromeItem::new(
        &format!("out-badge {}", outcome.class()),
        outcome.text(),
        &format!("outcome: {wire_outcome}"),
        Some(&format!("outcome: {}", outcome.aria())),
    ))
}

/// `relationBadges` + a consequential outcome badge. Structural relations
/// take over the semantic-tag slot; Activity remains in its own column.
fn relation_chrome(row: &Value, options: BadgeOptions) -> Vec<ChromeItem> {
    let mut items = relation_badges(row);
    if !items.is_empty() {
        let outcome = row_str(row, "outcome");
        if matches!(outcome.as_str(), "warning" | "failure" | "cancelled") {
            if let Some(badge) = outcome_badge(row, options) {
                items.push(badge);
            }
        }
    }
    items
}

/// `rowSemanticChrome` — restrained row tags. Activity is rendered in its own
/// grid column and is intentionally absent here.
pub(crate) fn row_semantic_chrome(
    row: &Value,
    is_bundle: bool,
    view: ViewMode,
    options: BadgeOptions,
) -> Vec<ChromeItem> {
    if is_bundle {
        return bundle_chrome(row, view, options);
    }
    let relations = relation_chrome(row, options);
    if !relations.is_empty() {
        return relations;
    }
    let mut items = Vec::new();
    if let Some(outcome) = outcome_badge(row, options) {
        items.push(outcome);
    }
    items
}

// --- Activity work-unit / bundle / promotion layer --------------------------

/// The raw `work_unit` payload of one Activity row.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct WorkUnitData {
    pub(crate) id: String,
    pub(crate) title: Option<String>,
    pub(crate) count: Option<u64>,
    pub(crate) is_start: bool,
    pub(crate) is_end: bool,
}

/// The additive `session_summary` payload present on one Activity row per
/// session group.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct SessionSummaryData {
    pub(crate) count: Option<u64>,
}

/// Compact two-line header metadata for a work-unit start row.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct WorkUnitHeader {
    pub(crate) id: String,
    pub(crate) title: String,
    pub(crate) show_count: bool,
    pub(crate) count_text: String,
    pub(crate) count_title: String,
    pub(crate) title_only: bool,
}

/// `WORK_UNIT_FALLBACK_LABELS` — short human labels keyed by activity kind.
fn work_unit_fallback(activity_kind: &str) -> Option<&'static str> {
    match activity_kind {
        "work" => Some("Work"),
        "execute" => Some("Run"),
        "change" => Some("Change"),
        "verify" => Some("Verify"),
        "plan" => Some("Plan"),
        "explore" => Some("Explore"),
        "diagnose" => Some("Diagnose"),
        "coordinate" => Some("Coordinate"),
        "source_control" => Some("Git"),
        "external" => Some("External"),
        "system" => Some("System"),
        _ => None,
    }
}

/// `workUnitOf` — the row's Activity work-unit payload (never on sub-ops).
pub(crate) fn work_unit_of(row: &Value, view: ViewMode) -> Option<WorkUnitData> {
    if view != ViewMode::Activity || wire::bool(row, "is_subop") {
        return None;
    }
    let wu = row.get("work_unit").filter(|value| js_truthy(value))?;
    Some(WorkUnitData {
        id: wu
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned(),
        title: wu
            .get("title")
            .and_then(Value::as_str)
            .filter(|title| !title.is_empty())
            .map(str::to_owned),
        count: wu.get("count").and_then(Value::as_u64),
        is_start: wu.get("is_start").and_then(Value::as_bool).unwrap_or(false),
        is_end: wu.get("is_end").and_then(Value::as_bool).unwrap_or(false),
    })
}

/// Read the explicit whole-session marker emitted on the session's true newest
/// visible row. Work-unit identity is intentionally irrelevant: Codex mixes
/// turn-scoped rows with session-scoped lifecycle rows.
pub(crate) fn session_summary_of(row: &Value, view: ViewMode) -> Option<SessionSummaryData> {
    if view != ViewMode::Activity || wire::bool(row, "is_subop") {
        return None;
    }
    let summary = row
        .get("session_summary")
        .filter(|value| js_truthy(value))?;
    Some(SessionSummaryData {
        count: summary.get("count").and_then(Value::as_u64),
    })
}

/// `workUnitTitle` — DTO title's first meaningful line, else human fallbacks.
pub(crate) fn work_unit_title(row: &Value, view: ViewMode) -> String {
    if let Some(wu) = work_unit_of(row, view) {
        if let Some(title) = wu.title {
            let first_line = title
                .replace("\r\n", "\n")
                .replace('\r', "\n")
                .split('\n')
                .map(markdown_plain_line)
                .find(|line| !line.is_empty());
            if let Some(first) = first_line {
                return first;
            }
        }
    }
    let activity_kind = row_str(row, "activity_kind");
    if let Some(fallback) = work_unit_fallback(&activity_kind) {
        return fallback.to_owned();
    }
    let kind = row_str(row, "kind");
    if kind == "message" || kind == "command" {
        return "Request".to_owned();
    }
    group_label_text(&row_str(row, "group"), None, None)
}

/// `workUnitCountText` — human count text for a unit header.
pub(crate) fn work_unit_count_text(count: u64) -> String {
    format!("{count} entr{}", if count == 1 { "y" } else { "ies" })
}

/// `showWorkUnitCount` — keep grouping counts sparse.
pub(crate) fn show_work_unit_count(row: &Value, wu: &WorkUnitData) -> bool {
    wu.count.is_some_and(|count| count > 1) && row_str(row, "activity_kind") != "source_control"
}

/// `workUnitCountTitle` — tooltip explaining what the count measures.
pub(crate) fn work_unit_count_title(count: u64) -> String {
    format!("{} grouped in this activity", work_unit_count_text(count))
}

/// Tooltip for the whole-session count shown on a session-summary row.
pub(crate) fn session_summary_count_title(count: u64) -> String {
    format!("{} in this session", work_unit_count_text(count))
}

/// One session-provenance chip (`sessionMetaValues` item).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SessionChip {
    pub(crate) class: String,
    pub(crate) label: String,
    pub(crate) title: String,
}

/// `sessionMetaValues` — the small, display-safe session provenance.
pub(crate) fn session_meta_values(row: &Value) -> Vec<SessionChip> {
    let mut out = Vec::new();
    let Some(meta) = row.get("session_meta").filter(|value| js_truthy(value)) else {
        return out;
    };
    if let Some(provider) = meta.get("model_provider").and_then(Value::as_str) {
        let trimmed = provider.trim();
        if !trimmed.is_empty() {
            out.push(SessionChip {
                class: "session-chip session-chip-model".to_owned(),
                label: trimmed.to_owned(),
                title: "Model provider".to_owned(),
            });
        }
    }
    if let Some(agent) = meta.get("agent_nickname").and_then(Value::as_str) {
        let trimmed = agent.trim();
        if !trimmed.is_empty() {
            out.push(SessionChip {
                class: "session-chip session-chip-agent".to_owned(),
                label: trimmed.to_owned(),
                title: "Agent".to_owned(),
            });
        }
    }
    out
}

/// `sessionMetaDescription` — accessible prose for the visible chips.
pub(crate) fn session_meta_description(row: &Value) -> String {
    session_meta_values(row)
        .iter()
        .map(|chip| format!("{} {}", chip.title, chip.label))
        .collect::<Vec<String>>()
        .join(", ")
}

/// The typed Activity-bundle kinds the renderer understands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BundleKind {
    WorkGroup,
    ExecuteRun,
    PlanRepeat,
}

impl BundleKind {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            BundleKind::WorkGroup => "work-group",
            BundleKind::ExecuteRun => "execute-run",
            BundleKind::PlanRepeat => "plan-repeat",
        }
    }

    pub(crate) fn from_wire(kind: &str) -> Option<BundleKind> {
        match kind {
            "work-group" => Some(BundleKind::WorkGroup),
            "execute-run" => Some(BundleKind::ExecuteRun),
            "plan-repeat" => Some(BundleKind::PlanRepeat),
            _ => None,
        }
    }
}

/// `activityBundleKind` — the recognized typed bundle kind of a row.
pub(crate) fn activity_bundle_kind(row: &Value, view: ViewMode) -> Option<BundleKind> {
    if view != ViewMode::Activity {
        return None;
    }
    let bundle = row
        .get("activity_bundle")
        .filter(|value| js_truthy(value))?;
    let kind = bundle.get("kind").and_then(Value::as_str).unwrap_or("");
    BundleKind::from_wire(kind)
}

/// `isActivityBundle` — any recognized typed bundle row.
#[cfg(test)]
pub(crate) fn is_activity_bundle(row: &Value, view: ViewMode) -> bool {
    activity_bundle_kind(row, view).is_some()
}

/// `isExecuteRunBundle`.
pub(crate) fn is_execute_run_bundle(row: &Value, view: ViewMode) -> bool {
    activity_bundle_kind(row, view) == Some(BundleKind::ExecuteRun)
}

/// `isPlanRepeatBundle`.
#[cfg(test)]
pub(crate) fn is_plan_repeat_bundle(row: &Value, view: ViewMode) -> bool {
    activity_bundle_kind(row, view) == Some(BundleKind::PlanRepeat)
}

/// Typed bundle row metadata (count only from the DTO, never the summary).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BundleInfo {
    pub(crate) kind: BundleKind,
    pub(crate) member_count: Option<u64>,
    pub(crate) count_text: String,
    pub(crate) status_success: bool,
}

/// `bundleCountText` — one concise label for a typed bundle row.
pub(crate) fn bundle_count_text(row: &Value, view: ViewMode) -> String {
    let Some(kind) = activity_bundle_kind(row, view) else {
        return String::new();
    };
    let Some(member_count) = row
        .get("activity_bundle")
        .and_then(|bundle| bundle.get("member_count"))
        .and_then(Value::as_u64)
    else {
        return String::new();
    };
    if kind == BundleKind::WorkGroup {
        return format!(
            "{member_count} activit{}",
            if member_count == 1 { "y" } else { "ies" }
        );
    }
    if kind == BundleKind::PlanRepeat {
        return format!(
            "{member_count} update{}",
            if member_count == 1 { "" } else { "s" }
        );
    }
    let command = row_str(row, "kind") == "command";
    if command {
        format!(
            "{member_count} command{}",
            if member_count == 1 { "" } else { "s" }
        )
    } else {
        format!(
            "{member_count} tool step{}",
            if member_count == 1 { "" } else { "s" }
        )
    }
}

/// `bundleChrome` — count label plus the execute-specific status glyph.
pub(crate) fn bundle_chrome(
    row: &Value,
    view: ViewMode,
    _options: BadgeOptions,
) -> Vec<ChromeItem> {
    let count_text = bundle_count_text(row, view);
    if count_text.is_empty() {
        return Vec::new();
    }
    let success = is_execute_run_bundle(row, view) && row_str(row, "outcome") == "success";
    let title = if success {
        format!("{count_text}, completed")
    } else {
        count_text.clone()
    };
    let mut items = vec![ChromeItem::new("bundle-count", &count_text, &title, None)];
    if success {
        items.push(ChromeItem::new(
            "bundle-status bundle-status-success",
            "✓",
            "completed",
            Some("completed"),
        ));
    }
    items
}

/// Promotion rails: strong accent or quiet rail, never a badge.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PromotedKind {
    Failure,
    Warning,
    Cancelled,
    Change,
    Verify,
    Rail,
}

impl PromotedKind {
    pub(crate) fn classes(self) -> &'static str {
        match self {
            PromotedKind::Failure => "row-promoted row-promoted-failure",
            PromotedKind::Warning => "row-promoted row-promoted-warning",
            PromotedKind::Cancelled => "row-promoted row-promoted-cancelled",
            PromotedKind::Change => "row-promoted row-promoted-change",
            PromotedKind::Verify => "row-promoted row-promoted-verify",
            PromotedKind::Rail => "row-promoted row-promoted-rail",
        }
    }
}

/// `promotedClasses` for the Activity view (sub-ops are never promoted).
pub(crate) fn promoted_kind(row: &Value, view: ViewMode) -> Option<PromotedKind> {
    let promoted = view == ViewMode::Activity
        && !wire::bool(row, "is_subop")
        && row.get("promoted").and_then(Value::as_bool) == Some(true);
    if !promoted {
        return None;
    }
    match row_str(row, "outcome").as_str() {
        "failure" => Some(PromotedKind::Failure),
        "warning" => Some(PromotedKind::Warning),
        "cancelled" => Some(PromotedKind::Cancelled),
        _ => match row_str(row, "activity_kind").as_str() {
            "change" => Some(PromotedKind::Change),
            "verify" => Some(PromotedKind::Verify),
            _ => Some(PromotedKind::Rail),
        },
    }
}

/// `subopIcon` — Codicon glyph name for a sub-op semantic class.
pub(crate) fn subop_icon(subop_kind: &str) -> &'static str {
    match subop_kind {
        "edit" => "edit",
        "msg" => "comment",
        "tool_result" => "output",
        _ => "info",
    }
}

/// `hasSubOps` — whether a top-level row carries bundled metadata sub-ops.
pub(crate) fn has_sub_ops(row: &Value) -> bool {
    row.get("sub_ops")
        .and_then(Value::as_array)
        .is_some_and(|sub_ops| !sub_ops.is_empty())
}

// --- RowSpec assembly (buildRowHtml's presentation model) -------------------

/// The row's leading kind class (`row-tool` / `row-dim` / none).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum KindClass {
    #[default]
    Plain,
    Tool,
    Dim,
}

/// Work-unit boundary role class (`row-work-unit-start` / `-end`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WorkUnitClass {
    Start,
    End,
}

impl WorkUnitClass {
    #[cfg(test)]
    pub(crate) fn class(self) -> &'static str {
        match self {
            WorkUnitClass::Start => "row-work-unit-start",
            WorkUnitClass::End => "row-work-unit-end",
        }
    }
}

/// Row booleans grouped to keep the structs under the bool-count gate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct RowFlags {
    pub(crate) human: bool,
    pub(crate) selected: bool,
    pub(crate) find_current: bool,
}

/// Row state booleans grouped under the same gate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct StateFlags {
    pub(crate) group_start: bool,
    pub(crate) expandable: bool,
}

/// Content-cell class booleans.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct ContentFlags {
    pub(crate) work_unit_block: bool,
    pub(crate) work_unit_title_only: bool,
    pub(crate) has_badges: bool,
}

/// Disclosure button data (`subop-chevron`; glyph ▾/▸ derives from `expanded`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Disclosure {
    pub(crate) expanded: bool,
    pub(crate) label: String,
}

/// Sub-op row content: small Codicon and indented summary. Semantic tags live
/// in the dedicated Tags column.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SubopContent {
    pub(crate) icon: &'static str,
    pub(crate) summary: Summary,
}

/// Top-level row content after all chip-like metadata moves to Tags.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct TopContent {
    pub(crate) summary: Option<RowSummary>,
}

/// The content cell tokens (sub-op rows XOR top-level rows with a work-unit
/// ribbon wrapper on top when a unit header is present).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct RowContent {
    pub(crate) subop: Option<SubopContent>,
    pub(crate) top: Option<TopContent>,
    pub(crate) work_unit: Option<WorkUnitHeader>,
}

/// The `.row` element's ARIA/attribute contract values.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RowAria {
    /// Roving tabindex: `0` for the single tab stop, else `-1`.
    pub(crate) tabindex: i8,
    pub(crate) aria_selected: bool,
    /// Present exactly when the row is expandable, false when collapsed.
    pub(crate) aria_expanded: Option<bool>,
    pub(crate) aria_label: String,
    /// Hover title (detail summary text).
    pub(crate) title: String,
    pub(crate) base_aria_label: String,
}

/// The complete pure row presentation for one cached `HistoryRow` value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RowSpec {
    pub(crate) identity: RowIdentity,
    pub(crate) graph: GraphData,
    pub(crate) kind: String,
    pub(crate) record_role: String,
    pub(crate) activity_kind: String,
    pub(crate) classification: RowClassification,
    pub(crate) outcome: String,
    pub(crate) group: String,
    pub(crate) kind_class: KindClass,
    pub(crate) role_class: Option<&'static str>,
    pub(crate) flags: RowFlags,
    pub(crate) state: StateFlags,
    /// Activity-column disclosure rendered immediately after the activity label.
    pub(crate) disclosure: Option<Disclosure>,
    pub(crate) content_flags: ContentFlags,
    pub(crate) author_text: String,
    pub(crate) date_text: String,
    pub(crate) commit_text: String,
    pub(crate) commit_title: String,
    pub(crate) summary_source: String,
    pub(crate) display_summary: String,
    pub(crate) plain_summary: String,
    pub(crate) detail_summary: String,
    /// Ordered chips rendered exclusively in the dedicated Tags column.
    pub(crate) tags: Vec<ChromeItem>,
    pub(crate) session_description: String,
    pub(crate) work_unit: Option<WorkUnitData>,
    pub(crate) session_summary: Option<SessionSummaryData>,
    pub(crate) work_unit_header: Option<WorkUnitHeader>,
    pub(crate) bundle: Option<BundleInfo>,
    pub(crate) promoted: Option<PromotedKind>,
    pub(crate) content: RowContent,
    pub(crate) aria: RowAria,
    pub(crate) group_label: Option<String>,
    pub(crate) open_json: Option<Value>,
    pub(crate) placeholder: bool,
}

impl Default for RowSpec {
    fn default() -> Self {
        RowSpec {
            identity: RowIdentity::default(),
            graph: GraphData::default(),
            kind: String::new(),
            record_role: String::new(),
            activity_kind: String::new(),
            classification: RowClassification::default(),
            outcome: String::new(),
            group: String::new(),
            kind_class: KindClass::Plain,
            role_class: None,
            flags: RowFlags::default(),
            state: StateFlags::default(),
            disclosure: None,
            content_flags: ContentFlags::default(),
            author_text: String::new(),
            date_text: String::new(),
            commit_text: String::new(),
            commit_title: String::new(),
            summary_source: String::new(),
            display_summary: String::new(),
            plain_summary: String::new(),
            detail_summary: String::new(),
            tags: Vec::new(),
            session_description: String::new(),
            work_unit: None,
            session_summary: None,
            work_unit_header: None,
            bundle: None,
            promoted: None,
            content: RowContent::default(),
            aria: RowAria {
                tabindex: -1,
                aria_selected: false,
                aria_expanded: None,
                aria_label: String::new(),
                title: String::new(),
                base_aria_label: String::new(),
            },
            group_label: None,
            open_json: None,
            placeholder: false,
        }
    }
}

impl RowSpec {
    /// A placeholder slot: identity only, no data (`row row-placeholder`).
    pub(crate) fn placeholder(abs_index: i64) -> RowSpec {
        RowSpec {
            identity: RowIdentity {
                abs_index,
                ..RowIdentity::default()
            },
            placeholder: true,
            ..RowSpec::default()
        }
    }

    /// The production placeholder class list for `.row-placeholder` rows.
    pub(crate) const PLACEHOLDER_CLASSES: &'static str = "row row-placeholder";

    /// Build the full presentation model for one cached row value.
    pub(crate) fn from_value(row: &Value, context: &RowContext) -> RowSpec {
        let view = context.view;
        let is_subop = wire::bool(row, "is_subop");
        let node_key = row_str(row, "node_key");
        let selected = context
            .selected_key
            .as_deref()
            .is_some_and(|key| key == node_key);
        let work_unit = work_unit_of(row, view);
        let is_work_unit_start = work_unit.as_ref().is_some_and(|wu| wu.is_start);
        let session_summary = session_summary_of(row, view);
        let is_session_summary = session_summary.is_some();
        let has_boundary_header = work_unit.is_some() && (is_work_unit_start || is_session_summary);
        let bundle_kind = activity_bundle_kind(row, view);
        let is_bundle = bundle_kind.is_some();
        let is_work_group = bundle_kind == Some(BundleKind::WorkGroup);
        let is_execute_run = bundle_kind == Some(BundleKind::ExecuteRun);
        let is_plan_repeat = bundle_kind == Some(BundleKind::PlanRepeat);
        let semantic_tags = row_semantic_chrome(row, is_bundle, view, BadgeOptions::default());
        let classification = if is_session_summary {
            RowClassification::session_summary()
        } else {
            row_classification(row)
        };
        let session = session_meta_values(row);
        let has_session_meta = !is_subop && !session.is_empty();
        let session_description = session_meta_description(row);
        let promoted = promoted_kind(row, view);
        let summary_source = summary_source(row);
        let display_summary = display_summary_for_row(row, &summary_source);
        let plain_summary = plain_row_summary(row, &display_summary);
        let plain_summary = if plain_summary.is_empty() {
            "(no summary)".to_owned()
        } else {
            plain_summary
        };
        let detail_summary = markdown_detail_summary(&summary_source);
        let detail_summary = if detail_summary.is_empty() {
            "(no summary)".to_owned()
        } else {
            detail_summary
        };
        let unit_title = if has_boundary_header {
            work_unit_title(row, view)
        } else {
            String::new()
        };
        let sub_op_count = row
            .get("sub_ops")
            .and_then(Value::as_array)
            .map_or(0usize, Vec::len);
        let has_subs = has_sub_ops(row);
        let expanded_state = has_subs && context.expanded;
        let expandable = has_subs;
        let child_label = if is_bundle {
            let count_text = bundle_count_text(row, view);
            if count_text.is_empty() {
                sub_op_label(sub_op_count)
            } else {
                count_text
            }
        } else {
            sub_op_label(sub_op_count)
        };
        let disclosure_label = if expanded_state {
            format!("Collapse {child_label}")
        } else {
            format!("Expand {child_label}")
        };
        let disclosure = if expandable {
            Some(Disclosure {
                expanded: expanded_state,
                label: disclosure_label,
            })
        } else {
            None
        };
        let row_summary =
            (!is_subop && !is_execute_run).then(|| RowSummary::parse(row, &display_summary));
        let work_unit_header = if has_boundary_header {
            work_unit.as_ref().map(|wu| {
                let count = if is_session_summary {
                    session_summary.as_ref().and_then(|summary| summary.count)
                } else {
                    wu.count
                };
                WorkUnitHeader {
                    id: wu.id.clone(),
                    title: unit_title.clone(),
                    show_count: if is_session_summary {
                        count.is_some_and(|count| count > 1)
                    } else {
                        show_work_unit_count(row, wu)
                    },
                    count_text: count.map_or_else(String::new, work_unit_count_text),
                    count_title: count.map_or_else(String::new, |count| {
                        if is_session_summary {
                            session_summary_count_title(count)
                        } else {
                            work_unit_count_title(count)
                        }
                    }),
                    title_only: unit_title == plain_summary,
                }
            })
        } else {
            None
        };

        // Tags have one stable, row-level home. Preserve semantic ordering,
        // then add summary/session provenance that previously occupied several
        // different positions inside Content.
        let mut tags = semantic_tags;
        if let Some(header) = work_unit_header.as_ref().filter(|header| header.show_count) {
            tags.push(ChromeItem::new(
                "work-unit-count",
                &header.count_text,
                &header.count_title,
                None,
            ));
        }
        if let Some(prefix) = row_summary
            .as_ref()
            .and_then(|summary| summary.git_prefix.as_deref())
        {
            let label = format!("Commit prefix: {prefix}");
            tags.push(ChromeItem::new(
                "git-prefix-chip",
                prefix,
                &label,
                Some(&label),
            ));
        }
        if context.is_group_start && has_session_meta {
            tags.extend(session.iter().map(|chip| {
                let label = format!("{}: {}", chip.title, chip.label);
                ChromeItem::new(&chip.class, &chip.label, &label, Some(&label))
            }));
        }
        let has_badges = !tags.is_empty();
        let row_content = if is_subop {
            RowContent {
                subop: Some(SubopContent {
                    icon: subop_icon(&row_str(row, "subop_kind")),
                    summary: Summary::parse(&display_summary),
                }),
                top: None,
                work_unit: None,
            }
        } else {
            RowContent {
                subop: None,
                top: Some(TopContent {
                    summary: row_summary,
                }),
                work_unit: None,
            }
        };
        let group_start = context.is_group_start && !has_boundary_header;
        let group_label = if !is_subop && is_graph_endpoint(row) {
            Some(group_label_text(
                &row_str(row, "group"),
                session_meta_field(row, "session_title"),
                session_meta_field(row, "agent_nickname"),
            ))
        } else {
            None
        };
        // aria-label composition, mirroring buildRowHtml exactly.
        let mut aria_label = plain_summary.clone();
        if is_bundle {
            let member_count = row
                .get("activity_bundle")
                .and_then(|bundle| bundle.get("member_count"))
                .and_then(Value::as_u64);
            let label = match bundle_kind {
                Some(BundleKind::WorkGroup) => "Work group",
                Some(BundleKind::PlanRepeat) => "Plan group",
                Some(BundleKind::ExecuteRun) | None => "Execute run",
            };
            label.clone_into(&mut aria_label);
            aria_label.push_str(", ");
            aria_label.push_str(&member_count.map_or_else(String::new, |count| {
                let noun = match bundle_kind {
                    Some(BundleKind::WorkGroup) if count == 1 => " activity",
                    Some(BundleKind::WorkGroup) => " activities",
                    Some(BundleKind::PlanRepeat) if count == 1 => " update",
                    Some(BundleKind::PlanRepeat) => " updates",
                    Some(BundleKind::ExecuteRun) | None if count == 1 => " step",
                    Some(BundleKind::ExecuteRun) | None => " steps",
                };
                format!("{count}{noun}")
            }));
            if is_execute_run && row_str(row, "outcome") == "success" {
                aria_label.push_str(", completed");
            }
            if is_plan_repeat || is_work_group {
                aria_label.push_str(": ");
                aria_label.push_str(&plain_summary);
            }
        } else if has_boundary_header {
            if unit_title == plain_summary {
                aria_label.clone_from(&unit_title);
            } else {
                aria_label = format!("{unit_title}: {plain_summary}");
            }
        }
        let base_aria_label = aria_label.clone();
        if context.is_group_start && has_session_meta {
            aria_label.push_str(", ");
            aria_label.push_str(&session_description);
        }
        let roving = context.roving_abs == Some(context.abs_index);
        let aria = RowAria {
            tabindex: if roving { 0 } else { -1 },
            aria_selected: selected,
            aria_expanded: expandable.then_some(expanded_state),
            aria_label,
            title: detail_summary.clone(),
            base_aria_label: base_aria_label.clone(),
        };
        let mut bundle_info = None;
        if let Some(kind) = bundle_kind {
            bundle_info = Some(BundleInfo {
                kind,
                member_count: row
                    .get("activity_bundle")
                    .and_then(|bundle| bundle.get("member_count"))
                    .and_then(Value::as_u64),
                count_text: bundle_count_text(row, view),
                status_success: is_execute_run && row_str(row, "outcome") == "success",
            });
        }
        RowSpec {
            identity: RowIdentity {
                abs_index: context.abs_index,
                node_key: node_key.clone(),
                is_subop,
                hierarchy_depth: row
                    .get("hierarchy_depth")
                    .and_then(Value::as_u64)
                    .and_then(|depth| u8::try_from(depth).ok())
                    .unwrap_or(u8::from(is_subop)),
                subop_kind: row_str(row, "subop_kind"),
                op_id: row_str(row, "op_id"),
                git_oid: row_str(row, "git_oid"),
                repository: row_str(row, "repository"),
                commit_id: row_str(row, "commit_id"),
                turn_id: row_str(row, "turn_id"),
            },
            graph: GraphData {
                lane: wire::lane(row),
                above: wire::above(row),
                below: wire::below(row),
                transitions: wire::transitions(row),
                muted_above: wire::muted_above(row),
                muted_below: wire::muted_below(row),
                muted_transitions: wire::muted_transitions(row),
                chain_state: ChainState::from_wire(&row_str(row, "chain_state")),
                is_subop,
                is_bundle,
                expanded: expanded_state,
            },
            kind: row_str(row, "kind"),
            record_role: row_str(row, "record_role"),
            activity_kind: row_str(row, "activity_kind"),
            classification,
            outcome: row_str(row, "outcome"),
            group: row_str(row, "group"),
            kind_class: kind_class_of(row),
            role_class: role_class_of(row),
            flags: RowFlags {
                human: row_str(row, "author") == "human",
                selected,
                find_current: context.find_current,
            },
            state: StateFlags {
                group_start,
                expandable,
            },
            disclosure,
            content_flags: ContentFlags {
                work_unit_block: has_boundary_header,
                work_unit_title_only: work_unit_header
                    .as_ref()
                    .is_some_and(|header| header.title_only),
                has_badges,
            },
            author_text: row_str(row, "author"),
            date_text: format_date(row_ms(row, "timestamp_ms")),
            commit_text: short_commit_id(row),
            commit_title: commit_cell_title(row),
            summary_source: summary_source.clone(),
            display_summary: display_summary.clone(),
            plain_summary: plain_summary.clone(),
            detail_summary: detail_summary.clone(),
            tags,
            session_description,
            work_unit,
            session_summary,
            work_unit_header,
            bundle: bundle_info,
            promoted,
            content: row_content,
            aria,
            group_label,
            open_json: open_json_envelope(row),
            placeholder: false,
        }
    }

    /// The row's exact production class list (`.row` plus ordered suffixes).
    pub(crate) fn classes(&self) -> String {
        let mut classes = String::from("row");
        match self.kind_class {
            KindClass::Tool => classes.push_str(" row-tool"),
            KindClass::Dim => classes.push_str(" row-dim"),
            KindClass::Plain => {}
        }
        if self.flags.human {
            classes.push_str(" row-human");
        }
        if self.graph.chain_state.is_muted() {
            classes.push_str(" row-chain-muted");
        }
        if self.identity.is_subop {
            classes.push_str(" row-subop");
        }
        if let Some(role) = self.role_class {
            classes.push_str(" row-role-");
            classes.push_str(role);
        }
        if self.content_flags.has_badges {
            classes.push_str(" row-has-badges");
        }
        if self.flags.selected {
            classes.push_str(" row-selected");
        }
        if self.flags.find_current {
            classes.push_str(" row-find-current");
        }
        if self.state.group_start {
            classes.push_str(" row-group-start");
        }
        if let Some(work_unit_class) = self.work_unit_class() {
            classes.push_str(" row-");
            classes.push_str(match work_unit_class {
                WorkUnitClass::Start => "work-unit-start",
                WorkUnitClass::End => "work-unit-end",
            });
        }
        if self.session_summary.is_some() {
            classes.push_str(" row-session-summary");
        }
        if self.bundle.is_some() {
            classes.push_str(" row-activity-bundle");
        }
        if self.state.expandable {
            classes.push_str(" row-expandable");
        }
        if let Some(promoted) = self.promoted {
            classes.push(' ');
            classes.push_str(promoted.classes());
        }
        classes
    }

    /// The work-unit boundary class of this row, if any.
    pub(crate) fn work_unit_class(&self) -> Option<WorkUnitClass> {
        match self.work_unit.as_ref() {
            Some(wu) if wu.is_start => Some(WorkUnitClass::Start),
            Some(wu) if wu.is_end => Some(WorkUnitClass::End),
            _ => None,
        }
    }

    /// The resolved disclosure state (`aria-expanded` value when expandable).
    #[cfg(test)]
    pub(crate) fn expanded(&self) -> Option<bool> {
        self.disclosure
            .as_ref()
            .map(|disclosure| disclosure.expanded)
    }
}

/// `subOpCount` details label (`N detail` / `N details`).
fn sub_op_label(sub_op_count: usize) -> String {
    format!(
        "{sub_op_count} detail{}",
        if sub_op_count == 1 { "" } else { "s" }
    )
}

/// The `kindClass` mapping (system -> tool, message/command -> none, else dim).
fn kind_class_of(row: &Value) -> KindClass {
    if wire::bool(row, "is_system") {
        KindClass::Tool
    } else {
        let kind = row_str(row, "kind");
        if kind == "message" || kind == "command" {
            KindClass::Plain
        } else {
            KindClass::Dim
        }
    }
}

/// `RECORD_ROLE_CLASSES` whitelist lookup (suffix after `row-role-`).
fn role_class_of(row: &Value) -> Option<&'static str> {
    let record_role = row_str(row, "record_role");
    RECORD_ROLE_CLASSES
        .iter()
        .find(|candidate| **candidate == record_role)
        .copied()
}

/// `openJson` identity envelope for eligible rows (exact production shape).
pub(crate) fn open_json_envelope(row: &Value) -> Option<Value> {
    if let Some(git_oid) = row.get("git_oid").filter(|value| js_truthy(value)).cloned() {
        let mut envelope = serde_json::Map::new();
        drop(envelope.insert("type".to_owned(), Value::String("openJson".to_owned())));
        drop(envelope.insert("git_oid".to_owned(), git_oid));
        if let Some(repository) = row.get("repository").cloned() {
            drop(envelope.insert("repository".to_owned(), repository));
        }
        return Some(Value::Object(envelope));
    }
    if let Some(op_id) = row.get("op_id").filter(|value| js_truthy(value)).cloned() {
        let mut envelope = serde_json::Map::new();
        drop(envelope.insert("type".to_owned(), Value::String("openJson".to_owned())));
        drop(envelope.insert("op_id".to_owned(), op_id));
        return Some(Value::Object(envelope));
    }
    None
}

/// Whether a row is eligible for the raw-JSON editor activation.
#[cfg(test)]
pub(crate) fn is_open_json_eligible(row: &Value) -> bool {
    open_json_envelope(row).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::collections::BTreeMap;
    use std::fmt::Write as _;

    // ---- Test-only HTML renderers -----------------------------------------
    // These mirror the legacy JS renderer's span assembly byte-for-byte so
    // the structured model is verified against its frozen golden markup.
    // They are intentionally test-only: the real DOM shell owns rendering.

    fn esc(text: &str) -> String {
        html_escape(text)
    }

    fn render_inline_html(inline: &[MdInline]) -> String {
        let mut html = String::new();
        for token in inline {
            match token {
                MdInline::Text(text) => {
                    html.push_str("<span class=\"md-text\">");
                    html.push_str(&esc(text));
                    html.push_str("</span>");
                }
                MdInline::Space => {
                    html.push_str("<span class=\"md-space\" aria-hidden=\"true\">\u{00a0}</span>");
                }
                MdInline::Code(code) => {
                    html.push_str("<code class=\"md-code\">");
                    html.push_str(&esc(code));
                    html.push_str("</code>");
                }
                MdInline::Strong(inner) => {
                    html.push_str("<strong class=\"md-strong\">");
                    html.push_str(&render_inline_html(inner));
                    html.push_str("</strong>");
                }
                MdInline::Em(inner) => {
                    html.push_str("<em class=\"md-em\">");
                    html.push_str(&render_inline_html(inner));
                    html.push_str("</em>");
                }
                MdInline::Strike(inner) => {
                    html.push_str("<span class=\"md-strike\">");
                    html.push_str(&render_inline_html(inner));
                    html.push_str("</span>");
                }
                MdInline::Link { label, target } => {
                    html.push_str("<span class=\"md-link\" title=\"");
                    html.push_str(&esc(target));
                    html.push_str("\">");
                    html.push_str(&render_inline_html(label));
                    html.push_str("</span>");
                }
                MdInline::Image { label, target } => {
                    html.push_str("<span class=\"md-image\" title=\"");
                    html.push_str(&esc(target));
                    html.push_str("\"><span aria-hidden=\"true\">image · </span>");
                    html.push_str(&render_inline_html(label));
                    html.push_str("</span>");
                }
            }
        }
        html
    }

    fn render_line_html(line: &MdLine) -> String {
        match &line.kind {
            MdLineKind::Heading { level } => {
                format!(
                    "<span class=\"md-line md-heading md-h{level}\">{}</span>",
                    render_inline_html(&line.inline)
                )
            }
            MdLineKind::Task { done } => {
                format!(
                    "<span class=\"md-line md-list md-task{}{}\"><span class=\"md-marker\" aria-hidden=\"true\">{}</span>{}</span>",
                    if *done { " md-task-done" } else { "" },
                    "",
                    if *done { "\u{2713}" } else { "\u{25cb}" },
                    render_inline_html(&line.inline)
                )
            }
            MdLineKind::Unordered => {
                format!(
                    "<span class=\"md-line md-list\"><span class=\"md-marker\" aria-hidden=\"true\">\u{2022}</span>{}</span>",
                    render_inline_html(&line.inline)
                )
            }
            MdLineKind::Ordered { marker } => {
                format!(
                    "<span class=\"md-line md-list\"><span class=\"md-marker\" aria-hidden=\"true\">{}</span>{}</span>",
                    esc(marker),
                    render_inline_html(&line.inline)
                )
            }
            MdLineKind::Quote { callout } => {
                if let Some(callout) = callout {
                    format!(
                        "<span class=\"md-line md-quote\"><span class=\"md-callout\">{}</span>{}</span>",
                        esc(callout),
                        render_inline_html(&line.inline)
                    )
                } else {
                    format!(
                        "<span class=\"md-line md-quote\"><span class=\"md-marker\" aria-hidden=\"true\">\u{203a}</span>{}</span>",
                        render_inline_html(&line.inline)
                    )
                }
            }
            MdLineKind::Fence { language } => {
                let language = if language.is_empty() {
                    "code"
                } else {
                    language
                };
                format!(
                    "<span class=\"md-line md-fence\"><span class=\"md-callout\">{}</span>{}</span>",
                    esc(language),
                    render_inline_html(&line.inline)
                )
            }
            MdLineKind::Plain => {
                format!(
                    "<span class=\"md-line\">{}</span>",
                    render_inline_html(&line.inline)
                )
            }
            MdLineKind::Empty => {
                format!(
                    "<span class=\"md-line md-empty\">{}</span>",
                    Summary::EMPTY_LABEL
                )
            }
        }
    }

    fn render_summary_html(summary: &Summary) -> String {
        let mut html = render_line_html(&summary.line);
        if summary.more > 0 {
            let more = format!(
                "<span class=\"md-more\" aria-hidden=\"true\">+{}{}</span>",
                summary.more,
                if summary.more == 1 { " line" } else { " lines" }
            );
            html.push_str(&more);
        }
        html
    }

    fn render_row_summary_html(row_summary: &RowSummary) -> String {
        let mut html = String::new();
        if let Some(content) = &row_summary.content {
            html.push_str("<span class=\"summary-text");
            if row_summary.git_prefix.is_some() {
                html.push_str(" git-summary-text");
            }
            html.push_str("\">");
            html.push_str(&render_summary_html(content));
            html.push_str("</span>");
        }
        html
    }

    fn render_tags_html(spec: &RowSpec) -> String {
        render_chrome_html(&spec.tags)
    }

    fn render_chrome_html(chrome: &[ChromeItem]) -> String {
        let mut html = String::new();
        for item in chrome {
            html.push_str("<span class=\"");
            html.push_str(&item.classes);
            html.push_str("\" title=\"");
            html.push_str(&esc(&item.title));
            html.push('"');
            if let Some(aria) = &item.aria_label {
                html.push_str(" aria-label=\"");
                html.push_str(&esc(aria));
                html.push('"');
            }
            html.push('>');
            html.push_str(&esc(&item.text));
            html.push_str("</span>");
        }
        html
    }

    fn render_chevron_html(disclosure: &Disclosure) -> String {
        format!(
            "<button type=\"button\" class=\"subop-chevron\" title=\"{0}\" aria-label=\"{0}\" aria-expanded=\"{1}\">{2}</button>",
            esc(&disclosure.label),
            disclosure.expanded,
            if disclosure.expanded { "\u{25be}" } else { "\u{25b8}" }
        )
    }

    fn render_activity_html(spec: &RowSpec) -> String {
        let mut content = format!(
            "<span class=\"activity-label\">{}</span>",
            esc(&spec.classification.label)
        );
        if let Some(disclosure) = &spec.disclosure {
            content.push_str(&render_chevron_html(disclosure));
        }
        content
    }

    fn render_content_html(spec: &RowSpec) -> String {
        if let Some(subop) = &spec.content.subop {
            let mut content = String::new();
            write!(
                content,
                "<span class=\"subop-icon codicon codicon-{}\" aria-hidden=\"true\"></span><span class=\"subop-summary\">{}</span>",
                subop.icon,
                render_summary_html(&subop.summary)
            )
            .expect("writing sub-op fixture HTML to a String cannot fail");
            return content;
        }
        let top = spec.content.top.as_ref().expect("top-level row content");
        let wu_start = spec.content_flags.work_unit_block;
        let mut content = String::new();
        if let Some(row_summary) = &top.summary {
            content.push_str(&render_row_summary_html(row_summary));
        }
        if wu_start {
            let header = spec.work_unit_header.as_ref().expect("work-unit header");
            let mut ribbon = format!(
                "<span class=\"work-unit-ribbon-line\"><span class=\"work-unit-ribbon\" title=\"{}\">{}</span>",
                esc(&header.title),
                esc(&header.title)
            );
            ribbon.push_str("</span>");
            return if spec.content_flags.work_unit_title_only {
                ribbon
            } else {
                format!("{ribbon}<span class=\"work-unit-row-line\">{content}</span>")
            };
        }
        content
    }

    fn render_attrs(spec: &RowSpec) -> BTreeMap<String, String> {
        let mut attrs = BTreeMap::new();
        drop(attrs.insert("role".to_owned(), "row".to_owned()));
        drop(attrs.insert("tabindex".to_owned(), spec.aria.tabindex.to_string()));
        drop(attrs.insert(
            "aria-selected".to_owned(),
            spec.aria.aria_selected.to_string(),
        ));
        if let Some(expanded) = spec.aria.aria_expanded {
            drop(attrs.insert("aria-expanded".to_owned(), expanded.to_string()));
        }
        drop(attrs.insert("aria-label".to_owned(), spec.aria.aria_label.clone()));
        drop(attrs.insert("title".to_owned(), spec.aria.title.clone()));
        drop(attrs.insert(
            "data-base-aria-label".to_owned(),
            spec.aria.base_aria_label.clone(),
        ));
        drop(attrs.insert("data-key".to_owned(), spec.identity.node_key.clone()));
        drop(attrs.insert("data-row".to_owned(), spec.identity.abs_index.to_string()));
        drop(attrs.insert(
            "data-hierarchy-depth".to_owned(),
            spec.identity.hierarchy_depth.to_string(),
        ));
        drop(attrs.insert(
            "data-classification".to_owned(),
            spec.classification.label.clone(),
        ));
        if let Some(header) = &spec.work_unit_header {
            drop(attrs.insert("data-work-unit-id".to_owned(), header.id.clone()));
        }
        if let Some(count) = spec
            .session_summary
            .as_ref()
            .and_then(|summary| summary.count)
        {
            drop(attrs.insert("data-session-count".to_owned(), count.to_string()));
        }
        if let Some(bundle) = &spec.bundle {
            drop(attrs.insert(
                "data-activity-bundle".to_owned(),
                bundle.kind.as_str().to_owned(),
            ));
            if let Some(count) = bundle.member_count {
                drop(attrs.insert("data-bundle-count".to_owned(), count.to_string()));
            }
        }
        attrs
    }

    // ---- Fixtures ----------------------------------------------------------
    // Deterministic fixtures mirroring extensions/vscode-editchain/test/
    // harness/fixtures.js (op/git row shapes, additive r4 fields, work-unit /
    // bundle / promotion metadata) and the sub-op rows the service ships in
    // GetWindow.

    fn now() -> i64 {
        1_768_492_800_000
    }

    fn base_row() -> Value {
        json!({
            "op_id": null,
            "git_oid": null,
            "repository": null,
            "summary": "Agent turn with metadata",
            "timestamp_ms": now().wrapping_sub(1000),
            "group": "session:s1",
            "group_end": false,
            "node_key": "op:1",
            "parents": [],
            "is_submodule": false,
            "is_system": false,
            "author": "agent",
            "commit_id": "",
            "kind": "message",
            "record_role": "narrative",
            "activity_kind": "conversation",
            "visibility": "primary",
            "outcome": "unknown",
            "turn_id": "t1",
            "lane": 0,
            "above": [0],
            "below": [0],
            "transitions": [],
            "sub_ops": [],
            "is_subop": false,
        })
    }

    fn with(base: &Value, overrides: &[(&str, Value)]) -> Value {
        let mut map = base.as_object().expect("fixture base is an object").clone();
        for (key, value) in overrides {
            drop(map.insert((*key).to_owned(), value.clone()));
        }
        Value::Object(map)
    }

    fn git_row() -> Value {
        json!({
            "op_id": null,
            "git_oid": "git:abc123def4567890abcdef1234567890abcdef12",
            "repository": "9007199254740993",
            "summary": "feat: add **search** bar\n\nsecond line",
            "timestamp_ms": now(),
            "group": "repo:9007199254740993",
            "group_end": true,
            "node_key": "git:abc123def456",
            "parents": [],
            "is_submodule": false,
            "is_system": false,
            "author": "ambientlight",
            "commit_id": "git:abc123d",
            "kind": "git",
            "record_role": "artifact",
            "activity_kind": "source_control",
            "visibility": "primary",
            "outcome": "success",
            "turn_id": "",
            "lane": 1,
            "above": [0, 1],
            "below": [1],
            "transitions": [[0, 1]],
            "sub_ops": [],
            "is_subop": false,
        })
    }

    fn subop_row() -> Value {
        with(
            &base_row(),
            &[
                ("node_key", json!("node:1::sub:0")),
                ("op_id", json!("node:1::sub:0")),
                ("summary", json!("custom-title metadata")),
                ("kind", json!("custom-title")),
                ("subop_kind", json!("edit")),
                ("is_subop", json!(true)),
                ("above", json!([0, 1])),
                ("below", json!([0, 1])),
                ("timestamp_ms", json!(now())),
                ("author", json!("")),
            ],
        )
    }

    fn expandable_row() -> Value {
        with(
            &base_row(),
            &[
                ("node_key", json!("node:5")),
                ("summary", json!("Agent turn with metadata")),
                (
                    "sub_ops",
                    json!([
                        { "op_id": "n::s0", "summary": "s0", "kind": "k" },
                        { "op_id": "n::s1", "summary": "s1", "kind": "k" },
                    ]),
                ),
            ],
        )
    }

    fn bundle_row() -> Value {
        with(
            &base_row(),
            &[
                ("node_key", json!("bundle:exec1")),
                ("op_id", json!("node:bundle1")),
                ("summary", json!("tool result: execute run (2 steps)")),
                ("kind", json!("command")),
                ("record_role", json!("action")),
                ("activity_kind", json!("execute")),
                ("outcome", json!("success")),
                ("is_system", json!(true)),
                ("author", json!("")),
                ("commit_id", json!("bundle:exec1")),
                ("turn_id", json!("")),
                ("group", json!("session:s1")),
                ("timestamp_ms", json!(now())),
                (
                    "activity_bundle",
                    json!({ "kind": "execute-run", "member_count": 2 }),
                ),
                (
                    "sub_ops",
                    json!([
                        { "op_id": "n::s0", "summary": "s0", "kind": "tool" },
                        { "op_id": "n::s1", "summary": "s1", "kind": "tool" },
                    ]),
                ),
            ],
        )
    }

    fn bundle_plan_row() -> Value {
        with(
            &bundle_row(),
            &[
                ("node_key", json!("bundle:plan1")),
                ("summary", json!("Repeated plan: same step plan")),
                ("kind", json!("reflection")),
                ("record_role", json!("narrative")),
                ("activity_kind", json!("plan")),
                ("outcome", json!("unknown")),
                ("is_system", json!(false)),
                ("author", json!("agent")),
                (
                    "activity_bundle",
                    json!({ "kind": "plan-repeat", "member_count": 3 }),
                ),
                (
                    "sub_ops",
                    json!([
                        { "op_id": "n::s0", "summary": "s0", "kind": "reflection" },
                        { "op_id": "n::s1", "summary": "s1", "kind": "reflection" },
                        { "op_id": "n::s2", "summary": "s2", "kind": "reflection" },
                    ]),
                ),
            ],
        )
    }

    fn wu_start_row() -> Value {
        with(
            &base_row(),
            &[
                ("node_key", json!("wu:req1")),
                ("summary", json!("User request: make the search faster")),
                ("author", json!("human")),
                ("timestamp_ms", json!(now())),
                (
                    "work_unit",
                    json!({
                        "id": "session:s1/turn:t1",
                        "title": "User request: make the search faster",
                        "is_start": true,
                        "is_end": false,
                        "count": 5,
                    }),
                ),
                (
                    "session_meta",
                    json!({ "model_provider": "sglang_dsv4", "agent_nickname": "Harvey" }),
                ),
            ],
        )
    }

    fn wu_start_title_only_row() -> Value {
        with(
            &wu_start_row(),
            &[
                ("node_key", json!("wu:req2")),
                ("summary", json!("Implement retry with backoff")),
                ("turn_id", json!("t2")),
                (
                    "work_unit",
                    json!({
                        "id": "session:s1/turn:t2",
                        "title": "Implement retry with backoff",
                        "is_start": true,
                        "is_end": false,
                        "count": 1,
                    }),
                ),
                ("session_meta", Value::Null),
            ],
        )
    }

    fn session_summary_row() -> Value {
        with(
            &base_row(),
            &[
                ("node_key", json!("wu:session-summary")),
                (
                    "summary",
                    json!("system last-prompt custom-title — 1 activity · 1 plan"),
                ),
                ("group", json!("session:s1")),
                ("kind", json!("work-group")),
                ("record_role", json!("action")),
                ("activity_kind", json!("work")),
                ("turn_id", json!("")),
                (
                    "activity_bundle",
                    json!({ "kind": "work-group", "member_count": 1 }),
                ),
                (
                    "sub_ops",
                    json!([{ "op_id": "n::meta", "summary": "system last-prompt custom-title", "kind": "import" }]),
                ),
                (
                    "work_unit",
                    json!({
                        "id": "session:s1/turn:t9",
                        "title": "Initial user request",
                        "is_start": true,
                        "is_end": false,
                        "count": 4,
                    }),
                ),
                ("session_summary", json!({ "count": 87 })),
                ("session_meta", Value::Null),
            ],
        )
    }

    fn promoted_failure_row() -> Value {
        with(
            &base_row(),
            &[
                ("node_key", json!("wu:fail")),
                ("summary", json!("command failed with exit 1")),
                ("kind", json!("command")),
                ("record_role", json!("action")),
                ("activity_kind", json!("execute")),
                ("outcome", json!("failure")),
                ("promoted", json!(true)),
                (
                    "work_unit",
                    json!({
                        "id": "u1",
                        "is_start": false,
                        "is_end": false,
                        "count": 0,
                    }),
                ),
                ("session_meta", Value::Null),
            ],
        )
    }

    fn session_meta_row() -> Value {
        with(
            &base_row(),
            &[
                ("node_key", json!("sess:1")),
                ("summary", json!("session boundary row")),
                (
                    "session_meta",
                    json!({
                        "session_title": "q0",
                        "model_provider": "  sglang_dsv4  ",
                        "agent_nickname": "Harvey",
                    }),
                ),
                ("turn_id", json!("")),
                ("op_id", json!("node:1")),
            ],
        )
    }

    // ---- Function-level contract goldens (production-captured) ---------------

    #[test]
    fn identity_helpers_match_production_values() {
        assert_eq!(short_id("90071992547409931234567890"), "931234567890");
        assert_eq!(short_id("abc123def456"), "abc123def456");
        assert_eq!(short_id(""), "");
        assert_eq!(
            group_label_text("repo:9007199254740993", None, None),
            "Git · repo 199254740993"
        );
        assert_eq!(
            group_label_text("session:session_abcdefghijklmnop", None, None),
            "Session efghijklmnop"
        );
        assert_eq!(group_label_text("ops", None, None), "EditChain ops");
        assert_eq!(
            group_label_text("session:opaque", Some("q0"), Some("Tesla")),
            "q0 · Tesla"
        );
        assert_eq!(
            group_label_text("session:opaque", Some("q0"), Some("q0")),
            "q0"
        );
    }

    #[test]
    fn date_labels_are_deterministic_utc_and_match_the_en_us_contract() {
        assert_eq!(format_date(0), "");
        assert_eq!(format_date(1_768_492_800_000), "Jan 15, 2026 04:00 PM");
        assert_eq!(format_date(-86_400_000), "Dec 31, 1969 12:00 AM");
        assert_eq!(format_date(1_768_521_600_000), "Jan 16, 2026 12:00 AM");
        assert_eq!(format_date(1_768_516_440_000), "Jan 15, 2026 10:34 PM");
        assert_eq!(format_date(1_768_455_660_000), "Jan 15, 2026 05:41 AM");
    }

    #[test]
    fn commit_and_id_labels_match_short_commit_id() {
        assert_eq!(
            short_commit_id(&git_row()),
            "git:abc123d",
            "git rows keep the abbreviated commit id"
        );
        assert_eq!(short_commit_id(&subop_row()), "ode:1::sub:0");
        assert_eq!(
            short_commit_id(&with(
                &base_row(),
                &[("turn_id", json!("turn:abcdefghijklmnop"))]
            )),
            "efghijklmnop"
        );
        assert_eq!(
            short_commit_id(&with(
                &base_row(),
                &[
                    ("op_id", json!("node:1234567890abcdef")),
                    ("commit_id", json!("")),
                    ("turn_id", json!("")),
                ]
            )),
            "567890abcdef"
        );
        assert_eq!(commit_cell_title(&git_row()), "git:abc123d");
        assert_eq!(
            commit_cell_title(&with(&base_row(), &[("op_id", json!("node:1"))])),
            "node:1"
        );
        assert_eq!(
            commit_cell_title(&with(&base_row(), &[("op_id", Value::Null)])),
            ""
        );
    }

    #[test]
    fn subop_icon_mapping_matches_codicon_names() {
        assert_eq!(subop_icon("edit"), "edit");
        assert_eq!(subop_icon("msg"), "comment");
        assert_eq!(subop_icon("tool_result"), "output");
        assert_eq!(subop_icon("meta"), "info");
        assert_eq!(subop_icon("future_kind"), "info");
    }

    #[test]
    fn work_unit_titles_match_fallbacks() {
        let titled = with(
            &base_row(),
            &[
                ("activity_kind", json!("execute")),
                ("kind", json!("tool")),
                (
                    "work_unit",
                    json!({
                        "id": "u",
                        "title": "Run the **integration** suite",
                        "is_start": true,
                        "count": 3,
                    }),
                ),
            ],
        );
        assert_eq!(
            work_unit_title(&titled, ViewMode::Activity),
            "Run the integration suite"
        );
        assert_eq!(
            work_unit_title(
                &with(
                    &titled,
                    &[(
                        "work_unit",
                        json!({
                            "id": "u", "title": "", "is_start": true, "count": 3
                        })
                    )]
                ),
                ViewMode::Activity
            ),
            "Run"
        );
        assert_eq!(
            work_unit_title(
                &with(
                    &titled,
                    &[
                        ("activity_kind", json!("conversation")),
                        ("kind", json!("message")),
                        (
                            "work_unit",
                            json!({ "id": "u", "title": "", "is_start": true, "count": 3 })
                        ),
                    ]
                ),
                ViewMode::Activity
            ),
            "Request"
        );
        assert_eq!(
            work_unit_title(
                &with(
                    &titled,
                    &[
                        ("activity_kind", json!("future_kind")),
                        ("kind", json!("reflection")),
                        ("group", json!("repo:9007199254740993")),
                        (
                            "work_unit",
                            json!({ "id": "u", "title": "", "is_start": true, "count": 3 })
                        ),
                    ]
                ),
                ViewMode::Activity
            ),
            "Git · repo 199254740993"
        );
    }

    #[test]
    fn work_unit_count_labels_and_visibility() {
        assert_eq!(work_unit_count_text(1), "1 entry");
        assert_eq!(work_unit_count_text(5), "5 entries");
        assert_eq!(
            work_unit_count_title(5),
            "5 entries grouped in this activity"
        );
        let wu = WorkUnitData {
            id: "u".to_owned(),
            title: None,
            count: Some(5),
            is_start: true,
            is_end: false,
        };
        assert!(show_work_unit_count(&base_row(), &wu));
        assert!(!show_work_unit_count(
            &with(&base_row(), &[("activity_kind", json!("source_control"))]),
            &wu
        ));
        assert!(!show_work_unit_count(
            &base_row(),
            &WorkUnitData {
                count: Some(1),
                ..wu.clone()
            }
        ));
    }

    #[test]
    fn bundle_count_text_matches_the_dto_count_contract() {
        assert_eq!(
            bundle_count_text(&bundle_row(), ViewMode::Activity),
            "2 commands"
        );
        assert_eq!(
            bundle_count_text(
                &with(
                    &bundle_row(),
                    &[(
                        "activity_bundle",
                        json!({ "kind": "execute-run", "member_count": 1 })
                    )]
                ),
                ViewMode::Activity
            ),
            "1 command"
        );
        assert_eq!(
            bundle_count_text(&bundle_plan_row(), ViewMode::Activity),
            "3 updates"
        );
        assert_eq!(
            bundle_count_text(
                &with(
                    &bundle_plan_row(),
                    &[(
                        "activity_bundle",
                        json!({ "kind": "plan-repeat", "member_count": 1 })
                    )]
                ),
                ViewMode::Activity
            ),
            "1 update"
        );
        assert_eq!(
            bundle_count_text(
                &with(&bundle_row(), &[("kind", json!("tool"))]),
                ViewMode::Activity
            ),
            "2 tool steps"
        );
        assert_eq!(bundle_count_text(&base_row(), ViewMode::Activity), "");
        // Raw view never bundles.
        assert_eq!(bundle_count_text(&bundle_row(), ViewMode::Raw), "");
    }

    #[test]
    fn work_group_and_nested_bundle_keep_two_independent_disclosures() {
        let work = with(
            &base_row(),
            &[
                ("kind", json!("work-group")),
                ("activity_kind", json!("work")),
                (
                    "activity_bundle",
                    json!({ "kind": "work-group", "member_count": 4 }),
                ),
                (
                    "sub_ops",
                    json!([
                        { "op_id": "1:0:2", "summary": "run tools", "kind": "command", "timestamp_ms": now() }
                    ]),
                ),
            ],
        );
        let work_spec =
            RowSpec::from_value(&work, &RowContext::for_row(ViewMode::Activity, 0, false));
        assert_eq!(work_spec.classification.label, "work");
        assert_eq!(
            work_spec.bundle.as_ref().map(|bundle| bundle.kind),
            Some(BundleKind::WorkGroup)
        );
        assert_eq!(
            work_spec
                .bundle
                .as_ref()
                .map(|bundle| bundle.count_text.as_str()),
            Some("4 activities")
        );
        assert!(work_spec.state.expandable);
        assert_eq!(work_spec.expanded(), Some(false));
        assert_eq!(
            render_attrs(&work_spec)
                .get("data-activity-bundle")
                .map(String::as_str),
            Some("work-group")
        );

        let nested = with(
            &bundle_row(),
            &[
                ("is_subop", json!(true)),
                ("hierarchy_depth", json!(1)),
                ("parent_row", json!(0)),
            ],
        );
        let nested_spec =
            RowSpec::from_value(&nested, &RowContext::for_row(ViewMode::Activity, 1, false));
        assert!(nested_spec.identity.is_subop);
        assert_eq!(nested_spec.identity.hierarchy_depth, 1);
        assert!(nested_spec.state.expandable);
        assert_eq!(nested_spec.expanded(), Some(false));
        assert!(nested_spec.classes().contains("row-activity-bundle"));
        assert!(nested_spec.classes().contains("row-expandable"));
        assert!(nested_spec.disclosure.is_some());
        let nested_activity = render_activity_html(&nested_spec);
        assert!(nested_activity.starts_with("<span class=\"activity-label\">tooluse</span>"));
        assert!(nested_activity.contains("class=\"subop-chevron\""));
        let nested_html = render_tags_html(&nested_spec);
        assert!(!nested_html.contains("class=\"subop-chevron\""));
        assert!(nested_html.contains("class=\"bundle-count\""));
    }

    #[test]
    fn chrome_items_match_exact_span_contracts() {
        let rel = relation_badges(&with(
            &base_row(),
            &[("parent_relations", json!([{ "kind": "subagent" }]))],
        ));
        assert_eq!(
            render_chrome_html(&rel),
            "<span class=\"rel-badge rel-subagent\" title=\"Starts a subagent branch\" aria-label=\"Starts a subagent branch\">↳ subagent</span>"
        );
        let all = relation_badges(&with(
            &base_row(),
            &[(
                "parent_relations",
                json!([
                    { "kind": "subagent" },
                    { "kind": "reconnect" },
                    { "kind": "fork" },
                    { "kind": "unknown-kind" },
                ]),
            )],
        ));
        assert_eq!(
            render_chrome_html(&all),
            "<span class=\"rel-badge rel-subagent\" title=\"Starts a subagent branch\" aria-label=\"Starts a subagent branch\">↳ subagent</span>\
             <span class=\"rel-badge rel-reconnect\" title=\"Completion returns into the subagent branch\" aria-label=\"Completion returns into the subagent branch\">↩ return</span>\
             <span class=\"rel-badge rel-fork\" title=\"Branches off the target row at a fork boundary\" aria-label=\"Branches off the target row at a fork boundary\">⇉ fork</span>"
        );
        let dup = relation_badges(&with(
            &base_row(),
            &[(
                "parent_relations",
                json!([{ "kind": "fork" }, { "kind": "fork" }]),
            )],
        ));
        assert_eq!(dup.len(), 1);
    }

    #[test]
    fn activity_column_classifies_every_real_row_with_stable_fallbacks() {
        let cases = [
            ("conversation", "agent"),
            ("plan", "plan"),
            ("explore", "explore"),
            ("execute", "tooluse"),
            ("change", "change"),
            ("verify", "verify"),
            ("diagnose", "diagnose"),
            ("coordinate", "coordinate"),
            ("source_control", "git"),
            ("external", "external"),
            ("system", "system"),
        ];
        for (wire, label) in cases {
            let classification =
                row_classification(&with(&base_row(), &[("activity_kind", json!(wire))]));
            assert_eq!(classification.label, label, "{wire} label");
            assert_eq!(classification.source, "activity_kind", "{wire} source");
            assert_eq!(classification.title, format!("Activity: {wire}"));
        }

        let user = row_classification(&with(&base_row(), &[("author", json!("human"))]));
        assert_eq!(user.label, "user");
        assert_eq!(user.source, "activity_kind");
        assert_eq!(user.title, "Activity: conversation");

        let git = row_classification(&with(
            &base_row(),
            &[
                ("kind", json!("message")),
                ("activity_kind", json!("unknown")),
                ("git_oid", json!("abc123")),
            ],
        ));
        assert_eq!(git.label, "git", "Git identity is authoritative");
        assert_eq!(git.source, "git_oid");

        let system = row_classification(&with(
            &base_row(),
            &[
                ("activity_kind", json!("unknown")),
                ("kind", json!("unknown")),
                ("is_system", json!(true)),
            ],
        ));
        assert_eq!(system.label, "system");
        assert_eq!(system.source, "is_system");

        let kind = row_classification(&with(
            &base_row(),
            &[
                ("activity_kind", json!("unknown")),
                ("kind", json!("tool_result")),
            ],
        ));
        assert_eq!(kind.label, "tool-result");
        assert_eq!(kind.source, "kind");

        let role = row_classification(&with(
            &base_row(),
            &[
                ("activity_kind", json!("unknown")),
                ("kind", json!("unknown")),
                ("record_role", json!("artifact")),
            ],
        ));
        assert_eq!(role.label, "artifact");
        assert_eq!(role.source, "record_role");

        let other = row_classification(&with(
            &base_row(),
            &[
                ("activity_kind", json!("unknown")),
                ("kind", json!("unknown")),
                ("record_role", json!("unknown")),
            ],
        ));
        assert_eq!(other.label, "other");
        assert!(!other.label.is_empty());
    }

    #[test]
    fn session_scoped_boundary_uses_the_session_activity_name() {
        let row = session_summary_row();
        let spec = RowSpec::from_value(&row, &RowContext::for_row(ViewMode::Activity, 0, false));
        assert_eq!(spec.classification.label, "session");
        assert_eq!(spec.classification.source, "session_summary");
        assert_eq!(spec.classification.title, "Session summary");
        assert!(spec.classes().contains("row-session-summary"));
        assert_eq!(
            spec.work_unit.as_ref().map(|unit| unit.id.as_str()),
            Some("session:s1/turn:t9"),
            "the session marker does not replace the row's turn work unit"
        );
        assert_eq!(
            spec.work_unit_header
                .as_ref()
                .map(|header| (header.count_text.as_str(), header.count_title.as_str())),
            Some(("87 entries", "87 entries in this session"))
        );
        assert_eq!(
            spec.tags
                .iter()
                .find(|tag| tag.classes == "work-unit-count")
                .map(|tag| tag.text.as_str()),
            Some("87 entries")
        );
        assert_eq!(
            render_activity_html(&spec),
            "<span class=\"activity-label\">session</span><button type=\"button\" class=\"subop-chevron\" title=\"Expand 1 activity\" aria-label=\"Expand 1 activity\" aria-expanded=\"false\">▸</button>"
        );
        assert_eq!(
            render_attrs(&spec)
                .get("data-classification")
                .map(String::as_str),
            Some("session")
        );
        assert_eq!(
            render_attrs(&spec)
                .get("data-session-count")
                .map(String::as_str),
            Some("87")
        );

        let raw = RowSpec::from_value(&row, &RowContext::for_row(ViewMode::Raw, 0, false));
        assert_eq!(raw.classification.label, "work");
        assert_eq!(raw.classification.source, "activity_kind");
        assert!(raw.session_summary.is_none());

        let turn_start = RowSpec::from_value(
            &wu_start_row(),
            &RowContext::for_row(ViewMode::Activity, 0, false),
        );
        assert_eq!(turn_start.classification.label, "user");
    }

    #[test]
    fn chrome_suppressions_match_the_frozen_badge_options() {
        assert!(outcome_badge(
            &with(&base_row(), &[("outcome", json!("success"))]),
            BadgeOptions::default()
        )
        .is_none());
        let warning = outcome_badge(
            &with(&base_row(), &[("outcome", json!("warning"))]),
            BadgeOptions::default(),
        )
        .expect("warning badge");
        assert_eq!(
            render_chrome_html(&[warning]),
            "<span class=\"out-badge outcome-warning\" title=\"outcome: warning\" aria-label=\"outcome: warn\">warn</span>"
        );
        let failure = outcome_badge(
            &with(&base_row(), &[("outcome", json!("failure"))]),
            BadgeOptions::default(),
        )
        .expect("failure badge");
        assert_eq!(
            render_chrome_html(&[failure]),
            "<span class=\"out-badge outcome-failure\" title=\"outcome: failure\" aria-label=\"outcome: failed\">✕</span>"
        );
        let cancelled = outcome_badge(
            &with(&base_row(), &[("outcome", json!("cancelled"))]),
            BadgeOptions::default(),
        )
        .expect("cancelled badge");
        assert_eq!(
            render_chrome_html(&[cancelled]),
            "<span class=\"out-badge outcome-neutral\" title=\"outcome: cancelled\" aria-label=\"outcome: cancelled\">cancelled</span>"
        );
        assert!(outcome_badge(
            &with(&base_row(), &[("outcome", json!("unknown"))]),
            BadgeOptions::default()
        )
        .is_none());
    }

    #[test]
    fn semantic_chrome_order_and_bundle_priority_match_production() {
        let relations = row_semantic_chrome(
            &with(
                &base_row(),
                &[
                    ("parent_relations", json!([{ "kind": "fork" }])),
                    ("outcome", json!("unknown")),
                ],
            ),
            false,
            ViewMode::Activity,
            BadgeOptions::default(),
        );
        assert_eq!(relations.len(), 1);
        let relations_warning = row_semantic_chrome(
            &with(
                &base_row(),
                &[
                    ("parent_relations", json!([{ "kind": "fork" }])),
                    ("outcome", json!("warning")),
                ],
            ),
            false,
            ViewMode::Activity,
            BadgeOptions::default(),
        );
        assert_eq!(
            render_chrome_html(&relations_warning),
            "<span class=\"rel-badge rel-fork\" title=\"Branches off the target row at a fork boundary\" aria-label=\"Branches off the target row at a fork boundary\">⇉ fork</span>\
             <span class=\"out-badge outcome-warning\" title=\"outcome: warning\" aria-label=\"outcome: warn\">warn</span>"
        );
        let activity = row_semantic_chrome(
            &with(
                &base_row(),
                &[
                    ("activity_kind", json!("plan")),
                    ("outcome", json!("success")),
                ],
            ),
            false,
            ViewMode::Activity,
            BadgeOptions::default(),
        );
        assert!(
            activity.is_empty(),
            "activity lives in its own column and common success stays suppressed"
        );
        let bundle = row_semantic_chrome(
            &bundle_row(),
            true,
            ViewMode::Activity,
            BadgeOptions::default(),
        );
        assert_eq!(
            render_chrome_html(&bundle),
            "<span class=\"bundle-count\" title=\"2 commands, completed\">2 commands</span>\
             <span class=\"bundle-status bundle-status-success\" title=\"completed\" aria-label=\"completed\">✓</span>"
        );
        let bundle_unknown_outcome = row_semantic_chrome(
            &with(&bundle_row(), &[("outcome", json!("unknown"))]),
            true,
            ViewMode::Activity,
            BadgeOptions::default(),
        );
        assert_eq!(
            render_chrome_html(&bundle_unknown_outcome),
            "<span class=\"bundle-count\" title=\"2 commands\">2 commands</span>"
        );
    }

    #[test]
    fn session_chips_trim_labels_and_describe_provenance() {
        let row = session_meta_row();
        let chips = session_meta_values(&row);
        assert_eq!(chips.len(), 2);
        let model = chips.first().expect("model chip");
        assert_eq!(model.class, "session-chip session-chip-model");
        assert_eq!(model.label, "sglang_dsv4");
        assert_eq!(model.title, "Model provider");
        let agent = chips.get(1).expect("agent chip");
        assert_eq!(agent.class, "session-chip session-chip-agent");
        assert_eq!(agent.label, "Harvey");
        assert_eq!(agent.title, "Agent");
        assert_eq!(
            session_meta_description(&row),
            "Model provider sglang_dsv4, Agent Harvey"
        );
        let partial = session_meta_values(&with(
            &base_row(),
            &[(
                "session_meta",
                json!({ "model_provider": "   ", "agent_nickname": "Harvey" }),
            )],
        ));
        assert_eq!(partial.len(), 1);
        assert_eq!(partial.first().expect("agent chip").label, "Harvey");
        assert!(session_meta_values(&base_row()).is_empty());
    }

    #[test]
    fn promoted_classes_match_the_rail_contract() {
        assert_eq!(
            promoted_kind(&promoted_failure_row(), ViewMode::Activity).expect("failure"),
            PromotedKind::Failure
        );
        assert_eq!(promoted_kind(&promoted_failure_row(), ViewMode::Raw), None);
        assert_eq!(
            promoted_kind(
                &with(
                    &promoted_failure_row(),
                    &[
                        ("promoted", json!(true)),
                        ("outcome", json!("unknown")),
                        ("activity_kind", json!("change")),
                    ]
                ),
                ViewMode::Activity
            ),
            Some(PromotedKind::Change)
        );
        assert_eq!(
            promoted_kind(
                &with(
                    &promoted_failure_row(),
                    &[
                        ("promoted", json!(true)),
                        ("outcome", json!("unknown")),
                        ("activity_kind", json!("verify")),
                    ]
                ),
                ViewMode::Activity
            ),
            Some(PromotedKind::Verify)
        );
        assert_eq!(
            promoted_kind(
                &with(
                    &promoted_failure_row(),
                    &[("promoted", json!(true)), ("outcome", json!("unknown")),]
                ),
                ViewMode::Activity
            ),
            Some(PromotedKind::Rail)
        );
        assert_eq!(
            PromotedKind::Failure.classes(),
            "row-promoted row-promoted-failure"
        );
        assert_eq!(
            PromotedKind::Change.classes(),
            "row-promoted row-promoted-change"
        );
        assert_eq!(
            PromotedKind::Verify.classes(),
            "row-promoted row-promoted-verify"
        );
        assert_eq!(
            PromotedKind::Rail.classes(),
            "row-promoted row-promoted-rail"
        );
    }

    #[test]
    fn work_unit_classes_distinguish_start_and_end() {
        assert_eq!(
            work_unit_class_of(&wu_start_row()).map(WorkUnitClass::class),
            Some("row-work-unit-start")
        );
        assert_eq!(
            work_unit_class_of(&with(
                &base_row(),
                &[(
                    "work_unit",
                    json!({ "id": "u", "is_start": false, "is_end": true, "count": 5 })
                ),]
            ))
            .map(WorkUnitClass::class),
            Some("row-work-unit-end")
        );
        assert_eq!(work_unit_class_of(&base_row()), None);
    }

    fn work_unit_class_of(row: &Value) -> Option<WorkUnitClass> {
        let wu = work_unit_of(row, ViewMode::Activity)?;
        if wu.is_start {
            Some(WorkUnitClass::Start)
        } else if wu.is_end {
            Some(WorkUnitClass::End)
        } else {
            None
        }
    }

    #[test]
    fn git_summary_parts_split_the_conventional_prefix_exactly() {
        assert_eq!(
            git_summary_parts(&git_row(), "feat: add thing"),
            Some(("feat".to_owned(), "add thing".to_owned()))
        );
        assert_eq!(git_summary_parts(&git_row(), "no prefix here"), None);
        assert_eq!(git_summary_parts(&git_row(), ": leading colon"), None);
        assert_eq!(git_summary_parts(&base_row(), "feat: add thing"), None);
        assert_eq!(
            git_summary_parts(&git_row(), "  fix  :  spaced  prefix  "),
            Some(("fix".to_owned(), "spaced  prefix  ".to_owned()))
        );
    }

    #[test]
    fn plain_summaries_drop_the_colon_and_markdown_decoration() {
        assert_eq!(
            plain_row_summary(&git_row(), "feat: add thing"),
            "feat add thing"
        );
        assert_eq!(plain_row_summary(&git_row(), "feat:"), "feat");
        assert_eq!(
            plain_row_summary(&base_row(), "# Hello **world**\n- item"),
            "Hello world · item"
        );
    }

    #[test]
    fn display_summaries_compact_only_operational_payloads() {
        let tool = |overrides: &[(&str, Value)]| {
            with(
                &with(
                    &base_row(),
                    &[
                        ("kind", json!("tool")),
                        ("record_role", json!("result")),
                        ("activity_kind", json!("execute")),
                        ("outcome", json!("success")),
                        ("is_system", json!(true)),
                    ],
                ),
                overrides,
            )
        };
        assert_eq!(
            display_summary_for_row(&tool(&[]), "tool result: {\"text\": \"did thing\"}"),
            "tool result: {\"text\": \"did thing\"}"
        );
        assert_eq!(
            display_summary_for_row(&tool(&[]), "{\"text\": \"did thing\"}"),
            "did thing"
        );
        assert_eq!(
            display_summary_for_row(
                &tool(&[("outcome", json!("success"))]),
                "{\"opaque\": true}"
            ),
            "Completed"
        );
        assert_eq!(
            display_summary_for_row(
                &tool(&[("outcome", json!("failure"))]),
                "{\"opaque\": true}"
            ),
            "Failed"
        );
        assert_eq!(
            display_summary_for_row(
                &with(
                    &base_row(),
                    &[
                        ("kind", json!("command")),
                        ("record_role", json!("action")),
                        ("activity_kind", json!("execute")),
                    ]
                ),
                "{\"opaque\": true}"
            ),
            "Tool request"
        );
        assert_eq!(
            display_summary_for_row(
                &with(
                    &base_row(),
                    &[
                        ("record_role", json!("result")),
                        ("activity_kind", json!("execute")),
                    ],
                ),
                "{\"opaque\": true}"
            ),
            "Tool result"
        );
        let narrative = with(
            &base_row(),
            &[
                ("kind", json!("message")),
                ("record_role", json!("narrative")),
                ("activity_kind", json!("conversation")),
            ],
        );
        assert_eq!(
            display_summary_for_row(&narrative, "{\"narrative\": \"authored\"}"),
            "{\"narrative\": \"authored\"}"
        );
        assert_eq!(
            display_summary_for_row(&narrative, "just prose **bold**"),
            "just prose **bold**"
        );
        assert_eq!(
            display_summary_for_row(
                &tool(&[]),
                "Script completed in 12.3s\nwall time 12s\ncell 5"
            ),
            "Script completed in 12.3s\nwall time 12s\ncell 5",
            "non-wrapper prose is returned verbatim before compaction"
        );
    }

    // ---- Markdown plain-text goldens (production-captured) -------------------

    struct MdCase {
        source: &'static str,
        plain_inline: &'static str,
        plain_line: &'static str,
        plain_summary: &'static str,
        summary_html: &'static str,
    }

    const MD_CASES: &[MdCase] = &[
        MdCase { source: "# Hello **world**", plain_inline: "# Hello world", plain_line: "Hello world", plain_summary: "Hello world", summary_html: "<span class=\"md-line md-heading md-h1\"><span class=\"md-text\">Hello\u{00a0}</span><strong class=\"md-strong\"><span class=\"md-text\">world</span></strong></span>" },
        MdCase { source: "### Deep ## title ###", plain_inline: "### Deep ## title ###", plain_line: "Deep ## title ###", plain_summary: "Deep ## title ###", summary_html: "<span class=\"md-line md-heading md-h3\"><span class=\"md-text\">Deep ## title</span></span>" },
        MdCase { source: "- [x] done task with **bold**", plain_inline: "- [x] done task with bold", plain_line: "done task with bold", plain_summary: "done task with bold", summary_html: "<span class=\"md-line md-list md-task md-task-done\"><span class=\"md-marker\" aria-hidden=\"true\">\u{2713}</span><span class=\"md-text\">done task with\u{00a0}</span><strong class=\"md-strong\"><span class=\"md-text\">bold</span></strong></span>" },
        MdCase { source: "* [ ] open task", plain_inline: "* [ ] open task", plain_line: "open task", plain_summary: "open task", summary_html: "<span class=\"md-line md-list md-task\"><span class=\"md-marker\" aria-hidden=\"true\">\u{25cb}</span><span class=\"md-text\">open task</span></span>" },
        MdCase { source: "- first item", plain_inline: "- first item", plain_line: "first item", plain_summary: "first item", summary_html: "<span class=\"md-line md-list\"><span class=\"md-marker\" aria-hidden=\"true\">\u{2022}</span><span class=\"md-text\">first item</span></span>" },
        MdCase { source: "1. ordered item", plain_inline: "1. ordered item", plain_line: "ordered item", plain_summary: "ordered item", summary_html: "<span class=\"md-line md-list\"><span class=\"md-marker\" aria-hidden=\"true\">1.</span><span class=\"md-text\">ordered item</span></span>" },
        MdCase { source: "42) paren ordered", plain_inline: "42) paren ordered", plain_line: "paren ordered", plain_summary: "paren ordered", summary_html: "<span class=\"md-line md-list\"><span class=\"md-marker\" aria-hidden=\"true\">42)</span><span class=\"md-text\">paren ordered</span></span>" },
        MdCase { source: "> quoted text", plain_inline: "> quoted text", plain_line: "quoted text", plain_summary: "quoted text", summary_html: "<span class=\"md-line md-quote\"><span class=\"md-marker\" aria-hidden=\"true\">\u{203a}</span><span class=\"md-text\">quoted text</span></span>" },
        MdCase { source: "> [!NOTE] callout body", plain_inline: "> [!NOTE] callout body", plain_line: "callout body", plain_summary: "callout body", summary_html: "<span class=\"md-line md-quote\"><span class=\"md-callout\">NOTE</span><span class=\"md-text\">callout body</span></span>" },
        MdCase { source: "```rust\nfn main() {}", plain_inline: "rust fn main() {}", plain_line: "fn main() {}", plain_summary: "fn main() {}", summary_html: "<span class=\"md-line\"><span class=\"md-text\">fn main() {}</span></span>" },
        MdCase { source: "``` plain code", plain_inline: "plain code", plain_line: "code", plain_summary: "code", summary_html: "<span class=\"md-line md-fence\"><span class=\"md-callout\">plain</span><span class=\"md-text\">code</span></span>" },
        MdCase { source: "just plain text with `code` and *em* and _em2_ and ~~strike~~", plain_inline: "just plain text with code and em and em2 and strike", plain_line: "just plain text with code and em and em2 and strike", plain_summary: "just plain text with code and em and em2 and strike", summary_html: "<span class=\"md-line\"><span class=\"md-text\">just plain text with\u{00a0}</span><code class=\"md-code\">code</code><span class=\"md-text\">\u{00a0}and\u{00a0}</span><em class=\"md-em\"><span class=\"md-text\">em</span></em><span class=\"md-text\">\u{00a0}and\u{00a0}</span><em class=\"md-em\"><span class=\"md-text\">em2</span></em><span class=\"md-text\">\u{00a0}and\u{00a0}</span><span class=\"md-strike\"><span class=\"md-text\">strike</span></span></span>" },
        MdCase { source: "work_unit_id stays *literal*, but *this* is em", plain_inline: "work_unit_id stays literal, but this is em", plain_line: "work_unit_id stays literal, but this is em", plain_summary: "work_unit_id stays literal, but this is em", summary_html: "<span class=\"md-line\"><span class=\"md-text\">work_unit_id stays\u{00a0}</span><em class=\"md-em\"><span class=\"md-text\">literal</span></em><span class=\"md-text\">, but\u{00a0}</span><em class=\"md-em\"><span class=\"md-text\">this</span></em><span class=\"md-text\">\u{00a0}is em</span></span>" },
        MdCase { source: "outer **strong with *inner* em** end", plain_inline: "outer strong with inner em end", plain_line: "outer strong with inner em end", plain_summary: "outer strong with inner em end", summary_html: "<span class=\"md-line\"><span class=\"md-text\">outer\u{00a0}</span><strong class=\"md-strong\"><span class=\"md-text\">strong with\u{00a0}</span><em class=\"md-em\"><span class=\"md-text\">inner</span></em><span class=\"md-text\">\u{00a0}em</span></strong><span class=\"md-text\">\u{00a0}end</span></span>" },
        MdCase { source: "see [the docs](https://example.com/a) and ![alt img](img.png) and [empty](x) ", plain_inline: "see the docs and alt img and empty", plain_line: "see the docs and alt img and empty", plain_summary: "see the docs and alt img and empty", summary_html: "<span class=\"md-line\"><span class=\"md-text\">see\u{00a0}</span><span class=\"md-link\" title=\"https://example.com/a\"><span class=\"md-text\">the docs</span></span><span class=\"md-text\">\u{00a0}and\u{00a0}</span><span class=\"md-image\" title=\"img.png\"><span aria-hidden=\"true\">image · </span><span class=\"md-text\">alt img</span></span><span class=\"md-text\">\u{00a0}and\u{00a0}</span><span class=\"md-link\" title=\"x\"><span class=\"md-text\">empty</span></span></span>" },
        MdCase { source: "escaped \\*stars\\* and \\`tick\\` and \\[brackets\\]", plain_inline: "escaped stars and tick and [brackets]", plain_line: "escaped stars and tick and [brackets]", plain_summary: "escaped stars and tick and [brackets]", summary_html: "<span class=\"md-line\"><span class=\"md-text\">escaped stars and tick and [brackets]</span></span>" },
        MdCase { source: "use `let x = 1;` and ``double`` and `unclosed", plain_inline: "use let x = 1; and double and unclosed", plain_line: "use let x = 1; and double and unclosed", plain_summary: "use let x = 1; and double and unclosed", summary_html: "<span class=\"md-line\"><span class=\"md-text\">use\u{00a0}</span><code class=\"md-code\">let x = 1;</code><span class=\"md-text\">\u{00a0}and\u{00a0}</span><code class=\"md-code\">double</code><span class=\"md-text\">\u{00a0}and unclosed</span></span>" },
        MdCase { source: "**bold **unclosed* and ~~tildes~~ and `backtick", plain_inline: "bold unclosed* and tildes and backtick", plain_line: "bold unclosed* and tildes and backtick", plain_summary: "bold unclosed* and tildes and backtick", summary_html: "<span class=\"md-line\"><strong class=\"md-strong\"><span class=\"md-text\">bold\u{00a0}</span></strong><span class=\"md-text\">unclosed* and\u{00a0}</span><span class=\"md-strike\"><span class=\"md-text\">tildes</span></span><span class=\"md-text\">\u{00a0}and backtick</span></span>" },
        MdCase { source: "<b>bold html</b> and <a href=\"x\">link</a>", plain_inline: "bold html and link", plain_line: "bold html and link", plain_summary: "bold html and link", summary_html: "<span class=\"md-line\"><span class=\"md-text\">bold html and link</span></span>" },
        MdCase { source: "<b>x</b> and <i>y</i>", plain_inline: "x and y", plain_line: "x and y", plain_summary: "x and y", summary_html: "<span class=\"md-line\"><span class=\"md-text\">x and y</span></span>" },
        MdCase { source: "<b>a</b>\n<b>c</b>", plain_inline: "a c", plain_line: "a c", plain_summary: "a · c", summary_html: "<span class=\"md-line\"><span class=\"md-text\">a</span></span><span class=\"md-more\" aria-hidden=\"true\">+1 line</span>" },
        MdCase { source: "first line\n\n- second **line**\nthird", plain_inline: "first line - second line third", plain_line: "first line - second line third", plain_summary: "first line · second line · third", summary_html: "<span class=\"md-line\"><span class=\"md-text\">first line</span></span><span class=\"md-more\" aria-hidden=\"true\">+2 lines</span>" },
        MdCase { source: "   ", plain_inline: "", plain_line: "", plain_summary: "", summary_html: "<span class=\"md-line md-empty\">Structured content</span>" },
        MdCase { source: "", plain_inline: "", plain_line: "", plain_summary: "", summary_html: "<span class=\"md-line md-empty\">Structured content</span>" },
        MdCase { source: "line one\r\nline two\r\n", plain_inline: "line one line two", plain_line: "line one line two", plain_summary: "line one · line two", summary_html: "<span class=\"md-line\"><span class=\"md-text\">line one</span></span><span class=\"md-more\" aria-hidden=\"true\">+1 line</span>" },
        MdCase { source: "{\"text\": \"hello from json\", \"type\": \"output\"}", plain_inline: "{\"text\": \"hello from json\", \"type\": \"output\"}", plain_line: "{\"text\": \"hello from json\", \"type\": \"output\"}", plain_summary: "{\"text\": \"hello from json\", \"type\": \"output\"}", summary_html: "<span class=\"md-line\"><span class=\"md-text\">{&quot;text&quot;: &quot;hello from json&quot;, &quot;type&quot;: &quot;output&quot;}</span></span>" },
    ];

    #[test]
    fn markdown_plain_and_summary_goldens_match_production() {
        for case in MD_CASES {
            assert_eq!(
                markdown_plain_inline(case.source),
                case.plain_inline,
                "plainInline for {:?}",
                case.source
            );
            assert_eq!(
                markdown_plain_line(case.source),
                case.plain_line,
                "plainLine for {:?}",
                case.source
            );
            assert_eq!(
                markdown_plain_summary(case.source),
                case.plain_summary,
                "plainSummary for {:?}",
                case.source
            );
            let summary = Summary::parse(case.source);
            assert_eq!(
                render_summary_html(&summary),
                case.summary_html,
                "renderMarkdownSummary for {:?}",
                case.source
            );
        }
    }

    #[test]
    fn summary_structure_and_more_tail_follow_production() {
        let summary = Summary::parse("first\nsecond\nthird");
        assert_eq!(summary.more, 2);
        assert_eq!(summary.line.kind, MdLineKind::Plain);
        let heading = Summary::parse("# Title ###");
        assert_eq!(
            heading.line.kind,
            MdLineKind::Heading { level: 1 },
            "trailing hashes are stripped from the heading content"
        );
        assert_eq!(heading.more, 0);
        let empty = Summary::parse("\n  \n");
        assert_eq!(empty.line.kind, MdLineKind::Empty);
        assert_eq!(empty.more, 0);
    }

    #[test]
    fn markdown_inline_depth_cap_falls_back_to_plain_text() {
        let inline = render_markdown_inline("**a**", 5);
        assert_eq!(
            inline,
            vec![MdInline::Text("a".to_owned())],
            "depth > 4 renders plain text"
        );
    }

    #[test]
    fn code_span_edges_strip_one_space_each_side() {
        let summary = Summary::parse("`` x ``");
        assert_eq!(
            summary.line.kind,
            MdLineKind::Plain,
            "single-line code span classifies as plain"
        );
        let code = find_code(&summary.line.inline).expect("code token");
        assert_eq!(code, "x");
    }

    fn find_code(inline: &[MdInline]) -> Option<String> {
        for token in inline {
            if let MdInline::Code(code) = token {
                return Some(code.clone());
            }
        }
        None
    }

    #[test]
    fn html_tag_strip_removes_tags_but_keeps_same_line_text() {
        // JS `\<\/?[A-Za-z][^>\n]*>` ends at the tag's own first `>`, so the
        // text between tags on one line survives exactly like the legacy JS
        // renderer.
        assert_eq!(markdown_plain_inline("<b>a</b> x"), "a x");
        assert_eq!(markdown_plain_inline("<b>a</b> and <i>y</i>"), "a and y");
        assert_eq!(markdown_plain_inline("<b>a</b> x <i>y</i>"), "a x y");
        assert_eq!(
            markdown_plain_inline("<b class=\"x\">bold</b> text"),
            "bold text"
        );
    }

    #[test]
    fn whitespace_fragments_become_space_tokens() {
        let summary = Summary::parse("a `x`   **b**");
        let has_space = summary
            .line
            .inline
            .iter()
            .any(|t| matches!(t, MdInline::Space));
        assert!(
            has_space,
            "whitespace-only fragment becomes an md-space token"
        );
        assert_eq!(summary.line.inline.len(), 4, "Text, Code, Space, Strong");
    }

    // ---- RowSpec contract goldens (production-captured) ----------------------

    fn context(view: ViewMode, abs: i64, group_start: bool) -> RowContext {
        RowContext::for_row(view, abs, group_start)
    }

    fn assert_row(
        name: &str,
        row: &Value,
        ctx: &RowContext,
        expected: (&[&str], &str, &str, &str, &str, &str),
    ) {
        let spec = RowSpec::from_value(row, ctx);
        let (classes, summary_class, content, date, author, commit) = expected;
        assert_eq!(
            spec.classes().split_whitespace().collect::<Vec<&str>>(),
            classes,
            "{name}: class list",
        );
        let summary_div_class = format!(
            "summary {}{}",
            if spec.content_flags.work_unit_block {
                "work-unit-block "
            } else {
                ""
            },
            if spec.content_flags.work_unit_title_only {
                "work-unit-title-only"
            } else {
                ""
            }
        );
        assert_eq!(
            summary_div_class.trim(),
            summary_class,
            "{name}: summary div class"
        );
        let rendered_content = render_content_html(&spec);
        assert_eq!(rendered_content, content, "{name}: content cell");
        let rendered_tags = render_tags_html(&spec);
        assert_eq!(
            rendered_tags.matches("<span").count(),
            spec.tags.len(),
            "{name}: every tag renders once in the Tags column"
        );
        for chip_class in [
            "git-prefix-chip",
            "bundle-count",
            "bundle-status",
            "session-chip",
            "rel-badge",
            "out-badge",
            "work-unit-count",
        ] {
            assert!(
                !rendered_content.contains(chip_class),
                "{name}: {chip_class} must not render in Content"
            );
        }
        assert_eq!(spec.date_text, date, "{name}: date cell");
        assert_eq!(spec.author_text, author, "{name}: author cell");
        assert_eq!(spec.commit_text, commit, "{name}: commit cell");
    }

    #[test]
    fn plain_row_spec_matches_the_production_row() {
        assert_row("plain",
&base_row(),
&context(ViewMode::Activity, 3, true),
(
&["row", "row-role-narrative", "row-group-start"],
"summary",
"<span class=\"summary-text\"><span class=\"md-line\"><span class=\"md-text\">Agent turn with metadata</span></span></span>",
"Jan 15, 2026 03:59 PM",
"agent",
"t1"
));
        assert_row("plain_not_group_start",
&base_row(),
&context(ViewMode::Activity, 3, false),
(
&["row", "row-role-narrative"],
"summary",
"<span class=\"summary-text\"><span class=\"md-line\"><span class=\"md-text\">Agent turn with metadata</span></span></span>",
"Jan 15, 2026 03:59 PM",
"agent",
"t1"
));
    }

    #[test]
    fn muted_chain_state_drives_row_class_and_graph_masks() {
        let row = with(
            &base_row(),
            &[
                ("chain_state", json!("muted")),
                ("muted_above", json!([1])),
                ("muted_below", json!([1, 2])),
                ("muted_transitions", json!([[1, 0]])),
            ],
        );
        let spec = RowSpec::from_value(&row, &context(ViewMode::Activity, 3, false));
        assert!(spec.classes().contains("row-chain-muted"));
        assert_eq!(spec.graph.chain_state, ChainState::Muted);
        assert_eq!(spec.graph.muted_above, vec![1]);
        assert_eq!(spec.graph.muted_below, vec![1, 2]);
        assert_eq!(spec.graph.muted_transitions, vec![(1, 0)]);

        let active = RowSpec::from_value(&base_row(), &context(ViewMode::Activity, 3, false));
        assert!(!active.classes().contains("row-chain-muted"));
        assert_eq!(active.graph.chain_state, ChainState::Active);
    }

    #[test]
    fn selection_find_and_roving_tabindex_are_context_driven() {
        let mut ctx = context(ViewMode::Activity, 3, false);
        ctx.selected_key = Some("op:1".to_owned());
        ctx.roving_abs = Some(3);
        let spec = RowSpec::from_value(&base_row(), &ctx);
        assert!(spec.classes().contains("row-selected"));
        assert!(spec.aria.aria_selected);
        assert_eq!(spec.aria.tabindex, 0);
        assert!(spec.flags.selected);

        let mut ctx = context(ViewMode::Activity, 7, false);
        ctx.find_current = true;
        let spec = RowSpec::from_value(&base_row(), &ctx);
        assert!(spec.classes().contains("row-find-current"));
        assert!(!spec.classes().contains("row-selected"));
        assert!(!spec.aria.aria_selected);
        assert_eq!(spec.aria.tabindex, -1);
    }

    #[test]
    fn git_rows_carry_the_prefix_chip_and_deterministic_aria() {
        let ctx = context(ViewMode::Activity, 1, true);
        let spec = RowSpec::from_value(&git_row(), &ctx);
        assert_row("git",
&git_row(),
&ctx.clone(),
(
&["row", "row-dim", "row-role-artifact", "row-has-badges", "row-group-start"],
"summary",
"<span class=\"summary-text git-summary-text\"><span class=\"md-line\"><span class=\"md-text\">add\u{00a0}</span><strong class=\"md-strong\"><span class=\"md-text\">search</span></strong><span class=\"md-text\">\u{00a0}bar</span></span><span class=\"md-more\" aria-hidden=\"true\">+1 line</span></span>",
"Jan 15, 2026 04:00 PM",
"ambientlight",
"git:abc123d"
));
        assert_eq!(
            render_tags_html(&spec),
            "<span class=\"git-prefix-chip\" title=\"Commit prefix: feat\" aria-label=\"Commit prefix: feat\">feat</span>"
        );
        assert_eq!(spec.aria.aria_label, "feat add search bar · second line");
        assert_eq!(spec.aria.title, "feat: add search bar · second line");
        assert_eq!(
            spec.aria.base_aria_label,
            "feat add search bar · second line"
        );
        let summary = spec
            .content
            .top
            .as_ref()
            .and_then(|top| top.summary.as_ref())
            .expect("git row summary");
        assert_eq!(summary.git_prefix.as_deref(), Some("feat"));
        assert_eq!(
            summary.plain_content.as_deref(),
            Some("add search bar · second line"),
            "the DOM content excludes the prefix already rendered in the chip"
        );
        assert_eq!(spec.identity.node_key, "git:abc123def456");
        assert_eq!(spec.identity.abs_index, 1);
        assert_eq!(
            spec.identity.git_oid,
            "git:abc123def4567890abcdef1234567890abcdef12"
        );
        assert_eq!(spec.identity.repository, "9007199254740993");
        assert_eq!(spec.identity.turn_id, "");
        assert_eq!(spec.classification.label, "git");
        assert_eq!(spec.classification.source, "activity_kind");
        assert_eq!(
            spec.group_label, None,
            "group markers must not label a graph node connected on both sides"
        );
    }

    #[test]
    fn graph_data_feeds_the_wgpu_frame_contract() {
        let spec = RowSpec::from_value(&git_row(), &context(ViewMode::Activity, 1, false));
        assert_eq!(spec.graph.lane, 1);
        assert_eq!(spec.graph.above, vec![0, 1]);
        assert_eq!(spec.graph.below, vec![1]);
        assert_eq!(spec.graph.transitions, vec![(0, 1)]);
        assert!(!spec.graph.is_subop);
        assert!(!spec.graph.is_bundle);
        assert!(!spec.identity.is_subop);
        assert_eq!(spec.identity.node_key, "git:abc123def456");
        let bundle_spec = RowSpec::from_value(&bundle_row(), &context(ViewMode::Activity, 0, true));
        assert!(bundle_spec.graph.is_bundle);
        assert!(!bundle_spec.graph.expanded);
        assert_eq!(
            bundle_spec.identity.node_key, "bundle:exec1",
            "bundle rows keep their own data-key identity"
        );
        let mut expanded_context = context(ViewMode::Activity, 0, true);
        expanded_context.expanded = true;
        let expanded_bundle = RowSpec::from_value(&bundle_row(), &expanded_context);
        assert!(
            expanded_bundle.graph.expanded,
            "the live disclosure state reaches graph marker selection"
        );
    }

    #[test]
    fn subop_rows_keep_identity_for_opened_group_markers() {
        let ctx = context(ViewMode::Activity, 4, false);
        let spec = RowSpec::from_value(&subop_row(), &ctx);
        assert!(spec.identity.is_subop);
        assert!(spec.graph.is_subop);
        assert_eq!(spec.identity.node_key, "node:1::sub:0");
        assert_eq!(spec.identity.subop_kind, "edit");
        assert_row("subop_edit",
&subop_row(),
&ctx,
(
&["row", "row-dim", "row-subop", "row-role-narrative"],
"summary",
"<span class=\"subop-icon codicon codicon-edit\" aria-hidden=\"true\"></span><span class=\"subop-summary\"><span class=\"md-line\"><span class=\"md-text\">custom-title metadata</span></span></span>",
"Jan 15, 2026 04:00 PM",
"",
"ode:1::sub:0"
));
        let msg_spec = RowSpec::from_value(
            &with(
                &subop_row(),
                &[
                    ("subop_kind", json!("msg")),
                    ("summary", json!("mode")),
                    ("node_key", json!("node:1::sub:1")),
                ],
            ),
            &context(ViewMode::Activity, 5, false),
        );
        assert_eq!(
            msg_spec.identity.node_key.as_str(),
            "node:1::sub:1",
            "sub-op data-key is its own wire identity, never synthesized from the parent"
        );
        assert_eq!(
            msg_spec.content.subop.as_ref().expect("subop content").icon,
            "comment"
        );
    }

    #[test]
    fn disclosure_rows_expose_expandable_and_chevron_labels() {
        let collapsed =
            RowSpec::from_value(&expandable_row(), &context(ViewMode::Activity, 0, false));
        assert!(collapsed.state.expandable);
        assert_eq!(collapsed.expanded(), Some(false));
        assert_eq!(collapsed.aria.aria_expanded, Some(false));
        assert!(collapsed.classes().contains("row-expandable"));
        let disclosure = collapsed.disclosure.as_ref().expect("chevron");
        assert_eq!(disclosure.label, "Expand 2 details");
        assert_eq!(
            render_activity_html(&collapsed),
            "<span class=\"activity-label\">agent</span><button type=\"button\" class=\"subop-chevron\" title=\"Expand 2 details\" aria-label=\"Expand 2 details\" aria-expanded=\"false\">\u{25b8}</button>",
            "the collapsed arrow follows the Activity label"
        );
        assert!(!render_content_html(&collapsed).contains("subop-chevron"));

        let mut ctx = context(ViewMode::Activity, 0, false);
        ctx.expanded = true;
        let expanded = RowSpec::from_value(&expandable_row(), &ctx);
        assert_eq!(expanded.expanded(), Some(true));
        assert_eq!(expanded.aria.aria_expanded, Some(true));
        assert_eq!(
            expanded.disclosure.as_ref().expect("chevron").label,
            "Collapse 2 details"
        );
        assert!(render_activity_html(&expanded).ends_with("\u{25be}</button>"));
    }

    #[test]
    fn bundle_rows_own_chrome_aria_and_data_attributes() {
        let ctx = context(ViewMode::Activity, 0, true);
        let spec = RowSpec::from_value(&bundle_row(), &ctx);
        assert_row(
            "bundle_execute",
            &bundle_row(),
            &ctx.clone(),
            (
                &[
                    "row",
                    "row-tool",
                    "row-role-action",
                    "row-has-badges",
                    "row-group-start",
                    "row-activity-bundle",
                    "row-expandable",
                ],
                "summary",
                "",
                "Jan 15, 2026 04:00 PM",
                "",
                "bundle:exec1",
            ),
        );
        assert_eq!(
            render_tags_html(&spec),
            "<span class=\"bundle-count\" title=\"2 commands, completed\">2 commands</span><span class=\"bundle-status bundle-status-success\" title=\"completed\" aria-label=\"completed\">\u{2713}</span>"
        );
        assert_eq!(spec.aria.aria_label, "Execute run, 2 steps, completed");
        assert!(is_activity_bundle(&bundle_row(), ViewMode::Activity));
        assert!(is_execute_run_bundle(&bundle_row(), ViewMode::Activity));
        assert!(!is_plan_repeat_bundle(&bundle_row(), ViewMode::Activity));
        assert!(
            !is_activity_bundle(&bundle_row(), ViewMode::Raw),
            "raw view gates bundle chrome"
        );
        let bundle = spec.bundle.as_ref().expect("bundle info");
        assert_eq!(bundle.kind, BundleKind::ExecuteRun);
        assert_eq!(bundle.member_count, Some(2));
        assert!(bundle.status_success);
        let attrs = render_attrs(&spec);
        assert_eq!(
            attrs.get("data-activity-bundle").map(String::as_str),
            Some("execute-run")
        );
        assert_eq!(
            attrs.get("data-bundle-count").map(String::as_str),
            Some("2")
        );

        let plan = RowSpec::from_value(&bundle_plan_row(), &ctx);
        assert_eq!(
            plan.aria.aria_label,
            "Plan group, 3 updates: Repeated plan: same step plan"
        );
        assert_eq!(
            plan.bundle.as_ref().expect("bundle").kind,
            BundleKind::PlanRepeat
        );
        assert!(is_plan_repeat_bundle(
            &bundle_plan_row(),
            ViewMode::Activity
        ));
        assert!(
            plan.content
                .top
                .as_ref()
                .and_then(|top| top.summary.as_ref())
                .is_some(),
            "plan bundles keep their narrative heading"
        );
    }

    #[test]
    fn raw_view_disables_the_semantic_layer() {
        let raw = RowSpec::from_value(&bundle_row(), &context(ViewMode::Raw, 0, true));
        assert!(raw.bundle.is_none());
        assert!(raw.promoted.is_none());
        assert!(raw.work_unit.is_none());
        assert!(!raw.classes().contains("row-activity-bundle"));
        assert_eq!(raw.aria.aria_label, "tool result: execute run (2 steps)");
        assert!(raw
            .content
            .top
            .as_ref()
            .and_then(|top| top.summary.as_ref())
            .is_some());
        let promoted_raw =
            RowSpec::from_value(&promoted_failure_row(), &context(ViewMode::Raw, 3, false));
        assert!(promoted_raw.promoted.is_none());
        assert!(!promoted_raw.classes().contains("row-promoted"));
    }

    #[test]
    fn work_unit_start_rows_render_the_ribbon_header() {
        let ctx = context(ViewMode::Activity, 0, true);
        let spec = RowSpec::from_value(&wu_start_row(), &ctx);
        assert_row("wu_start",
&wu_start_row(),
&ctx.clone(),
(
&["row", "row-human", "row-role-narrative", "row-has-badges", "row-work-unit-start"],
"summary work-unit-block work-unit-title-only",
"<span class=\"work-unit-ribbon-line\"><span class=\"work-unit-ribbon\" title=\"User request: make the search faster\">User request: make the search faster</span></span>",
"Jan 15, 2026 04:00 PM",
"human",
"t1"
));
        assert_eq!(
            render_tags_html(&spec),
            "<span class=\"work-unit-count\" title=\"5 entries grouped in this activity\">5 entries</span><span class=\"session-chip session-chip-model\" title=\"Model provider: sglang_dsv4\" aria-label=\"Model provider: sglang_dsv4\">sglang_dsv4</span><span class=\"session-chip session-chip-agent\" title=\"Agent: Harvey\" aria-label=\"Agent: Harvey\">Harvey</span>"
        );
        assert_eq!(
            spec.aria.aria_label,
            "User request: make the search faster, Model provider sglang_dsv4, Agent Harvey"
        );
        assert_eq!(
            spec.aria.base_aria_label,
            "User request: make the search faster"
        );
        assert!(spec.content_flags.work_unit_title_only);
        let attrs = render_attrs(&spec);
        assert_eq!(
            attrs.get("data-work-unit-id").map(String::as_str),
            Some("session:s1/turn:t1")
        );

        // Not at a group start: no session tags, aria suffix, or group class;
        // the row's own work-unit count tag remains.
        let not_start =
            RowSpec::from_value(&wu_start_row(), &context(ViewMode::Activity, 0, false));
        assert!(!not_start.classes().contains("row-group-start"));
        assert_eq!(
            not_start.aria.aria_label,
            "User request: make the search faster"
        );

        // Title-only start: single ribbon line, no count for a single entry.
        let title_only = RowSpec::from_value(
            &wu_start_title_only_row(),
            &context(ViewMode::Activity, 1, false),
        );
        assert!(title_only.content_flags.work_unit_title_only);
        assert!(
            title_only
                .work_unit_header
                .as_ref()
                .expect("header")
                .title_only
        );
        assert!(
            !title_only
                .work_unit_header
                .as_ref()
                .expect("header")
                .show_count
        );
    }

    #[test]
    fn promoted_rows_get_rails_not_badges() {
        let spec = RowSpec::from_value(
            &promoted_failure_row(),
            &context(ViewMode::Activity, 3, false),
        );
        assert_eq!(spec.promoted, Some(PromotedKind::Failure));
        assert!(spec.classes().contains("row-promoted row-promoted-failure"));
        assert_eq!(spec.classification.label, "tooluse");
        assert_eq!(spec.tags.len(), 1);
        assert!(spec
            .tags
            .first()
            .is_some_and(|item| item.classes.starts_with("out-badge")));
    }

    #[test]
    fn graph_subtitles_only_appear_on_true_tips_and_roots() {
        let tip = with(
            &git_row(),
            &[
                ("above", json!([0])),
                ("transitions", json!([])),
                ("group_end", json!(false)),
            ],
        );
        let tip = RowSpec::from_value(&tip, &context(ViewMode::Activity, 1, false));
        assert_eq!(tip.group_label.as_deref(), Some("Git · repo 199254740993"));

        let root = with(
            &git_row(),
            &[
                ("below", json!([0])),
                ("transitions", json!([])),
                ("group_end", json!(false)),
            ],
        );
        let root = RowSpec::from_value(&root, &context(ViewMode::Activity, 2, false));
        assert_eq!(root.group_label.as_deref(), Some("Git · repo 199254740993"));

        let parent_anchored_middle = with(
            &git_row(),
            &[
                ("above", json!([0])),
                ("below", json!([1])),
                ("transitions", json!([[0, 1]])),
            ],
        );
        let parent_anchored_middle = RowSpec::from_value(
            &parent_anchored_middle,
            &context(ViewMode::Activity, 3, true),
        );
        assert_eq!(parent_anchored_middle.group_label, None);

        let child_anchored_middle = with(
            &git_row(),
            &[
                ("above", json!([1])),
                ("below", json!([0])),
                ("transitions", json!([[1, 0]])),
            ],
        );
        let child_anchored_middle = RowSpec::from_value(
            &child_anchored_middle,
            &context(ViewMode::Activity, 4, true),
        );
        assert_eq!(child_anchored_middle.group_label, None);
    }

    #[test]
    fn session_chips_stay_on_group_boundaries_while_labels_follow_graph_endpoints() {
        let ctx = context(ViewMode::Activity, 9, true);
        let spec = RowSpec::from_value(&session_meta_row(), &ctx);
        assert!(spec.classes().contains("row-has-badges"));
        assert!(spec.classes().contains("row-group-start"));
        assert_eq!(
            spec.aria.aria_label,
            "session boundary row, Model provider sglang_dsv4, Agent Harvey"
        );
        assert!(spec
            .tags
            .iter()
            .any(|tag| tag.classes == "session-chip session-chip-agent"));
        assert_eq!(
            spec.tags
                .iter()
                .map(|tag| (tag.classes.as_str(), tag.text.as_str()))
                .collect::<Vec<(&str, &str)>>(),
            vec![
                ("session-chip session-chip-model", "sglang_dsv4"),
                ("session-chip session-chip-agent", "Harvey"),
            ]
        );
        assert_eq!(spec.group_label, None);

        let not_group_start =
            RowSpec::from_value(&session_meta_row(), &context(ViewMode::Activity, 10, false));
        assert!(!not_group_start.classes().contains("row-has-badges"));
        assert!(!not_group_start.classes().contains("row-group-start"));
        assert!(not_group_start.tags.is_empty());
        assert_eq!(not_group_start.aria.aria_label, "session boundary row");

        let terminal = with(&session_meta_row(), &[("group_end", json!(true))]);
        let terminal = RowSpec::from_value(&terminal, &context(ViewMode::Activity, 12, false));
        assert_eq!(terminal.group_label, None);
        assert!(!terminal.classes().contains("row-group-start"));

        let tip = with(&session_meta_row(), &[("above", json!([]))]);
        let tip = RowSpec::from_value(&tip, &context(ViewMode::Activity, 13, true));
        assert_eq!(tip.group_label.as_deref(), Some("q0 · Harvey"));

        let root = with(&session_meta_row(), &[("below", json!([]))]);
        let root = RowSpec::from_value(&root, &context(ViewMode::Activity, 14, false));
        assert_eq!(root.group_label.as_deref(), Some("q0 · Harvey"));
    }

    #[test]
    fn placeholders_carry_identity_only() {
        let spec = RowSpec::placeholder(42);
        assert!(spec.placeholder);
        assert_eq!(spec.identity.abs_index, 42);
        assert_eq!(spec.identity.node_key, "");
        assert_eq!(RowSpec::PLACEHOLDER_CLASSES, "row row-placeholder");
        assert_eq!(spec.classes(), "row");
        assert_eq!(spec.open_json, None);
        assert!(spec.tags.is_empty());
        assert!(spec.classification.label.is_empty());
        assert!(spec.content.top.is_none());
    }

    #[test]
    fn open_json_envelopes_are_exact_and_eligibility_is_strict() {
        let git = git_row();
        let spec = RowSpec::from_value(&git, &context(ViewMode::Activity, 1, false));
        assert!(spec.open_json.is_some());
        let envelope = open_json_envelope(&git).expect("git envelope");
        assert_eq!(
            envelope.get("type").and_then(Value::as_str),
            Some("openJson")
        );
        assert_eq!(
            envelope.get("git_oid").and_then(Value::as_str),
            Some("git:abc123def4567890abcdef1234567890abcdef12")
        );
        assert_eq!(
            envelope.get("repository").and_then(Value::as_str),
            Some("9007199254740993")
        );

        let op_row = with(&base_row(), &[("op_id", json!("node:1"))]);
        let envelope = open_json_envelope(&op_row).expect("op envelope");
        assert_eq!(
            envelope.get("type").and_then(Value::as_str),
            Some("openJson")
        );
        assert_eq!(
            envelope.get("op_id").and_then(Value::as_str),
            Some("node:1")
        );
        assert!(envelope.get("git_oid").is_none());

        // Sub-ops carry their own op_id and stay eligible on their own key.
        let sub = subop_row();
        assert_eq!(
            open_json_envelope(&sub)
                .expect("subop")
                .get("op_id")
                .and_then(Value::as_str),
            Some("node:1::sub:0")
        );

        // A row with neither identifier is ineligible.
        let empty = with(&base_row(), &[("op_id", Value::Null)]);
        assert_eq!(open_json_envelope(&empty), None);
        assert!(!is_open_json_eligible(&empty));
        // Falsy identifiers are ineligible exactly like JS truthiness.
        assert_eq!(
            open_json_envelope(&with(&base_row(), &[("op_id", json!(""))])),
            None
        );
    }

    #[test]
    fn missing_additive_fields_default_like_the_production_row() {
        let sparse = json!({
            "node_key": "k",
            "summary": "sparse row",
        });
        let spec = RowSpec::from_value(&sparse, &context(ViewMode::Activity, 0, false));
        assert_eq!(spec.identity.op_id, "");
        assert_eq!(spec.identity.git_oid, "");
        assert_eq!(spec.identity.turn_id, "");
        assert_eq!(spec.record_role, "");
        assert_eq!(spec.activity_kind, "");
        assert_eq!(spec.outcome, "");
        assert_eq!(spec.role_class, None);
        assert_eq!(spec.kind_class, KindClass::Dim, "unknown kinds dim");
        assert_eq!(spec.graph.lane, 0);
        assert!(spec.graph.above.is_empty());
        assert!(spec.graph.below.is_empty());
        assert!(spec.graph.transitions.is_empty());
        assert_eq!(spec.date_text, "", "missing timestamp renders no date");
        assert_eq!(spec.author_text, "");
        assert_eq!(spec.commit_text, "");
        assert_eq!(spec.summary_source, "sparse row");
        assert!(spec.work_unit.is_none());
        assert!(spec.bundle.is_none());
        assert!(spec.promoted.is_none());
        assert_eq!(
            spec.open_json, None,
            "no identifiers -> no openJson envelope"
        );
        // Summary pipeline defaults.
        let no_summary = RowSpec::from_value(
            &with(&base_row(), &[("summary", json!(""))]),
            &context(ViewMode::Activity, 14, false),
        );
        assert_eq!(no_summary.summary_source, "(no summary)");
        assert_eq!(no_summary.plain_summary, "(no summary)");
        assert_eq!(no_summary.detail_summary, "(no summary)");
        assert_eq!(no_summary.aria.aria_label, "(no summary)");
        assert_eq!(no_summary.aria.title, "(no summary)");
    }

    #[test]
    fn row_aria_attrs_match_build_row_html_exactly() {
        let mut ctx = context(ViewMode::Activity, 3, true);
        ctx.roving_abs = Some(3);
        let spec = RowSpec::from_value(&base_row(), &ctx);
        let attrs = render_attrs(&spec);
        assert_eq!(attrs.get("role").map(String::as_str), Some("row"));
        assert_eq!(attrs.get("tabindex").map(String::as_str), Some("0"));
        assert_eq!(
            attrs.get("aria-selected").map(String::as_str),
            Some("false")
        );
        assert_eq!(attrs.get("data-row").map(String::as_str), Some("3"));
        assert_eq!(attrs.get("data-key").map(String::as_str), Some("op:1"));
        assert_eq!(
            attrs.get("data-classification").map(String::as_str),
            Some("agent")
        );
        assert_eq!(
            attrs.get("aria-label").map(String::as_str),
            Some("Agent turn with metadata")
        );
        assert_eq!(
            attrs.get("title").map(String::as_str),
            Some("Agent turn with metadata")
        );
        assert_eq!(
            attrs.get("data-base-aria-label").map(String::as_str),
            Some("Agent turn with metadata")
        );
        assert!(
            !attrs.contains_key("aria-expanded"),
            "no aria-expanded without sub-ops"
        );
        assert!(!attrs.contains_key("data-work-unit-id"));
        assert!(!attrs.contains_key("data-activity-bundle"));
        assert!(!attrs.contains_key("data-bundle-count"));
    }

    #[test]
    fn tool_payload_rows_render_the_compacted_display_summary() {
        let tool = with(
            &base_row(),
            &[
                ("node_key", json!("tool:1")),
                (
                    "summary",
                    json!("tool result: {\"text\": \"done the thing\", \"type\": \"output\"}"),
                ),
                ("kind", json!("tool")),
                ("record_role", json!("result")),
                ("activity_kind", json!("execute")),
                ("outcome", json!("success")),
                ("is_system", json!(true)),
                ("timestamp_ms", json!(now())),
                ("session_meta", Value::Null),
            ],
        );
        let spec = RowSpec::from_value(&tool, &context(ViewMode::Activity, 11, false));
        assert_eq!(
            spec.display_summary,
            "tool result: {\"text\": \"done the thing\", \"type\": \"output\"}"
        );
        assert_row("tool_payload",
&tool,
&context(ViewMode::Activity, 11, false),
(
&["row", "row-tool", "row-role-result"],
"summary",
"<span class=\"summary-text\"><span class=\"md-line\"><span class=\"md-text\">tool result: {&quot;text&quot;: &quot;done the thing&quot;, &quot;type&quot;: &quot;output&quot;}</span></span></span>",
"Jan 15, 2026 04:00 PM",
"agent",
"t1"
));
    }

    #[test]
    fn relation_badges_dedupe_and_skip_unknown_kinds() {
        let spec = RowSpec::from_value(
            &with(
                &base_row(),
                &[(
                    "parent_relations",
                    json!([
                        { "kind": "fork" },
                        { "kind": "fork" },
                        { "kind": "subagent" },
                        { "kind": "future-kind" },
                    ]),
                )],
            ),
            &context(ViewMode::Activity, 6, false),
        );
        assert_eq!(spec.tags.len(), 2);
        assert!(spec.classes().contains("row-has-badges"));
        let fork = spec.tags.first().expect("fork badge");
        assert_eq!(fork.classes, "rel-badge rel-fork");
        let subagent = spec.tags.get(1).expect("subagent badge");
        assert_eq!(subagent.classes, "rel-badge rel-subagent");
        assert_eq!(spec.classification.label, "agent");
    }

    #[test]
    fn additional_row_goldens_cover_the_flat_and_raw_surfaces() {
        // Human message: strong + code inline, author class.
        let human = with(
            &base_row(),
            &[
                ("author", json!("human")),
                ("summary", json!("human message with **bold** and `code`")),
            ],
        );
        assert_row("human",
&human,
&context(ViewMode::Activity, 2, false),
(
&["row", "row-human", "row-role-narrative"],
"summary",
"<span class=\"summary-text\"><span class=\"md-line\"><span class=\"md-text\">human message with\u{00a0}</span><strong class=\"md-strong\"><span class=\"md-text\">bold</span></strong><span class=\"md-text\">\u{00a0}and\u{00a0}</span><code class=\"md-code\">code</code></span></span>",
"Jan 15, 2026 03:59 PM",
"human",
"t1"
));

        // System rows are tool-classed; their activity is rendered separately.
        let system = with(
            &base_row(),
            &[
                ("node_key", json!("op:3")),
                ("summary", json!("system record")),
                ("kind", json!("tool")),
                ("is_system", json!(true)),
                ("record_role", json!("lifecycle")),
                ("activity_kind", json!("system")),
                ("author", json!("")),
                ("commit_id", json!("")),
                ("turn_id", json!("")),
                ("op_id", Value::Null),
                ("timestamp_ms", json!(now())),
            ],
        );
        assert_row("system",
&system,
&context(ViewMode::Activity, 5, false),
(
&["row", "row-tool", "row-role-lifecycle"],
"summary",
"<span class=\"summary-text\"><span class=\"md-line\"><span class=\"md-text\">system record</span></span></span>",
"Jan 15, 2026 04:00 PM",
"",
""
));

        // A tool result row keeps its flat summary in the raw view.
        let dim = with(
            &base_row(),
            &[
                ("node_key", json!("op:2")),
                ("summary", json!("op result: tool output")),
                ("kind", json!("tool")),
                ("record_role", json!("result")),
                ("activity_kind", json!("execute")),
                ("author", json!("")),
                ("op_id", json!("node:2::sub:1")),
                ("commit_id", json!("node:2")),
                ("turn_id", json!("")),
                ("timestamp_ms", json!(now())),
            ],
        );
        assert_row("dim_tool",
&dim,
&context(ViewMode::Raw, 4, false),
(
&["row", "row-dim", "row-role-result"],
"summary",
"<span class=\"summary-text\"><span class=\"md-line\"><span class=\"md-text\">op result: tool output</span></span></span>",
"Jan 15, 2026 04:00 PM",
"",
"node:2"
));

        // Raw view: bundles flatten back to ordinary expandable rows.
        assert_row("raw_view_bundle",
&bundle_row(),
&context(ViewMode::Raw, 0, true),
(
&["row", "row-tool", "row-role-action", "row-group-start", "row-expandable"],
"summary",
"<span class=\"summary-text\"><span class=\"md-line\"><span class=\"md-text\">tool result: execute run (2 steps)</span></span></span>",
"Jan 15, 2026 04:00 PM",
"",
"bundle:exec1"
));

        // Unknown bundle kinds stay flat (no bundle chrome, no bundle class).
        let unknown = with(
            &bundle_row(),
            &[
                ("node_key", json!("bundle:future")),
                (
                    "activity_bundle",
                    json!({ "kind": "future-kind", "member_count": 4 }),
                ),
            ],
        );
        assert_row("bundle_unknown_kind",
&unknown,
&context(ViewMode::Activity, 0, false),
(
&["row", "row-tool", "row-role-action", "row-expandable"],
"summary",
"<span class=\"summary-text\"><span class=\"md-line\"><span class=\"md-text\">tool result: execute run (2 steps)</span></span></span>",
"Jan 15, 2026 04:00 PM",
"",
"bundle:exec1"
));

        // Work-unit end rows carry the boundary class and stay flat.
        let wu_end = with(
            &base_row(),
            &[
                ("node_key", json!("wu:a2")),
                ("kind", json!("tool")),
                ("record_role", json!("result")),
                ("activity_kind", json!("execute")),
                ("outcome", json!("success")),
                (
                    "work_unit",
                    json!({ "id": "session:s1/turn:t1", "title": "x", "is_start": false, "is_end": true, "count": 5 }),
                ),
                ("session_meta", Value::Null),
            ],
        );
        assert_row("wu_end",
&wu_end,
&context(ViewMode::Activity, 2, false),
(
&["row", "row-dim", "row-role-result", "row-work-unit-end"],
"summary",
"<span class=\"summary-text\"><span class=\"md-line\"><span class=\"md-text\">Agent turn with metadata</span></span></span>",
"Jan 15, 2026 03:59 PM",
"agent",
"t1"
));

        // Narrative JSON stays authored content (no compaction).
        let narrative_json = with(
            &base_row(),
            &[(
                "summary",
                json!("{\"narrative\": \"keep this as authored\"}"),
            )],
        );
        assert_row("narrative_json",
&narrative_json,
&context(ViewMode::Activity, 13, false),
(
&["row", "row-role-narrative"],
"summary",
"<span class=\"summary-text\"><span class=\"md-line\"><span class=\"md-text\">{&quot;narrative&quot;: &quot;keep this as authored&quot;}</span></span></span>",
"Jan 15, 2026 03:59 PM",
"agent",
"t1"
));

        // Relation + consequential outcome tags keep the branch semantics.
        let rel_warning = with(
            &base_row(),
            &[
                ("parent_relations", json!([{ "kind": "reconnect" }])),
                ("outcome", json!("warning")),
                ("session_meta", Value::Null),
            ],
        );
        assert_row("relations_reconnect_warning",
&rel_warning,
&context(ViewMode::Activity, 7, false),
(
&["row", "row-role-narrative", "row-has-badges"],
"summary",
"<span class=\"summary-text\"><span class=\"md-line\"><span class=\"md-text\">Agent turn with metadata</span></span></span>",
"Jan 15, 2026 03:59 PM",
"agent",
"t1"
));
        let relation_spec =
            RowSpec::from_value(&rel_warning, &context(ViewMode::Activity, 7, false));
        assert_eq!(
            relation_spec
                .tags
                .iter()
                .map(|tag| tag.text.as_str())
                .collect::<Vec<&str>>(),
            vec!["↩ return", "warn"]
        );

        // Session metadata renders no tags away from the group boundary and
        // contributes no badge class there.
        assert_row("session_meta_not_group_start",
&session_meta_row(),
&context(ViewMode::Activity, 10, false),
(
&["row", "row-role-narrative"],
"summary",
"<span class=\"summary-text\"><span class=\"md-line\"><span class=\"md-text\">session boundary row</span></span></span>",
"Jan 15, 2026 03:59 PM",
"agent",
"node:1"
));
    }

    #[test]
    fn subop_icon_variants_render_in_the_content_cell() {
        let msg = with(
            &subop_row(),
            &[
                ("node_key", json!("node:1::sub:1")),
                ("summary", json!("mode")),
                ("subop_kind", json!("msg")),
            ],
        );
        assert_row("subop_msg",
&msg,
&context(ViewMode::Activity, 5, false),
(
&["row", "row-dim", "row-subop", "row-role-narrative"],
"summary",
"<span class=\"subop-icon codicon codicon-comment\" aria-hidden=\"true\"></span><span class=\"subop-summary\"><span class=\"md-line\"><span class=\"md-text\">mode</span></span></span>",
"Jan 15, 2026 04:00 PM",
"",
"ode:1::sub:0"
));
        let msg_spec = RowSpec::from_value(&msg, &context(ViewMode::Activity, 5, false));
        assert_eq!(
            msg_spec.classification.label, "agent",
            "expanded sub-rows retain the Activity-column classification"
        );
        let meta = with(
            &subop_row(),
            &[
                ("node_key", json!("node:1::sub:2")),
                ("summary", json!("tool_result stuff")),
                ("subop_kind", json!("meta")),
            ],
        );
        let spec = RowSpec::from_value(&meta, &context(ViewMode::Activity, 6, false));
        assert_eq!(
            spec.content.subop.as_ref().expect("subop").icon,
            "info",
            "meta and unknown kinds fall back to the info codicon"
        );
    }

    #[test]
    fn find_match_and_expanded_states_apply_classes_and_aria() {
        let expanded = RowSpec::from_value(
            &expandable_row(),
            &RowContext {
                view: ViewMode::Activity,
                abs_index: 0,
                is_group_start: false,
                selected_key: None,
                find_current: true,
                expanded: true,
                roving_abs: Some(0),
            },
        );
        assert!(expanded.classes().contains("row-find-current"));
        assert!(expanded.classes().contains("row-expandable"));
        assert_eq!(expanded.aria.aria_expanded, Some(true));
        assert_eq!(expanded.aria.tabindex, 0);
        assert_eq!(
            render_content_html(&expanded),
            "<span class=\"summary-text\"><span class=\"md-line\"><span class=\"md-text\">Agent turn with metadata</span></span></span>",
            "moving disclosure leaves the content summary unchanged"
        );
        assert!(render_activity_html(&expanded).contains("aria-expanded=\"true\">\u{25be}"));
    }
}
