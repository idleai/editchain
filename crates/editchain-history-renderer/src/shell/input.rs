//! Browser input callbacks and the active column drag.
//!
//! Callbacks submit bounded reducer transitions through the shared runtime.
//! A replaced or completed drag releases its own window registrations.

use super::diagnostics::{js_value_text, record_error};
use super::resources::{EventListener, Timer};
use super::runtime::{run_transition, TransitionOutput};
use super::{ShellData, SHELL_DATA};
use crate::app::dom::ColKey;
use crate::app::rows;
use crate::app::state::{DomOp, RetryAction, Step};
use std::cell::RefCell;
use wasm_bindgen::prelude::*;

thread_local! {
    static COLUMN_DRAG: RefCell<Option<ColumnDrag>> = const { RefCell::new(None) };
}

/// An active column-divider drag (window-level listeners persist for the
/// whole gesture; `move`/`up` closures remove themselves on mouseup).
#[derive(Debug)]
struct ColumnDrag {
    col: ColKey,
    start_x: f64,
    start_w: f64,
    _move_listener: EventListener,
    _up_listener: EventListener,
}

/// Scroll handler: keep the bounded window in sync with the viewport.
pub(super) fn on_scroll() {
    run_transition(|shell| {
        let viewport = shell.dom.viewport();
        let mut step = Step::new();
        shell.state.sync_window(&viewport, &mut step);
        shell.state.fetch_window(&viewport, &mut step);
        shell.apply_step_ops(&step);
        TransitionOutput {
            sends: std::mem::take(&mut step.sends),
            save_state: step.save_state.take(),
        }
    });
}

/// Terminal request-error Retry button.
pub(super) fn on_retry(action: RetryAction) {
    run_transition(|shell| {
        let viewport = shell.dom.viewport();
        let mut step = Step::new();
        match action {
            RetryAction::ResetHistory => shell.state.reset_history(&viewport, &mut step),
            RetryAction::RefreshSnapshot => shell.state.refresh_snapshot(&viewport, &mut step),
        }
        shell.apply_step_ops(&step);
        TransitionOutput {
            sends: std::mem::take(&mut step.sends),
            save_state: step.save_state.take(),
        }
    });
}

/// Progressive loader tick: buffer history ahead of the scroll position.
pub(super) fn on_progressive_tick() {
    run_transition(|shell| {
        let viewport = shell.dom.viewport();
        let mut step = Step::new();
        shell.state.fetch_window(&viewport, &mut step);
        shell.state.sync_window(&viewport, &mut step);
        shell.apply_step_ops(&step);
        TransitionOutput {
            sends: std::mem::take(&mut step.sends),
            save_state: step.save_state.take(),
        }
    });
}

/// Exit the in-place find interaction without changing the history view.
fn exit_search() {
    run_transition(|shell| {
        let mut step = Step::new();
        shell.state.clear_find(&mut step);
        shell.apply_step_ops(&step);
        TransitionOutput {
            sends: std::mem::take(&mut step.sends),
            save_state: step.save_state.take(),
        }
    });
}

/// Find-in-chain keyboard behaviour:
/// Enter submits / advances the same settled query; Shift+Enter steps
/// back; Escape/empty clears; ArrowDown/Up navigate settled matches for
/// the exact submitted query.
pub(super) fn on_search_keydown(event: &web_sys::KeyboardEvent) {
    let key = event.key();
    let trimmed = SHELL_DATA.with(|cell| {
        cell.borrow()
            .as_ref()
            .map(|shell| shell.dom.search_input_value())
            .unwrap_or_default()
    });
    if key == "Enter" {
        if trimmed.is_empty() {
            exit_search();
            return;
        }
        let same_settled = SHELL_DATA.with(|cell| {
            cell.borrow().as_ref().is_some_and(|shell| {
                shell.state.find_active() && trimmed == shell.state.search_query()
            })
        });
        if same_settled {
            event.prevent_default();
            let delta: isize = if event.shift_key() { -1 } else { 1 };
            run_transition(|shell| {
                let viewport = shell.dom.viewport();
                let mut step = Step::new();
                shell.state.navigate_find(delta, &viewport, &mut step);
                shell.apply_step_ops(&step);
                TransitionOutput {
                    sends: std::mem::take(&mut step.sends),
                    save_state: step.save_state.take(),
                }
            });
        } else {
            run_transition(|shell| {
                let mut step = Step::new();
                shell.state.submit_find(&trimmed, &mut step);
                shell.apply_step_ops(&step);
                TransitionOutput {
                    sends: std::mem::take(&mut step.sends),
                    save_state: step.save_state.take(),
                }
            });
        }
        return;
    }
    if key == "Escape" {
        event.prevent_default();
        exit_search();
        return;
    }
    if key == "ArrowDown" || key == "ArrowUp" {
        let navigable = SHELL_DATA.with(|cell| {
            cell.borrow()
                .as_ref()
                .is_some_and(|shell| shell.state.find_navigation_enabled(&trimmed))
        });
        if navigable {
            event.prevent_default();
            let delta: isize = if key == "ArrowDown" { 1 } else { -1 };
            run_transition(|shell| {
                let viewport = shell.dom.viewport();
                let mut step = Step::new();
                shell.state.navigate_find(delta, &viewport, &mut step);
                shell.apply_step_ops(&step);
                TransitionOutput {
                    sends: std::mem::take(&mut step.sends),
                    save_state: step.save_state.take(),
                }
            });
        }
    }
}

/// `input` handler: an emptied input exits the find interaction;
/// edited-but-unsubmitted text disables the nav buttons (the guard lives
/// in the shell's `sync_find_nav`).
pub(super) fn on_search_input() {
    run_transition(|shell| {
        let mut step = Step::new();
        if shell.dom.search_input_value().is_empty() {
            shell.state.clear_find(&mut step);
        }
        shell.apply_step_ops(&step);
        TransitionOutput {
            sends: std::mem::take(&mut step.sends),
            save_state: step.save_state.take(),
        }
    });
}

/// Previous/Next find-nav click: the same wrapping `navigateFind` path the
/// arrows use, then focus returns to the input so editing stays immediate.
pub(super) fn on_search_nav(delta: isize) {
    run_transition(|shell| {
        let viewport = shell.dom.viewport();
        let mut step = Step::new();
        shell.state.navigate_find(delta, &viewport, &mut step);
        shell.apply_step_ops(&step);
        drop(shell.dom.search_input().focus());
        TransitionOutput {
            sends: std::mem::take(&mut step.sends),
            save_state: step.save_state.take(),
        }
    });
}

/// The absolute index of the closest `.row` to an event target, if any.
fn closest_row_abs(target: &web_sys::EventTarget) -> Option<i64> {
    let element: web_sys::Element = target.clone().dyn_into().ok()?;
    let row = element.closest(".row").ok().flatten()?;
    row.get_attribute("data-row")?.trim().parse::<i64>().ok()
}

/// Whether the event target is inside `selector` (delegation guards).
fn target_inside(target: &web_sys::EventTarget, selector: &str) -> bool {
    target
        .clone()
        .dyn_into::<web_sys::Element>()
        .ok()
        .and_then(|element| element.closest(selector).ok().flatten())
        .is_some()
}

/// Chevron or ordinary-row click: select the row; expandable rows toggle
/// their disclosure (the detail guard lives in the click handler).
pub(super) fn on_row_click(event: &web_sys::Event) {
    let mouse: web_sys::MouseEvent = (*event).clone().unchecked_into();
    let Some(target) = event.target() else {
        return;
    };
    let Some(abs) = closest_row_abs(&target) else {
        return; // header / handles / spacer are never row targets
    };
    let chevron = target_inside(&target, ".subop-chevron");
    let in_button = target_inside(&target, "button");
    if chevron {
        mouse.prevent_default();
        mouse.stop_propagation();
    } else if in_button || mouse.detail() > 1 {
        return;
    }
    run_transition(|shell| {
        let viewport = shell.dom.viewport();
        let mut step = Step::new();
        shell.row_select_and_toggle(abs, &viewport, &mut step);
        shell.apply_step_ops(&step);
        TransitionOutput {
            sends: std::mem::take(&mut step.sends),
            save_state: step.save_state.take(),
        }
    });
}

/// Double-click on an ordinary row opens raw JSON. File rows already open
/// their native diff on the first click and must never replace it with the
/// normalized operation JSON on the second click.
pub(super) fn on_row_dblclick(event: &web_sys::Event) {
    let Some(target) = event.target() else {
        return;
    };
    if target_inside(&target, "button") || target_inside(&target, ".row-file") {
        return;
    }
    let Some(abs) = closest_row_abs(&target) else {
        return;
    };
    run_transition(|shell| {
        let mut step = Step::new();
        shell.state.select_row(abs);
        if let Err(error) = shell.dom.apply_selection(abs) {
            record_error(&format!(
                "selection apply failed: {}",
                js_value_text(&error)
            ));
        }
        shell.open_json_for_abs(abs, &mut step);
        shell.apply_step_ops(&step);
        TransitionOutput {
            sends: std::mem::take(&mut step.sends),
            save_state: step.save_state.take(),
        }
    });
}

/// `focusin`: any row receiving focus becomes the roving anchor.
pub(super) fn on_row_focusin(event: &web_sys::Event) {
    let Some(target) = event.target() else {
        return;
    };
    let Some(abs) = closest_row_abs(&target) else {
        return;
    };
    run_transition(|shell| {
        if abs != shell.state.roving_abs() {
            shell.state.set_roving_abs(abs);
            shell.apply_roving_tabindex();
        }
        TransitionOutput::default()
    });
}

/// Roving keyboard navigation over the rendered rows: ArrowUp/Down move
/// focus, Home/End jump to the window
/// edges, ArrowRight/ArrowLeft toggle expandable rows, Enter/Space
/// activate (disclosure for expandable rows, raw JSON for ordinary ones).
pub(super) fn on_row_keydown(event: &web_sys::KeyboardEvent) {
    let key = event.key();
    let Some(target) = event.target() else {
        return;
    };
    let Some(abs) = closest_row_abs(&target) else {
        return;
    };
    match key.as_str() {
        "ArrowDown" | "ArrowUp" | "Home" | "End" => {
            let direction = if key == "ArrowDown" {
                1
            } else if key == "ArrowUp" {
                -1
            } else {
                0
            };
            let home_end = key == "Home" || key == "End";
            run_transition(|shell| {
                let Some(wrap) = shell.dom.wrap() else {
                    return TransitionOutput::default();
                };
                let Ok(list) = wrap.query_selector_all(".row") else {
                    return TransitionOutput::default();
                };
                let length = list.length();
                if length == 0 {
                    return TransitionOutput::default();
                }
                let rows_capacity = usize::try_from(length).unwrap_or(usize::MAX);
                let mut rows = Vec::with_capacity(rows_capacity);
                for index in 0..length {
                    let item = list.item(index);
                    let Some(node) = item else {
                        continue;
                    };
                    let Some(element) = node.dyn_into::<web_sys::Element>().ok() else {
                        continue;
                    };
                    let Some(row_abs) = element
                        .get_attribute("data-row")
                        .and_then(|raw| raw.trim().parse::<i64>().ok())
                    else {
                        continue;
                    };
                    rows.push((row_abs, element));
                }
                if rows.is_empty() {
                    return TransitionOutput::default();
                }
                let current = rows.iter().position(|(row_abs, _)| *row_abs == abs);
                let len = rows.len();
                let next = if home_end {
                    if key == "Home" {
                        0
                    } else {
                        len.saturating_sub(1)
                    }
                } else {
                    let current = current.unwrap_or(0);
                    let moved = if direction > 0 {
                        current.checked_add(1)
                    } else {
                        current.checked_sub(1)
                    };
                    if let Some(moved) = moved.filter(|index| *index < len) {
                        moved
                    } else {
                        return TransitionOutput::default();
                    }
                };
                let Some((target_abs, element)) = rows.get(next) else {
                    return TransitionOutput::default();
                };
                event.prevent_default();
                let Some(target) = element.dyn_ref::<web_sys::HtmlElement>() else {
                    return TransitionOutput::default();
                };
                shell.state.set_roving_abs(*target_abs);
                shell.apply_roving_tabindex();
                drop(target.focus());
                TransitionOutput::default()
            });
        }
        "ArrowRight" | "ArrowLeft" => {
            let expanded = SHELL_DATA.with(|cell| {
                cell.borrow()
                    .as_ref()
                    .is_some_and(|shell| shell.state.is_row_expanded(abs))
            });
            let wants_toggle = SHELL_DATA.with(|cell| {
                cell.borrow().as_ref().is_some_and(|shell| {
                    let row = shell.state.cache.get_by_index(abs);
                    let Some(row) = row else {
                        return false;
                    };
                    if !rows::has_sub_ops(row) {
                        return false;
                    }
                    (key == "ArrowRight" && !expanded) || (key == "ArrowLeft" && expanded)
                })
            });
            if wants_toggle {
                event.prevent_default();
                toggle_row_disclosure(abs);
            }
        }
        "Enter" | " " => {
            if target_inside(&target, "button") {
                return; // native button activation handles it
            }
            let expandable = SHELL_DATA.with(|cell| {
                cell.borrow().as_ref().is_some_and(|shell| {
                    shell
                        .state
                        .cache
                        .get_by_index(abs)
                        .is_some_and(rows::has_sub_ops)
                })
            });
            if expandable {
                event.prevent_default();
                toggle_row_disclosure(abs);
                return;
            }
            event.prevent_default();
            run_transition(|shell| {
                let mut step = Step::new();
                shell.state.select_row(abs);
                if let Err(error) = shell.dom.apply_selection(abs) {
                    record_error(&format!(
                        "selection apply failed: {}",
                        js_value_text(&error)
                    ));
                }
                if key == "Enter" && !shell.open_diff_for_abs(abs, &mut step) {
                    shell.open_json_for_abs(abs, &mut step);
                }
                shell.apply_step_ops(&step);
                TransitionOutput {
                    sends: std::mem::take(&mut step.sends),
                    save_state: step.save_state.take(),
                }
            });
        }
        _ => {}
    }
}

/// `toggleDisclosureKeyboard` — toggle a row's reveal state, rebuild the
/// desired window, and re-focus the fresh parent row so keyboard focus
/// survives the DOM replacement.
fn toggle_row_disclosure(abs: i64) {
    run_transition(|shell| {
        let viewport = shell.dom.viewport();
        let mut step = Step::new();
        shell.state.toggle_expanded_ui(abs, &viewport, &mut step);
        shell.apply_step_ops(&step);
        let selector = format!(".row[data-row=\"{abs}\"]");
        if let Ok(Some(fresh)) = shell.dom.rows().query_selector(&selector) {
            if let Ok(fresh) = fresh.dyn_into::<web_sys::HtmlElement>() {
                drop(fresh.focus());
            }
        }
        TransitionOutput {
            sends: std::mem::take(&mut step.sends),
            save_state: step.save_state.take(),
        }
    });
}

/// `mousedown` on a `.col-resize-handle` starts the column drag.
pub(super) fn on_rows_mousedown(event: &web_sys::Event) {
    let mouse: web_sys::MouseEvent = (*event).clone().unchecked_into();
    let Some(target) = event.target() else {
        return;
    };
    let Some(element) = target.clone().dyn_into::<web_sys::Element>().ok() else {
        return;
    };
    let Some(handle) = element.closest(".col-resize-handle").ok().flatten() else {
        return;
    };
    let Some(col) = handle
        .get_attribute("data-col")
        .and_then(|raw| ColKey::parse(&raw))
    else {
        return;
    };
    mouse.prevent_default();
    let started = start_column_drag(col, f64::from(mouse.client_x()));
    if let Err(error) = started {
        record_error(&format!(
            "column drag failed to start: {}",
            js_value_text(&error)
        ));
    }
}

/// The current effective width of a resizable column (drag start).
fn column_start_width(shell: &ShellData, col: ColKey) -> f64 {
    match col {
        ColKey::Graph => shell.current_graph_width(),
        ColKey::Activity
        | ColKey::Tags
        | ColKey::Content
        | ColKey::Date
        | ColKey::Author
        | ColKey::Commit => shell.dom.header_cell_width(col).max(col.min_width()),
    }
}

/// Begin a divider drag: pin the start geometry, mark the body, and
/// install window-level move/up listeners (removed on mouseup).
fn start_column_drag(col: ColKey, client_x: f64) -> Result<(), JsValue> {
    let start_w = SHELL_DATA.with(|cell| {
        cell.borrow()
            .as_ref()
            .map_or(0.0, |shell| column_start_width(shell, col))
    });
    let Some(window) = web_sys::window() else {
        return Ok(());
    };
    let target: web_sys::EventTarget = window.into();
    let drag = ColumnDrag {
        col,
        start_x: client_x,
        start_w,
        _move_listener: EventListener::new(target.clone(), "mousemove", |event| {
            on_column_move(&event);
        })?,
        _up_listener: EventListener::new(target, "mouseup", |event| {
            on_column_up(&event);
        })?,
    };
    let body = web_sys::window()
        .and_then(|window| window.document())
        .and_then(|document| document.body());
    if let Some(body) = body {
        drop(body.class_list().add_1("col-resizing"));
    }
    COLUMN_DRAG.with(|cell| {
        *cell.borrow_mut() = Some(drag);
    });
    Ok(())
}

/// Drag move: update the dragged column's width and re-render the current
/// window so the grid tracks the mouse.
fn on_column_move(event: &web_sys::Event) {
    let mouse: web_sys::MouseEvent = (*event).clone().unchecked_into();
    let Some((col, start_x, start_w)) = COLUMN_DRAG.with(|cell| {
        cell.borrow()
            .as_ref()
            .map(|drag| (drag.col, drag.start_x, drag.start_w))
    }) else {
        return;
    };
    let next = (start_w + f64::from(mouse.client_x()) - start_x).max(col.min_width());
    run_transition(|shell| {
        shell.col_widths.set(col, Some(next));
        let viewport = shell.dom.viewport();
        let mut step = Step::new();
        let (top, bottom) = (shell.state.render_top, shell.state.render_bottom);
        step.ops.push(DomOp::Reanchor { top, bottom });
        shell.state.sync_window(&viewport, &mut step);
        shell.state.fetch_window(&viewport, &mut step);
        shell.apply_step_ops(&step);
        TransitionOutput {
            sends: std::mem::take(&mut step.sends),
            save_state: step.save_state.take(),
        }
    });
}

/// Drag up: remove the window listeners and the resizing body class.
fn on_column_up(_event: &web_sys::Event) {
    let drag = COLUMN_DRAG.with(|cell| cell.borrow_mut().take());
    let Some(drag) = drag else {
        return;
    };
    drop(drag);
    let body = web_sys::window()
        .and_then(|window| window.document())
        .and_then(|document| document.body());
    if let Some(body) = body {
        drop(body.class_list().remove_1("col-resizing"));
    }
}

/// `ResizeObserver` / window-resize entry: ignore height-only changes, then
/// debounce the full layout re-render (`onViewportResize`, 150ms).
pub(super) fn on_resize_observed() {
    let changed = SHELL_DATA.with(|cell| {
        let mut borrow = cell.borrow_mut();
        let Some(shell) = borrow.as_mut() else {
            return false;
        };
        let width = f64::from(shell.dom.rows().client_width());
        if (width - shell.last_rows_width).abs() < 0.5 {
            return false;
        }
        shell.last_rows_width = width;
        shell.resize_timer = None;
        true
    });
    if !changed {
        return;
    }
    match Timer::timeout(150, on_resize_debounced) {
        Ok(timer) => {
            SHELL_DATA.with(|cell| {
                if let Some(shell) = cell.borrow_mut().as_mut() {
                    shell.resize_timer = Some(timer);
                }
            });
        }
        Err(error) => {
            let message = format!(
                "resize debounce failed to schedule: {}",
                js_value_text(&error)
            );
            record_error(&message);
        }
    }
}

/// The debounced viewport-resize rebuild (`onViewportResize`): re-render
/// the current window at the new width, then re-sync/fetch as needed.
fn on_resize_debounced() {
    SHELL_DATA.with(|cell| {
        if let Some(shell) = cell.borrow_mut().as_mut() {
            shell.resize_timer = None;
        }
    });
    run_transition(|shell| {
        let viewport = shell.dom.viewport();
        let mut step = Step::new();
        step.ops.push(DomOp::Reanchor {
            top: shell.state.render_top,
            bottom: shell.state.render_bottom,
        });
        shell.state.sync_window(&viewport, &mut step);
        shell.state.fetch_window(&viewport, &mut step);
        shell.apply_step_ops(&step);
        TransitionOutput {
            sends: std::mem::take(&mut step.sends),
            save_state: step.save_state.take(),
        }
    });
}
