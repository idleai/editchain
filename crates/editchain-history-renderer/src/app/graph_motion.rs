//! Keyed SVG segments retain ongoing animations while new connections grow.

use super::graph_growth::{self, Point, Segment};
use std::collections::{HashMap, HashSet};
use wasm_bindgen::{JsCast as _, JsValue};
use web_sys::{Element, HtmlDivElement, HtmlElement};

pub(super) type Capture = HashMap<String, HashSet<String>>;

fn elements(root: &Element, selector: &str) -> Result<Vec<Element>, JsValue> {
    let list = root.query_selector_all(selector)?;
    Ok((0..list.length())
        .filter_map(|index| list.item(index)?.dyn_into().ok())
        .collect())
}

pub(super) fn capture(root: &HtmlDivElement) -> Result<Capture, JsValue> {
    let mut captured = Capture::new();
    for row in elements(root, ".row[data-continuity]")? {
        let key = row.get_attribute("data-continuity").unwrap_or_default();
        let keys = elements(&row, "[data-graph-key]")?
            .into_iter()
            .filter_map(|part| part.get_attribute("data-graph-key"))
            .collect();
        drop(captured.insert(key, keys));
    }
    Ok(captured)
}

/// Update colors and frame bounds, retaining identical paths and their clocks.
pub(super) fn patch_cell(cell: &Element, replacement: &Element) -> Result<(), JsValue> {
    let Some(svg) = cell.first_element_child() else {
        drop(cell.append_child(replacement)?);
        return Ok(());
    };
    for name in ["width", "height", "viewBox"] {
        if let Some(value) = replacement.get_attribute(name) {
            svg.set_attribute(name, &value)?;
        }
    }
    let mut existing: HashMap<_, _> = elements(&svg, "[data-graph-key]")?
        .into_iter()
        .filter_map(|part| Some((part.get_attribute("data-graph-key")?, part)))
        .collect();
    let mut cursor = svg.first_child();
    for fresh in elements(replacement, "[data-graph-key]")? {
        let key = fresh.get_attribute("data-graph-key").unwrap_or_default();
        let retained = existing.remove(&key);
        let part = if let Some(retained) = retained {
            let motion = retained
                .get_attribute("data-graph-motion")
                .unwrap_or_default();
            if let Some(style) = fresh.get_attribute("style") {
                retained.set_attribute("style", &format!("{style};{motion}"))?;
            }
            if let Some(fill) = fresh.get_attribute("fill") {
                retained.set_attribute("fill", &fill)?;
            }
            retained
        } else {
            fresh
        };
        if cursor
            .as_ref()
            .is_some_and(|cursor| part.is_same_node(Some(cursor)))
        {
            cursor = part.next_sibling();
        } else {
            drop(svg.insert_before(&part, cursor.as_ref())?);
        }
    }
    for retired in existing.into_values() {
        retired.remove();
    }
    Ok(())
}

/// Rebuild changed text around the mounted graph; detaching it cancels CSS clocks.
pub(super) fn patch_row(row: &Element, replacement: &Element) -> Result<(), JsValue> {
    let old_cell = row.query_selector(".graph-cell")?;
    let new_cell = replacement.query_selector(".graph-cell")?;
    let retained = match (old_cell, new_cell) {
        (Some(cell), Some(fresh)) => {
            if let Some(svg) = fresh.first_element_child() {
                patch_cell(&cell, &svg)?;
            }
            Some(cell)
        }
        _ => None,
    };
    for child in elements(row, ":scope > *")? {
        if !retained
            .as_ref()
            .is_some_and(|cell| cell.is_same_node(Some(&child)))
        {
            child.remove();
        }
    }
    let mut cursor = retained.as_ref();
    for child in elements(replacement, ":scope > *")? {
        if retained.is_some() && child.class_list().contains("graph-cell") {
            cursor = None;
        } else {
            drop(row.insert_before(&child, cursor.map(AsRef::as_ref))?);
        }
    }
    Ok(())
}

fn points(element: &Element, top: i64) -> Vec<Point> {
    let coordinates: Vec<_> = element
        .get_attribute("data-graph-points")
        .unwrap_or_default()
        .split_whitespace()
        .filter_map(|value| value.parse::<f64>().ok())
        .collect();
    coordinates
        .chunks_exact(2)
        .filter_map(|pair| {
            Some(Point {
                x: super::f64_round_to_i64(pair.first()? * 100.0),
                y: top
                    .saturating_mul(100)
                    .saturating_add(super::f64_round_to_i64(pair.get(1)? * 100.0)),
            })
        })
        .collect()
}

pub(super) fn animate(root: &HtmlDivElement, before: &Capture) -> Result<(), JsValue> {
    let viewport = root.get_bounding_client_rect();
    let mut edges = Vec::new();
    let mut nodes = Vec::new();
    // Collect all geometry before setting animation styles.
    for row in elements(root, ".row[data-continuity]")? {
        let bounds = row.get_bounding_client_rect();
        if bounds.bottom() < viewport.top() - 34.0 || bounds.top() > viewport.bottom() + 34.0 {
            continue;
        }
        let row_key = row.get_attribute("data-continuity").unwrap_or_default();
        let top = row
            .dyn_ref::<HtmlElement>()
            .map_or(0, |row| i64::from(row.offset_top()));
        for part in elements(&row, "[data-graph-key]")? {
            let key = part.get_attribute("data-graph-key").unwrap_or_default();
            if before.get(&row_key).is_some_and(|keys| keys.contains(&key)) {
                continue;
            }
            let positions = points(&part, top);
            if let Some(start) = positions.first().copied() {
                if let Some(end) = positions.get(1).copied() {
                    edges.push((part, Segment { start, end }));
                } else {
                    nodes.push((part, start));
                }
            }
        }
    }
    let growth = graph_growth::plan(
        &edges
            .iter()
            .map(|(_, segment)| *segment)
            .collect::<Vec<_>>(),
    );
    for ((element, _), timing) in edges.iter().zip(&growth.segments) {
        start_motion(element, "graph-live-edge", timing.delay, timing.duration)?;
    }
    for (element, point) in nodes {
        start_motion(
            &element,
            "graph-live-node",
            growth.arrivals.get(&point).copied().unwrap_or(0.0),
            180.0,
        )?;
    }
    Ok(())
}

fn start_motion(element: &Element, class: &str, delay: f64, duration: f64) -> Result<(), JsValue> {
    // The existing rows first settle into their new slots, then the connection
    // grows from the parent. Later deltas retain these SVG nodes and clocks.
    let motion = format!(
        "--graph-delay:{}ms;--graph-duration:{}ms",
        super::round2(360.0 + delay),
        super::round2(duration)
    );
    let style = element.get_attribute("style").unwrap_or_default();
    element.set_attribute("style", &format!("{style};{motion}"))?;
    element.set_attribute("data-graph-motion", &motion)?;
    element.class_list().add_1(class)
}
