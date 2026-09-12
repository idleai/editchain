use super::chrome::{
    content_icon, content_title, is_activity_bundle, is_execute_run_bundle, is_plan_repeat_bundle,
    outcome_badge, relation_badges,
};
use super::markdown::{
    markdown_plain_inline, markdown_plain_line, render_markdown_inline, MdInline, MdLine,
    MdLineKind,
};
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

    let work_group = RowSpec::from_value(&session_summary_row(), &RowContext::for_row(0, false));
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
        rendered_content = rendered_content.replacen(&render_content_heading_html(heading), "", 1);
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
    let parent_anchored_middle = RowSpec::from_value(&parent_anchored_middle, &context(3, true));
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
            task: TaskView::default(),
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
