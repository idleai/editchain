use std::sync::Arc;
use editchain_core::OpId;
use crate::action::Action;
use crate::data::header::OpOrdinal;
use crate::data::snapshot::TuiSnapshot;
use crate::data::filters::FilterExpr;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppMode { Browser, /* LocalGraph — reserved for future use */ FullNode }

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus { DagLog, Inspector, Popup }

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InspectorTab { Summary, Content, Relations, Raw }

impl InspectorTab {
    // pub fn next(&self) -> Self {
    //     match self {
    //         Self::Summary => Self::Content,
    //         Self::Content => Self::Relations,
    //         Self::Relations => Self::Raw,
    //         Self::Raw => Self::Summary,
    //     }
    // }
    // pub fn prev(&self) -> Self {
    //     match self {
    //         Self::Summary => Self::Raw,
    //         Self::Content => Self::Summary,
    //         Self::Relations => Self::Content,
    //         Self::Raw => Self::Relations,
    //     }
    // }
    // pub fn name(&self) -> &'static str {
    //     match self {
    //         Self::Summary => "Summary",
    //         Self::Content => "Content",
    //         Self::Relations => "Relations",
    //         Self::Raw => "Raw",
    //     }
    // }
}

#[derive(Debug, Clone)]
pub enum Popup { Help, Search, Filters }

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub enum StatusState { Ready, Info(String), Warning(String), Error(String) }

#[allow(dead_code)]
#[derive(Debug, Clone, Default)]
pub struct SearchState {
    pub query: String,
    pub results: Vec<OpOrdinal>,
    pub current_result: Option<usize>,
    pub active: bool,
}

use crate::dag::lanes::DagRow;

#[allow(dead_code)]
pub struct App {
    pub mode: AppMode,
    pub focus: Focus,
    pub snapshot: Option<Arc<TuiSnapshot>>,
    pub visible_rows: Arc<Vec<OpOrdinal>>,
    /// Cached DAG lane rows — recomputed when visible_rows changes.
    pub dag_rows: Vec<DagRow>,
    /// Generation counter bumped when visible_rows changes (for cache invalidation).
    pub visible_gen: u64,
    pub selected_visible_index: usize,
    pub selected_op: Option<OpId>,
    pub inspector_tab: InspectorTab,
    pub filters: FilterExpr,
    pub search: SearchState,
    pub popup: Option<Popup>,
    pub status: StatusState,
    pub should_quit: bool,
    pub terminal_height: u16,
    pub terminal_width: u16,
    pub scroll_offset: usize,
    pub show_raw_imports: bool,
    pub show_private: bool,
}

impl App {
    pub fn new() -> Self {
        Self {
            mode: AppMode::Browser,
            focus: Focus::DagLog,
            snapshot: None,
            visible_rows: Arc::new(Vec::new()),
            dag_rows: Vec::new(),
            visible_gen: 0,
            selected_visible_index: 0,
            selected_op: None,
            inspector_tab: InspectorTab::Summary,
            filters: FilterExpr::default(),
            search: SearchState::default(),
            popup: None,
            status: StatusState::Ready,
            should_quit: false,
            terminal_height: 0,
            terminal_width: 0,
            scroll_offset: 0,
            show_raw_imports: false,
            show_private: false,
        }
    }

    pub fn load_snapshot(&mut self, snapshot: TuiSnapshot) {
        let stats = snapshot.statistics.clone();
        let snapshot = Arc::new(snapshot);
        let all_ords: Vec<OpOrdinal> = (0..snapshot.headers.len() as OpOrdinal).collect();
        self.snapshot = Some(snapshot);
        self.visible_rows = Arc::new(all_ords);
        self.selected_visible_index = 0;
        self.selected_op = None;
        self.scroll_offset = 0;
        self.status = StatusState::Info(format!("Loaded {} operations", stats.total_ops));
        self.recompute_dag_rows();
    }

    /// Recompute cached DAG rows from the current visible set.
    pub fn recompute_dag_rows(&mut self) {
        let snapshot = match &self.snapshot {
            Some(s) => s.clone(),
            None => return,
        };
        let visible = self.visible_rows.clone();
        if visible.is_empty() {
            self.dag_rows = Vec::new();
            return;
        }
        self.dag_rows = crate::dag::lanes::LaneState::compute_rows(
            &visible,
            |ord| snapshot.header_at(ord).map(|h| h.id).unwrap_or(editchain_core::OpId::new(editchain_core::NodeId(0), 0, 0)),
            |ord| snapshot.parents.get(ord as usize).cloned().unwrap_or_default(),
        );
        self.visible_gen += 1;
    }

    fn sync_selection(&mut self) {
        if let Some(ref snapshot) = self.snapshot {
            if self.selected_visible_index < self.visible_rows.len() {
                let ord = self.visible_rows[self.selected_visible_index];
                if let Some(header) = snapshot.header_at(ord) {
                    self.selected_op = Some(header.id);
                    return;
                }
            }
        }
        self.selected_op = None;
    }

    fn ensure_visible(&mut self) {
        let vh = (self.terminal_height.saturating_sub(3) as usize).max(5);
        if self.selected_visible_index < self.scroll_offset {
            self.scroll_offset = self.selected_visible_index;
        } else if self.selected_visible_index >= self.scroll_offset + vh {
            self.scroll_offset = self.selected_visible_index.saturating_sub(vh) + 1;
        }
    }

    fn find_visible_index(&self, ord: OpOrdinal) -> Option<usize> {
        self.visible_rows.iter().position(|&r| r == ord)
    }

    fn jump_to_ordinal(&mut self, ord: OpOrdinal) -> bool {
        if let Some(idx) = self.find_visible_index(ord) {
            self.selected_visible_index = idx;
            if let Some(ref snapshot) = self.snapshot {
                if let Some(header) = snapshot.header_at(ord) {
                    self.selected_op = Some(header.id);
                }
            }
            self.ensure_visible();
            true
        } else {
            false
        }
    }

    fn copy_to_clipboard(text: &str) -> bool {
        use std::io::Write;
        #[cfg(target_os = "linux")]
        {
            std::process::Command::new("xclip")
                .arg("-selection").arg("clipboard")
                .stdin(std::process::Stdio::piped())
                .spawn()
                .and_then(|mut child| {
                    child.stdin.as_mut().map(|s| s.write_all(text.as_bytes()));
                    child.wait()
                })
                .is_ok()
        }
        #[cfg(target_os = "macos")]
        {
            std::process::Command::new("pbcopy")
                .stdin(std::process::Stdio::piped())
                .spawn()
                .and_then(|mut child| {
                    child.stdin.as_mut().map(|s| s.write_all(text.as_bytes()));
                    child.wait()
                })
                .is_ok()
        }
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        { false }
    }

    pub fn handle_action(&mut self, action: Action) -> bool {
        match action {
            Action::Quit => { self.should_quit = true; true }

            Action::Down => {
                if self.focus == Focus::DagLog && !self.visible_rows.is_empty() && self.selected_visible_index + 1 < self.visible_rows.len() {
                    self.selected_visible_index += 1;
                    self.sync_selection();
                    self.ensure_visible();
                }
                true
            }

            Action::Up => {
                if self.focus == Focus::DagLog && self.selected_visible_index > 0 {
                    self.selected_visible_index -= 1;
                    self.sync_selection();
                    self.ensure_visible();
                }
                true
            }

            Action::PageDown => {
                if self.focus == Focus::DagLog && !self.visible_rows.is_empty() {
                    let page = (self.terminal_height.saturating_sub(3) as usize).max(5);
                    let max_idx = self.visible_rows.len().saturating_sub(1);
                    self.selected_visible_index = (self.selected_visible_index + page).min(max_idx);
                    self.sync_selection();
                    self.ensure_visible();
                }
                true
            }

            Action::PageUp => {
                if self.focus == Focus::DagLog {
                    let page = (self.terminal_height.saturating_sub(3) as usize).max(5);
                    self.selected_visible_index = self.selected_visible_index.saturating_sub(page);
                    self.sync_selection();
                    self.ensure_visible();
                }
                true
            }

            Action::HalfPageDown => {
                if self.focus == Focus::DagLog && !self.visible_rows.is_empty() {
                    let half = ((self.terminal_height.saturating_sub(3) as usize) / 2).max(2);
                    let max_idx = self.visible_rows.len().saturating_sub(1);
                    self.selected_visible_index = (self.selected_visible_index + half).min(max_idx);
                    self.sync_selection();
                    self.ensure_visible();
                }
                true
            }

            Action::HalfPageUp => {
                if self.focus == Focus::DagLog {
                    let half = ((self.terminal_height.saturating_sub(3) as usize) / 2).max(2);
                    self.selected_visible_index = self.selected_visible_index.saturating_sub(half);
                    self.sync_selection();
                    self.ensure_visible();
                }
                true
            }

            Action::Top => {
                if self.focus == Focus::DagLog && !self.visible_rows.is_empty() {
                    self.selected_visible_index = 0;
                    self.scroll_offset = 0;
                    self.sync_selection();
                }
                true
            }

            Action::Bottom => {
                if self.focus == Focus::DagLog && !self.visible_rows.is_empty() {
                    let len = self.visible_rows.len();
                    self.selected_visible_index = len.saturating_sub(1);
                    self.sync_selection();
                    self.ensure_visible();
                }
                true
            }

            Action::ToggleFocus => {
                match (self.focus, &self.popup) {
                    (Focus::DagLog, None) => { self.focus = Focus::Inspector; }
                    (Focus::Inspector, None) => { self.focus = Focus::DagLog; }
                    _ => {}
                }
                true
            }

            Action::SelectTab1 => {
                if self.focus == Focus::Inspector || self.focus == Focus::DagLog {
                    self.focus = Focus::Inspector;
                    self.inspector_tab = InspectorTab::Summary;
                }
                true
            }

            Action::SelectTab2 => {
                if self.focus == Focus::Inspector || self.focus == Focus::DagLog {
                    self.focus = Focus::Inspector;
                    self.inspector_tab = InspectorTab::Content;
                }
                true
            }

            Action::SelectTab3 => {
                if self.focus == Focus::Inspector || self.focus == Focus::DagLog {
                    self.focus = Focus::Inspector;
                    self.inspector_tab = InspectorTab::Relations;
                }
                true
            }

            Action::SelectTab4 => {
                if self.focus == Focus::Inspector || self.focus == Focus::DagLog {
                    self.focus = Focus::Inspector;
                    self.inspector_tab = InspectorTab::Raw;
                }
                true
            }

            Action::OpenHelp => {
                if self.popup.is_some() {
                    self.popup = None;
                } else {
                    self.popup = Some(Popup::Help);
                    self.focus = Focus::Popup;
                }
                true
            }

            Action::ClosePopup => {
                if self.popup.is_some() {
                    self.popup = None;
                    self.focus = Focus::DagLog;
                }
                true
            }

            Action::EnterInspect => {
                if self.mode == AppMode::FullNode {
                    self.mode = AppMode::Browser;
                } else if self.selected_op.is_some() {
                    self.focus = Focus::Inspector;
                }
                true
            }

            Action::JumpToParent => {
                if let Some(op_id) = self.selected_op {
                    if let Some(ref snapshot) = self.snapshot {
                        if let Some(ord) = snapshot.ordinal_of(&op_id) {
                            if let Some(parents) = snapshot.parents.get(ord as usize) {
                                if let Some(&parent_ord) = parents.first() {
                                    self.jump_to_ordinal(parent_ord);
                                }
                            }
                        }
                    }
                }
                true
            }

            Action::JumpToChild => {
                if let Some(op_id) = self.selected_op {
                    if let Some(ref snapshot) = self.snapshot {
                        if let Some(ord) = snapshot.ordinal_of(&op_id) {
                            if let Some(children) = snapshot.children.get(ord as usize) {
                                if let Some(&child_ord) = children.first() {
                                    self.jump_to_ordinal(child_ord);
                                }
                            }
                        }
                    }
                }
                true
            }

            Action::CopyOpId => {
                if let Some(op_id) = self.selected_op {
                    let id_str = op_id.to_string();
                    if Self::copy_to_clipboard(&id_str) {
                        self.status = StatusState::Info(format!("Copied {}", id_str));
                    } else {
                        self.status = StatusState::Info(id_str);
                    }
                }
                true
            }

            Action::ToggleRawImports => {
                self.show_raw_imports = !self.show_raw_imports;
                self.status = StatusState::Info(format!("Raw imports: {}", if self.show_raw_imports { "shown" } else { "hidden" }));
                true
            }

            Action::TogglePrivate => {
                self.show_private = !self.show_private;
                self.status = StatusState::Info(format!("Private records: {}", if self.show_private { "shown" } else { "hidden" }));
                true
            }

            Action::Redraw => { true }

            Action::OpenSearch => {
                self.popup = Some(Popup::Search);
                self.focus = Focus::Popup;
                true
            }

            Action::OpenFilters => {
                self.popup = Some(Popup::Filters);
                self.focus = Focus::Popup;
                true
            }

            Action::None => { false }
        }
    }
}
