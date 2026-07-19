/// Filter expression for narrowing the visible operation set.
#[allow(dead_code)]
#[derive(Debug, Clone, Default)]
pub struct FilterExpr {
    /// Only show these kind codes (empty = all).
    pub kinds: Vec<u8>,
    /// Tags: any of these bits must be set.
    pub tags_any: u64,
    /// Tags: all of these bits must be set.
    pub tags_all: u64,
    /// Tags: none of these bits may be set.
    pub tags_exclude: u64,
    /// Only show these actors (empty = all).
    pub actors: Vec<u64>,
    /// Only show operations after this clock value.
    pub after_ms: Option<u64>,
    /// Only show operations before this clock value.
    pub before_ms: Option<u64>,
    /// Include raw imports (default: hidden).
    pub include_raw_imports: bool,
    /// Include private records (default: hidden).
    pub include_private: bool,
}

impl FilterExpr {
    /// Returns true if the given header passes this filter.
    #[allow(dead_code)]
    pub fn matches(&self, header: &crate::data::header::OpHeader) -> bool {
        // Kind filter
        if !self.kinds.is_empty() && !self.kinds.contains(&header.kind_code) {
            return false;
        }

        // Tag filters
        if self.tags_any != 0 && !header.has_any_tag(self.tags_any) {
            return false;
        }
        if self.tags_all != 0 && !header.has_all_tags(self.tags_all) {
            return false;
        }
        if self.tags_exclude != 0 && header.has_any_tag(self.tags_exclude) {
            return false;
        }

        // Actor filter
        if !self.actors.is_empty() && !self.actors.contains(&header.actor) {
            return false;
        }

        // Time filters
        if let Some(after) = self.after_ms {
            if header.clock_value < after {
                return false;
            }
        }
        if let Some(before) = self.before_ms {
            if header.clock_value > before {
                return false;
            }
        }

        // Privacy defaults
        if !self.include_raw_imports && header.kind_code == 7 {
            return false; // Import kind
        }
        if !self.include_private && header.has_any_tag(1 << 10) {
            return false; // PRIVATE tag
        }

        true
    }
}