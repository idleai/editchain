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
//! ([`markdown::MdInline`]/[`markdown::MdLine`]/[`markdown::Summary`]) instead of markup: no HTML string is
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

mod chrome;
mod files;
mod markdown;

pub(crate) use chrome::{
    ActivityIcon, BundleKind, ContentHeading, SessionSummaryData, WorkUnitData,
};

use crate::app::row_input::RowInput;
use crate::app::ChainState;
use chrome::{
    activity_bundle_kind, bundle_count_text, content_heading, promoted_kind, row_classification,
    row_semantic_chrome, session_meta_description, session_meta_values,
    session_summary_count_title, session_summary_of, show_work_unit_count, work_unit_count_text,
    work_unit_count_title, work_unit_of, work_unit_title, BadgeOptions, BundleInfo, ChromeItem,
    PromotedKind, RowClassification, WorkUnitHeader,
};
use files::{file_content, FileContent};
use markdown::{markdown_detail_summary, markdown_plain_summary, Summary};
use serde_json::Value;

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
    pub(crate) continuity_key: String,
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

/// A task header carries passing edges but does not invent a graph node.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) enum GraphRole {
    #[default]
    Node,
    PassThrough,
}

/// Graph fields the row-local SVG renderer and retained frame contract consume,
/// verbatim from the wire row.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct GraphData {
    pub(crate) role: GraphRole,
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

/// `hasSubOps` — whether a top-level row carries bundled metadata sub-ops.
pub(crate) fn has_sub_ops(row: &RowInput) -> bool {
    !row.source.sub_ops.is_empty()
        || row
            .source
            .task_group
            .as_ref()
            .is_some_and(|task| task.member_count > 0)
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

/// Sub-op row content: semantic icon, compact type/tool title, and indented
/// authored summary. Semantic tags live in the dedicated Tags column.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SubopContent {
    pub(crate) heading: Option<ContentHeading>,
    pub(crate) summary: Summary,
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
        let classification = if row.source.task_group.is_some() {
            RowClassification::new("task", "activity_kind", "task")
        } else if is_file {
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
        let group_label = if !is_subop && row.source.task_group.is_none() && is_graph_endpoint(row)
        {
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
                continuity_key: row.continuity_key().to_owned(),
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
                role: if row.source.task_group.is_some() {
                    GraphRole::PassThrough
                } else {
                    GraphRole::Node
                },
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
        if self.kind == "task" {
            classes.push_str(" row-task-group");
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
mod tests;
