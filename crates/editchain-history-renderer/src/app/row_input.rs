//! A decoded service row and its once-resolved compatibility presentation.
//!
//! The source DTO remains available for exact action identities and diagnostic
//! hooks. Rendering never converts it back to a JSON object to read fields.

use editchain_protocol::{ActivityBundleKind, HistoryRow};

use super::legacy_content::{display_summary_for_row, tool_name_from_summary};
use super::rows::{BundleKind, SessionSummaryData, WorkUnitData};
use super::ChainState;

#[derive(Debug, Clone)]
pub(crate) struct RowInput {
    pub(crate) source: HistoryRow,
    pub(crate) record_role: String,
    pub(crate) activity_kind: String,
    pub(crate) outcome: String,
    pub(crate) chain_state: ChainState,
    pub(crate) work_unit: Option<WorkUnitData>,
    pub(crate) session_summary: Option<SessionSummaryData>,
    pub(crate) bundle_kind: Option<BundleKind>,
    pub(crate) bundle_count: Option<u64>,
    pub(crate) display_summary: String,
    pub(crate) tool_label: Option<String>,
}

impl From<HistoryRow> for RowInput {
    fn from(source: HistoryRow) -> Self {
        let mut row = Self {
            record_role: source.record_role.as_str().to_owned(),
            activity_kind: source.activity_kind.as_str().to_owned(),
            outcome: source.outcome.as_str().to_owned(),
            chain_state: if source.chain_state.is_active() {
                ChainState::Active
            } else {
                ChainState::Muted
            },
            work_unit: source.work_unit.as_ref().map(|unit| WorkUnitData {
                id: unit.id.clone(),
                title: unit.title.clone().filter(|title| !title.is_empty()),
                count: Some(unit.count),
                is_start: unit.is_start,
                is_end: unit.is_end,
            }),
            session_summary: source
                .session_summary
                .as_ref()
                .map(|summary| SessionSummaryData {
                    count: Some(summary.count),
                }),
            bundle_kind: source
                .activity_bundle
                .as_ref()
                .and_then(|bundle| match bundle.kind {
                    ActivityBundleKind::WorkGroup => Some(BundleKind::WorkGroup),
                    ActivityBundleKind::ExecuteRun => Some(BundleKind::ExecuteRun),
                    ActivityBundleKind::PlanRepeat => Some(BundleKind::PlanRepeat),
                    ActivityBundleKind::Unknown => None,
                }),
            bundle_count: source
                .activity_bundle
                .as_ref()
                .map(|bundle| bundle.member_count),
            display_summary: String::new(),
            tool_label: None,
            source,
        };
        row.resolve_content();
        row
    }
}

impl RowInput {
    /// Charge the encoded DTO and copied presentation text once at ingestion.
    /// This measures retained evidence, not allocator overhead or process RSS.
    pub(super) fn cache_bytes(&self) -> Result<u64, serde_json::Error> {
        let mut encoded = EncodedLength::default();
        serde_json::to_writer(&mut encoded, &self.source)?;
        let presentation = [
            self.record_role.as_str(),
            &self.activity_kind,
            &self.outcome,
            &self.display_summary,
            self.tool_label.as_deref().unwrap_or_default(),
            self.work_unit.as_ref().map_or("", |unit| unit.id.as_str()),
            self.work_unit
                .as_ref()
                .and_then(|unit| unit.title.as_deref())
                .unwrap_or_default(),
        ];
        Ok(presentation.into_iter().fold(encoded.0, |bytes, text| {
            bytes.saturating_add(u64::try_from(text.len()).unwrap_or(u64::MAX))
        }))
    }

    pub(crate) fn summary_source(&self) -> &str {
        if self.source.content.is_some() {
            return &self.display_summary;
        }
        if self.source.summary.is_empty() {
            "(no summary)"
        } else {
            &self.source.summary
        }
    }

    fn resolve_content(&mut self) {
        if let Some(content) = &self.source.content {
            self.tool_label = content
                .tool_label
                .as_ref()
                .map(|label| label.text.clone())
                .filter(|label| !label.is_empty());
            self.display_summary = content
                .display_text()
                .map(|text| text.text.clone())
                .filter(|text| !text.is_empty())
                .unwrap_or_else(|| {
                    if self.tool_label.is_some() {
                        self.empty_tool_summary()
                    } else {
                        "(no summary)".to_owned()
                    }
                });
            return;
        }
        self.display_summary = display_summary_for_row(self, self.summary_source());
        self.tool_label = tool_name_from_summary(self.summary_source());
    }

    pub(super) fn empty_tool_summary(&self) -> String {
        match self.outcome.as_str() {
            "success" => "Completed",
            "failure" => "Failed",
            "warning" => "Completed with warnings",
            "cancelled" => "Cancelled",
            _ if self.record_role == "action" || self.source.kind == "command" => "Tool request",
            _ => "Tool result",
        }
        .to_owned()
    }

    pub(crate) fn lane(&self) -> u32 {
        u32::try_from(self.source.lane).unwrap_or(0)
    }

    pub(crate) fn above(&self) -> Vec<u32> {
        lanes(&self.source.above)
    }
    pub(crate) fn below(&self) -> Vec<u32> {
        lanes(&self.source.below)
    }
    pub(crate) fn muted_above(&self) -> Vec<u32> {
        lanes(&self.source.muted_above)
    }
    pub(crate) fn muted_below(&self) -> Vec<u32> {
        lanes(&self.source.muted_below)
    }
    pub(crate) fn transitions(&self) -> Vec<(u32, u32)> {
        transitions(&self.source.transitions)
    }
    pub(crate) fn muted_transitions(&self) -> Vec<(u32, u32)> {
        transitions(&self.source.muted_transitions)
    }

    #[cfg(target_arch = "wasm32")]
    pub(crate) fn wire_json(&self) -> String {
        serde_json::to_string(&self.source).unwrap_or_else(|_| "null".to_owned())
    }
}

#[derive(Default)]
struct EncodedLength(u64);

impl std::io::Write for EncodedLength {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0 = self
            .0
            .saturating_add(u64::try_from(bytes.len()).unwrap_or(u64::MAX));
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn lanes(values: &[usize]) -> Vec<u32> {
    values
        .iter()
        .filter_map(|lane| u32::try_from(*lane).ok())
        .collect()
}

fn transitions(values: &[(usize, usize)]) -> Vec<(u32, u32)> {
    values
        .iter()
        .filter_map(|&(from, to)| Some((u32::try_from(from).ok()?, u32::try_from(to).ok()?)))
        .collect()
}

#[cfg(test)]
mod fixtures;
