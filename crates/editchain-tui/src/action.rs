/// Actions that can be triggered by keyboard input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Quit,
    Down,
    Up,
    PageDown,
    PageUp,
    Top,
    Bottom,
    HalfPageDown,
    HalfPageUp,
    // FocusDagLog,     // Reserved — not yet mapped to a key
    // FocusInspector,  // Reserved — not yet mapped to a key
    ToggleFocus,
    // NextTab,         // Reserved — not yet mapped to a key
    // PrevTab,         // Reserved — not yet mapped to a key
    SelectTab1,
    SelectTab2,
    SelectTab3,
    SelectTab4,
    OpenHelp,
    ClosePopup,
    OpenSearch,
    OpenFilters,
    EnterInspect,
    JumpToParent,
    JumpToChild,
    CopyOpId,
    ToggleRawImports,
    TogglePrivate,
    Redraw,
    None,
}