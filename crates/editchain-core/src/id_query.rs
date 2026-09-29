//! Full identifiers and repository-local, hexadecimal query prefixes.

use crate::OpId;

/// Validated full identity or hexadecimal prefix (at least four digits).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IdQuery {
    text: String,
    lower: OpId,
    upper: OpId,
}

impl IdQuery {
    /// Parse a full ID, a prefix, or an EC02 `node:boot:seq` address.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        let text = if value.contains(':') {
            OpId::from_display_str(value)?.to_string()
        } else {
            value.to_ascii_lowercase()
        };
        if !(4..=64).contains(&text.len()) || !text.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return None;
        }
        let pad = 64usize.saturating_sub(text.len());
        let lower = OpId::from_display_str(&format!("{text}{}", "0".repeat(pad)))?;
        let upper = OpId::from_display_str(&format!("{text}{}", "f".repeat(pad)))?;
        Some(Self { text, lower, upper })
    }

    /// Inclusive binary bounds suitable for an ordered ID index.
    #[must_use]
    pub const fn bounds(&self) -> (OpId, OpId) {
        (self.lower, self.upper)
    }

    /// Full IDs can address missing records without prefix resolution.
    #[must_use]
    pub fn full(&self) -> Option<OpId> {
        (self.text.len() == 64).then_some(self.lower)
    }
}

impl std::str::FromStr for IdQuery {
    type Err = &'static str;
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::parse(value)
            .ok_or("expected a 4–64 digit hexadecimal ID or a legacy node:boot:seq address")
    }
}
