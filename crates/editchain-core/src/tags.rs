use serde::{Deserialize, Serialize};

/// Bitflags for zero-database operation filtering.
///
/// Tags are the primary filter surface for header scans.
/// Multiple tags can be OR'd together for compound queries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Tags(pub u64);

impl Tags {
    pub const NONE: Self = Self(0);

    pub const AGENT: Self = Self(1 << 0);
    pub const HUMAN: Self = Self(1 << 1);
    pub const FILE: Self = Self(1 << 2);
    pub const MESSAGE: Self = Self(1 << 3);
    pub const TOOL: Self = Self(1 << 4);
    pub const COMMAND: Self = Self(1 << 5);
    pub const IMPORT: Self = Self(1 << 6);
    pub const REFLECTION: Self = Self(1 << 7);
    pub const NOTE: Self = Self(1 << 8);
    pub const ERROR: Self = Self(1 << 9);
    pub const PRIVATE: Self = Self(1 << 10);
    pub const LARGE_PAYLOAD: Self = Self(1 << 11);

    /// Returns true if any of the given tags are set.
    pub fn matches_any(&self, filter: Tags) -> bool {
        self.0 & filter.0 != 0
    }

    /// Returns true if all of the given tags are set.
    pub fn matches_all(&self, filter: Tags) -> bool {
        self.0 & filter.0 == filter.0
    }
}

impl core::ops::BitOr for Tags {
    type Output = Self;
    fn bitor(self, rhs: Self) -> Self {
        Self(self.0 | rhs.0)
    }
}

impl core::ops::BitOrAssign for Tags {
    fn bitor_assign(&mut self, rhs: Self) {
        self.0 |= rhs.0;
    }
}

impl core::fmt::Display for Tags {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let mut first = true;
        let pairs = [
            (Self::AGENT, "agent"),
            (Self::HUMAN, "human"),
            (Self::FILE, "file"),
            (Self::MESSAGE, "message"),
            (Self::TOOL, "tool"),
            (Self::COMMAND, "command"),
            (Self::IMPORT, "import"),
            (Self::REFLECTION, "reflection"),
            (Self::NOTE, "note"),
            (Self::ERROR, "error"),
            (Self::PRIVATE, "private"),
            (Self::LARGE_PAYLOAD, "large_payload"),
        ];
        for (tag, name) in &pairs {
            if self.matches_any(*tag) {
                if !first {
                    f.write_str("|")?;
                }
                f.write_str(name)?;
                first = false;
            }
        }
        if first {
            f.write_str("none")?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tag_constants() {
        let t = Tags::AGENT | Tags::FILE;
        assert!(t.matches_any(Tags::AGENT));
        assert!(t.matches_any(Tags::FILE));
        assert!(!t.matches_any(Tags::HUMAN));
        assert!(t.matches_all(Tags::AGENT | Tags::FILE));
        assert!(!t.matches_all(Tags::AGENT | Tags::FILE | Tags::HUMAN));
    }

    #[test]
    fn tag_display() {
        assert_eq!(format!("{}", Tags::NONE), "none");
        assert_eq!(format!("{}", Tags::AGENT), "agent");
        let t = Tags::AGENT | Tags::FILE;
        let s = format!("{}", t);
        assert!(s.contains("agent"));
        assert!(s.contains("file"));
    }
}