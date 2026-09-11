//! Find lifecycle, result cursor, and pending destination share one owner.

use super::coordinates::ExpandedRow;

#[derive(Debug, Clone)]
pub(crate) struct FindMatch {
    pub(crate) row: ExpandedRow,
    pub(crate) node_key: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct FindTarget {
    pub(crate) abs: ExpandedRow,
    pub(crate) index: usize,
}

#[derive(Debug, Clone, Copy)]
enum Cursor {
    Loading(usize),
    Ready(usize),
}

impl Cursor {
    fn index(self) -> usize {
        match self {
            Self::Loading(index) | Self::Ready(index) => index,
        }
    }
}

#[derive(Debug, Clone)]
struct Results {
    query: String,
    matches: Vec<FindMatch>,
    more: bool,
    cursor: Option<Cursor>,
}

#[derive(Debug, Clone, Default)]
enum Phase {
    #[default]
    Idle,
    Pending {
        query: String,
        epoch: u64,
    },
    Settled(Results),
    Failed {
        query: String,
    },
}

#[derive(Debug, Clone, Default)]
pub(crate) struct FindSession {
    epoch: u64,
    phase: Phase,
}

impl FindSession {
    pub(super) fn clear(&mut self) {
        self.phase = Phase::Idle;
    }

    pub(super) fn begin(&mut self, query: &str) -> u64 {
        self.epoch = self.epoch.saturating_add(1);
        self.phase = Phase::Pending {
            query: query.to_owned(),
            epoch: self.epoch,
        };
        self.epoch
    }

    pub(super) fn settle(&mut self, matches: Vec<FindMatch>, more: bool) {
        if let Phase::Pending { query, .. } = &self.phase {
            self.phase = Phase::Settled(Results {
                query: query.clone(),
                matches,
                more,
                cursor: None,
            });
        }
    }

    pub(super) fn fail(&mut self) {
        self.phase = Phase::Failed {
            query: self.query().to_owned(),
        };
    }

    pub(super) fn query(&self) -> &str {
        match &self.phase {
            Phase::Idle => "",
            Phase::Pending { query, .. } | Phase::Failed { query } => query,
            Phase::Settled(results) => &results.query,
        }
    }

    pub(super) fn pending_epoch(&self) -> Option<u64> {
        if let Phase::Pending { epoch, .. } = self.phase {
            Some(epoch)
        } else {
            None
        }
    }

    pub(super) fn active(&self) -> bool {
        matches!(self.phase, Phase::Settled(_))
    }

    pub(super) fn matches(&self) -> &[FindMatch] {
        if let Phase::Settled(results) = &self.phase {
            &results.matches
        } else {
            &[]
        }
    }

    pub(super) fn index(&self) -> usize {
        if let Phase::Settled(results) = &self.phase {
            results.cursor.map_or(0, Cursor::index)
        } else {
            0
        }
    }

    pub(super) fn more(&self) -> bool {
        matches!(&self.phase, Phase::Settled(results) if results.more)
    }

    pub(super) fn focus(&mut self, index: usize) -> Option<ExpandedRow> {
        let Phase::Settled(results) = &mut self.phase else {
            return None;
        };
        let row = results.matches.get(index)?.row;
        results.cursor = Some(Cursor::Loading(index));
        Some(row)
    }

    pub(super) fn pending_target(&self) -> Option<FindTarget> {
        let Phase::Settled(results) = &self.phase else {
            return None;
        };
        let Cursor::Loading(index) = results.cursor? else {
            return None;
        };
        Some(FindTarget {
            abs: results.matches.get(index)?.row,
            index,
        })
    }

    pub(super) fn complete_jump(&mut self) {
        if let Phase::Settled(results) = &mut self.phase {
            results.cursor = results.cursor.map(|cursor| Cursor::Ready(cursor.index()));
        }
    }

    pub(super) fn next_index(&self, delta: isize) -> Option<usize> {
        let len = self.matches().len();
        if len == 0 {
            return None;
        }
        let step = delta.unsigned_abs().rem_euclid(len);
        let index = self.index();
        Some(if delta >= 0 {
            index.saturating_add(step).rem_euclid(len)
        } else {
            index
                .saturating_add(len)
                .saturating_sub(step)
                .rem_euclid(len)
        })
    }
}
