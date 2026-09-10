//! Bounded row display text. Full source content is addressed by row identity.

use serde::{Deserialize, Serialize};

/// Maximum UTF-8 bytes in one authored summary or output preview.
pub const MAX_ROW_TEXT_BYTES: usize = 4096;
/// Maximum UTF-8 bytes in a displayed tool label.
pub const MAX_TOOL_LABEL_BYTES: usize = 256;

/// Text selected from source evidence, with explicit completeness.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContentTextDto {
    /// Plain or authored Markdown text, without a provider JSON envelope.
    pub text: String,
    /// True only when the complete source text survived preparation and this
    /// field's byte limit. Derived summaries and unknown provenance use false.
    #[serde(default)]
    pub complete: bool,
}

impl ContentTextDto {
    /// Bound an authored summary or output preview without splitting UTF-8.
    #[must_use]
    pub fn new(text: String, complete: bool) -> Self {
        Self::bounded(text, complete, MAX_ROW_TEXT_BYTES)
    }

    /// Bound a tool's display name. The source tool identity is unchanged.
    #[must_use]
    pub fn tool_label(text: String, complete: bool) -> Self {
        Self::bounded(text, complete, MAX_TOOL_LABEL_BYTES)
    }

    fn bounded(mut text: String, complete: bool, limit: usize) -> Self {
        if text.len() <= limit {
            return Self { text, complete };
        }
        let mut end = limit.saturating_sub('…'.len_utf8());
        while !text.is_char_boundary(end) {
            end = end.saturating_sub(1);
        }
        text.truncate(end);
        text.push('…');
        text.shrink_to_fit();
        Self {
            text,
            complete: false,
        }
    }
}

/// Source-selected content roles. Absence of this additive DTO identifies a
/// legacy row whose summary needs compatibility recovery at ingestion.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RowContentDto {
    /// Concrete tool name from normalized source evidence.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_label: Option<ContentTextDto>,
    /// Authored prose, invocation, or aggregate activity summary.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub authored_summary: Option<ContentTextDto>,
    /// Output from a tool or command. Invocation text takes display priority
    /// when both roles are present; output remains separately identifiable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_preview: Option<ContentTextDto>,
}

impl RowContentDto {
    /// Check untrusted decoded text before a renderer publishes the row.
    #[must_use]
    pub fn is_bounded(&self) -> bool {
        self.tool_label
            .as_ref()
            .is_none_or(|text| text.text.len() <= MAX_TOOL_LABEL_BYTES)
            && self
                .authored_summary
                .as_ref()
                .is_none_or(|text| text.text.len() <= MAX_ROW_TEXT_BYTES)
            && self
                .output_preview
                .as_ref()
                .is_none_or(|text| text.text.len() <= MAX_ROW_TEXT_BYTES)
    }

    /// Selected visible content, preferring nonempty authored text over output.
    #[must_use]
    pub fn display_text(&self) -> Option<&ContentTextDto> {
        self.authored_summary
            .as_ref()
            .filter(|text| !text.text.is_empty())
            .or(self.output_preview.as_ref())
            .or(self.authored_summary.as_ref())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preview_limits_preserve_utf8_and_upstream_incompleteness() {
        let exact = "界".repeat(MAX_ROW_TEXT_BYTES / 3);
        assert_eq!(ContentTextDto::new(exact.clone(), true).text, exact);
        assert!(ContentTextDto::new(exact.clone(), true).complete);
        assert!(!ContentTextDto::new(exact, false).complete);
        let clipped = ContentTextDto::new("界".repeat(MAX_ROW_TEXT_BYTES), true);
        assert!(clipped.text.len() <= MAX_ROW_TEXT_BYTES);
        assert!(clipped.text.ends_with('…'));
        assert!(!clipped.complete);
        let label = ContentTextDto::tool_label("x".repeat(MAX_TOOL_LABEL_BYTES + 1), true);
        assert_eq!(label.text.len(), MAX_TOOL_LABEL_BYTES);
        assert!(!label.complete);
    }
}
