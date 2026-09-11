//! Validated snapshot topology and disclosure rank/select over visible runs.
//!
//! Installing metadata scans its wire arrays once. Toggling a row only visits
//! expandable spans; plain history ranges never allocate an entry per row.

use std::collections::BTreeSet;

use editchain_protocol::{ErrorCode, ExpansionSpanDto, ServiceError};

use super::coordinates::{ExpandedRow, VisibleRow, MAX_RENDER_ROWS};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Span {
    row: ExpandedRow,
    end: i64,
}

#[derive(Debug, Clone)]
struct VisibleRun {
    first: ExpandedRow,
    end: i64,
    rank: VisibleRow,
}

#[derive(Debug, Clone)]
pub(crate) struct ExpansionIndex {
    total: i64,
    spans: Vec<Span>,
    expanded: BTreeSet<ExpandedRow>,
    runs: Vec<VisibleRun>,
    visible_total: i64,
}

impl ExpansionIndex {
    pub(super) fn from_metadata(
        total: i64,
        counts: Option<&[usize]>,
        spans: Option<&[ExpansionSpanDto]>,
    ) -> Result<Self, ServiceError> {
        if !(0..=MAX_RENDER_ROWS).contains(&total) {
            return Err(invalid("History coordinates exceed the exact pixel range."));
        }
        let roots = counts
            .map(|counts| legacy_roots(counts, total))
            .transpose()?;
        let spans = match spans {
            Some(spans) => validate_spans(spans, roots.as_deref(), total)?,
            None => roots.ok_or_else(|| invalid("Missing history expansion metadata."))?,
        };
        let mut index = Self {
            total,
            spans,
            expanded: BTreeSet::new(),
            runs: Vec::new(),
            visible_total: 0,
        };
        index.rebuild_runs();
        Ok(index)
    }

    pub(super) fn same_snapshot(&self, other: &Self) -> bool {
        self.total == other.total && self.spans == other.spans
    }

    pub(super) const fn total(&self) -> i64 {
        self.total
    }

    pub(super) const fn visible_total(&self) -> i64 {
        self.visible_total
    }

    pub(super) fn expanded_for(&self, visible: VisibleRow) -> Option<ExpandedRow> {
        let next = self.runs.partition_point(|run| run.rank <= visible);
        let run = self.runs.get(next.checked_sub(1)?)?;
        let absolute = run
            .first
            .get()
            .saturating_add(visible.get().saturating_sub(run.rank.get()));
        (absolute < run.end)
            .then(|| ExpandedRow::new(absolute))
            .flatten()
    }

    pub(super) fn visible_for(&self, absolute: ExpandedRow) -> Option<VisibleRow> {
        let next = self.runs.partition_point(|run| run.first <= absolute);
        let run = self.runs.get(next.checked_sub(1)?)?;
        (absolute.get() < run.end)
            .then(|| {
                VisibleRow::new(
                    run.rank
                        .get()
                        .saturating_add(absolute.get().saturating_sub(run.first.get())),
                )
            })
            .flatten()
    }

    pub(super) fn visible_between(
        &self,
        top: ExpandedRow,
        bottom: ExpandedRow,
    ) -> impl Iterator<Item = ExpandedRow> + '_ {
        let first = self.runs.partition_point(|run| run.end <= top.get());
        self.runs
            .iter()
            .skip(first)
            .take_while(move |run| run.first <= bottom)
            .flat_map(move |run| {
                run.first.get().max(top.get())..run.end.min(bottom.get().saturating_add(1))
            })
            .filter_map(ExpandedRow::new)
    }

    pub(super) fn is_expanded(&self, row: ExpandedRow) -> bool {
        self.expanded.contains(&row)
    }

    pub(super) fn toggle(&mut self, row: ExpandedRow) -> bool {
        if self
            .spans
            .binary_search_by_key(&row, |span| span.row)
            .is_err()
        {
            return false;
        }
        if !self.expanded.remove(&row) {
            let _: bool = self.expanded.insert(row);
        }
        self.rebuild_runs();
        true
    }

    fn rebuild_runs(&mut self) {
        let mut next = 0;
        let mut rank = 0;
        self.runs.clear();
        for span in &self.spans {
            if span.row.get() < next || self.expanded.contains(&span.row) {
                continue;
            }
            let end = span.row.get().saturating_add(1);
            push_run(&mut self.runs, next, end, rank);
            rank = rank.saturating_add(end.saturating_sub(next));
            next = span.end;
        }
        push_run(&mut self.runs, next, self.total, rank);
        self.visible_total = rank.saturating_add(self.total.saturating_sub(next));
    }
}

fn push_run(runs: &mut Vec<VisibleRun>, first: i64, end: i64, rank: i64) {
    if first < end {
        if let (Some(first), Some(rank)) = (ExpandedRow::new(first), VisibleRow::new(rank)) {
            runs.push(VisibleRun { first, end, rank });
        }
    }
}

fn legacy_roots(counts: &[usize], total: i64) -> Result<Vec<Span>, ServiceError> {
    let mut next = 0i64;
    let mut roots = Vec::new();
    for count in counts {
        let count = i64::try_from(*count)
            .map_err(|error| invalid(&format!("Invalid descendant count: {error}")))?;
        let end = next
            .checked_add(1)
            .and_then(|start| start.checked_add(count))
            .filter(|end| *end <= total)
            .ok_or_else(|| invalid("Top-level history blocks exceed the snapshot total."))?;
        if count > 0 {
            let row = ExpandedRow::new(next).ok_or_else(|| invalid("Invalid parent row."))?;
            roots.push(Span { row, end });
        }
        next = end;
    }
    if next != total {
        return Err(invalid(
            "Top-level history blocks do not cover the snapshot total.",
        ));
    }
    Ok(roots)
}

fn validate_spans(
    wire: &[ExpansionSpanDto],
    roots: Option<&[Span]>,
    total: i64,
) -> Result<Vec<Span>, ServiceError> {
    let mut spans = wire
        .iter()
        .map(|span| checked_span(*span, total))
        .collect::<Result<Vec<_>, _>>()?;
    spans.sort_unstable_by_key(|span| span.row);
    let mut previous = None;
    let mut ancestors = Vec::new();
    let mut root_index = 0;
    for span in &spans {
        if previous == Some(span.row) {
            return Err(invalid("Duplicate history expansion parent."));
        }
        previous = Some(span.row);
        while ancestors.last().is_some_and(|end| *end <= span.row.get()) {
            let _: Option<i64> = ancestors.pop();
        }
        if ancestors.last().is_some_and(|end| *end < span.end) {
            return Err(invalid("Crossing history expansion spans."));
        }
        if ancestors.is_empty() {
            if roots.is_some_and(|roots| roots.get(root_index) != Some(span)) {
                return Err(invalid(
                    "Expansion spans disagree with top-level history blocks.",
                ));
            }
            root_index = root_index.saturating_add(1);
        }
        ancestors.push(span.end);
    }
    if roots.is_some_and(|roots| roots.len() != root_index) {
        return Err(invalid("Expansion spans omit a top-level history block."));
    }
    Ok(spans)
}

fn checked_span(span: ExpansionSpanDto, total: i64) -> Result<Span, ServiceError> {
    let row = i64::try_from(span.row)
        .ok()
        .and_then(ExpandedRow::new)
        .ok_or_else(|| invalid("Invalid history expansion parent."))?;
    let end = span
        .row
        .checked_add(1)
        .and_then(|start| start.checked_add(span.descendant_count))
        .and_then(|end| i64::try_from(end).ok())
        .filter(|end| *end <= total && span.descendant_count > 0)
        .ok_or_else(|| invalid("History descendants exceed the snapshot total."))?;
    Ok(Span { row, end })
}

fn invalid(message: &str) -> ServiceError {
    ServiceError::new(ErrorCode::InvalidInput, message)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn span(row: u64, descendant_count: u64) -> ExpansionSpanDto {
        ExpansionSpanDto {
            row,
            descendant_count,
        }
    }

    #[test]
    fn invalid_metadata_cannot_construct_an_index() {
        for spans in [
            vec![span(0, 5), span(0, 5)],
            vec![span(0, 5), span(4, 3)],
            vec![span(9, 1)],
            vec![span(0, 0)],
            vec![span(u64::MAX, 1)],
            vec![span(0, u64::MAX)],
        ] {
            assert!(ExpansionIndex::from_metadata(10, None, Some(&spans)).is_err());
        }
        for counts in [vec![], vec![0], vec![10], vec![usize::MAX]] {
            assert!(ExpansionIndex::from_metadata(10, Some(&counts), None).is_err());
        }
        assert!(ExpansionIndex::from_metadata(2, Some(&[1]), Some(&[])).is_err());
        assert!(ExpansionIndex::from_metadata(2, Some(&[0, 0]), Some(&[span(0, 1)])).is_err());
        assert!(
            ExpansionIndex::from_metadata(MAX_RENDER_ROWS.saturating_add(1), None, Some(&[]))
                .is_err()
        );
    }

    #[test]
    fn nested_forest_maps_every_disclosure_combination_and_page_boundary() {
        let spans = [span(0, 7), span(1, 2), span(4, 2), span(9, 1)];
        for mask in 0u8..16 {
            let mut index =
                ExpansionIndex::from_metadata(12, Some(&[7, 0, 1, 0]), Some(&spans)).unwrap();
            for (bit, parent) in [0, 1, 4, 9].into_iter().enumerate() {
                if mask & (1 << bit) != 0 {
                    assert!(index.toggle(ExpandedRow::new(parent).unwrap()));
                }
            }
            let mut expected = vec![0];
            if mask & 1 != 0 {
                expected.push(1);
                if mask & 2 != 0 {
                    expected.extend([2, 3]);
                }
                expected.push(4);
                if mask & 4 != 0 {
                    expected.extend([5, 6]);
                }
                expected.push(7);
            }
            expected.extend([8, 9]);
            if mask & 8 != 0 {
                expected.push(10);
            }
            expected.push(11);
            assert_eq!(
                index.visible_total(),
                i64::try_from(expected.len()).unwrap()
            );
            for (rank, row) in expected.iter().copied().enumerate() {
                let rank = VisibleRow::new(i64::try_from(rank).unwrap()).unwrap();
                let row = ExpandedRow::new(row).unwrap();
                assert_eq!(index.expanded_for(rank), Some(row));
                assert_eq!(index.visible_for(row), Some(rank));
            }
            for top in 0..12 {
                assert_eq!(
                    index.visible_for(ExpandedRow::new(top).unwrap()).is_some(),
                    expected.contains(&top)
                );
                for bottom in top..12 {
                    let actual: Vec<_> = index
                        .visible_between(
                            ExpandedRow::new(top).unwrap(),
                            ExpandedRow::new(bottom).unwrap(),
                        )
                        .map(ExpandedRow::get)
                        .collect();
                    let slice: Vec<_> = expected
                        .iter()
                        .copied()
                        .filter(|row| *row >= top && *row <= bottom)
                        .collect();
                    assert_eq!(actual, slice);
                }
            }
            assert!(index
                .expanded_for(VisibleRow::new(index.visible_total()).unwrap())
                .is_none());
            assert!(index.visible_for(ExpandedRow::new(12).unwrap()).is_none());
        }
    }

    #[test]
    fn plain_ranges_have_constant_storage_at_the_coordinate_limit() {
        let index = ExpansionIndex::from_metadata(MAX_RENDER_ROWS, None, Some(&[])).unwrap();
        assert!(index.spans.is_empty());
        assert_eq!(index.runs.len(), 1);
        let last = MAX_RENDER_ROWS.saturating_sub(1);
        assert_eq!(
            index
                .expanded_for(VisibleRow::new(last).unwrap())
                .map(ExpandedRow::get),
            Some(last)
        );
        assert_eq!(
            index
                .visible_for(ExpandedRow::new(last).unwrap())
                .map(VisibleRow::get),
            Some(last)
        );
        assert_eq!(
            VisibleRow::new(last).unwrap().pixel_offset().row().get(),
            last
        );
    }
}
