//! Pure row presentation model for one decoded `HistoryRow`.
//!
//! This module ports the row-presentation half of the legacy JS controller
//! into a deterministic, target-independent layer. Given a raw cached row
//! ([`RowSpec::from_row`]) it resolves every presentation input the DOM
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
//! - graph data consumed by the SVG frame contract (`lane`, `above`, `below`,
//!   `transitions`, `is_subop`, `is_bundle`), and
//! - the exact `openJson` identity envelope for eligible rows and `openDiff`
//!   envelope for source-control-style file rows. Ineligible rows yield
//!   `None` (production announces "No raw record is available for this row").
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

use serde_json::Value;

use super::row_input::RowInput;
use super::ChainState;
use editchain_protocol::{FileChangeSource, FileChangeStatus, ParentRelationKind};

/// Shell-owned state a row build depends on (selection, find highlight,
/// expansion, roving tabindex, and group boundary).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct RowContext {
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

impl RowContext {
    /// The common per-row context with defaulted stateful flags. Production
    /// contexts come from the state machine (`HistoryAppState::row_context`);
    /// this helper exists for the native spec goldens.
    #[cfg(test)]
    pub(crate) fn for_row(abs_index: i64, is_group_start: bool) -> RowContext {
        RowContext {
            abs_index,
            is_group_start,
            ..RowContext::default()
        }
    }
}

/// One row's identity + graph frame data, the inputs the later DOM/SVG
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
    /// `subop_kind` wire value used as a semantic fallback for detail rows.
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
fn is_graph_endpoint(row: &RowInput) -> bool {
    let lane = row.lane();
    let above = row.above();
    let below = row.below();
    let transitions = row.transitions();
    let connected_above =
        above.contains(&lane) || transitions.iter().any(|&(_, to_lane)| to_lane == lane);
    let connected_below =
        below.contains(&lane) || transitions.iter().any(|&(from_lane, _)| from_lane == lane);

    !connected_above || !connected_below
}

/// `shortCommitId` — display value for the Commit/ID column.
pub(crate) fn short_commit_id(row: &RowInput) -> String {
    let source = &row.source;
    let git_oid = source.git_oid.as_deref().unwrap_or_default();
    let op_id = source.op_id.as_deref().unwrap_or_default();
    let turn_id = source.turn_id.as_deref().unwrap_or_default();
    let preferred = if !git_oid.is_empty() {
        if source.commit_id.is_empty() {
            git_oid
        } else {
            &source.commit_id
        }
    } else if source.is_subop {
        op_id
    } else if !turn_id.is_empty() {
        turn_id
    } else if source.commit_id.is_empty() {
        op_id
    } else {
        &source.commit_id
    };
    short_id(preferred)
}

/// Commit/ID column hover title (`row.commit_id || row.op_id || ''`).
pub(crate) fn commit_cell_title(row: &RowInput) -> String {
    if row.source.commit_id.is_empty() {
        row.source.op_id.clone().unwrap_or_default()
    } else {
        row.source.commit_id.clone()
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
pub(crate) fn git_summary_parts(row: &RowInput, value: &str) -> Option<(String, String)> {
    if row.source.git_oid.as_deref().unwrap_or_default().is_empty() {
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
    pub(crate) fn parse(row: &RowInput, display_summary: &str) -> RowSummary {
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
pub(crate) fn plain_row_summary(row: &RowInput, value: &str) -> String {
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
pub(crate) fn relation_badges(row: &RowInput) -> Vec<ChromeItem> {
    let relations = &row.source.parent_relations;
    let mut seen: Vec<RelationKind> = Vec::new();
    let mut out = Vec::new();
    for relation in relations {
        let kind = match relation.kind {
            ParentRelationKind::Subagent => RelationKind::Subagent,
            ParentRelationKind::Reconnect => RelationKind::Reconnect,
            ParentRelationKind::Fork => RelationKind::Fork,
            ParentRelationKind::ProducedCommit | ParentRelationKind::Unknown => continue,
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

/// One self-contained VS Code Codicon used at the start of structured Content.
///
/// The SVG geometry is compiled into the WASM renderer so icons remain visible
/// in a sandboxed webview without relying on workbench-global icon fonts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ActivityIcon {
    Work,
    Agent,
    User,
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

impl ActivityIcon {
    /// Stable Codicon name exposed to DOM/test probes.
    pub(crate) fn name(self) -> &'static str {
        match self {
            ActivityIcon::Work => "layers",
            ActivityIcon::Agent => "robot",
            ActivityIcon::User => "account",
            ActivityIcon::Plan => "checklist",
            ActivityIcon::Explore => "search",
            ActivityIcon::Execute => "tools",
            ActivityIcon::Change => "edit",
            ActivityIcon::Verify => "pass",
            ActivityIcon::Diagnose => "bug",
            ActivityIcon::Coordinate => "type-hierarchy",
            ActivityIcon::SourceControl => "source-control",
            ActivityIcon::External => "link-external",
            ActivityIcon::System => "settings",
        }
    }

    /// Source view box for this Codicon's vector geometry.
    pub(crate) fn view_box(self) -> &'static str {
        match self {
            ActivityIcon::SourceControl => "0 0 24 24",
            ActivityIcon::Work
            | ActivityIcon::Agent
            | ActivityIcon::User
            | ActivityIcon::Plan
            | ActivityIcon::Explore
            | ActivityIcon::Execute
            | ActivityIcon::Change
            | ActivityIcon::Verify
            | ActivityIcon::Diagnose
            | ActivityIcon::Coordinate
            | ActivityIcon::External
            | ActivityIcon::System => "0 0 16 16",
        }
    }

    /// Vector paths that make up the icon.
    pub(crate) fn paths(self) -> &'static [ActivityIconPath] {
        match self {
            ActivityIcon::Work => &[
                ActivityIconPath {
                    d: r"M8 8.99993C7.819 8.99993 7.643 8.95093 7.486 8.85793L2.486 5.85693C2.186 5.67793 2 5.34893 2 4.99993C2 4.65093 2.187 4.32093 2.486 4.14193L7.486 1.14293C7.789 0.95693 8.207 0.95493 8.517 1.14493L13.513 4.14293C13.813 4.32293 13.999 4.65093 13.999 4.99993C13.999 5.34893 13.812 5.67893 13.513 5.85793L8.513 8.85693C8.357 8.95093 8.181 8.99993 8 8.99993ZM8 1.99993L3 4.99993L8 7.99993L13 4.99993L8 1.99993Z",
                    even_odd: false,
                },
                ActivityIconPath {
                    d: r"M2.146 6.9873L8 10.5003L13.854 6.9873C13.946 7.1413 14 7.3173 14 7.5003C14 7.8493 13.814 8.1783 13.514 8.3583L8.514 11.3573C8.357 11.4513 8.181 11.5003 8 11.5003C7.819 11.5003 7.642 11.4513 7.486 11.3583L2.486 8.35731C2.187 8.17931 2 7.8503 2 7.5003C2 7.3163 2.054 7.1403 2.146 6.9873Z",
                    even_odd: false,
                },
                ActivityIconPath {
                    d: r"M2.146 9.4873L8 13.0003L13.854 9.4873C13.946 9.6413 14 9.8173 14 10.0003C14 10.3493 13.814 10.6783 13.514 10.8583L8.514 13.8573C8.357 13.9513 8.181 14.0003 8 14.0003C7.819 14.0003 7.642 13.9513 7.486 13.8583L2.486 10.8573C2.187 10.6793 2 10.3503 2 10.0003C2 9.8163 2.054 9.6403 2.146 9.4873Z",
                    even_odd: false,
                },
            ],
            ActivityIcon::Agent => &[ActivityIconPath {
                d: r"M12 9H4C3.173 9 2.5 9.673 2.5 10.5V11C2.5 11.123 2.562 14 8 14C13.438 14 13.5 11.123 13.5 11V10.5C13.5 9.673 12.827 9 12 9ZM12.5 10.991C12.497 11.073 12.372 13 8 13C3.628 13 3.503 11.073 3.5 11V10.5C3.5 10.224 3.724 10 4 10H12C12.276 10 12.5 10.224 12.5 10.5V10.991ZM5.5 8H10.5C11.327 8 12 7.327 12 6.5V3.5C12 2.673 11.327 2 10.5 2H8.5V1.5C8.5 1.224 8.276 1 8 1C7.724 1 7.5 1.224 7.5 1.5V2H5.5C4.673 2 4 2.673 4 3.5V6.5C4 7.327 4.673 8 5.5 8ZM5 3.5C5 3.224 5.224 3 5.5 3H10.5C10.776 3 11 3.224 11 3.5V6.5C11 6.776 10.776 7 10.5 7H5.5C5.224 7 5 6.776 5 6.5V3.5ZM5.75 5C5.75 4.586 6.086 4.25 6.5 4.25C6.914 4.25 7.25 4.586 7.25 5C7.25 5.414 6.914 5.75 6.5 5.75C6.086 5.75 5.75 5.414 5.75 5ZM8.75 5C8.75 4.586 9.086 4.25 9.5 4.25C9.914 4.25 10.25 4.586 10.25 5C10.25 5.414 9.914 5.75 9.5 5.75C9.086 5.75 8.75 5.414 8.75 5Z",
                even_odd: false,
            }],
            ActivityIcon::User => &[ActivityIconPath {
                d: r"M8 2C4.686 2 2 4.686 2 8C2 11.314 4.686 14 8 14C11.314 14 14 11.314 14 8C14 4.686 11.314 2 8 2ZM1 8C1 4.134 4.134 1 8 1C11.866 1 15 4.134 15 8C15 11.866 11.866 15 8 15C4.134 15 1 11.866 1 8ZM8 12.25C9.933 12.25 11.5 11.036 11.5 9.214C11.5 8.543 10.956 8 10.286 8H5.715C5.044 8 4.501 8.544 4.501 9.214C4.501 11.035 6.068 12.25 8.001 12.25H8ZM8 7.25C9.036 7.25 9.875 6.411 9.875 5.375C9.875 4.339 9.036 3.5 8 3.5C6.964 3.5 6.125 4.339 6.125 5.375C6.125 6.411 6.964 7.25 8 7.25Z",
                even_odd: false,
            }],
            ActivityIcon::Plan => &[ActivityIconPath {
                d: r"M4.85401 2.146C5.04901 2.341 5.04901 2.658 4.85401 2.853L2.85401 4.853C2.65901 5.048 2.34201 5.048 2.14701 4.853L1.14701 3.853C0.952013 3.658 0.952013 3.341 1.14701 3.146C1.34201 2.951 1.65901 2.951 1.85401 3.146L2.50001 3.792L4.14601 2.146C4.34101 1.951 4.65901 1.951 4.85401 2.146ZM14.5 4H6.50001C6.22401 4 6.00001 3.776 6.00001 3.5C6.00001 3.224 6.22401 3 6.50001 3H14.5C14.776 3 15 3.224 15 3.5C15 3.776 14.776 4 14.5 4ZM4.85401 11.146C5.04901 11.341 5.04901 11.658 4.85401 11.853L2.85401 13.853C2.65901 14.048 2.34201 14.048 2.14701 13.853L1.14701 12.853C0.952013 12.658 0.952013 12.341 1.14701 12.146C1.34201 11.951 1.65901 11.951 1.85401 12.146L2.50001 12.792L4.14601 11.146C4.34101 10.951 4.65901 10.951 4.85401 11.146ZM14.5 13H6.50001C6.22401 13 6.00001 12.776 6.00001 12.5C6.00001 12.224 6.22401 12 6.50001 12H14.5C14.776 12 15 12.224 15 12.5C15 12.776 14.776 13 14.5 13ZM4.85401 6.646C5.04901 6.841 5.04901 7.158 4.85401 7.353L2.85401 9.353C2.65901 9.548 2.34201 9.548 2.14701 9.353L1.14701 8.353C0.952013 8.158 0.952013 7.841 1.14701 7.646C1.34201 7.451 1.65901 7.451 1.85401 7.646L2.50001 8.292L4.14601 6.646C4.34101 6.451 4.65901 6.451 4.85401 6.646ZM14.5 8.5H6.50001C6.22401 8.5 6.00001 8.276 6.00001 8C6.00001 7.724 6.22401 7.5 6.50001 7.5H14.5C14.776 7.5 15 7.724 15 8C15 8.276 14.776 8.5 14.5 8.5Z",
                even_odd: false,
            }],
            ActivityIcon::Explore => &[ActivityIconPath {
                d: r"M10.0195 10.7266C9.06578 11.5217 7.83875 12 6.5 12C3.46243 12 1 9.53757 1 6.5C1 3.46243 3.46243 1 6.5 1C9.53757 1 12 3.46243 12 6.5C12 7.83875 11.5217 9.06578 10.7266 10.0195L13.8535 13.1464C14.0488 13.3417 14.0488 13.6583 13.8535 13.8536C13.6583 14.0488 13.3417 14.0488 13.1464 13.8536L10.0195 10.7266ZM11 6.5C11 4.01472 8.98528 2 6.5 2C4.01472 2 2 4.01472 2 6.5C2 8.98528 4.01472 11 6.5 11C8.98528 11 11 8.98528 11 6.5Z",
                even_odd: false,
            }],
            ActivityIcon::Execute => &[ActivityIconPath {
                d: r"M5.66901 0.999997C5.52101 0.945997 5.34701 0.968997 5.21401 1.062C5.08101 1.155 5.00201 1.308 5.00201 1.47V3.286C5.00201 3.561 4.77701 3.786 4.50201 3.786C4.22701 3.786 4.00201 3.561 4.00201 3.286V1.47C4.00201 1.308 3.92301 1.156 3.79001 1.062C3.65801 0.967997 3.48501 0.945997 3.33501 0.999997C1.93901 1.495 1.00201 2.816 1.00201 4.287C1.00201 5.646 1.79201 6.876 3.00201 7.449V13.5C3.00201 14.327 3.67501 15 4.50201 15C5.32901 15 6.00201 14.327 6.00201 13.5V7.449C7.21201 6.876 8.00201 5.646 8.00201 4.287C8.00201 2.816 7.06401 1.495 5.66901 0.999997ZM5.33601 6.644C5.13601 6.714 5.00201 6.904 5.00201 7.116V13.501C5.00201 13.776 4.77701 14.001 4.50201 14.001C4.22701 14.001 4.00201 13.776 4.00201 13.501V7.116C4.00201 6.904 3.86801 6.715 3.66801 6.644C2.67201 6.292 2.00201 5.345 2.00201 4.288C2.00201 3.496 2.38501 2.765 3.00201 2.301V3.288C3.00201 4.115 3.67501 4.788 4.50201 4.788C5.32901 4.788 6.00201 4.115 6.00201 3.288V2.301C6.61901 2.765 7.00201 3.496 7.00201 4.288C7.00201 5.346 6.33201 6.293 5.33601 6.644ZM13.5 8H13.002V4.118L13.449 3.223C13.509 3.105 13.518 2.967 13.476 2.841L12.976 1.341C12.908 1.137 12.716 0.998997 12.501 0.998997H10.501C10.286 0.998997 10.095 1.137 10.026 1.341L9.52601 2.841C9.48401 2.967 9.49401 3.105 9.55301 3.223L10 4.118V8H9.50001C9.22401 8 9.00001 8.224 9.00001 8.5V12.5C9.00001 13.879 10.121 15 11.5 15C12.879 15 14 13.879 14 12.5V8.5C14 8.224 13.776 8 13.5 8ZM10.862 2.001H12.141L12.461 2.963L12.054 3.777C12.02 3.846 12.001 3.923 12.001 4.001V8.001H11.001V4.001C11.001 3.924 10.983 3.847 10.949 3.777L10.542 2.963L10.862 2.001ZM13.002 12.5C13.002 13.327 12.329 14 11.502 14C10.675 14 10.002 13.327 10.002 12.5V9H13.002V12.5Z",
                even_odd: false,
            }],
            ActivityIcon::Change => &[ActivityIconPath {
                d: r"M14.236 1.76386C13.2123 0.740172 11.5525 0.740171 10.5289 1.76386L2.65722 9.63549C2.28304 10.0097 2.01623 10.4775 1.88467 10.99L1.01571 14.3755C0.971767 14.5467 1.02148 14.7284 1.14646 14.8534C1.27144 14.9783 1.45312 15.028 1.62432 14.9841L5.00978 14.1151C5.52234 13.9836 5.99015 13.7168 6.36433 13.3426L14.236 5.47097C15.2596 4.44728 15.2596 2.78755 14.236 1.76386ZM11.236 2.47097C11.8691 1.8378 12.8957 1.8378 13.5288 2.47097C14.162 3.10413 14.162 4.1307 13.5288 4.76386L12.75 5.54269L10.4571 3.24979L11.236 2.47097ZM9.75002 3.9569L12.0429 6.24979L5.65722 12.6355C5.40969 12.883 5.10023 13.0595 4.76117 13.1465L2.19447 13.8053L2.85327 11.2386C2.9403 10.8996 3.1168 10.5901 3.36433 10.3426L9.75002 3.9569Z",
                even_odd: false,
            }],
            ActivityIcon::Verify => &[
                ActivityIconPath {
                    d: r"M10.6484 5.64648C10.8434 5.45148 11.1605 5.45148 11.3555 5.64648C11.5498 5.84137 11.5499 6.15766 11.3555 6.35254L7.35547 10.3525C7.25747 10.4495 7.12898 10.499 7.00098 10.499C6.87299 10.499 6.74545 10.4505 6.64746 10.3525L4.64746 8.35254C4.45247 8.15754 4.45248 7.84148 4.64746 7.64648C4.84246 7.45148 5.15949 7.45148 5.35449 7.64648L7 9.29199L10.6465 5.64648H10.6484Z",
                    even_odd: false,
                },
                ActivityIconPath {
                    d: r"M8 1C11.86 1 15 4.14 15 8C15 11.86 11.86 15 8 15C4.14 15 1 11.86 1 8C1 4.14 4.14 1 8 1ZM8 2C4.691 2 2 4.691 2 8C2 11.309 4.691 14 8 14C11.309 14 14 11.309 14 8C14 4.691 11.309 2 8 2Z",
                    even_odd: true,
                },
            ],
            ActivityIcon::Diagnose => &[ActivityIconPath {
                d: r"M14.5 8H13V6C13 5.63 12.898 5.283 12.722 4.985L13.853 3.854C14.048 3.659 14.048 3.342 13.853 3.147C13.658 2.952 13.341 2.952 13.146 3.147L12.015 4.278C11.717 4.102 11.37 4 11 4C11 2.346 9.654 1 8 1C6.346 1 5 2.346 5 4C4.63 4 4.283 4.102 3.985 4.278L2.854 3.147C2.659 2.952 2.342 2.952 2.147 3.147C1.952 3.342 1.952 3.659 2.147 3.854L3.278 4.985C3.102 5.283 3 5.63 3 6V8H1.5C1.224 8 1 8.224 1 8.5C1 8.776 1.224 9 1.5 9H3C3 10.199 3.424 11.3 4.13 12.163L2.396 13.897C2.201 14.092 2.201 14.409 2.396 14.604C2.494 14.702 2.622 14.75 2.75 14.75C2.878 14.75 3.006 14.701 3.104 14.604L4.838 12.87C5.7 13.576 6.802 14 8.001 14C9.2 14 10.301 13.576 11.164 12.87L12.898 14.604C12.996 14.702 13.124 14.75 13.252 14.75C13.38 14.75 13.508 14.701 13.606 14.604C13.801 14.409 13.801 14.092 13.606 13.897L11.872 12.163C12.578 11.301 13.002 10.199 13.002 9H14.502C14.778 9 15.002 8.776 15.002 8.5C15.002 8.224 14.778 8 14.502 8H14.5ZM8 2C9.103 2 10 2.897 10 4H6C6 2.897 6.897 2 8 2ZM12 9C12 11.206 10.206 13 8 13C5.794 13 4 11.206 4 9V6C4 5.449 4.448 5 5 5H11C11.552 5 12 5.449 12 6V9Z",
                even_odd: false,
            }],
            ActivityIcon::Coordinate => &[ActivityIconPath {
                d: r"M8 1C6.61929 1 5.5 2.11929 5.5 3.5C5.5 4.7093 6.35863 5.71806 7.4995 5.94989V6.99994H5.36684C4.61209 6.99994 4.00024 7.61178 4.00024 8.36653V10.05C2.859 10.2815 2 11.2904 2 12.5C2 13.8807 3.11929 15 4.5 15C5.88071 15 7 13.8807 7 12.5C7 11.2906 6.14124 10.2818 5.00024 10.0501V8.36653C5.00024 8.16407 5.16437 7.99994 5.36684 7.99994H10.6337C10.8361 7.99994 11.0002 8.16407 11.0002 8.36653V10.05C9.859 10.2815 9 11.2904 9 12.5C9 13.8807 10.1193 15 11.5 15C12.8807 15 14 13.8807 14 12.5C14 11.2906 13.1412 10.2818 12.0002 10.0501V8.36653C12.0002 7.61178 11.3884 6.99994 10.6337 6.99994H8.4995V5.95009C9.64087 5.71865 10.5 4.70966 10.5 3.5C10.5 2.11929 9.38071 1 8 1ZM6.5 3.5C6.5 2.67157 7.17157 2 8 2C8.82843 2 9.5 2.67157 9.5 3.5C9.5 4.32843 8.82843 5 8 5C7.17157 5 6.5 4.32843 6.5 3.5ZM3 12.5C3 11.6716 3.67157 11 4.5 11C5.32843 11 6 11.6716 6 12.5C6 13.3284 5.32843 14 4.5 14C3.67157 14 3 13.3284 3 12.5ZM11.5 11C12.3284 11 13 11.6716 13 12.5C13 13.3284 12.3284 14 11.5 14C10.6716 14 10 13.3284 10 12.5C10 11.6716 10.6716 11 11.5 11Z",
                even_odd: false,
            }],
            ActivityIcon::SourceControl => &[ActivityIconPath {
                d: r"M21 8.25C21 6.1815 19.3185 4.5 17.25 4.5C15.1815 4.5 13.5 6.1815 13.5 8.25C13.5 10.023 14.739 11.5035 16.395 11.892C16.116 12.819 15.2655 13.5 14.25 13.5H9.75C8.9025 13.5 8.1285 13.7925 7.5 14.268V7.4235C9.21 7.0755 10.5 5.5605 10.5 3.75C10.5 1.6815 8.8185 0 6.75 0C4.6815 0 3 1.6815 3 3.75C3 5.562 4.29 7.0755 6 7.4235V16.575C4.29 16.923 3 18.438 3 20.2485C3 22.317 4.6815 23.9985 6.75 23.9985C8.8185 23.9985 10.5 22.317 10.5 20.2485C10.5 18.4755 9.261 16.995 7.605 16.6065C7.884 15.6795 8.7345 14.9985 9.75 14.9985H14.25C16.0845 14.9985 17.61 13.6725 17.931 11.9295C19.674 11.607 21 10.0845 21 8.25ZM4.5 3.75C4.5 2.5095 5.5095 1.5 6.75 1.5C7.9905 1.5 9 2.5095 9 3.75C9 4.9905 7.9905 6 6.75 6C5.5095 6 4.5 4.9905 4.5 3.75ZM9 20.25C9 21.4905 7.9905 22.5 6.75 22.5C5.5095 22.5 4.5 21.4905 4.5 20.25C4.5 19.0095 5.5095 18 6.75 18C7.9905 18 9 19.0095 9 20.25ZM17.25 10.5C16.0095 10.5 15 9.4905 15 8.25C15 7.0095 16.0095 6 17.25 6C18.4905 6 19.5 7.0095 19.5 8.25C19.5 9.4905 18.4905 10.5 17.25 10.5Z",
                even_odd: false,
            }],
            ActivityIcon::External => &[ActivityIconPath {
                d: r"M15 9.5V12.5C15 13.879 13.879 15 12.5 15H3.5C2.121 15 1 13.879 1 12.5V3.5C1 2.121 2.121 1 3.5 1H6.5C6.776 1 7 1.224 7 1.5C7 1.776 6.776 2 6.5 2H3.5C2.673 2 2 2.673 2 3.5V12.5C2 13.327 2.673 14 3.5 14H12.5C13.327 14 14 13.327 14 12.5V9.5C14 9.224 14.224 9 14.5 9C14.776 9 15 9.224 15 9.5ZM14.5 1H9.5C9.224 1 9 1.224 9 1.5C9 1.776 9.224 2 9.5 2H13.293L9.147 6.146C8.952 6.341 8.952 6.658 9.147 6.853C9.245 6.951 9.373 6.999 9.501 6.999C9.629 6.999 9.757 6.95 9.855 6.853L14.001 2.707V6.5C14.001 6.776 14.225 7 14.501 7C14.777 7 15.001 6.776 15.001 6.5V1.5C15.001 1.224 14.777 1 14.501 1H14.5Z",
                even_odd: false,
            }],
            ActivityIcon::System => &[ActivityIconPath {
                d: r"M6 9.5C6.93191 9.5 7.71496 10.1374 7.93699 11L13.5 11C13.7761 11 14 11.2239 14 11.5C14 11.7455 13.8231 11.9496 13.5899 11.9919L13.5 12L7.93673 12.001C7.71435 12.8631 6.93155 13.5 6 13.5C5.06845 13.5 4.28565 12.8631 4.06327 12.001L2.5 12C2.22386 12 2 11.7761 2 11.5C2 11.2545 2.17688 11.0504 2.41012 11.0081L2.5 11L4.06301 11C4.28504 10.1374 5.06809 9.5 6 9.5ZM6 10.5C5.44772 10.5 5 10.9477 5 11.5C5 12.0523 5.44772 12.5 6 12.5C6.55228 12.5 7 12.0523 7 11.5C7 10.9477 6.55228 10.5 6 10.5ZM10 2.5C10.9319 2.5 11.715 3.13738 11.937 3.99998L13.5 4C13.7761 4 14 4.22386 14 4.5C14 4.74546 13.8231 4.94961 13.5899 4.99194L13.5 5L11.9367 5.00102C11.7144 5.86312 10.9316 6.5 10 6.5C9.06845 6.5 8.28565 5.86312 8.06327 5.00102L2.5 5C2.22386 5 2 4.77614 2 4.5C2 4.25454 2.17688 4.05039 2.41012 4.00806L2.5 4L8.06301 3.99998C8.28504 3.13738 9.06809 2.5 10 2.5ZM10 3.5C9.44772 3.5 9 3.94772 9 4.5C9 5.05228 9.44772 5.5 10 5.5C10.5523 5.5 11 5.05228 11 4.5C11 3.94772 10.5523 3.5 10 3.5Z",
                even_odd: false,
            }],
        }
    }

    /// Resolve the Content icon for one stable Activity classification.
    fn from_label(label: &str) -> Option<ActivityIcon> {
        match label {
            "work" => Some(ActivityIcon::Work),
            "agent" => Some(ActivityIcon::Agent),
            "user" => Some(ActivityIcon::User),
            "plan" => Some(ActivityIcon::Plan),
            "explore" => Some(ActivityIcon::Explore),
            "tooluse" => Some(ActivityIcon::Execute),
            "change" => Some(ActivityIcon::Change),
            "verify" => Some(ActivityIcon::Verify),
            "diagnose" => Some(ActivityIcon::Diagnose),
            "coordinate" => Some(ActivityIcon::Coordinate),
            "git" => Some(ActivityIcon::SourceControl),
            "external" => Some(ActivityIcon::External),
            "meta" | "system" => Some(ActivityIcon::System),
            _ => None,
        }
    }
}

/// One vector path inside a Content icon.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ActivityIconPath {
    pub(crate) d: &'static str,
    pub(crate) even_odd: bool,
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
            ActivityKind::System => "meta",
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
    /// Hover/accessibility description retaining the semantic display value.
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
        let title = if label == "meta" {
            "Activity: meta".to_owned()
        } else {
            format!("{source_title}: {wire_value}")
        };
        RowClassification {
            label: label.to_owned(),
            source: source.to_owned(),
            title,
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
pub(crate) fn row_classification(row: &RowInput) -> RowClassification {
    let kind = row.source.kind.as_str();
    let activity = row.activity_kind.as_str();
    let git_oid = row.source.git_oid.as_deref().unwrap_or_default();
    if !git_oid.is_empty() || kind == "git" {
        return if activity == "source_control" {
            RowClassification::new("git", "activity_kind", "source_control")
        } else if !git_oid.is_empty() {
            RowClassification::new("git", "git_oid", git_oid)
        } else {
            RowClassification::new("git", "kind", "git")
        };
    }

    if let Some(activity_kind) = ActivityKind::from_wire(activity.trim()) {
        let label = if activity_kind == ActivityKind::Conversation
            && matches!(row.source.author.trim(), "human" | "user")
        {
            "user"
        } else {
            activity_kind.text()
        };
        return RowClassification::new(label, "activity_kind", activity.trim());
    }

    if row.source.is_system {
        return RowClassification::new("meta", "is_system", "true");
    }
    if let Some(kind) = classification_token(kind) {
        return RowClassification::new(&classification_fallback(kind), "kind", kind);
    }

    let role = row.record_role.as_str();
    if let Some(role) = classification_token(role) {
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
pub(crate) fn outcome_badge(row: &RowInput, options: BadgeOptions) -> Option<ChromeItem> {
    let wire_outcome = row.outcome.as_str();
    if wire_outcome == "success" && !options.show_success_outcome {
        return None;
    }
    let outcome = OutcomeKind::from_wire(wire_outcome)?;
    Some(ChromeItem::new(
        &format!("out-badge {}", outcome.class()),
        outcome.text(),
        &format!("outcome: {wire_outcome}"),
        Some(&format!("outcome: {}", outcome.aria())),
    ))
}

/// `relationBadges` + a consequential outcome badge. Structural relations
/// take over the semantic-tag slot; Activity remains in its own column.
fn relation_chrome(row: &RowInput, options: BadgeOptions) -> Vec<ChromeItem> {
    let mut items = relation_badges(row);
    if !items.is_empty() {
        let outcome = row.outcome.as_str();
        if matches!(outcome, "warning" | "failure" | "cancelled") {
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
    row: &RowInput,
    is_bundle: bool,
    options: BadgeOptions,
) -> Vec<ChromeItem> {
    if is_bundle {
        return bundle_chrome(row, options);
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
        "system" => Some("Meta"),
        _ => None,
    }
}

/// `workUnitOf` — the row's Activity work-unit payload (never on sub-ops).
pub(crate) fn work_unit_of(row: &RowInput) -> Option<WorkUnitData> {
    if row.source.is_subop {
        return None;
    }
    row.work_unit.clone()
}

/// Read the explicit whole-session marker emitted on the session's true newest
/// visible row. Work-unit identity is intentionally irrelevant: Codex mixes
/// turn-scoped rows with session-scoped lifecycle rows.
pub(crate) fn session_summary_of(row: &RowInput) -> Option<SessionSummaryData> {
    if row.source.is_subop {
        return None;
    }
    row.session_summary.clone()
}

/// `workUnitTitle` — DTO title's first meaningful line, else human fallbacks.
pub(crate) fn work_unit_title(row: &RowInput) -> String {
    if let Some(wu) = work_unit_of(row) {
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
    let activity_kind = row.activity_kind.as_str();
    if let Some(fallback) = work_unit_fallback(activity_kind) {
        return fallback.to_owned();
    }
    let kind = row.source.kind.as_str();
    if kind == "message" || kind == "command" {
        return "Request".to_owned();
    }
    group_label_text(&row.source.group, None, None)
}

/// `workUnitCountText` — human count text for a unit header.
pub(crate) fn work_unit_count_text(count: u64) -> String {
    format!("{count} entr{}", if count == 1 { "y" } else { "ies" })
}

/// `showWorkUnitCount` — keep grouping counts sparse.
pub(crate) fn show_work_unit_count(row: &RowInput, wu: &WorkUnitData) -> bool {
    wu.count.is_some_and(|count| count > 1) && row.activity_kind != "source_control"
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
pub(crate) fn session_meta_values(row: &RowInput) -> Vec<SessionChip> {
    let mut out = Vec::new();
    let Some(meta) = &row.source.session_meta else {
        return out;
    };
    if let Some(provider) = meta.model_provider.as_deref() {
        let trimmed = provider.trim();
        if !trimmed.is_empty() {
            out.push(SessionChip {
                class: "session-chip session-chip-model".to_owned(),
                label: trimmed.to_owned(),
                title: "Model provider".to_owned(),
            });
        }
    }
    if let Some(agent) = meta.agent_nickname.as_deref() {
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
pub(crate) fn session_meta_description(row: &RowInput) -> String {
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
}

/// `activityBundleKind` — the recognized typed bundle kind of a row.
pub(crate) fn activity_bundle_kind(row: &RowInput) -> Option<BundleKind> {
    row.bundle_kind
}

/// `isActivityBundle` — any recognized typed bundle row.
#[cfg(test)]
pub(crate) fn is_activity_bundle(row: &RowInput) -> bool {
    activity_bundle_kind(row).is_some()
}

/// `isExecuteRunBundle`.
pub(crate) fn is_execute_run_bundle(row: &RowInput) -> bool {
    activity_bundle_kind(row) == Some(BundleKind::ExecuteRun)
}

/// `isPlanRepeatBundle`.
#[cfg(test)]
pub(crate) fn is_plan_repeat_bundle(row: &RowInput) -> bool {
    activity_bundle_kind(row) == Some(BundleKind::PlanRepeat)
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
pub(crate) fn bundle_count_text(row: &RowInput) -> String {
    let Some(kind) = activity_bundle_kind(row) else {
        return String::new();
    };
    let Some(member_count) = row.bundle_count else {
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
    let command = row.source.kind == "command";
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
pub(crate) fn bundle_chrome(row: &RowInput, _options: BadgeOptions) -> Vec<ChromeItem> {
    let count_text = bundle_count_text(row);
    if count_text.is_empty() {
        return Vec::new();
    }
    let success = is_execute_run_bundle(row) && row.outcome == "success";
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
pub(crate) fn promoted_kind(row: &RowInput) -> Option<PromotedKind> {
    let promoted = !row.source.is_subop && row.source.promoted;
    if !promoted {
        return None;
    }
    match row.outcome.as_str() {
        "failure" => Some(PromotedKind::Failure),
        "warning" => Some(PromotedKind::Warning),
        "cancelled" => Some(PromotedKind::Cancelled),
        _ => match row.activity_kind.as_str() {
            "change" => Some(PromotedKind::Change),
            "verify" => Some(PromotedKind::Verify),
            _ => Some(PromotedKind::Rail),
        },
    }
}

/// Resolve the semantic icon used by an ordinary Content row. The explicit
/// activity taxonomy is authoritative; kind/sub-op fallbacks keep older and
/// forward-compatible rows inside the same visual grammar.
fn content_icon(row: &RowInput) -> ActivityIcon {
    if let Some(icon) = ActivityIcon::from_label(&row_classification(row).label) {
        return icon;
    }
    match row.source.subop_kind.as_deref().unwrap_or_default() {
        "edit" => return ActivityIcon::Change,
        "msg" => {
            return if matches!(row.source.author.trim(), "human" | "user") {
                ActivityIcon::User
            } else {
                ActivityIcon::Agent
            };
        }
        "tool_result" => return ActivityIcon::Execute,
        "meta" | "" => {}
        _ => return ActivityIcon::System,
    }
    match row.source.kind.as_str() {
        "git" => ActivityIcon::SourceControl,
        "file" => ActivityIcon::Change,
        "message" => {
            if matches!(row.source.author.trim(), "human" | "user") {
                ActivityIcon::User
            } else {
                ActivityIcon::Agent
            }
        }
        "reflection" => ActivityIcon::Plan,
        "command" | "tool" | "tool_result" => ActivityIcon::Execute,
        "error" => ActivityIcon::Diagnose,
        "import" if row.record_role == "artifact" => ActivityIcon::Change,
        "import" if row.record_role == "narrative" => ActivityIcon::Plan,
        _ => ActivityIcon::System,
    }
}

/// Humanize a forward-compatible kind token without inventing provider
/// semantics (`future_kind` -> `Future kind`).
fn humanize_content_kind(value: &str) -> String {
    let words = value
        .trim()
        .split(|c: char| c == '_' || c == '-' || c.is_whitespace())
        .filter(|word| !word.is_empty())
        .collect::<Vec<&str>>()
        .join(" ");
    let mut chars = words.chars();
    let Some(first) = chars.next() else {
        return "Activity".to_owned();
    };
    first.to_uppercase().chain(chars).collect()
}

/// Resolve the optional compact title between Content's icon and authored
/// summary. Commit, message, and aggregate work rows omit the redundant noun;
/// tool wrappers can provide a more useful concrete tool name for less obvious
/// activity.
fn content_title(row: &RowInput) -> String {
    let kind = row.source.kind.as_str();
    let role = row.record_role.as_str();
    if kind == "git" || !row.source.git_oid.as_deref().unwrap_or_default().is_empty() {
        return String::new();
    }
    if kind == "tool" {
        if let Some(tool_name) = &row.tool_label {
            return tool_name.clone();
        }
        return if role == "result" {
            "Tool result".to_owned()
        } else {
            "Tool".to_owned()
        };
    }
    match kind {
        "message" | "work-group" => String::new(),
        "command" if role == "result" => "Command output".to_owned(),
        "command" => "Command".to_owned(),
        "tool_result" => "Tool result".to_owned(),
        "file" => "Change".to_owned(),
        "reflection" => "Reflection".to_owned(),
        "error" => "Error".to_owned(),
        "note" => "Note".to_owned(),
        "import" => "Import".to_owned(),
        "token_usage_record" => "Token usage".to_owned(),
        "token_count" => "Token count".to_owned(),
        "world_state" => "World state".to_owned(),
        "turn_context" => "Turn context".to_owned(),
        "task_complete" => "Task complete".to_owned(),
        "session_title" => "Session title".to_owned(),
        "session_meta" => "Session metadata".to_owned(),
        "turn_aborted" => "Turn aborted".to_owned(),
        "execute-run" => "Run".to_owned(),
        "plan-repeat" => "Plan".to_owned(),
        "" => match role {
            "narrative" => String::new(),
            "action" => "Action".to_owned(),
            "result" => "Result".to_owned(),
            "artifact" => "Artifact".to_owned(),
            "lifecycle" => "Metadata".to_owned(),
            "echo" => "Echo".to_owned(),
            _ => "Activity".to_owned(),
        },
        other => humanize_content_kind(other),
    }
}

fn content_heading(row: &RowInput) -> ContentHeading {
    ContentHeading {
        icon: content_icon(row),
        title: content_title(row),
    }
}

/// `hasSubOps` — whether a top-level row carries bundled metadata sub-ops.
pub(crate) fn has_sub_ops(row: &RowInput) -> bool {
    !row.source.sub_ops.is_empty()
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

/// Semantic lead for a structured Content cell. Obvious row kinds may leave
/// `title` empty so the icon leads directly into the subtitle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ContentHeading {
    pub(crate) icon: ActivityIcon,
    pub(crate) title: String,
}

/// Sub-op row content: semantic icon, compact type/tool title, and indented
/// authored summary. Semantic tags live in the dedicated Tags column.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SubopContent {
    pub(crate) heading: Option<ContentHeading>,
    pub(crate) summary: Summary,
}

/// Source-control status for a rendered file row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FileRowStatus {
    Added,
    Modified,
    Deleted,
    Renamed,
    Copied,
    TypeChanged,
    Unknown,
}

impl FileRowStatus {
    pub(crate) const fn code(self) -> &'static str {
        match self {
            Self::Added => "A",
            Self::Modified => "M",
            Self::Deleted => "D",
            Self::Renamed => "R",
            Self::Copied => "C",
            Self::TypeChanged => "T",
            Self::Unknown => "?",
        }
    }

    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Added => "Added",
            Self::Modified => "Modified",
            Self::Deleted => "Deleted",
            Self::Renamed => "Renamed",
            Self::Copied => "Copied",
            Self::TypeChanged => "Type changed",
            Self::Unknown => "Changed",
        }
    }

    pub(crate) const fn class(self) -> &'static str {
        match self {
            Self::Added => "added",
            Self::Modified => "modified",
            Self::Deleted => "deleted",
            Self::Renamed => "renamed",
            Self::Copied => "copied",
            Self::TypeChanged => "type-changed",
            Self::Unknown => "unknown",
        }
    }
}

/// Column-aligned SCM content for one expandable Git/agent edit child.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FileContent {
    pub(crate) path: String,
    pub(crate) name: String,
    pub(crate) directory: String,
    pub(crate) status: FileRowStatus,
    pub(crate) source: String,
    pub(crate) fidelity: String,
    pub(crate) binary: bool,
    pub(crate) partial: bool,
    pub(crate) title: String,
    pub(crate) aria_label: String,
}

/// Top-level row content after all chip-like metadata moves to Tags.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct TopContent {
    pub(crate) heading: Option<ContentHeading>,
    pub(crate) summary: Option<RowSummary>,
}

/// The content cell tokens (sub-op rows XOR top-level rows with a work-unit
/// ribbon wrapper on top when a unit header is present).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct RowContent {
    pub(crate) file: Option<FileContent>,
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
    pub(crate) open_diff: Option<Value>,
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
            open_diff: None,
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

    #[cfg(test)]
    pub(crate) fn from_value(row: &Value, context: &RowContext) -> RowSpec {
        Self::from_row(&RowInput::from_legacy(row), context)
    }

    /// Build the full presentation model for one cached row value.
    pub(crate) fn from_row(row: &RowInput, context: &RowContext) -> RowSpec {
        let is_subop = row.source.is_subop;
        let file_content = file_content(row);
        let is_file = file_content.is_some();
        let node_key = row.source.node_key.clone();
        let selected = context
            .selected_key
            .as_deref()
            .is_some_and(|key| key == node_key);
        let work_unit = work_unit_of(row);
        let is_work_unit_start = work_unit.as_ref().is_some_and(|wu| wu.is_start);
        let session_summary = session_summary_of(row);
        let is_session_summary = session_summary.is_some();
        let has_boundary_header = work_unit.is_some() && (is_work_unit_start || is_session_summary);
        let bundle_kind = activity_bundle_kind(row);
        let is_bundle = bundle_kind.is_some();
        let is_work_group = bundle_kind == Some(BundleKind::WorkGroup);
        let is_execute_run = bundle_kind == Some(BundleKind::ExecuteRun);
        let is_plan_repeat = bundle_kind == Some(BundleKind::PlanRepeat);
        let semantic_tags = row_semantic_chrome(row, is_bundle, BadgeOptions::default());
        let classification = if is_file {
            RowClassification::new("change", "activity_kind", "change")
        } else if is_session_summary {
            RowClassification::session_summary()
        } else {
            row_classification(row)
        };
        let session = session_meta_values(row);
        let has_session_meta = !is_subop && !session.is_empty();
        let session_description = session_meta_description(row);
        let promoted = promoted_kind(row);
        let summary_source = row.summary_source().to_owned();
        let display_summary = row.display_summary.clone();
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
            work_unit_title(row)
        } else {
            String::new()
        };
        let sub_op_count = row.source.sub_ops.len();
        let has_subs = has_sub_ops(row);
        let expanded_state = has_subs && context.expanded;
        let expandable = has_subs;
        let child_label = if is_bundle {
            let count_text = bundle_count_text(row);
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
        let row_summary = (!is_subop).then(|| RowSummary::parse(row, &display_summary));
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
        if let Some(file) = &file_content {
            tags.clear();
            let status_class = format!("file-status file-status-{}", file.status.class());
            tags.push(ChromeItem::new(
                &status_class,
                file.status.code(),
                file.status.label(),
                Some(file.status.label()),
            ));
            if !file.fidelity.is_empty() {
                let fidelity_title = if file.binary {
                    "Binary file content"
                } else {
                    "Recorded agent edit"
                };
                tags.push(ChromeItem::new(
                    "file-fidelity",
                    &file.fidelity,
                    fidelity_title,
                    Some(fidelity_title),
                ));
            }
        }
        let has_badges = !tags.is_empty();
        let structured_heading = (!is_file).then(|| content_heading(row));
        let row_content = if let Some(file) = file_content.clone() {
            RowContent {
                file: Some(file),
                subop: None,
                top: None,
                work_unit: None,
            }
        } else if is_subop {
            RowContent {
                file: None,
                subop: Some(SubopContent {
                    heading: structured_heading,
                    summary: Summary::parse(&display_summary),
                }),
                top: None,
                work_unit: None,
            }
        } else {
            RowContent {
                file: None,
                subop: None,
                top: Some(TopContent {
                    heading: structured_heading,
                    summary: row_summary,
                }),
                work_unit: None,
            }
        };
        let group_start = context.is_group_start && !has_boundary_header;
        let group_label = if !is_subop && is_graph_endpoint(row) {
            Some(group_label_text(
                &row.source.group,
                row.source
                    .session_meta
                    .as_ref()
                    .and_then(|meta| meta.session_title.as_deref()),
                row.source
                    .session_meta
                    .as_ref()
                    .and_then(|meta| meta.agent_nickname.as_deref()),
            ))
        } else {
            None
        };
        // aria-label composition, mirroring buildRowHtml exactly.
        let mut aria_label = plain_summary.clone();
        if is_bundle {
            let member_count = row.bundle_count;
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
            if is_execute_run && row.outcome == "success" {
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
        if let Some(file) = &file_content {
            aria_label.clone_from(&file.aria_label);
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
            title: file_content
                .as_ref()
                .map_or_else(|| detail_summary.clone(), |file| file.title.clone()),
            base_aria_label: base_aria_label.clone(),
        };
        let mut bundle_info = None;
        if let Some(kind) = bundle_kind {
            bundle_info = Some(BundleInfo {
                kind,
                member_count: row.bundle_count,
                count_text: bundle_count_text(row),
                status_success: is_execute_run && row.outcome == "success",
            });
        }
        RowSpec {
            identity: RowIdentity {
                abs_index: context.abs_index,
                node_key: node_key.clone(),
                is_subop,
                hierarchy_depth: row.source.hierarchy_depth,
                subop_kind: row.source.subop_kind.clone().unwrap_or_default(),
                op_id: row.source.op_id.clone().unwrap_or_default(),
                git_oid: row.source.git_oid.clone().unwrap_or_default(),
                repository: row.source.repository.clone().unwrap_or_default(),
                commit_id: row.source.commit_id.clone(),
                turn_id: row.source.turn_id.clone().unwrap_or_default(),
            },
            graph: GraphData {
                lane: row.lane(),
                above: row.above(),
                below: row.below(),
                transitions: row.transitions(),
                muted_above: row.muted_above(),
                muted_below: row.muted_below(),
                muted_transitions: row.muted_transitions(),
                chain_state: row.chain_state,
                is_subop,
                is_bundle,
                expanded: expanded_state,
            },
            kind: row.source.kind.clone(),
            record_role: row.record_role.clone(),
            activity_kind: row.activity_kind.clone(),
            classification,
            outcome: row.outcome.clone(),
            group: row.source.group.clone(),
            kind_class: kind_class_of(row),
            role_class: role_class_of(row),
            flags: RowFlags {
                human: row.source.author == "human",
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
            author_text: row.source.author.clone(),
            date_text: format_date(i64::try_from(row.source.timestamp_ms).unwrap_or(0)),
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
            open_json: (!is_file).then(|| open_json_envelope(row)).flatten(),
            open_diff: open_diff_envelope(row),
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
        if let Some(file) = &self.content.file {
            classes.push_str(" row-file row-file-");
            classes.push_str(file.status.class());
            if file.binary {
                classes.push_str(" row-file-binary");
            }
            if file.partial {
                classes.push_str(" row-file-partial");
            }
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

fn file_content(row: &RowInput) -> Option<FileContent> {
    let change = row.source.file_change.as_ref()?;
    let path = change.path.trim().replace('\\', "/");
    if path.is_empty() {
        return None;
    }
    let (directory, name) = path.rsplit_once('/').map_or_else(
        || (String::new(), path.clone()),
        |(directory, name)| (directory.to_owned(), name.to_owned()),
    );
    let status = match change.status {
        FileChangeStatus::Added => FileRowStatus::Added,
        FileChangeStatus::Modified => FileRowStatus::Modified,
        FileChangeStatus::Deleted => FileRowStatus::Deleted,
        FileChangeStatus::Renamed => FileRowStatus::Renamed,
        FileChangeStatus::Copied => FileRowStatus::Copied,
        FileChangeStatus::TypeChanged => FileRowStatus::TypeChanged,
        FileChangeStatus::Unknown => FileRowStatus::Unknown,
    };
    let source = match change.source {
        FileChangeSource::Git => "git",
        FileChangeSource::Agent => "agent",
        FileChangeSource::Unknown => "unknown",
    }
    .to_owned();
    let binary = change.binary;
    let partial = change.partial;
    let fidelity = if binary {
        "binary"
    } else if partial {
        "recorded"
    } else {
        ""
    }
    .to_owned();
    let old_path = change.old_path.as_deref().filter(|old| !old.is_empty());
    let mut title = format!("{} · {path}", status.label());
    if let Some(old_path) = old_path {
        title.push_str(" ← ");
        title.push_str(old_path);
    }
    if binary {
        title.push_str(" · binary content");
    } else if partial {
        title.push_str(" · recorded edit (partial file evidence)");
    } else if source == "git" {
        title.push_str(" · exact Git blobs");
    }
    let source_label = if source == "git" {
        "Git commit"
    } else if partial {
        "recorded agent edit"
    } else {
        "agent edit"
    };
    let fidelity_label = if binary {
        ", binary content"
    } else if partial {
        ", partial file evidence"
    } else {
        ""
    };
    let aria_label = format!(
        "{} {path}, {source_label}{fidelity_label}; open diff",
        status.label()
    );
    Some(FileContent {
        path,
        name,
        directory,
        status,
        source,
        fidelity,
        binary,
        partial,
        title,
        aria_label,
    })
}

/// `subOpCount` details label (`N detail` / `N details`).
fn sub_op_label(sub_op_count: usize) -> String {
    format!(
        "{sub_op_count} detail{}",
        if sub_op_count == 1 { "" } else { "s" }
    )
}

/// The `kindClass` mapping (system -> tool, message/command -> none, else dim).
fn kind_class_of(row: &RowInput) -> KindClass {
    if row.source.is_system {
        KindClass::Tool
    } else {
        let kind = row.source.kind.as_str();
        if kind == "message" || kind == "command" {
            KindClass::Plain
        } else {
            KindClass::Dim
        }
    }
}

/// `RECORD_ROLE_CLASSES` whitelist lookup (suffix after `row-role-`).
fn role_class_of(row: &RowInput) -> Option<&'static str> {
    let record_role = row.record_role.clone();
    RECORD_ROLE_CLASSES
        .iter()
        .find(|candidate| **candidate == record_role)
        .copied()
}

/// `openDiff` identity envelope for a source-control-style file row.
pub(crate) fn open_diff_envelope(row: &RowInput) -> Option<Value> {
    let change = row.source.file_change.as_ref()?;
    if change.path.is_empty() {
        return None;
    }
    Some(serde_json::json!({ "type": "openDiff", "change": change }))
}

/// `openJson` identity envelope for eligible rows (exact production shape).
pub(crate) fn open_json_envelope(row: &RowInput) -> Option<Value> {
    if let Some(git_oid) = row
        .source
        .git_oid
        .as_deref()
        .filter(|value| !value.is_empty())
    {
        return Some(serde_json::json!({
            "type": "openJson", "git_oid": git_oid, "repository": row.source.repository,
        }));
    }
    row.source
        .op_id
        .as_deref()
        .filter(|value| !value.is_empty())
        .map(|op_id| serde_json::json!({ "type": "openJson", "op_id": op_id }))
}

/// Whether a row is eligible for the raw-JSON editor activation.
#[cfg(test)]
pub(crate) fn is_open_json_eligible(row: &RowInput) -> bool {
    open_json_envelope(row).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::legacy_content::display_summary_for_row;
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

    fn render_row_summary_html(row_summary: &RowSummary, subtitle: bool) -> String {
        let mut html = String::new();
        if let Some(content) = &row_summary.content {
            html.push_str("<span class=\"summary-text");
            if row_summary.git_prefix.is_some() {
                html.push_str(" git-summary-text");
            }
            if subtitle {
                html.push_str(" content-subtitle");
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

    fn render_content_icon_html(icon: ActivityIcon) -> String {
        let mut html = format!(
            "<span class=\"content-icon\" data-content-icon=\"{}\" aria-hidden=\"true\"><svg class=\"content-icon-svg\" viewBox=\"{}\" focusable=\"false\">",
            icon.name(),
            icon.view_box(),
        );
        for path in icon.paths() {
            if path.even_odd {
                write!(
                    html,
                    "<path d=\"{}\" fill-rule=\"evenodd\" clip-rule=\"evenodd\"></path>",
                    esc(path.d),
                )
                .expect("writing Content icon HTML to a String cannot fail");
            } else {
                write!(html, "<path d=\"{}\"></path>", esc(path.d))
                    .expect("writing Content icon HTML to a String cannot fail");
            }
        }
        html.push_str("</svg></span>");
        html
    }

    fn render_content_heading_html(heading: &ContentHeading) -> String {
        let icon = render_content_icon_html(heading.icon);
        if heading.title.is_empty() {
            icon
        } else {
            format!(
                "{icon}<span class=\"content-title\">{}</span>",
                esc(&heading.title),
            )
        }
    }

    fn render_content_html(spec: &RowSpec) -> String {
        if let Some(file) = &spec.content.file {
            let mut content = format!(
                "<span class=\"file-icon\" aria-hidden=\"true\"></span><span class=\"file-name\">{}</span>",
                esc(&file.name)
            );
            if !file.directory.is_empty() {
                write!(
                    content,
                    "<span class=\"file-directory\">{}</span>",
                    esc(&file.directory)
                )
                .expect("writing file-row fixture HTML to a String cannot fail");
            }
            return content;
        }
        if let Some(subop) = &spec.content.subop {
            let mut content = String::new();
            if let Some(heading) = &subop.heading {
                content.push_str(&render_content_heading_html(heading));
            }
            write!(
                content,
                "<span class=\"{}\">{}</span>",
                if subop.heading.is_some() {
                    "content-subtitle subop-summary"
                } else {
                    "subop-summary"
                },
                render_summary_html(&subop.summary)
            )
            .expect("writing sub-op fixture HTML to a String cannot fail");
            return content;
        }
        let top = spec.content.top.as_ref().expect("top-level row content");
        let wu_start = spec.content_flags.work_unit_block;
        let mut content = String::new();
        if let Some(heading) = &top.heading {
            content.push_str(&render_content_heading_html(heading));
        }
        if let Some(row_summary) = &top.summary {
            content.push_str(&render_row_summary_html(row_summary, top.heading.is_some()));
        }
        if wu_start {
            if spec.content_flags.work_unit_title_only {
                return content;
            }
            let header = spec.work_unit_header.as_ref().expect("work-unit header");
            let mut ribbon = format!(
                "<span class=\"work-unit-ribbon-line\"><span class=\"work-unit-ribbon\" title=\"{}\">{}</span>",
                esc(&header.title),
                esc(&header.title)
            );
            ribbon.push_str("</span>");
            return format!("{ribbon}<span class=\"work-unit-row-line\">{content}</span>");
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
        if let Some(file) = &spec.content.file {
            drop(attrs.insert("data-file-path".to_owned(), file.path.clone()));
            drop(attrs.insert(
                "data-file-status".to_owned(),
                file.status.class().to_owned(),
            ));
            drop(attrs.insert("data-file-source".to_owned(), file.source.clone()));
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

    fn file_row() -> Value {
        with(
            &subop_row(),
            &[
                ("node_key", json!("node:1::file:0")),
                ("op_id", json!("node:1::edit:0")),
                ("summary", json!("crates/service/src/lib.rs")),
                ("kind", json!("file")),
                ("record_role", json!("artifact")),
                ("activity_kind", json!("change")),
                ("subop_kind", json!("edit")),
                ("hierarchy_depth", json!(1)),
                (
                    "file_change",
                    json!({
                        "source": "agent",
                        "path": "crates/service/src/lib.rs",
                        "status": "modified",
                        "partial": true,
                        "binary": false,
                        "op_id": "node:1::edit:0"
                    }),
                ),
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
                ("summary", json!("1 planning step")),
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
            short_commit_id(&RowInput::from_legacy(&git_row())),
            "git:abc123d",
            "git rows keep the abbreviated commit id"
        );
        assert_eq!(
            short_commit_id(&RowInput::from_legacy(&subop_row())),
            "ode:1::sub:0"
        );
        assert_eq!(
            short_commit_id(&RowInput::from_legacy(&with(
                &base_row(),
                &[("turn_id", json!("turn:abcdefghijklmnop"))]
            ))),
            "efghijklmnop"
        );
        assert_eq!(
            short_commit_id(&RowInput::from_legacy(&with(
                &base_row(),
                &[
                    ("op_id", json!("node:1234567890abcdef")),
                    ("commit_id", json!("")),
                    ("turn_id", json!("")),
                ]
            ))),
            "567890abcdef"
        );
        assert_eq!(
            commit_cell_title(&RowInput::from_legacy(&git_row())),
            "git:abc123d"
        );
        assert_eq!(
            commit_cell_title(&RowInput::from_legacy(&with(
                &base_row(),
                &[("op_id", json!("node:1"))]
            ))),
            "node:1"
        );
        assert_eq!(
            commit_cell_title(&RowInput::from_legacy(&with(
                &base_row(),
                &[("op_id", Value::Null)]
            ))),
            ""
        );
    }

    #[test]
    fn content_heading_uses_tool_names_and_forward_compatible_titles() {
        let tool = with(
            &base_row(),
            &[
                ("kind", json!("tool")),
                ("record_role", json!("action")),
                ("activity_kind", json!("execute")),
                ("summary", json!("tool: exec inspect the workspace")),
            ],
        );
        let heading = content_heading(&RowInput::from_legacy(&tool));
        assert_eq!(heading.icon.name(), "tools");
        assert_eq!(heading.title, "exec");

        let future = with(
            &base_row(),
            &[
                ("kind", json!("future_kind")),
                ("activity_kind", json!("unknown")),
            ],
        );
        let heading = content_heading(&RowInput::from_legacy(&future));
        assert_eq!(heading.icon.name(), "settings");
        assert_eq!(heading.title, "Future kind");
    }

    #[test]
    fn content_titles_cover_the_supported_record_surface() {
        let cases = [
            ("message", "narrative", ""),
            ("command", "action", "Command"),
            ("command", "result", "Command output"),
            ("tool", "result", "Tool result"),
            ("tool_result", "result", "Tool result"),
            ("reflection", "narrative", "Reflection"),
            ("file", "artifact", "Change"),
            ("error", "result", "Error"),
            ("note", "lifecycle", "Note"),
            ("import", "artifact", "Import"),
            ("token_usage_record", "lifecycle", "Token usage"),
            ("token_count", "lifecycle", "Token count"),
            ("world_state", "lifecycle", "World state"),
            ("turn_context", "lifecycle", "Turn context"),
            ("task_complete", "lifecycle", "Task complete"),
            ("session_title", "lifecycle", "Session title"),
            ("session_meta", "lifecycle", "Session metadata"),
            ("turn_aborted", "lifecycle", "Turn aborted"),
            ("work-group", "action", ""),
            ("", "lifecycle", "Metadata"),
        ];
        for (kind, role, expected) in cases {
            let row = with(
                &base_row(),
                &[("kind", json!(kind)), ("record_role", json!(role))],
            );
            assert_eq!(
                content_title(&RowInput::from_legacy(&row)),
                expected,
                "{kind}"
            );
        }

        let git = git_row();
        assert_eq!(content_title(&RowInput::from_legacy(&git)), "");
    }

    #[test]
    fn token_count_rows_render_as_meta_with_a_numeric_subtitle() {
        let token = with(
            &subop_row(),
            &[
                ("summary", json!("17,502 / 258,400")),
                ("kind", json!("token_count")),
                ("record_role", json!("lifecycle")),
                ("activity_kind", json!("system")),
                ("is_system", json!(true)),
                ("hierarchy_depth", json!(2)),
            ],
        );
        let spec = RowSpec::from_value(&token, &RowContext::for_row(0, false));
        assert_eq!(
            render_activity_html(&spec),
            "<span class=\"activity-label\">meta</span>"
        );
        let content = render_content_html(&spec);
        assert!(content.contains("data-content-icon=\"settings\""));
        assert!(content.contains("class=\"content-title\">Token count</span>"));
        assert!(content.contains("17,502"));
        assert!(content.contains("258,400"));
    }

    #[test]
    fn token_bearing_exec_parent_renders_its_command_as_content() {
        let parent = with(
            &base_row(),
            &[
                ("summary", json!("tool: exec rsync -a source/ destination/")),
                ("kind", json!("tool")),
                ("record_role", json!("action")),
                ("activity_kind", json!("execute")),
                (
                    "sub_ops",
                    json!([{
                        "op_id": "node:2",
                        "kind": "token_usage_record",
                        "summary": "114,757",
                    }]),
                ),
            ],
        );
        let spec = RowSpec::from_value(&parent, &RowContext::for_row(0, false));
        let content = render_content_html(&spec);

        assert!(content.contains("data-content-icon=\"tools\""));
        assert!(content.contains("class=\"content-title\">exec</span>"));
        assert!(content.contains("rsync -a source/ destination/"));
        assert!(!content.contains("114,757"));
    }

    #[test]
    fn command_output_renders_stdout_as_its_subtitle() {
        let command = with(
            &base_row(),
            &[
                (
                    "summary",
                    json!("{\"stdout\":\"actual stdout\",\"formatted_output\":\"fallback\"}"),
                ),
                ("kind", json!("command")),
                ("record_role", json!("result")),
                ("activity_kind", json!("execute")),
            ],
        );
        let spec = RowSpec::from_value(&command, &RowContext::for_row(0, false));
        let content = render_content_html(&spec);

        assert!(content.contains("data-content-icon=\"tools\""));
        assert!(content.contains("class=\"content-title\">Command output</span>"));
        assert!(content.contains("class=\"summary-text content-subtitle\""));
        assert!(content.contains("actual stdout"));
        assert!(!content.contains("formatted_output"));
    }

    #[test]
    fn activity_stays_text_while_all_non_file_content_gets_a_heading() {
        let ordinary = RowSpec::from_value(&base_row(), &RowContext::for_row(0, false));
        assert_eq!(
            render_activity_html(&ordinary),
            "<span class=\"activity-label\">agent</span>"
        );
        let ordinary_content = render_content_html(&ordinary);
        assert!(ordinary_content
            .contains("class=\"content-icon\" data-content-icon=\"robot\" aria-hidden=\"true\""));
        assert!(!ordinary_content.contains("content-title"));
        assert!(ordinary_content.contains("class=\"summary-text content-subtitle\""));

        let work_group =
            RowSpec::from_value(&session_summary_row(), &RowContext::for_row(0, false));
        let work_group_content = render_content_html(&work_group);
        assert!(work_group_content
            .contains("class=\"content-icon\" data-content-icon=\"layers\" aria-hidden=\"true\""));
        assert!(!work_group_content.contains("content-title"));
        assert!(work_group_content.contains("class=\"summary-text content-subtitle\""));
        assert!(work_group_content.contains("1 planning step"));
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
            work_unit_title(&RowInput::from_legacy(&titled)),
            "Run the integration suite"
        );
        assert_eq!(
            work_unit_title(&RowInput::from_legacy(&with(
                &titled,
                &[(
                    "work_unit",
                    json!({
                        "id": "u", "title": "", "is_start": true, "count": 3
                    })
                )]
            ))),
            "Run"
        );
        assert_eq!(
            work_unit_title(&RowInput::from_legacy(&with(
                &titled,
                &[
                    ("activity_kind", json!("conversation")),
                    ("kind", json!("message")),
                    (
                        "work_unit",
                        json!({ "id": "u", "title": "", "is_start": true, "count": 3 })
                    ),
                ]
            ))),
            "Request"
        );
        assert_eq!(
            work_unit_title(&RowInput::from_legacy(&with(
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
            ))),
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
        assert!(show_work_unit_count(
            &RowInput::from_legacy(&base_row()),
            &wu
        ));
        assert!(!show_work_unit_count(
            &RowInput::from_legacy(&with(
                &base_row(),
                &[("activity_kind", json!("source_control"))]
            )),
            &wu
        ));
        assert!(!show_work_unit_count(
            &RowInput::from_legacy(&base_row()),
            &WorkUnitData {
                count: Some(1),
                ..wu.clone()
            }
        ));
    }

    #[test]
    fn bundle_count_text_matches_the_dto_count_contract() {
        assert_eq!(
            bundle_count_text(&RowInput::from_legacy(&bundle_row())),
            "2 commands"
        );
        assert_eq!(
            bundle_count_text(&RowInput::from_legacy(&with(
                &bundle_row(),
                &[(
                    "activity_bundle",
                    json!({ "kind": "execute-run", "member_count": 1 })
                )]
            ))),
            "1 command"
        );
        assert_eq!(
            bundle_count_text(&RowInput::from_legacy(&bundle_plan_row())),
            "3 updates"
        );
        assert_eq!(
            bundle_count_text(&RowInput::from_legacy(&with(
                &bundle_plan_row(),
                &[(
                    "activity_bundle",
                    json!({ "kind": "plan-repeat", "member_count": 1 })
                )]
            ))),
            "1 update"
        );
        assert_eq!(
            bundle_count_text(&RowInput::from_legacy(&with(
                &bundle_row(),
                &[("kind", json!("tool"))]
            ))),
            "2 tool steps"
        );
        assert_eq!(bundle_count_text(&RowInput::from_legacy(&base_row())), "");
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
        let work_spec = RowSpec::from_value(&work, &RowContext::for_row(0, false));
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
        let nested_spec = RowSpec::from_value(&nested, &RowContext::for_row(1, false));
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
        assert!(render_content_html(&nested_spec)
            .contains("class=\"content-icon\" data-content-icon=\"tools\""));
        let nested_html = render_tags_html(&nested_spec);
        assert!(!nested_html.contains("class=\"subop-chevron\""));
        assert!(nested_html.contains("class=\"bundle-count\""));
    }

    #[test]
    fn chrome_items_match_exact_span_contracts() {
        let rel = relation_badges(&RowInput::from_legacy(&with(
            &base_row(),
            &[("parent_relations", json!([{ "kind": "subagent" }]))],
        )));
        assert_eq!(
            render_chrome_html(&rel),
            "<span class=\"rel-badge rel-subagent\" title=\"Starts a subagent branch\" aria-label=\"Starts a subagent branch\">↳ subagent</span>"
        );
        let all = relation_badges(&RowInput::from_legacy(&with(
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
        )));
        assert_eq!(
            render_chrome_html(&all),
            "<span class=\"rel-badge rel-subagent\" title=\"Starts a subagent branch\" aria-label=\"Starts a subagent branch\">↳ subagent</span>\
             <span class=\"rel-badge rel-reconnect\" title=\"Completion returns into the subagent branch\" aria-label=\"Completion returns into the subagent branch\">↩ return</span>\
             <span class=\"rel-badge rel-fork\" title=\"Branches off the target row at a fork boundary\" aria-label=\"Branches off the target row at a fork boundary\">⇉ fork</span>"
        );
        let dup = relation_badges(&RowInput::from_legacy(&with(
            &base_row(),
            &[(
                "parent_relations",
                json!([{ "kind": "fork" }, { "kind": "fork" }]),
            )],
        )));
        assert_eq!(dup.len(), 1);
    }

    #[test]
    fn activity_column_classifies_every_real_row_with_stable_fallbacks() {
        let cases = [
            ("work", "work", "layers"),
            ("conversation", "agent", "robot"),
            ("plan", "plan", "checklist"),
            ("explore", "explore", "search"),
            ("execute", "tooluse", "tools"),
            ("change", "change", "edit"),
            ("verify", "verify", "pass"),
            ("diagnose", "diagnose", "bug"),
            ("coordinate", "coordinate", "type-hierarchy"),
            ("source_control", "git", "source-control"),
            ("external", "external", "link-external"),
            ("system", "meta", "settings"),
        ];
        for (wire, label, icon) in cases {
            let row = with(&base_row(), &[("activity_kind", json!(wire))]);
            let classification = row_classification(&RowInput::from_legacy(&row));
            assert_eq!(classification.label, label, "{wire} label");
            assert_eq!(
                content_icon(&RowInput::from_legacy(&row)).name(),
                icon,
                "{wire} Content icon"
            );
            assert_eq!(classification.source, "activity_kind", "{wire} source");
            let title_value = if wire == "system" { "meta" } else { wire };
            assert_eq!(classification.title, format!("Activity: {title_value}"));
        }

        let user_row = with(&base_row(), &[("author", json!("human"))]);
        let user = row_classification(&RowInput::from_legacy(&user_row));
        assert_eq!(user.label, "user");
        assert_eq!(
            content_icon(&RowInput::from_legacy(&user_row)).name(),
            "account"
        );
        assert_eq!(user.source, "activity_kind");
        assert_eq!(user.title, "Activity: conversation");

        let git = row_classification(&RowInput::from_legacy(&with(
            &base_row(),
            &[
                ("kind", json!("message")),
                ("activity_kind", json!("unknown")),
                ("git_oid", json!("abc123")),
            ],
        )));
        assert_eq!(git.label, "git", "Git identity is authoritative");
        assert_eq!(git.source, "git_oid");

        let system = row_classification(&RowInput::from_legacy(&with(
            &base_row(),
            &[
                ("activity_kind", json!("unknown")),
                ("kind", json!("unknown")),
                ("is_system", json!(true)),
            ],
        )));
        assert_eq!(system.label, "meta");
        assert_eq!(system.source, "is_system");
        assert_eq!(system.title, "Activity: meta");

        let kind = row_classification(&RowInput::from_legacy(&with(
            &base_row(),
            &[
                ("activity_kind", json!("unknown")),
                ("kind", json!("tool_result")),
            ],
        )));
        assert_eq!(kind.label, "tool-result");
        assert_eq!(kind.source, "kind");

        let role = row_classification(&RowInput::from_legacy(&with(
            &base_row(),
            &[
                ("activity_kind", json!("unknown")),
                ("kind", json!("unknown")),
                ("record_role", json!("artifact")),
            ],
        )));
        assert_eq!(role.label, "artifact");
        assert_eq!(role.source, "record_role");

        let other = row_classification(&RowInput::from_legacy(&with(
            &base_row(),
            &[
                ("activity_kind", json!("unknown")),
                ("kind", json!("unknown")),
                ("record_role", json!("unknown")),
            ],
        )));
        assert_eq!(other.label, "other");
        assert!(!other.label.is_empty());
    }

    #[test]
    fn session_scoped_boundary_uses_the_session_activity_name() {
        let row = session_summary_row();
        let spec = RowSpec::from_value(&row, &RowContext::for_row(0, false));
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

        let turn_start = RowSpec::from_value(&wu_start_row(), &RowContext::for_row(0, false));
        assert_eq!(turn_start.classification.label, "user");
    }

    #[test]
    fn chrome_suppressions_match_the_frozen_badge_options() {
        assert!(outcome_badge(
            &RowInput::from_legacy(&with(&base_row(), &[("outcome", json!("success"))])),
            BadgeOptions::default()
        )
        .is_none());
        let warning = outcome_badge(
            &RowInput::from_legacy(&with(&base_row(), &[("outcome", json!("warning"))])),
            BadgeOptions::default(),
        )
        .expect("warning badge");
        assert_eq!(
            render_chrome_html(&[warning]),
            "<span class=\"out-badge outcome-warning\" title=\"outcome: warning\" aria-label=\"outcome: warn\">warn</span>"
        );
        let failure = outcome_badge(
            &RowInput::from_legacy(&with(&base_row(), &[("outcome", json!("failure"))])),
            BadgeOptions::default(),
        )
        .expect("failure badge");
        assert_eq!(
            render_chrome_html(&[failure]),
            "<span class=\"out-badge outcome-failure\" title=\"outcome: failure\" aria-label=\"outcome: failed\">✕</span>"
        );
        let cancelled = outcome_badge(
            &RowInput::from_legacy(&with(&base_row(), &[("outcome", json!("cancelled"))])),
            BadgeOptions::default(),
        )
        .expect("cancelled badge");
        assert_eq!(
            render_chrome_html(&[cancelled]),
            "<span class=\"out-badge outcome-neutral\" title=\"outcome: cancelled\" aria-label=\"outcome: cancelled\">cancelled</span>"
        );
        assert!(outcome_badge(
            &RowInput::from_legacy(&with(&base_row(), &[("outcome", json!("unknown"))])),
            BadgeOptions::default()
        )
        .is_none());
    }

    #[test]
    fn semantic_chrome_order_and_bundle_priority_match_production() {
        let relations = row_semantic_chrome(
            &RowInput::from_legacy(&with(
                &base_row(),
                &[
                    ("parent_relations", json!([{ "kind": "fork" }])),
                    ("outcome", json!("unknown")),
                ],
            )),
            false,
            BadgeOptions::default(),
        );
        assert_eq!(relations.len(), 1);
        let relations_warning = row_semantic_chrome(
            &RowInput::from_legacy(&with(
                &base_row(),
                &[
                    ("parent_relations", json!([{ "kind": "fork" }])),
                    ("outcome", json!("warning")),
                ],
            )),
            false,
            BadgeOptions::default(),
        );
        assert_eq!(
            render_chrome_html(&relations_warning),
            "<span class=\"rel-badge rel-fork\" title=\"Branches off the target row at a fork boundary\" aria-label=\"Branches off the target row at a fork boundary\">⇉ fork</span>\
             <span class=\"out-badge outcome-warning\" title=\"outcome: warning\" aria-label=\"outcome: warn\">warn</span>"
        );
        let activity = row_semantic_chrome(
            &RowInput::from_legacy(&with(
                &base_row(),
                &[
                    ("activity_kind", json!("plan")),
                    ("outcome", json!("success")),
                ],
            )),
            false,
            BadgeOptions::default(),
        );
        assert!(
            activity.is_empty(),
            "activity lives in its own column and common success stays suppressed"
        );
        let bundle = row_semantic_chrome(
            &RowInput::from_legacy(&bundle_row()),
            true,
            BadgeOptions::default(),
        );
        assert_eq!(
            render_chrome_html(&bundle),
            "<span class=\"bundle-count\" title=\"2 commands, completed\">2 commands</span>\
             <span class=\"bundle-status bundle-status-success\" title=\"completed\" aria-label=\"completed\">✓</span>"
        );
        let bundle_unknown_outcome = row_semantic_chrome(
            &RowInput::from_legacy(&with(&bundle_row(), &[("outcome", json!("unknown"))])),
            true,
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
        let chips = session_meta_values(&RowInput::from_legacy(&row));
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
            session_meta_description(&RowInput::from_legacy(&row)),
            "Model provider sglang_dsv4, Agent Harvey"
        );
        let partial = session_meta_values(&RowInput::from_legacy(&with(
            &base_row(),
            &[(
                "session_meta",
                json!({ "model_provider": "   ", "agent_nickname": "Harvey" }),
            )],
        )));
        assert_eq!(partial.len(), 1);
        assert_eq!(partial.first().expect("agent chip").label, "Harvey");
        assert!(session_meta_values(&RowInput::from_legacy(&base_row())).is_empty());
    }

    #[test]
    fn promoted_classes_match_the_rail_contract() {
        assert_eq!(
            promoted_kind(&RowInput::from_legacy(&promoted_failure_row())).expect("failure"),
            PromotedKind::Failure
        );
        assert_eq!(
            promoted_kind(&RowInput::from_legacy(&with(
                &promoted_failure_row(),
                &[
                    ("promoted", json!(true)),
                    ("outcome", json!("unknown")),
                    ("activity_kind", json!("change")),
                ]
            ))),
            Some(PromotedKind::Change)
        );
        assert_eq!(
            promoted_kind(&RowInput::from_legacy(&with(
                &promoted_failure_row(),
                &[
                    ("promoted", json!(true)),
                    ("outcome", json!("unknown")),
                    ("activity_kind", json!("verify")),
                ]
            ))),
            Some(PromotedKind::Verify)
        );
        assert_eq!(
            promoted_kind(&RowInput::from_legacy(&with(
                &promoted_failure_row(),
                &[("promoted", json!(true)), ("outcome", json!("unknown")),]
            ))),
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
        let wu = work_unit_of(&RowInput::from_legacy(row))?;
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
            git_summary_parts(&RowInput::from_legacy(&git_row()), "feat: add thing"),
            Some(("feat".to_owned(), "add thing".to_owned()))
        );
        assert_eq!(
            git_summary_parts(&RowInput::from_legacy(&git_row()), "no prefix here"),
            None
        );
        assert_eq!(
            git_summary_parts(&RowInput::from_legacy(&git_row()), ": leading colon"),
            None
        );
        assert_eq!(
            git_summary_parts(&RowInput::from_legacy(&base_row()), "feat: add thing"),
            None
        );
        assert_eq!(
            git_summary_parts(
                &RowInput::from_legacy(&git_row()),
                "  fix  :  spaced  prefix  "
            ),
            Some(("fix".to_owned(), "spaced  prefix  ".to_owned()))
        );
    }

    #[test]
    fn plain_summaries_drop_the_colon_and_markdown_decoration() {
        assert_eq!(
            plain_row_summary(&RowInput::from_legacy(&git_row()), "feat: add thing"),
            "feat add thing"
        );
        assert_eq!(
            plain_row_summary(&RowInput::from_legacy(&git_row()), "feat:"),
            "feat"
        );
        assert_eq!(
            plain_row_summary(
                &RowInput::from_legacy(&base_row()),
                "# Hello **world**\n- item"
            ),
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
            display_summary_for_row(
                &RowInput::from_legacy(&tool(&[])),
                "tool result: {\"text\": \"did thing\"}"
            ),
            "tool result: {\"text\": \"did thing\"}"
        );
        assert_eq!(
            display_summary_for_row(
                &RowInput::from_legacy(&tool(&[])),
                "{\"text\": \"did thing\"}"
            ),
            "did thing"
        );
        assert_eq!(
            display_summary_for_row(
                &RowInput::from_legacy(&tool(&[("outcome", json!("success"))])),
                "{\"opaque\": true}"
            ),
            "Completed"
        );
        assert_eq!(
            display_summary_for_row(
                &RowInput::from_legacy(&tool(&[("outcome", json!("failure"))])),
                "{\"opaque\": true}"
            ),
            "Failed"
        );
        assert_eq!(
            display_summary_for_row(
                &RowInput::from_legacy(&with(
                    &base_row(),
                    &[
                        ("kind", json!("command")),
                        ("record_role", json!("action")),
                        ("activity_kind", json!("execute")),
                    ]
                )),
                "{\"opaque\": true}"
            ),
            "Tool request"
        );
        let command_output = with(
            &base_row(),
            &[
                ("kind", json!("command")),
                ("record_role", json!("result")),
                ("activity_kind", json!("execute")),
            ],
        );
        assert_eq!(
            display_summary_for_row(
                &RowInput::from_legacy(&command_output),
                "{\"output\":\"transport fallback\",\"stdout\":\"actual stdout\"}"
            ),
            "actual stdout"
        );
        assert_eq!(
            display_summary_for_row(
                &RowInput::from_legacy(&command_output),
                "{\"formatted_output\":\"formatted fallback\"}"
            ),
            "formatted fallback"
        );
        assert_eq!(
            display_summary_for_row(
                &RowInput::from_legacy(&with(
                    &base_row(),
                    &[
                        ("record_role", json!("result")),
                        ("activity_kind", json!("execute")),
                    ],
                )),
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
            display_summary_for_row(
                &RowInput::from_legacy(&narrative),
                "{\"narrative\": \"authored\"}"
            ),
            "{\"narrative\": \"authored\"}"
        );
        assert_eq!(
            display_summary_for_row(&RowInput::from_legacy(&narrative), "just prose **bold**"),
            "just prose **bold**"
        );
        assert_eq!(
            display_summary_for_row(
                &RowInput::from_legacy(&tool(&[])),
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

    fn context(abs: i64, group_start: bool) -> RowContext {
        RowContext::for_row(abs, group_start)
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
        // Existing goldens focus on each row's authored payload. The shared
        // Content icon/title lead has a dedicated cross-taxonomy contract
        // test below, so strip that common prefix here rather than copying
        // large inline-SVG paths into every unrelated row golden.
        let mut rendered_content = render_content_html(&spec);
        let heading = spec
            .content
            .subop
            .as_ref()
            .and_then(|content| content.heading.as_ref())
            .or_else(|| {
                spec.content
                    .top
                    .as_ref()
                    .and_then(|content| content.heading.as_ref())
            });
        if let Some(heading) = heading {
            rendered_content =
                rendered_content.replacen(&render_content_heading_html(heading), "", 1);
            rendered_content = rendered_content.replacen(" content-subtitle", "", 1);
            rendered_content =
                rendered_content.replacen("content-subtitle subop-summary", "subop-summary", 1);
        }
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
&context(3, true),
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
&context(3, false),
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
        let spec = RowSpec::from_value(&row, &context(3, false));
        assert!(spec.classes().contains("row-chain-muted"));
        assert_eq!(spec.graph.chain_state, ChainState::Muted);
        assert_eq!(spec.graph.muted_above, vec![1]);
        assert_eq!(spec.graph.muted_below, vec![1, 2]);
        assert_eq!(spec.graph.muted_transitions, vec![(1, 0)]);

        let active = RowSpec::from_value(&base_row(), &context(3, false));
        assert!(!active.classes().contains("row-chain-muted"));
        assert_eq!(active.graph.chain_state, ChainState::Active);
    }

    #[test]
    fn selection_find_and_roving_tabindex_are_context_driven() {
        let mut ctx = context(3, false);
        ctx.selected_key = Some("op:1".to_owned());
        ctx.roving_abs = Some(3);
        let spec = RowSpec::from_value(&base_row(), &ctx);
        assert!(spec.classes().contains("row-selected"));
        assert!(spec.aria.aria_selected);
        assert_eq!(spec.aria.tabindex, 0);
        assert!(spec.flags.selected);

        let mut ctx = context(7, false);
        ctx.find_current = true;
        let spec = RowSpec::from_value(&base_row(), &ctx);
        assert!(spec.classes().contains("row-find-current"));
        assert!(!spec.classes().contains("row-selected"));
        assert!(!spec.aria.aria_selected);
        assert_eq!(spec.aria.tabindex, -1);
    }

    #[test]
    fn git_rows_carry_the_prefix_chip_and_deterministic_aria() {
        let ctx = context(1, true);
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
    fn graph_data_feeds_the_svg_frame_contract() {
        let spec = RowSpec::from_value(&git_row(), &context(1, false));
        assert_eq!(spec.graph.lane, 1);
        assert_eq!(spec.graph.above, vec![0, 1]);
        assert_eq!(spec.graph.below, vec![1]);
        assert_eq!(spec.graph.transitions, vec![(0, 1)]);
        assert!(!spec.graph.is_subop);
        assert!(!spec.graph.is_bundle);
        assert!(!spec.identity.is_subop);
        assert_eq!(spec.identity.node_key, "git:abc123def456");
        let bundle_spec = RowSpec::from_value(&bundle_row(), &context(0, true));
        assert!(bundle_spec.graph.is_bundle);
        assert!(!bundle_spec.graph.expanded);
        assert_eq!(
            bundle_spec.identity.node_key, "bundle:exec1",
            "bundle rows keep their own data-key identity"
        );
        let mut expanded_context = context(0, true);
        expanded_context.expanded = true;
        let expanded_bundle = RowSpec::from_value(&bundle_row(), &expanded_context);
        assert!(
            expanded_bundle.graph.expanded,
            "the live disclosure state reaches graph marker selection"
        );
    }

    #[test]
    fn subop_rows_keep_identity_for_opened_group_markers() {
        let ctx = context(4, false);
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
"<span class=\"subop-summary\"><span class=\"md-line\"><span class=\"md-text\">custom-title metadata</span></span></span>",
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
            &context(5, false),
        );
        assert_eq!(
            msg_spec.identity.node_key.as_str(),
            "node:1::sub:1",
            "sub-op data-key is its own wire identity, never synthesized from the parent"
        );
        assert_eq!(
            msg_spec
                .content
                .subop
                .as_ref()
                .and_then(|content| content.heading.as_ref())
                .map(|heading| heading.icon.name()),
            Some("robot")
        );
    }

    #[test]
    fn disclosure_rows_expose_expandable_and_chevron_labels() {
        let collapsed = RowSpec::from_value(&expandable_row(), &context(0, false));
        assert!(collapsed.state.expandable);
        assert_eq!(collapsed.expanded(), Some(false));
        assert_eq!(collapsed.aria.aria_expanded, Some(false));
        assert!(collapsed.classes().contains("row-expandable"));
        let disclosure = collapsed.disclosure.as_ref().expect("chevron");
        assert_eq!(disclosure.label, "Expand 2 details");
        let activity = render_activity_html(&collapsed);
        assert!(activity.starts_with(
            "<span class=\"activity-label\">agent</span><button type=\"button\" class=\"subop-chevron\""
        ));
        assert!(activity.ends_with("aria-expanded=\"false\">\u{25b8}</button>"));
        assert!(!render_content_html(&collapsed).contains("subop-chevron"));

        let mut ctx = context(0, false);
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
        let ctx = context(0, true);
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
                "<span class=\"summary-text\"><span class=\"md-line\"><span class=\"md-text\">tool result: execute run (2 steps)</span></span></span>",
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
        assert!(is_activity_bundle(&RowInput::from_legacy(&bundle_row())));
        assert!(is_execute_run_bundle(&RowInput::from_legacy(&bundle_row())));
        assert!(!is_plan_repeat_bundle(
            &RowInput::from_legacy(&bundle_row())
        ));
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
        assert!(is_plan_repeat_bundle(&RowInput::from_legacy(
            &bundle_plan_row()
        )));
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
    fn work_unit_start_rows_avoid_duplicate_ribbons_and_keep_distinct_headers() {
        let ctx = context(0, true);
        let spec = RowSpec::from_value(&wu_start_row(), &ctx);
        assert_row("wu_start",
&wu_start_row(),
&ctx.clone(),
(
&["row", "row-human", "row-role-narrative", "row-has-badges", "row-work-unit-start"],
"summary work-unit-block work-unit-title-only",
"<span class=\"summary-text\"><span class=\"md-line\"><span class=\"md-text\">User request: make the search faster</span></span></span>",
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

        let distinct = with(
            &wu_start_row(),
            &[(
                "work_unit",
                json!({
                    "id": "session:s1/turn:t1",
                    "title": "Turn header",
                    "is_start": true,
                    "is_end": false,
                    "count": 5,
                }),
            )],
        );
        let distinct = RowSpec::from_value(&distinct, &context(0, false));
        let distinct_html = render_content_html(&distinct);
        assert!(distinct_html.starts_with(
            "<span class=\"work-unit-ribbon-line\"><span class=\"work-unit-ribbon\" title=\"Turn header\">Turn header</span></span><span class=\"work-unit-row-line\">"
        ));
        assert!(!distinct_html.contains("content-title"));
        assert!(distinct_html.contains("class=\"summary-text content-subtitle\""));

        // Not at a group start: no session tags, aria suffix, or group class;
        // the row's own work-unit count tag remains.
        let not_start = RowSpec::from_value(&wu_start_row(), &context(0, false));
        assert!(!not_start.classes().contains("row-group-start"));
        assert_eq!(
            not_start.aria.aria_label,
            "User request: make the search faster"
        );

        // Title-only start: one structured Content line, no count for a single entry.
        let title_only = RowSpec::from_value(&wu_start_title_only_row(), &context(1, false));
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
        let spec = RowSpec::from_value(&promoted_failure_row(), &context(3, false));
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
        let tip = RowSpec::from_value(&tip, &context(1, false));
        assert_eq!(tip.group_label.as_deref(), Some("Git · repo 199254740993"));

        let root = with(
            &git_row(),
            &[
                ("below", json!([0])),
                ("transitions", json!([])),
                ("group_end", json!(false)),
            ],
        );
        let root = RowSpec::from_value(&root, &context(2, false));
        assert_eq!(root.group_label.as_deref(), Some("Git · repo 199254740993"));

        let parent_anchored_middle = with(
            &git_row(),
            &[
                ("above", json!([0])),
                ("below", json!([1])),
                ("transitions", json!([[0, 1]])),
            ],
        );
        let parent_anchored_middle =
            RowSpec::from_value(&parent_anchored_middle, &context(3, true));
        assert_eq!(parent_anchored_middle.group_label, None);

        let child_anchored_middle = with(
            &git_row(),
            &[
                ("above", json!([1])),
                ("below", json!([0])),
                ("transitions", json!([[1, 0]])),
            ],
        );
        let child_anchored_middle = RowSpec::from_value(&child_anchored_middle, &context(4, true));
        assert_eq!(child_anchored_middle.group_label, None);
    }

    #[test]
    fn session_chips_stay_on_group_boundaries_while_labels_follow_graph_endpoints() {
        let ctx = context(9, true);
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

        let not_group_start = RowSpec::from_value(&session_meta_row(), &context(10, false));
        assert!(!not_group_start.classes().contains("row-has-badges"));
        assert!(!not_group_start.classes().contains("row-group-start"));
        assert!(not_group_start.tags.is_empty());
        assert_eq!(not_group_start.aria.aria_label, "session boundary row");

        let terminal = with(&session_meta_row(), &[("group_end", json!(true))]);
        let terminal = RowSpec::from_value(&terminal, &context(12, false));
        assert_eq!(terminal.group_label, None);
        assert!(!terminal.classes().contains("row-group-start"));

        let tip = with(&session_meta_row(), &[("above", json!([]))]);
        let tip = RowSpec::from_value(&tip, &context(13, true));
        assert_eq!(tip.group_label.as_deref(), Some("q0 · Harvey"));

        let root = with(&session_meta_row(), &[("below", json!([]))]);
        let root = RowSpec::from_value(&root, &context(14, false));
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
    fn file_rows_match_native_scm_content_and_open_diff_contract() {
        let row = file_row();
        let spec = RowSpec::from_value(&row, &context(4, false));
        assert_eq!(
            spec.classes().split_whitespace().collect::<Vec<_>>(),
            vec![
                "row",
                "row-dim",
                "row-subop",
                "row-file",
                "row-file-modified",
                "row-file-partial",
                "row-role-artifact",
                "row-has-badges",
            ]
        );
        assert_eq!(spec.classification.label, "change");
        assert_eq!(
            spec.tags
                .iter()
                .map(|tag| (tag.classes.as_str(), tag.text.as_str()))
                .collect::<Vec<_>>(),
            vec![
                ("file-status file-status-modified", "M"),
                ("file-fidelity", "recorded"),
            ]
        );
        assert_eq!(
            render_tags_html(&spec),
            "<span class=\"file-status file-status-modified\" title=\"Modified\" aria-label=\"Modified\">M</span><span class=\"file-fidelity\" title=\"Recorded agent edit\" aria-label=\"Recorded agent edit\">recorded</span>"
        );
        assert_eq!(
            render_content_html(&spec),
            "<span class=\"file-icon\" aria-hidden=\"true\"></span><span class=\"file-name\">lib.rs</span><span class=\"file-directory\">crates/service/src</span>"
        );
        assert_eq!(
            spec.aria.aria_label,
            "Modified crates/service/src/lib.rs, recorded agent edit, partial file evidence; open diff"
        );
        assert!(spec.aria.title.contains("recorded edit"));
        assert!(spec.open_json.is_none());
        assert_eq!(
            spec.open_diff
                .as_ref()
                .and_then(|envelope| envelope.get("type"))
                .and_then(Value::as_str),
            Some("openDiff")
        );
        assert_eq!(
            spec.open_diff
                .as_ref()
                .and_then(|envelope| envelope.get("change"))
                .and_then(|change| change.get("path"))
                .and_then(Value::as_str),
            Some("crates/service/src/lib.rs")
        );
        let attrs = render_attrs(&spec);
        assert_eq!(
            attrs.get("data-file-status").map(String::as_str),
            Some("modified")
        );
        assert_eq!(
            attrs.get("data-file-source").map(String::as_str),
            Some("agent")
        );
    }

    #[test]
    fn open_json_envelopes_are_exact_and_eligibility_is_strict() {
        let git = git_row();
        let spec = RowSpec::from_value(&git, &context(1, false));
        assert!(spec.open_json.is_some());
        let envelope = open_json_envelope(&RowInput::from_legacy(&git)).expect("git envelope");
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
        let envelope = open_json_envelope(&RowInput::from_legacy(&op_row)).expect("op envelope");
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
            open_json_envelope(&RowInput::from_legacy(&sub))
                .expect("subop")
                .get("op_id")
                .and_then(Value::as_str),
            Some("node:1::sub:0")
        );

        // A row with neither identifier is ineligible.
        let empty = with(&base_row(), &[("op_id", Value::Null)]);
        assert_eq!(open_json_envelope(&RowInput::from_legacy(&empty)), None);
        assert!(!is_open_json_eligible(&RowInput::from_legacy(&empty)));
        // Falsy identifiers are ineligible exactly like JS truthiness.
        assert_eq!(
            open_json_envelope(&RowInput::from_legacy(&with(
                &base_row(),
                &[("op_id", json!(""))]
            ))),
            None
        );
    }

    #[test]
    fn missing_additive_fields_default_like_the_production_row() {
        let sparse = json!({
            "node_key": "k",
            "summary": "sparse row",
        });
        let spec = RowSpec::from_value(&sparse, &context(0, false));
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
            &context(14, false),
        );
        assert_eq!(no_summary.summary_source, "(no summary)");
        assert_eq!(no_summary.plain_summary, "(no summary)");
        assert_eq!(no_summary.detail_summary, "(no summary)");
        assert_eq!(no_summary.aria.aria_label, "(no summary)");
        assert_eq!(no_summary.aria.title, "(no summary)");
    }

    #[test]
    fn row_aria_attrs_match_build_row_html_exactly() {
        let mut ctx = context(3, true);
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
        let spec = RowSpec::from_value(&tool, &context(11, false));
        assert_eq!(
            spec.display_summary,
            "tool result: {\"text\": \"done the thing\", \"type\": \"output\"}"
        );
        assert_row("tool_payload",
&tool,
&context(11, false),
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
            &context(6, false),
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
&context(2, false),
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
&context(5, false),
(
&["row", "row-tool", "row-role-lifecycle"],
"summary",
"<span class=\"summary-text\"><span class=\"md-line\"><span class=\"md-text\">system record</span></span></span>",
"Jan 15, 2026 04:00 PM",
"",
""
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
&context(0, false),
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
&context(2, false),
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
&context(13, false),
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
&context(7, false),
(
&["row", "row-role-narrative", "row-has-badges"],
"summary",
"<span class=\"summary-text\"><span class=\"md-line\"><span class=\"md-text\">Agent turn with metadata</span></span></span>",
"Jan 15, 2026 03:59 PM",
"agent",
"t1"
));
        let relation_spec = RowSpec::from_value(&rel_warning, &context(7, false));
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
&context(10, false),
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
    fn subop_content_uses_the_shared_icon_title_subtitle_grammar() {
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
&context(5, false),
(
&["row", "row-dim", "row-subop", "row-role-narrative"],
"summary",
"<span class=\"subop-summary\"><span class=\"md-line\"><span class=\"md-text\">mode</span></span></span>",
"Jan 15, 2026 04:00 PM",
"",
"ode:1::sub:0"
));
        let msg_spec = RowSpec::from_value(&msg, &context(5, false));
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
        let spec = RowSpec::from_value(&meta, &context(6, false));
        assert_eq!(
            spec.content
                .subop
                .as_ref()
                .and_then(|content| content.heading.as_ref())
                .map(|heading| heading.icon.name()),
            Some("robot"),
            "the semantic activity classification wins over the detail grouping kind"
        );
    }

    #[test]
    fn find_match_and_expanded_states_apply_classes_and_aria() {
        let expanded = RowSpec::from_value(
            &expandable_row(),
            &RowContext {
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
        let collapsed = RowSpec::from_value(&expandable_row(), &context(0, false));
        assert_eq!(
            render_content_html(&expanded),
            render_content_html(&collapsed),
            "moving disclosure leaves structured Content unchanged"
        );
        assert!(render_activity_html(&expanded).contains("aria-expanded=\"true\">\u{25be}"));
    }
}
