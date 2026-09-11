//! Distinct coordinates at the protocol, visibility, and DOM boundaries.

/// Fixed row height in CSS pixels.
pub(crate) const ROW_H: i64 = 34;
/// Every row's pixel offset must remain an exact JavaScript integer too.
pub(super) const MAX_RENDER_ROWS: i64 = 9_007_199_254_740_991 / ROW_H;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct ExpandedRow(i64);

impl ExpandedRow {
    pub(crate) fn new(value: i64) -> Option<Self> {
        (0..=MAX_RENDER_ROWS)
            .contains(&value)
            .then_some(Self(value))
    }

    pub(crate) const fn get(self) -> i64 {
        self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct VisibleRow(i64);

impl VisibleRow {
    pub(crate) fn new(value: i64) -> Option<Self> {
        (0..=MAX_RENDER_ROWS)
            .contains(&value)
            .then_some(Self(value))
    }

    pub(crate) const fn get(self) -> i64 {
        self.0
    }

    pub(crate) fn pixel_offset(self) -> Pixels {
        Pixels(self.0.saturating_mul(ROW_H))
    }
}

/// Nonnegative CSS measurements normalized at the browser boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Pixels(i64);

impl Pixels {
    pub(crate) fn new(value: i64) -> Self {
        Self(value.clamp(0, MAX_RENDER_ROWS.saturating_mul(ROW_H)))
    }

    pub(crate) const fn get(self) -> i64 {
        self.0
    }

    pub(crate) fn row(self) -> VisibleRow {
        VisibleRow(self.0.saturating_div(ROW_H))
    }
}
