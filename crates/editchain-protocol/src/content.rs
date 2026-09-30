//! Compatibility names for semantic content models now owned by app-core.
//! Retire with the legacy viewer protocol in f31/history-details.

pub use idle_history::{
    ContentText as ContentTextDto, RowContent as RowContentDto, MAX_ROW_TEXT_BYTES,
    MAX_TOOL_LABEL_BYTES,
};

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
