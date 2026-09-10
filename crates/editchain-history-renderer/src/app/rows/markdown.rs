//! Deterministic plain-text normalization and HTML-free markdown summary tokens.

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
