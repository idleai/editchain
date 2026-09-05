//! Rust/WebAssembly history graph renderer for the experimental VS Code
//! history view.
//!
//! The wasm32 build owns the Rust browser shell ([`shell`] module and
//! `app::dom`): it acquires the VS Code API through a narrow wasm-bindgen
//! binding exposed by VS Code (or the deterministic fixture bridge), installs
//! the host-message listener before posting `webviewReady`, owns
//! `HistoryAppState`, and renders real row DOM nodes whose `.graph-cell` each
//! carries its own SVG graph fragment (production `buildGraphCell`). The
//! graph scrolls inside the row DOM, so no fixed-viewport overlay exists to
//! reconcile. The production `media/main.js` controller is NOT loaded on the
//! Rust-owned path.
//!
//! The obsolete `GpuRenderer`/wgpu surface (target-independent geometry in
//! this module, `browser.rs` on wasm32) remains compiled for its native
//! geometry tests; the shell no longer instantiates it.

// Pure, target-independent HistoryApp application core (Stage 1+): the host
// protocol and the view state machine ported from the production
// media/main.js controller. Native unit tests exercise every decision path;
// the wasm32 build includes the module for the Rust-owned browser shell
// (`crate::shell` + `app::dom` is the Slice 3A runtime consumer).
#[cfg(any(target_arch = "wasm32", test))]
mod app;

#[cfg(any(target_arch = "wasm32", test))]
use serde::Deserialize;

#[cfg(any(target_arch = "wasm32", test))]
const FLOATS_PER_VERTEX: usize = 6;

/// Production lane palette (`COLORS` in
/// `extensions/vscode-editchain/media/main.js`), stored as sRGB bytes and
/// converted to linear space by [`lane_color`] so an sRGB swapchain displays
/// the exact production hexes.
#[cfg(any(target_arch = "wasm32", test))]
const PALETTE_HEX: [[u8; 3]; 10] = [
    [0x48, 0xf1, 0xdc],
    [0xa1, 0x8a, 0xff],
    [0x6e, 0xe7, 0xa2],
    [0x5c, 0xa8, 0xff],
    [0xff, 0xc8, 0x6a],
    [0xff, 0x70, 0xa6],
    [0x72, 0xdd, 0xf7],
    [0xc7, 0x7d, 0xff],
    [0x64, 0xdf, 0xdf],
    [0xff, 0x8f, 0xa3],
];

/// Lane count of [`PALETTE_HEX`], as `u32` so lane colors wrap with a modulo
/// that never needs a cast.
#[cfg(any(target_arch = "wasm32", test))]
const PALETTE_LEN: u32 = 10;

/// Bundle terminal radius is `dot_radius * BUNDLE_TERMINAL_RATIO`, never
/// smaller than [`BUNDLE_TERMINAL_MIN`] — the same floor the DOM renderer
/// applies in `bundleTerminalRadius`.
#[cfg(any(target_arch = "wasm32", test))]
const BUNDLE_TERMINAL_RATIO: f32 = 0.75;

#[cfg(any(target_arch = "wasm32", test))]
const BUNDLE_TERMINAL_MIN: f32 = 1.5;

/// The DOM renderer outlines dots, capsules, and terminals with a 1 CSS-px
/// editor-background hairline; a ring of this half-width around every glyph
/// edge reproduces it. Only drawn when the frame supplies a background color.
#[cfg(any(target_arch = "wasm32", test))]
const RING_HALF_WIDTH: f32 = 0.5;

/// Circle tessellation: 16 segments keep the sagitta for the largest glyph
/// (≈5px radius) well under 0.1 CSS px.
#[cfg(any(target_arch = "wasm32", test))]
const CIRCLE_SEGMENTS: u8 = 16;

/// Upper bound for quadratic-Bézier tessellation so pathological edges stay
/// cheap; [`curve_segments`] chooses `ceil(sqrt(|D|))` segments, which bounds
/// the geometric error to [`CURVE_ERROR_CSS_PX`].
#[cfg(any(target_arch = "wasm32", test))]
const MAX_CURVE_SEGMENTS: u8 = 32;

/// Maximum geometric error of a tessellated quadratic edge, in CSS pixels.
#[cfg(any(target_arch = "wasm32", test))]
const CURVE_ERROR_CSS_PX: f32 = 0.25;

/// An RGBA color in 0..=1 float components. Deserialization accepts three or
/// four components (a missing alpha defaults to fully opaque) or the CSS hex
/// string the browser bridge sends for the editor background (`#rgb`,
/// `#rgba`, `#rrggbb`, `#rrggbbaa`).
#[cfg(any(target_arch = "wasm32", test))]
#[derive(Debug, Clone, Copy)]
struct Rgba([f32; 4]);

#[cfg(any(target_arch = "wasm32", test))]
impl Rgba {
    /// Convert CSS/sRGB components to the linear RGB values expected by an
    /// sRGB swapchain. Alpha is a coverage value and remains unchanged.
    fn linear(self) -> [f32; 4] {
        let [red, green, blue, alpha] = self.0;
        [
            srgb_unit_to_linear(red),
            srgb_unit_to_linear(green),
            srgb_unit_to_linear(blue),
            alpha,
        ]
    }
}

/// One hex digit as an 0..=15 float, or `None` for a non-hex character.
#[cfg(any(target_arch = "wasm32", test))]
fn hex_nibble(digit: char) -> Option<f32> {
    digit
        .to_digit(16)
        .and_then(|nibble| u8::try_from(nibble).ok())
        .map(f32::from)
}

/// The `#rgb`/`#rgba` channel form where each digit is doubled (`#abc` means
/// `#aabbcc`), as a 0..=1 float.
#[cfg(any(target_arch = "wasm32", test))]
fn doubled_hex_channel(digit: char) -> Option<f32> {
    hex_nibble(digit).map(|value| value * 17.0 / 255.0)
}

/// The `#rrggbb`/`#rrggbbaa` channel form, as a 0..=1 float.
#[cfg(any(target_arch = "wasm32", test))]
fn paired_hex_channel(high: char, low: char) -> Option<f32> {
    let high = hex_nibble(high)?;
    let low = hex_nibble(low)?;
    Some((high * 16.0 + low) / 255.0)
}

/// Parse a CSS hex color string into 0..=1 float components, or `None` for
/// any other syntax.
#[cfg(any(target_arch = "wasm32", test))]
fn parse_css_hex_color(value: &str) -> Option<Rgba> {
    let body = value.strip_prefix('#')?;
    let digits: Vec<char> = body.chars().collect();
    let components: [f32; 4] = match digits.as_slice() {
        [red, green, blue] => [
            doubled_hex_channel(*red)?,
            doubled_hex_channel(*green)?,
            doubled_hex_channel(*blue)?,
            1.0,
        ],
        [red, green, blue, alpha] => [
            doubled_hex_channel(*red)?,
            doubled_hex_channel(*green)?,
            doubled_hex_channel(*blue)?,
            doubled_hex_channel(*alpha)?,
        ],
        [red_high, red_low, green_high, green_low, blue_high, blue_low] => [
            paired_hex_channel(*red_high, *red_low)?,
            paired_hex_channel(*green_high, *green_low)?,
            paired_hex_channel(*blue_high, *blue_low)?,
            1.0,
        ],
        [red_high, red_low, green_high, green_low, blue_high, blue_low, alpha_high, alpha_low] => [
            paired_hex_channel(*red_high, *red_low)?,
            paired_hex_channel(*green_high, *green_low)?,
            paired_hex_channel(*blue_high, *blue_low)?,
            paired_hex_channel(*alpha_high, *alpha_low)?,
        ],
        _ => return None,
    };
    Some(Rgba(components))
}

#[cfg(any(target_arch = "wasm32", test))]
impl<'de> Deserialize<'de> for Rgba {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        struct RgbaVisitor;

        impl<'de> serde::de::Visitor<'de> for RgbaVisitor {
            type Value = Rgba;

            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str(
                    "a color array of three or four 0..=1 floats or a CSS hex color string",
                )
            }

            fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                parse_css_hex_color(value).ok_or_else(|| {
                    serde::de::Error::invalid_value(serde::de::Unexpected::Str(value), &self)
                })
            }

            fn visit_seq<A>(self, mut seq: A) -> Result<Self::Value, A::Error>
            where
                A: serde::de::SeqAccess<'de>,
            {
                let red = seq
                    .next_element::<f32>()?
                    .ok_or_else(|| serde::de::Error::invalid_length(0, &self))?;
                let green = seq
                    .next_element::<f32>()?
                    .ok_or_else(|| serde::de::Error::invalid_length(1, &self))?;
                let blue = seq
                    .next_element::<f32>()?
                    .ok_or_else(|| serde::de::Error::invalid_length(2, &self))?;
                let alpha = seq.next_element::<f32>()?.unwrap_or(1.0);
                Ok(Rgba([red, green, blue, alpha]))
            }
        }

        deserializer.deserialize_any(RgbaVisitor)
    }
}

/// Layout shared by every row in one frame (CSS pixels).
#[cfg(any(target_arch = "wasm32", test))]
#[derive(Debug, Clone, Deserialize)]
struct FrameGraph {
    #[serde(default)]
    left: f32,
    #[serde(default)]
    width: f32,
    #[serde(default)]
    lane_x: Vec<f32>,
    #[serde(default)]
    dot_radius: f32,
    #[serde(default)]
    line_width: f32,
    #[serde(default)]
    bundle_half_height: f32,
    #[serde(default)]
    bundle_margin: f32,
    #[serde(default)]
    background_color: Option<Rgba>,
}

/// One history row's graph geometry (CSS pixels).
#[cfg(any(target_arch = "wasm32", test))]
#[derive(Debug, Clone, Deserialize)]
struct RenderRow {
    #[serde(default)]
    top: f32,
    #[serde(default)]
    bottom: f32,
    #[serde(default)]
    middle: Option<f32>,
    #[serde(default)]
    lane: u32,
    #[serde(default)]
    above: Vec<u32>,
    #[serde(default)]
    below: Vec<u32>,
    #[serde(default)]
    transitions: Vec<[u32; 2]>,
    #[serde(default)]
    is_subop: bool,
    #[serde(default)]
    is_bundle: bool,
}

#[cfg(any(target_arch = "wasm32", test))]
impl RenderRow {
    fn middle_y(&self) -> f32 {
        self.middle.unwrap_or((self.top + self.bottom) * 0.5)
    }
}

/// One complete render frame.
#[cfg(any(target_arch = "wasm32", test))]
#[derive(Debug, Clone, Deserialize)]
struct RenderFrame {
    graph: FrameGraph,
    #[serde(default)]
    rows: Vec<RenderRow>,
}

/// Vertical geometry of the Activity-bundle glyph on one row.
#[cfg(any(target_arch = "wasm32", test))]
#[derive(Clone, Copy)]
struct BundleAnchors {
    entry_y: f32,
    exit_y: f32,
    term_radius: f32,
    margin: f32,
}

#[cfg(any(target_arch = "wasm32", test))]
fn bundle_terminal_radius(dot_radius: f32) -> f32 {
    (dot_radius * BUNDLE_TERMINAL_RATIO).max(BUNDLE_TERMINAL_MIN)
}

/// Capsule placement for one bundle row (CSS pixels).
#[cfg(any(target_arch = "wasm32", test))]
#[derive(Clone, Copy)]
struct CapsuleShape {
    x: f32,
    entry_y: f32,
    exit_y: f32,
    margin: f32,
}

#[cfg(any(target_arch = "wasm32", test))]
impl CapsuleShape {
    fn stadium_top_y(self) -> f32 {
        self.entry_y + self.margin
    }

    fn stadium_bottom_y(self) -> f32 {
        self.exit_y - self.margin
    }
}

/// One transition whose endpoints passed the production anchor rules.
#[cfg(any(target_arch = "wasm32", test))]
#[derive(Clone, Copy)]
struct RenderedTransition {
    from_lane: u32,
    to_lane: u32,
    start_at_dot: bool,
    end_at_dot: bool,
}

/// Convert one sRGB byte to the linear component a shader must output for an
/// sRGB swapchain to display the original byte value.
#[cfg(any(target_arch = "wasm32", test))]
fn srgb_to_linear(component: u8) -> f32 {
    srgb_unit_to_linear(f32::from(component) / 255.0)
}

/// Convert a normalized sRGB component into linear light for the swapchain.
#[cfg(any(target_arch = "wasm32", test))]
fn srgb_unit_to_linear(component: f32) -> f32 {
    let c = component.clamp(0.0, 1.0);
    if c <= 0.040_45 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

/// Linear-space RGBA for a lane using the production palette (alpha 1).
#[cfg(any(target_arch = "wasm32", test))]
fn lane_color(lane: u32) -> [f32; 4] {
    let index = usize::try_from(lane % PALETTE_LEN).unwrap_or(0);
    let [red, green, blue] = PALETTE_HEX.get(index).copied().unwrap_or([0, 0, 0]);
    [
        srgb_to_linear(red),
        srgb_to_linear(green),
        srgb_to_linear(blue),
        1.0,
    ]
}

/// CSS-pixel x center of a lane from the supplied layout, clamped into the
/// graph column. Unknown lanes fall back to the last supplied center, then the
/// column middle when the layout is empty.
#[cfg(any(target_arch = "wasm32", test))]
fn lane_center(graph: &FrameGraph, lane: u32) -> f32 {
    let left = graph.left;
    let right = left + graph.width;
    let index = usize::try_from(lane).unwrap_or(usize::MAX);
    let fallback = graph
        .lane_x
        .last()
        .copied()
        .unwrap_or(left + graph.width * 0.5);
    let center = graph.lane_x.get(index).copied().unwrap_or(fallback);
    center.clamp(left, right)
}

/// Resolve the production anchor/drop rules: a transition is rendered only
/// when its start is the row's own node or backed by an `above` lane and its
/// end is the row's own node or backed by a `below` lane (main.js
/// `buildGraphCell`).
#[cfg(any(target_arch = "wasm32", test))]
fn resolved_transitions(row: &RenderRow) -> Vec<RenderedTransition> {
    let mut rendered = Vec::new();
    for transition in &row.transitions {
        let [from_lane, to_lane] = *transition;
        let start_at_dot = row.lane == from_lane;
        let end_at_dot = row.lane == to_lane;
        let end_at_boundary = !end_at_dot && row.below.contains(&to_lane);
        let start_connected = start_at_dot || row.above.contains(&from_lane);
        if !start_connected || (!end_at_boundary && !end_at_dot) {
            continue;
        }
        rendered.push(RenderedTransition {
            from_lane,
            to_lane,
            start_at_dot,
            end_at_dot,
        });
    }
    rendered
}

/// Build the two quadratic-halves control points and the shared seam for one
/// transition, mirroring production `buildTransitionPaths` exactly:
/// a node-to-boundary edge is one convex quadratic split at t=0.5; a
/// boundary-to-boundary edge uses two convex halves sharing one horizontal
/// tangent; the defensive seam for the impossible both-dot case stays one
/// smooth quadratic.
#[cfg(any(target_arch = "wasm32", test))]
fn transition_controls(
    start: [f32; 2],
    end: [f32; 2],
    transition: RenderedTransition,
) -> ([f32; 2], [f32; 2], [f32; 2]) {
    let [start_x, start_y] = start;
    let [end_x, end_y] = end;
    if transition.start_at_dot != transition.end_at_dot {
        let control = if transition.start_at_dot {
            [end_x, start_y]
        } else {
            [start_x, end_y]
        };
        let src_control = midpoint(start, control);
        let dst_control = midpoint(control, end);
        return (src_control, dst_control, midpoint(src_control, dst_control));
    }
    if !transition.start_at_dot {
        let seam = midpoint(start, end);
        let [_, seam_y] = seam;
        return ([start_x, seam_y], [end_x, seam_y], seam);
    }
    let control = midpoint(start, end);
    let src_control = midpoint(start, control);
    let dst_control = midpoint(control, end);
    (src_control, dst_control, midpoint(src_control, dst_control))
}

#[cfg(any(target_arch = "wasm32", test))]
fn midpoint(a: [f32; 2], b: [f32; 2]) -> [f32; 2] {
    let [ax, ay] = a;
    let [bx, by] = b;
    [(ax + bx) * 0.5, (ay + by) * 0.5]
}

/// Segment count for a quadratic Bézier with constant second difference
/// `D = start - 2·control + end`: the deviation of the curve from any chord of
/// a uniform `k`-segment polyline is at most `|D| / (4·k²)`, so
/// `ceil(sqrt(|D| / (4·CURVE_ERROR_CSS_PX)))` segments bound the geometric
/// error to [`CURVE_ERROR_CSS_PX`] CSS pixels, capped at
/// [`MAX_CURVE_SEGMENTS`].
///
/// The smallest k with `k² ≥ budget` is found by integer search over the u8
/// range because Rust std offers no lossless float→integer conversion without
/// a cast (casts are denied crate-wide).
#[cfg(any(target_arch = "wasm32", test))]
fn curve_segments(start: [f32; 2], control: [f32; 2], end: [f32; 2]) -> u8 {
    let [start_x, start_y] = start;
    let [control_x, control_y] = control;
    let [end_x, end_y] = end;
    let d_x = start_x - 2.0 * control_x + end_x;
    let d_y = start_y - 2.0 * control_y + end_y;
    let magnitude = d_x.hypot(d_y);
    let budget = magnitude / (4.0 * CURVE_ERROR_CSS_PX);
    (2..=MAX_CURVE_SEGMENTS)
        .find(|segments| f32::from(*segments) * f32::from(*segments) >= budget)
        .unwrap_or(MAX_CURVE_SEGMENTS)
}

#[cfg(any(target_arch = "wasm32", test))]
fn quadratic_point(start: [f32; 2], control: [f32; 2], end: [f32; 2], t: f32) -> [f32; 2] {
    let [start_x, start_y] = start;
    let [control_x, control_y] = control;
    let [end_x, end_y] = end;
    let u = 1.0 - t;
    [
        u * u * start_x + 2.0 * u * t * control_x + t * t * end_x,
        u * u * start_y + 2.0 * u * t * control_y + t * t * end_y,
    ]
}

/// Vertex emitter that owns the CSS→device scale and the default line width
/// — no fixed lane origin/gap/width survives in geometry code.
#[cfg(any(target_arch = "wasm32", test))]
struct GeometryBuilder {
    vertices: Vec<f32>,
    scale: f32,
    line_width: f32,
}

#[cfg(any(target_arch = "wasm32", test))]
impl GeometryBuilder {
    fn new(scale: f32, line_width: f32) -> Self {
        Self {
            vertices: Vec::new(),
            scale,
            line_width,
        }
    }

    fn push_vertex(&mut self, x: f32, y: f32, color: [f32; 4]) {
        let [red, green, blue, alpha] = color;
        let scale = self.scale;
        self.vertices
            .extend_from_slice(&[x * scale, y * scale, red, green, blue, alpha]);
    }

    fn push_triangle(&mut self, a: [f32; 2], b: [f32; 2], c: [f32; 2], color: [f32; 4]) {
        let [ax, ay] = a;
        let [bx, by] = b;
        let [cx, cy] = c;
        self.push_vertex(ax, ay, color);
        self.push_vertex(bx, by, color);
        self.push_vertex(cx, cy, color);
    }

    fn push_rect(&mut self, bounds: [f32; 4], color: [f32; 4]) {
        let [left, top, right, bottom] = bounds;
        self.push_triangle([left, top], [right, top], [left, bottom], color);
        self.push_triangle([right, top], [right, bottom], [left, bottom], color);
    }

    /// A width-stroked straight segment between two CSS-pixel points.
    fn line(&mut self, start: [f32; 2], end: [f32; 2], color: [f32; 4]) {
        let [start_x, start_y] = start;
        let [end_x, end_y] = end;
        let dx = end_x - start_x;
        let dy = end_y - start_y;
        let length = dx.hypot(dy);
        if length <= f32::EPSILON {
            return;
        }
        let half = self.line_width * 0.5;
        let normal_x = -dy / length * half;
        let normal_y = dx / length * half;
        let a = [start_x + normal_x, start_y + normal_y];
        let b = [start_x - normal_x, start_y - normal_y];
        let c = [end_x + normal_x, end_y + normal_y];
        let d = [end_x - normal_x, end_y - normal_y];
        self.push_triangle(a, b, c, color);
        self.push_triangle(c, b, d, color);
    }

    /// A filled circle with [`CIRCLE_SEGMENTS`] sides.
    fn circle(&mut self, center: [f32; 2], radius: f32, color: [f32; 4]) {
        let [cx, cy] = center;
        if radius <= f32::EPSILON {
            return;
        }
        let step = std::f32::consts::TAU / f32::from(CIRCLE_SEGMENTS);
        for segment in 0..CIRCLE_SEGMENTS {
            let angle = f32::from(segment) * step;
            let next_angle = angle + step;
            let a = [cx, cy];
            let b = [cx + radius * angle.cos(), cy + radius * angle.sin()];
            let c = [
                cx + radius * next_angle.cos(),
                cy + radius * next_angle.sin(),
            ];
            self.push_triangle(a, b, c, color);
        }
    }

    /// A filled stadium capsule. The DOM bundle glyph is a rounded rect of
    /// radius `term_radius + margin` spanning `entry_y ± term_radius` (main.js
    /// `BUNDLE_CAPSULE_MARGIN`), so a stadium with semicircle radius `radius`
    /// and centerline `entry_y + margin … exit_y - margin` reproduces it
    /// exactly; a radius one CSS px larger draws a hairline ring around it
    /// (see [`RING_HALF_WIDTH`] usage in `build_geometry`).
    fn capsule(&mut self, shape: CapsuleShape, radius: f32, color: [f32; 4]) {
        let top_y = shape.stadium_top_y();
        let bottom_y = shape.stadium_bottom_y();
        let left = shape.x - radius;
        let right = shape.x + radius;
        if bottom_y > top_y {
            self.push_rect([left, top_y, right, bottom_y], color);
        }
        let step = std::f32::consts::PI / f32::from(CIRCLE_SEGMENTS);
        // Top semicircle from angle π (left) to 2π (right).
        let mut previous = [left, top_y];
        for segment in 0..CIRCLE_SEGMENTS {
            let angle = std::f32::consts::PI + f32::from(segment) * step + step;
            let point = [shape.x + radius * angle.cos(), top_y + radius * angle.sin()];
            self.push_triangle([shape.x, top_y], previous, point, color);
            previous = point;
        }
        // Bottom semicircle from angle 0 (right) to π (left).
        let mut previous = [right, bottom_y];
        for segment in 0..CIRCLE_SEGMENTS {
            let angle = f32::from(segment) * step + step;
            let point = [
                shape.x + radius * angle.cos(),
                bottom_y + radius * angle.sin(),
            ];
            self.push_triangle([shape.x, bottom_y], previous, point, color);
            previous = point;
        }
    }

    /// One quadratic Bézier half stroked with the default line width, sampled
    /// finely enough to stay within [`CURVE_ERROR_CSS_PX`] of the true curve.
    fn quadratic(&mut self, start: [f32; 2], control: [f32; 2], end: [f32; 2], color: [f32; 4]) {
        let segments = curve_segments(start, control, end);
        let mut previous = start;
        for segment in 1..=segments {
            let t = f32::from(segment) / f32::from(segments);
            let point = quadratic_point(start, control, end, t);
            self.line(previous, point, color);
            previous = point;
        }
    }

    /// Node dot (or bundle terminal circle) with an optional editor-background
    /// hairline ring; no ring is invented when no background is supplied.
    fn dot(
        &mut self,
        center: [f32; 2],
        radius: f32,
        color: [f32; 4],
        background: Option<[f32; 4]>,
    ) {
        match background {
            Some(bg) => {
                self.circle(center, radius + RING_HALF_WIDTH, bg);
                self.circle(center, (radius - RING_HALF_WIDTH).max(0.0), color);
            }
            None => self.circle(center, radius, color),
        }
    }
}

/// Build all vertices for one frame in device pixels (CSS coordinates ×
/// `scale`), un-normalized.
#[cfg(any(target_arch = "wasm32", test))]
fn build_geometry(frame: &RenderFrame, scale: f32) -> Vec<f32> {
    let graph = &frame.graph;
    let mut builder = GeometryBuilder::new(scale, graph.line_width);

    for row in &frame.rows {
        let top = row.top;
        let bottom = row.bottom;
        let middle = row.middle_y();
        let node_lane = row.lane;
        let node_x = lane_center(graph, node_lane);
        let node_color = lane_color(node_lane);
        let background = graph.background_color.map(Rgba::linear);
        let bundle = row.is_bundle.then(|| BundleAnchors {
            entry_y: middle - graph.bundle_half_height,
            exit_y: middle + graph.bundle_half_height,
            term_radius: bundle_terminal_radius(graph.dot_radius),
            margin: graph.bundle_margin,
        });

        let rendered = resolved_transitions(row);
        let mut owns_top: Vec<u32> = Vec::new();
        let mut owns_bottom: Vec<u32> = Vec::new();
        for transition in &rendered {
            if !transition.start_at_dot && !owns_top.contains(&transition.from_lane) {
                owns_top.push(transition.from_lane);
            }
            if !transition.end_at_dot && !owns_bottom.contains(&transition.to_lane) {
                owns_bottom.push(transition.to_lane);
            }
        }

        for lane in &row.above {
            if owns_top.contains(lane) {
                continue;
            }
            let x = lane_center(graph, *lane);
            let end_y = match bundle {
                Some(anchors) if *lane == node_lane => anchors.entry_y,
                _ => middle,
            };
            builder.line([x, top], [x, end_y], lane_color(*lane));
        }
        for lane in &row.below {
            if owns_bottom.contains(lane) {
                continue;
            }
            let x = lane_center(graph, *lane);
            let start_y = match bundle {
                Some(anchors) if *lane == node_lane => anchors.exit_y,
                _ => middle,
            };
            builder.line([x, start_y], [x, bottom], lane_color(*lane));
        }

        for transition in &rendered {
            let x1 = lane_center(graph, transition.from_lane);
            let x2 = lane_center(graph, transition.to_lane);
            let start_y = if transition.start_at_dot {
                bundle.map_or(middle, |anchors| anchors.exit_y)
            } else {
                top
            };
            let end_y = if transition.end_at_dot {
                bundle.map_or(middle, |anchors| anchors.entry_y)
            } else {
                bottom
            };
            let start = [x1, start_y];
            let end = [x2, end_y];
            let (src_control, dst_control, seam) = transition_controls(start, end, *transition);
            builder.quadratic(start, src_control, seam, lane_color(transition.from_lane));
            builder.quadratic(seam, dst_control, end, lane_color(transition.to_lane));
        }

        if !row.is_subop {
            if let Some(anchors) = bundle {
                let shape = CapsuleShape {
                    x: node_x,
                    entry_y: anchors.entry_y,
                    exit_y: anchors.exit_y,
                    margin: anchors.margin,
                };
                let radius = anchors.term_radius + anchors.margin;
                match background {
                    Some(bg) => {
                        builder.capsule(shape, radius + RING_HALF_WIDTH, bg);
                        builder.capsule(shape, (radius - RING_HALF_WIDTH).max(0.0), node_color);
                    }
                    None => builder.capsule(shape, radius, node_color),
                }
                for terminal_y in [anchors.entry_y, anchors.exit_y] {
                    builder.dot(
                        [node_x, terminal_y],
                        anchors.term_radius,
                        node_color,
                        background,
                    );
                }
            } else {
                builder.dot([node_x, middle], graph.dot_radius, node_color, background);
            }
        }
    }

    builder.vertices
}

/// Build and normalize all vertices for one frame, matching the canvas
/// backing-store dimensions `width` × `height` in device pixels.
#[cfg(any(target_arch = "wasm32", test))]
fn build_vertices(frame: &RenderFrame, width: f32, height: f32, scale: f32) -> Vec<f32> {
    let mut vertices = build_geometry(frame, scale);
    normalize_vertices(&mut vertices, width, height);
    vertices
}

/// Map device-pixel vertex coordinates into clip space.
#[cfg(any(target_arch = "wasm32", test))]
fn normalize_vertices(vertices: &mut [f32], width: f32, height: f32) {
    if width <= f32::EPSILON || height <= f32::EPSILON {
        return;
    }
    for vertex in vertices.chunks_exact_mut(FLOATS_PER_VERTEX) {
        let [x, y, ..] = vertex else {
            continue;
        };
        *x = *x / width * 2.0 - 1.0;
        *y = 1.0 - *y / height * 2.0;
    }
}

/// Exact u32→f32 for values below 2^24 (far beyond any canvas size used
/// here): splitting into high/low u16 halves avoids the lossy float casts
/// that are denied crate-wide.
#[cfg(any(target_arch = "wasm32", test))]
fn u32_to_f32(value: u32) -> f32 {
    let high = u16::try_from(value >> 16).unwrap_or(u16::MAX);
    let low = u16::try_from(value & 0xffff).unwrap_or(0);
    f32::from(high) * 65536.0 + f32::from(low)
}

/// Convert a rounded, non-negative CSS/device float to u32 without a cast:
/// Rust std has no lossless float→integer conversion, so binary search runs
/// over the exact [`u32_to_f32`] mapping (correct for the integral values produced
/// here, which are bounded by the canvas size ≪ 2^24).
#[cfg(any(target_arch = "wasm32", test))]
fn f32_round_to_u32(value: f32) -> u32 {
    let max_exact = u32_to_f32(16_777_215);
    let target = value.round().clamp(0.0, max_exact);
    let mut low = 0_u32;
    let mut high = 16_777_215_u32;
    while low < high {
        let mid = low.saturating_add(high.saturating_sub(low).saturating_div(2));
        if u32_to_f32(mid) < target {
            low = mid.saturating_add(1);
        } else {
            high = mid;
        }
    }
    low
}

/// Device-pixel scissor rectangle that clips every draw to the graph column
/// `graph.left..graph.left + graph.width`, clamped to the canvas bounds.
#[cfg(any(target_arch = "wasm32", test))]
fn scissor_rect(
    graph: &FrameGraph,
    scale: f32,
    canvas_width: f32,
    canvas_height: f32,
) -> (u32, u32, u32, u32) {
    let x = f32_round_to_u32((graph.left * scale).clamp(0.0, canvas_width));
    let right = f32_round_to_u32(((graph.left + graph.width) * scale).clamp(0.0, canvas_width));
    let height = f32_round_to_u32(canvas_height.max(0.0));
    (x, 0, right.saturating_sub(x), height)
}

/// Whether this host build can create a browser GPU surface.
#[cfg(not(target_arch = "wasm32"))]
#[must_use]
pub const fn supported() -> bool {
    false
}

#[cfg(target_arch = "wasm32")]
mod browser;

#[cfg(target_arch = "wasm32")]
pub use browser::GpuRenderer;

#[cfg(test)]
mod tests {
    use super::{
        build_geometry, build_vertices, curve_segments, lane_center, lane_color, midpoint,
        resolved_transitions, scissor_rect, transition_controls, FrameGraph, RenderFrame,
        RenderRow, RenderedTransition, Rgba, FLOATS_PER_VERTEX, MAX_CURVE_SEGMENTS, PALETTE_HEX,
        PALETTE_LEN,
    };

    fn graph(lane_x: Vec<f32>) -> FrameGraph {
        FrameGraph {
            left: 0.0,
            width: 120.0,
            lane_x,
            dot_radius: 4.0,
            line_width: 2.0,
            bundle_half_height: 7.0,
            bundle_margin: 1.0,
            background_color: None,
        }
    }

    fn row(lane: u32, above: Vec<u32>, below: Vec<u32>, transitions: Vec<[u32; 2]>) -> RenderRow {
        RenderRow {
            top: 0.0,
            bottom: 34.0,
            middle: Some(17.0),
            lane,
            above,
            below,
            transitions,
            is_subop: false,
            is_bundle: false,
        }
    }

    fn frame(rows: Vec<RenderRow>) -> RenderFrame {
        RenderFrame {
            graph: graph(vec![18.0, 36.0, 54.0]),
            rows,
        }
    }

    fn positions(vertices: &[f32]) -> Vec<[f32; 2]> {
        vertices
            .chunks_exact(FLOATS_PER_VERTEX)
            .map(|vertex| {
                let x = vertex.first().copied().expect("vertex has an x");
                let y = vertex.get(1).copied().expect("vertex has a y");
                [x, y]
            })
            .collect()
    }

    fn colors(vertices: &[f32]) -> Vec<[f32; 4]> {
        vertices
            .chunks_exact(FLOATS_PER_VERTEX)
            .map(|vertex| {
                let red = vertex.get(2).copied().expect("vertex has red");
                let green = vertex.get(3).copied().expect("vertex has green");
                let blue = vertex.get(4).copied().expect("vertex has blue");
                let alpha = vertex.get(5).copied().expect("vertex has alpha");
                [red, green, blue, alpha]
            })
            .collect()
    }

    #[must_use]
    fn has_vertex(vertices: &[f32], x: f32, y: f32, tolerance: f32) -> bool {
        positions(vertices)
            .iter()
            .any(|&[vx, vy]| (vx - x).abs() <= tolerance && (vy - y).abs() <= tolerance)
    }

    #[must_use]
    fn has_vertex_color(vertices: &[f32], x: f32, y: f32, color: [f32; 4], tolerance: f32) -> bool {
        let [cr, cg, cb, ca] = color;
        vertices.chunks_exact(FLOATS_PER_VERTEX).any(|vertex| {
            let vx = vertex.first().copied().expect("vertex has an x");
            let vy = vertex.get(1).copied().expect("vertex has a y");
            let red = vertex.get(2).copied().expect("vertex has red");
            let green = vertex.get(3).copied().expect("vertex has green");
            let blue = vertex.get(4).copied().expect("vertex has blue");
            let alpha = vertex.get(5).copied().expect("vertex has alpha");
            (vx - x).abs() <= tolerance
                && (vy - y).abs() <= tolerance
                && (red - cr).abs() <= 1e-4
                && (green - cg).abs() <= 1e-4
                && (blue - cb).abs() <= 1e-4
                && (alpha - ca).abs() <= 1e-4
        })
    }

    /// sRGB encode one linear component and scale to a 0..=255 byte value,
    /// kept in f32 so no lossy float→integer conversion is needed.
    fn linear_to_srgb_bytes(component: f32) -> f32 {
        let encoded = if component <= 0.003_130_8 {
            component * 12.92
        } else {
            1.055 * component.powf(1.0 / 2.4) - 0.055
        };
        encoded.clamp(0.0, 1.0) * 255.0
    }

    fn assert_color_close(a: [f32; 4], b: [f32; 4]) {
        assert!(color_close(a, b), "colors match within float tolerance");
    }

    #[must_use]
    fn color_close(a: [f32; 4], b: [f32; 4]) -> bool {
        let [ar, ag, ab, aa] = a;
        let [br, bg, bb, ba] = b;
        (ar - br).abs() <= 1e-4
            && (ag - bg).abs() <= 1e-4
            && (ab - bb).abs() <= 1e-4
            && (aa - ba).abs() <= 1e-4
    }

    #[must_use]
    fn midpoint_on(a: [f32; 2], b: [f32; 2], x: f32, y: f32, tolerance: f32) -> bool {
        let [ax, ay] = a;
        let [bx, by] = b;
        ((ax + bx) * 0.5 - x).abs() <= tolerance && ((ay + by) * 0.5 - y).abs() <= tolerance
    }

    /// Whether any stroked-segment edge midpoint (the point a quad's end edge
    /// is centered on) lies at `(x, y)`. Curves are emitted as stroked quads,
    /// so the exact Bézier seam is an edge midpoint, not a vertex.
    #[must_use]
    fn has_edge_midpoint(vertices: &[f32], x: f32, y: f32, tolerance: f32) -> bool {
        let points = positions(vertices);
        points.iter().enumerate().any(|(index, a)| {
            points
                .iter()
                .skip(index.saturating_add(1))
                .any(|b| midpoint_on(*a, *b, x, y, tolerance))
        })
    }

    /// Like [`has_edge_midpoint`] but restricted to vertex pairs whose color
    /// matches `color` — used to verify the source/destination color handoff
    /// at the transition seam.
    #[must_use]
    fn has_edge_midpoint_color(
        vertices: &[f32],
        x: f32,
        y: f32,
        color: [f32; 4],
        tolerance: f32,
    ) -> bool {
        let pairs: Vec<([f32; 2], [f32; 4])> = positions(vertices)
            .into_iter()
            .zip(colors(vertices))
            .collect();
        pairs.iter().enumerate().any(|(index, (a, a_color))| {
            pairs
                .iter()
                .skip(index.saturating_add(1))
                .any(|(b, b_color)| {
                    midpoint_on(*a, *b, x, y, tolerance)
                        && color_close(*a_color, color)
                        && color_close(*b_color, color)
                })
        })
    }

    /// Dense-lane x center used by the dense fixtures below, converted from
    /// u32 without a float cast (std has no lossless u32→f32 conversion either).
    /// The 3.5px offset keeps every glyph (capsule half-width 2.5px) inside
    /// the 200px-wide test column.
    fn dense_center(lane: u32) -> f32 {
        3.5 + f32::from(u16::try_from(lane).expect("dense lane index fits u16")) * 0.6
    }

    #[test]
    fn frame_contract_parses_with_defaults() {
        let frame: RenderFrame = serde_json::from_str(
            r#"{
                "graph": {
                    "left": 0, "width": 132, "lane_x": [14.76, 29.52],
                    "dot_radius": 4, "line_width": 1.4, "bundle_half_height": 7,
                    "bundle_margin": 1, "background_color": [0.05, 0.06, 0.07, 0.9]
                },
                "rows": [{
                    "index": 3, "top": 0, "bottom": 34, "middle": 17, "lane": 1,
                    "above": [0], "below": [1], "transitions": [[0, 2]],
                    "is_subop": false, "is_bundle": true
                }]
            }"#,
        )
        .expect("the production frame contract must parse");
        assert_eq!(frame.graph.lane_x.len(), 2, "lane centers for lanes 0..=1");
        assert!(
            (frame.graph.dot_radius - 4.0).abs() < 1e-6,
            "dot radius passes through"
        );
        let row = frame.rows.first().expect("one row");
        assert!(
            row.middle.is_some_and(|m| (m - 17.0).abs() < 1e-6),
            "middle passes through"
        );
        assert_eq!(row.lane, 1, "lane passes through");
        assert_eq!(
            row.transitions.first(),
            Some(&[0, 2]),
            "directed transition passes through"
        );
        assert!(row.is_bundle, "bundle flag passes through");
        let [r, g, b, a] = frame
            .graph
            .background_color
            .expect("background color supplied")
            .0;
        assert!((r - 0.05).abs() < 1e-6, "red component");
        assert!((g - 0.06).abs() < 1e-6, "green component");
        assert!((b - 0.07).abs() < 1e-6, "blue component");
        assert!((a - 0.9).abs() < 1e-6, "alpha component");
    }

    #[test]
    fn frame_contract_minimal_defaults() {
        let frame: RenderFrame = serde_json::from_str(
            r#"{
                "graph": { "left": 4, "width": 100, "lane_x": [18], "line_width": 2 },
                "rows": [ { "lane": 0 } ]
            }"#,
        )
        .expect("a minimal frame must parse");
        assert!(
            frame.graph.background_color.is_none(),
            "no background, no ring"
        );
        assert!(
            (frame.graph.line_width - 2.0).abs() < 1e-6,
            "line width passes through"
        );
        assert_eq!(frame.graph.lane_x.len(), 1, "lane centers parse");
        let parsed_row = frame.rows.first().expect("one row");
        assert!((parsed_row.top - 0.0).abs() < 1e-6, "top defaults to zero");
        assert!(
            (parsed_row.bottom - 0.0).abs() < 1e-6,
            "bottom defaults to zero"
        );
        assert_eq!(parsed_row.middle, None, "absent middle defaults to None");
        assert!(!parsed_row.is_subop, "not a sub-op by default");
        assert!(!parsed_row.is_bundle, "not a bundle by default");
        assert!(parsed_row.above.is_empty(), "no above lanes by default");
        assert!(
            parsed_row.transitions.is_empty(),
            "no transitions by default"
        );
        let mut minimal_row = row(0, vec![0], Vec::new(), Vec::new());
        minimal_row.middle = None;
        let vertices = build_geometry(
            &RenderFrame {
                graph: frame.graph.clone(),
                rows: vec![minimal_row],
            },
            1.0,
        );
        assert!(
            has_vertex(&vertices, 19.0, 17.0, 1e-3),
            "rows render with middle computed as (top + bottom) / 2"
        );
    }

    #[test]
    fn background_color_accepts_three_components() {
        let frame: RenderFrame = serde_json::from_str(
            r#"{
                "graph": {
                    "left": 0, "width": 120, "lane_x": [18], "dot_radius": 4,
                    "line_width": 2, "bundle_half_height": 7, "bundle_margin": 1,
                    "background_color": [0.1, 0.2, 0.3]
                }
            }"#,
        )
        .expect("a three-component background color parses");
        let [r, g, b, a] = frame.graph.background_color.expect("supplied").0;
        assert!((r - 0.1).abs() < 1e-6, "red component");
        assert!((g - 0.2).abs() < 1e-6, "green component");
        assert!((b - 0.3).abs() < 1e-6, "blue component");
        assert!((a - 1.0).abs() < 1e-6, "alpha defaults to opaque");
    }

    #[test]
    fn background_color_accepts_css_hex_strings() {
        let frame: RenderFrame = serde_json::from_str(
            r##"{
                "graph": { "left": 0, "width": 120, "lane_x": [18],
                    "background_color": "#0E1116" }
            }"##,
        )
        .expect("the production CSS hex background color must parse");
        let [r, g, b, a] = frame.graph.background_color.expect("supplied").0;
        let red = 14.0_f32 / 255.0;
        let green = 17.0_f32 / 255.0;
        let blue = 22.0_f32 / 255.0;
        assert!((r - red).abs() < 1e-6, "red component");
        assert!((g - green).abs() < 1e-6, "green component");
        assert!((b - blue).abs() < 1e-6, "blue component");
        assert!((a - 1.0).abs() < 1e-6, "alpha defaults to opaque");
    }

    #[test]
    fn background_color_hex_forms_match_float_arrays() {
        let short_hex: RenderFrame =
            serde_json::from_str(r##"{"graph": { "background_color": "#abc" }}"##)
                .expect("the three-digit hex form parses");
        let long_hex: RenderFrame =
            serde_json::from_str(r##"{"graph": { "background_color": "#aabbcc" }}"##)
                .expect("the six-digit hex form parses");
        let alpha_hex: RenderFrame =
            serde_json::from_str(r##"{"graph": { "background_color": "#10203040" }}"##)
                .expect("the eight-digit hex form parses");
        let short = short_hex.graph.background_color.expect("short hex").0;
        let long = long_hex.graph.background_color.expect("long hex").0;
        let alpha = alpha_hex.graph.background_color.expect("alpha hex").0;
        assert_color_close(short, [170.0 / 255.0, 187.0 / 255.0, 204.0 / 255.0, 1.0]);
        assert_color_close(long, [170.0 / 255.0, 187.0 / 255.0, 204.0 / 255.0, 1.0]);
        assert_color_close(
            alpha,
            [16.0 / 255.0, 32.0 / 255.0, 48.0 / 255.0, 64.0 / 255.0],
        );
    }

    #[test]
    fn background_color_rejects_invalid_strings() {
        for invalid in ["red", "0e1116", "#12", "#12345", "#ggg", "#aabbccddee", ""] {
            let payload = format!(r#"{{"graph": {{"background_color": "{invalid}"}}}}"#);
            let result: serde_json::Result<RenderFrame> = serde_json::from_str(&payload);
            assert!(
                result.is_err(),
                "the non-hex color {invalid:?} must be rejected"
            );
        }
    }

    #[test]
    fn palette_matches_production_hexes() {
        assert_eq!(
            PALETTE_HEX.len(),
            usize::try_from(PALETTE_LEN).expect("palette length fits usize"),
            "palette length constant matches the table"
        );
        for lane in 0..PALETTE_LEN {
            let [red, green, blue, alpha] = lane_color(lane);
            assert!((alpha - 1.0).abs() <= f32::EPSILON, "lane {lane} is opaque");
            let expected = PALETTE_HEX
                .get(usize::try_from(lane).expect("lane fits usize"))
                .copied()
                .expect("lane inside palette table");
            let [er, eg, eb] = expected;
            let red_bytes = linear_to_srgb_bytes(red);
            let green_bytes = linear_to_srgb_bytes(green);
            let blue_bytes = linear_to_srgb_bytes(blue);
            assert!(
                (red_bytes - f32::from(er)).abs() <= 1.0
                    && (green_bytes - f32::from(eg)).abs() <= 1.0
                    && (blue_bytes - f32::from(eb)).abs() <= 1.0,
                "lane {lane} round-trips to the production hex"
            );
        }
        assert_color_close(lane_color(10), lane_color(0));
        assert_color_close(lane_color(13), lane_color(3));
    }

    #[test]
    fn dot_is_circular_and_subop_draws_no_node() {
        let plain = frame(vec![row(0, Vec::new(), Vec::new(), Vec::new())]);
        let vertices = build_geometry(&plain, 1.0);
        assert!(!vertices.is_empty(), "an ordinary row emits a node dot");
        let mut max_distance = 0.0_f32;
        for [x, y] in positions(&vertices) {
            let distance = (x - 18.0).hypot(y - 17.0);
            assert!(
                distance <= 4.0 + 1e-3,
                "dot vertices stay inside the radius"
            );
            max_distance = max_distance.max(distance);
        }
        assert!(
            (max_distance - 4.0).abs() <= 1e-3,
            "dot rim vertices reach exactly the supplied radius"
        );

        let subop = RenderFrame {
            graph: graph(vec![18.0]),
            rows: vec![RenderRow {
                top: 0.0,
                bottom: 34.0,
                middle: Some(17.0),
                lane: 0,
                above: vec![0],
                below: Vec::new(),
                transitions: Vec::new(),
                is_subop: true,
                is_bundle: false,
            }],
        };
        let vertices = build_geometry(&subop, 1.0);
        assert!(!vertices.is_empty(), "the sub-op lane line emits geometry");
        for [x, y] in positions(&vertices) {
            assert!(
                (x - 18.0).abs() <= 1.01,
                "sub-op rows keep only the lane line, no dot (vertex at {x},{y})"
            );
            assert!(
                (-1e-3..=17.0 + 1e-3).contains(&y),
                "sub-op above half ends at the midpoint (vertex at {x},{y})"
            );
        }
    }

    #[test]
    fn ordinary_above_line_reaches_the_midpoint() {
        let ordinary = frame(vec![row(0, vec![0], Vec::new(), Vec::new())]);
        let vertices = build_geometry(&ordinary, 1.0);
        assert!(
            has_vertex(&vertices, 19.0, 17.0, 1e-3),
            "an ordinary above line runs down to the row midpoint"
        );
    }

    #[test]
    fn transition_ownership_suppresses_generic_halves() {
        let with_transition = frame(vec![row(0, vec![0], vec![2], vec![[0, 2]])]);
        let without_transition = frame(vec![row(0, vec![0], vec![2], Vec::new())]);
        let transition_vertices = build_geometry(&with_transition, 1.0);
        let plain_vertices = build_geometry(&without_transition, 1.0);
        assert!(
            transition_vertices.len() > plain_vertices.len(),
            "a rendered transition adds curve geometry"
        );
        assert!(
            has_vertex(&plain_vertices, 53.0, 17.0, 1e-3),
            "without the transition the to-lane below half starts at the midpoint"
        );
        assert!(
            !has_vertex(&transition_vertices, 53.0, 17.0, 1e-3),
            "the transition owns the to-lane bottom half, so no generic line is drawn"
        );
        assert!(
            has_edge_midpoint(&transition_vertices, 45.0, 21.25, 1e-2),
            "the two curve halves meet at the shared seam"
        );
    }

    #[test]
    fn dangling_transitions_are_dropped() {
        let plain = frame(vec![row(0, Vec::new(), Vec::new(), Vec::new())]);
        let dangling_child = frame(vec![row(0, Vec::new(), Vec::new(), vec![[0, 2]])]);
        let dangling_parent = frame(vec![row(0, Vec::new(), Vec::new(), vec![[2, 0]])]);
        let plain_vertices = build_geometry(&plain, 1.0);
        assert_eq!(
            build_geometry(&dangling_child, 1.0),
            plain_vertices,
            "a child-lane transition with no boundary backing is dropped"
        );
        assert_eq!(
            build_geometry(&dangling_parent, 1.0),
            plain_vertices,
            "a parent-lane transition with no above backing is dropped"
        );
    }

    #[test]
    fn resolved_transitions_follow_production_rules() {
        let r = row(0, vec![0], vec![2], vec![[0, 2], [1, 0]]);
        let resolved = resolved_transitions(&r);
        assert_eq!(resolved.len(), 1, "only the connected transition is kept");
        let kept = resolved.first().expect("one transition");
        assert_eq!(kept.from_lane, 0, "from lane passes through");
        assert_eq!(kept.to_lane, 2, "to lane passes through");
        assert!(kept.start_at_dot, "starts at the row's own node");
        assert!(!kept.end_at_dot, "ends at the row boundary");
    }

    #[test]
    fn curve_halves_share_seam_colors_and_tangent() {
        let transition = RenderedTransition {
            from_lane: 0,
            to_lane: 2,
            start_at_dot: true,
            end_at_dot: false,
        };
        let start = [18.0_f32, 17.0_f32];
        let end = [54.0_f32, 34.0_f32];
        let (src_control, dst_control, seam) = transition_controls(start, end, transition);
        let [sx, sy] = seam;
        let [mx, my] = midpoint(src_control, dst_control);
        assert!(
            (sx - mx).abs() < 1e-6 && (sy - my).abs() < 1e-6,
            "the seam is the midpoint of both controls"
        );
        let [scx, scy] = src_control;
        let [dcx, dcy] = dst_control;
        assert!(
            (sx - scx - (dcx - sx)).abs() <= 1e-4,
            "tangent continuity holds at the seam (x)"
        );
        assert!(
            (sy - scy - (dcy - sy)).abs() <= 1e-4,
            "tangent continuity holds at the seam (y)"
        );

        let rows = frame(vec![row(0, Vec::new(), vec![2], vec![[0, 2]])]);
        let vertices = build_geometry(&rows, 1.0);
        assert!(
            has_edge_midpoint_color(&vertices, sx, sy, lane_color(0), 1e-2),
            "the source half reaches the seam in the source-lane color"
        );
        assert!(
            has_edge_midpoint_color(&vertices, sx, sy, lane_color(2), 1e-2),
            "the destination half starts at the seam in the destination color"
        );

        let boundary = RenderedTransition {
            from_lane: 0,
            to_lane: 2,
            start_at_dot: false,
            end_at_dot: false,
        };
        let (src_control, dst_control, seam) =
            transition_controls([18.0_f32, 0.0_f32], [54.0_f32, 34.0_f32], boundary);
        let [sx, sy] = seam;
        let [scx, scy] = src_control;
        let [dcx, dcy] = dst_control;
        assert!(
            (scy - sy).abs() <= 1e-4 && (dcy - sy).abs() <= 1e-4,
            "boundary-to-boundary halves meet with a horizontal tangent"
        );
        assert!(
            (scx - 18.0).abs() <= 1e-4 && (dcx - 54.0).abs() <= 1e-4,
            "controls keep their lane x"
        );
        assert!(
            (sx - 36.0).abs() <= 1e-4 && (sy - 17.0).abs() <= 1e-4,
            "the seam sits at the lane and row midpoints"
        );
    }

    #[test]
    fn bundle_capsule_terminals_and_anchors() {
        let mut bundle_row = row(0, vec![0], vec![0], Vec::new());
        bundle_row.is_bundle = true;
        let vertices = build_geometry(&frame(vec![bundle_row]), 1.0);
        assert!(has_vertex(&vertices, 18.0, 7.0, 1e-3), "capsule top cap");
        assert!(
            has_vertex(&vertices, 18.0, 27.0, 1e-3),
            "capsule bottom cap"
        );
        assert!(
            has_vertex(&vertices, 21.0, 10.0, 1e-3),
            "entry terminal right rim"
        );
        assert!(
            has_vertex(&vertices, 15.0, 10.0, 1e-3),
            "entry terminal left rim"
        );
        assert!(
            has_vertex(&vertices, 18.0, 13.0, 1e-3),
            "entry terminal bottom rim"
        );
        assert!(
            has_vertex(&vertices, 19.0, 10.0, 1e-3),
            "above line ends at the entry terminal"
        );
        assert!(
            !has_vertex(&vertices, 19.0, 17.0, 1e-3),
            "above line stops short of the midpoint"
        );
        assert!(
            has_vertex(&vertices, 19.0, 24.0, 1e-3),
            "below line starts at the exit terminal"
        );
        assert!(
            !has_vertex(&vertices, 19.0, 17.0, 1e-3),
            "below line stops short of the midpoint"
        );

        let mut transition_row = row(0, Vec::new(), vec![2], vec![[0, 2]]);
        transition_row.is_bundle = true;
        let vertices = build_geometry(&frame(vec![transition_row]), 1.0);
        assert!(
            has_edge_midpoint(&vertices, 45.0, 26.5, 1e-2),
            "an exit-anchored transition seam moves below the midpoint"
        );

        let mut incoming_row = row(0, vec![2], Vec::new(), vec![[2, 0]]);
        incoming_row.is_bundle = true;
        let vertices = build_geometry(&frame(vec![incoming_row]), 1.0);
        assert!(
            has_edge_midpoint(&vertices, 45.0, 7.5, 1e-2),
            "an entry-anchored transition seam moves above the midpoint"
        );
    }

    #[test]
    fn background_ring_uses_supplied_color_only() {
        let bg = Rgba([0.9, 0.1, 0.2, 1.0]);
        let mut themed = graph(vec![18.0]);
        themed.background_color = Some(bg);
        let with_bg = RenderFrame {
            graph: themed,
            rows: vec![row(0, Vec::new(), Vec::new(), Vec::new())],
        };
        let vertices = build_geometry(&with_bg, 1.0);
        let [br, bg_g, bb, ba] = bg.linear();
        assert!(
            has_vertex_color(&vertices, 22.5, 17.0, [br, bg_g, bb, ba], 1e-4),
            "outer ring rim sits at radius plus half the stroke width"
        );
        assert!(
            has_vertex_color(&vertices, 18.0, 21.5, [br, bg_g, bb, ba], 1e-4),
            "the ring covers the full circle"
        );
        assert!(
            has_vertex_color(&vertices, 18.0, 20.5, lane_color(0), 1e-4),
            "the interior circle is the lane color at radius minus half the stroke"
        );

        let plain = frame(vec![row(0, Vec::new(), Vec::new(), Vec::new())]);
        let vertices = build_geometry(&plain, 1.0);
        assert!(
            !has_vertex_color(&vertices, 18.0, 17.0, [br, bg_g, bb, ba], 1e-4),
            "no invented background ring without a supplied color"
        );
    }

    #[test]
    fn dense_supplied_lane_positions_are_used_exactly() {
        let lane_x: Vec<f32> = (0..200_u32).map(dense_center).collect();
        let mut rows: Vec<RenderRow> = Vec::new();
        for lane in [0_u32, 99, 199] {
            rows.push(row(lane, Vec::new(), Vec::new(), Vec::new()));
        }
        let mut dense_row = row(0, vec![0], vec![0], Vec::new());
        dense_row.is_bundle = true;
        rows.push(dense_row);
        rows.push(row(5, Vec::new(), vec![8], vec![[5, 8]]));
        let frame = RenderFrame {
            graph: FrameGraph {
                left: 0.0,
                width: 200.0,
                lane_x,
                dot_radius: 1.5,
                line_width: 2.0,
                bundle_half_height: 7.0,
                bundle_margin: 1.0,
                background_color: None,
            },
            rows,
        };
        let vertices = build_geometry(&frame, 1.0);
        for lane in [0_u32, 99, 199] {
            let expected = dense_center(lane);
            assert!(
                has_vertex(&vertices, expected, 17.0, 1e-3),
                "lane {lane} dot is centered exactly on the supplied position"
            );
        }
        assert!(
            (lane_center(&frame.graph, 0) - 3.5).abs() < 1e-3,
            "first lane center passes through"
        );
        assert!(
            (lane_center(&frame.graph, 199) - 122.9).abs() < 1e-3,
            "last lane center passes through"
        );
        assert!(
            (lane_center(&frame.graph, 999) - 122.9).abs() < 1e-3,
            "unknown lanes fall back to the last supplied center"
        );
        assert!(
            (lane_center(&frame.graph, 999) - lane_center(&frame.graph, 199)).abs() < 1e-6,
            "fallback matches the last supplied center"
        );
    }

    #[test]
    fn vertices_are_finite_and_normalize_in_bounds() {
        let lane_x: Vec<f32> = (0..200_u32).map(dense_center).collect();
        let mut rows: Vec<RenderRow> = (0..200_u32)
            .map(|lane| row(lane, vec![lane], vec![lane], Vec::new()))
            .collect();
        rows.push(row(0, vec![0], vec![2], vec![[0, 2]]));
        let frame = RenderFrame {
            graph: FrameGraph {
                left: 0.0,
                width: 200.0,
                lane_x,
                dot_radius: 1.5,
                line_width: 2.0,
                bundle_half_height: 7.0,
                bundle_margin: 1.0,
                background_color: None,
            },
            rows,
        };
        let vertices = build_vertices(&frame, 800.0, 600.0, 2.0);
        assert!(!vertices.is_empty(), "dense frame emits geometry");
        assert!(
            vertices.iter().all(|value| value.is_finite()),
            "all vertices are finite"
        );
        for vertex in vertices.chunks_exact(FLOATS_PER_VERTEX) {
            let x = vertex.first().copied().expect("vertex has an x");
            let y = vertex.get(1).copied().expect("vertex has a y");
            assert!(
                (-1.0 - 1e-4..=1.0 + 1e-4).contains(&x),
                "vertex x stays inside clip space"
            );
            assert!(
                (-1.0 - 1e-4..=1.0 + 1e-4).contains(&y),
                "vertex y stays inside clip space"
            );
        }
    }

    #[test]
    fn scissor_rect_clips_to_graph_column() {
        let column = FrameGraph {
            left: 0.0,
            width: 120.0,
            lane_x: vec![18.0, 36.0, 54.0],
            dot_radius: 4.0,
            line_width: 2.0,
            bundle_half_height: 7.0,
            bundle_margin: 1.0,
            background_color: None,
        };
        assert_eq!(
            scissor_rect(&column, 2.0, 800.0, 600.0),
            (0, 0, 240, 600),
            "scale maps CSS pixels to device pixels"
        );
        assert_eq!(
            scissor_rect(&column, 1.0, 800.0, 600.0),
            (0, 0, 120, 600),
            "identity scale"
        );
        assert_eq!(
            scissor_rect(
                &FrameGraph {
                    left: 10.0,
                    ..column.clone()
                },
                1.0,
                800.0,
                600.0
            ),
            (10, 0, 120, 600),
            "column offset passes through"
        );
        assert_eq!(
            scissor_rect(
                &FrameGraph {
                    left: 790.0,
                    ..column.clone()
                },
                1.0,
                800.0,
                600.0
            ),
            (790, 0, 10, 600),
            "column is clamped to the canvas edge"
        );
    }

    #[test]
    fn curve_tessellation_stays_within_error_budget() {
        let curves = [
            ([18.0_f32, 17.0], [54.0_f32, 17.0], [54.0_f32, 34.0]),
            ([0.0_f32, 0.0], [500.0_f32, 0.0], [500.0_f32, 0.0]),
            ([120.0_f32, 0.0], [0.0_f32, 34.0], [120.0_f32, 34.0]),
            ([18.0_f32, 0.0], [18.0_f32, 34.0], [54.0_f32, 34.0]),
        ];
        for (start, control, end) in curves {
            let segments = curve_segments(start, control, end);
            assert!(
                (2..=MAX_CURVE_SEGMENTS).contains(&segments),
                "segment count is bounded"
            );
            let [sx, sy] = start;
            let [cx, cy] = control;
            let [ex, ey] = end;
            let d_x = sx - 2.0 * cx + ex;
            let d_y = sy - 2.0 * cy + ey;
            let magnitude = d_x.hypot(d_y);
            let f32_segments = f32::from(segments);
            let error = magnitude / (4.0 * f32_segments * f32_segments);
            assert!(
                error <= 0.25 + 1e-6,
                "deviation |D|/(4k²) stays within 0.25 CSS px"
            );
        }
        assert_eq!(
            curve_segments([0.0_f32, 0.0], [5000.0_f32, 0.0], [5000.0_f32, 0.0]),
            MAX_CURVE_SEGMENTS,
            "pathological edges cap at the bounded maximum"
        );
    }
}

#[cfg(target_arch = "wasm32")]
mod shell {
    //! Rust-owned browser shell (Slice 3A).
    //!
    //! Owns startup sequencing (VS Code API acquisition through the narrow
    //! wasm-bindgen binding, message-listener installation before
    //! `webviewReady`, state restore/save), the `HistoryAppState` machine, the
    //! reducer `Step` sends and DOM ops, scroll/profile controls and
    //! persistence, the debug hooks, and the per-row SVG graph render pass.
    //!
    //! Reentrancy contract: the fixture bridge dispatches correlated
    //! responses synchronously inside `postMessage`, so host messages are
    //! queued and drained by [`pump_messages`] with sends executed only after
    //! the shell borrow is released; DOM ops are applied before sends within
    //! each transition so nested response steps never reorder the window.

    use std::cell::Cell;
    use std::cell::RefCell;
    use std::collections::VecDeque;

    use serde_json::{json, Value};
    use wasm_bindgen::prelude::*;

    use crate::app::dom::{self, ColKey, HistoryDom};
    use crate::app::host::{self, Send};
    use crate::app::rows::{self, RowContext, RowSpec, ViewMode};
    use crate::app::state::{
        DomOp, HistoryAppState, Profile, ProfileAction, RetryAction, Step, Viewport, ROW_H,
    };

    /// Narrow wasm-bindgen binding for VS Code API acquisition: the webview
    /// host (and the harness fixture bridge) provide `acquireVsCodeApi` as a
    /// window global. Every later API call goes through [`vscode_call`].
    #[wasm_bindgen]
    extern "C" {
        #[wasm_bindgen(js_name = "acquireVsCodeApi", catch)]
        fn acquire_vscode_api() -> Result<JsValue, JsValue>;
    }

    thread_local! {
        /// Pending host messages (structured-clone data payloads).
        static MSG_QUEUE: RefCell<VecDeque<JsValue>> = const { RefCell::new(VecDeque::new()) };
        /// True while the pump drains the queue (reentrancy guard for the
        /// synchronous fixture bridge dispatch inside `postMessage`).
        static PUMP_ACTIVE: Cell<bool> = const { Cell::new(false) };
        /// Host envelopes queued for deferred dispatch (see
        /// [`schedule_post_flush`]: wasm-bindgen `Closure`s reject recursive
        /// invocation, and the harness fixture bridge dispatches correlated
        /// responses synchronously inside `postMessage`, so posts must never
        /// happen inside a listener closure's own stack).
        static POST_QUEUE: RefCell<VecDeque<Value>> = const { RefCell::new(VecDeque::new()) };
        /// True while the deferred-post flush future is scheduled/running.
        static FLUSH_ACTIVE: Cell<bool> = const { Cell::new(false) };
        /// The live shell; created once at startup.
        static SHELL_DATA: RefCell<Option<ShellData>> = const { RefCell::new(None) };
        /// Active column-divider drag (window-level mousemove/mouseup state).
        static COLUMN_DRAG: RefCell<Option<ColumnDrag>> = const { RefCell::new(None) };
        /// True while a transition (or the message pump) holds the shell
        /// borrow. Synchronous DOM events such as `focusin` can fire inside
        /// one (e.g. `element.focus()` restores focus after a rebuild); nested
        /// transitions are skipped because the outer transition already owns
        /// the state change.
        static TRANSITION_ACTIVE: Cell<bool> = const { Cell::new(false) };
    }

    /// Boolean shell state, grouped under the `struct_excessive_bools` gate.
    #[derive(Debug, Default, Clone, Copy)]
    struct ShellFlags {
        /// The per-row SVG graph renderer is ready (first window rendered).
        renderer_ready: bool,
        /// Startup completed (listener installed, webviewReady posted).
        wasm_ready: bool,
    }

    /// The live shell: DOM + state machine + SVG render counters.
    #[derive(Debug)]
    struct ShellData {
        flags: ShellFlags,
        vscode: JsValue,
        dom: HistoryDom,
        state: HistoryAppState,
        instance_id: String,
        /// Successful per-row SVG render passes (replaces the GPU frame
        /// counter; the debug facade still reports it as `renderCount`).
        render_count: u64,
        /// DOM generation counter (the harness `whenIdle` settles on two
        /// stable generations, unchanged).
        generation: u64,
        started_at_ms: f64,
        first_window_ms: Option<f64>,
        last_render_ms: Option<f64>,
        last_frame_rows: Vec<dom::FrameRow>,
        last_error: Option<String>,
        progressive_timer: Option<i32>,
        /// Debounced viewport-resize timer (production `resizeTimer`).
        resize_timer: Option<i32>,
        /// Last observed `#rows` width (the resize observer ignores
        /// height-only notifications, matching `lastRowsWidth`).
        last_rows_width: f64,
        /// Retains the observer for the shell lifetime. Dropping the JS wrapper
        /// permits the browser to collect it and silently stop observations.
        resize_observer: Option<web_sys::ResizeObserver>,
        /// User-dragged column widths (divider state; `None` = natural).
        col_widths: dom::ColWidths,
    }

    /// An active column-divider drag (window-level listeners persist for the
    /// whole gesture; `move`/`up` closures remove themselves on mouseup).
    #[derive(Debug)]
    struct ColumnDrag {
        col: ColKey,
        start_x: f64,
        start_w: f64,
        move_closure: Option<Closure<dyn FnMut(web_sys::Event)>>,
        up_closure: Option<Closure<dyn FnMut(web_sys::Event)>>,
    }

    impl ShellData {
        fn layout(&self) -> dom::GraphLayout {
            dom::graph_layout(
                self.state.max_lane,
                self.dom.rows_client_width_css(),
                self.dom.window_inner_width_css(),
            )
        }

        /// `currentGraphWidth()` — the effective graph column width (divider
        /// override or natural). Lane centers stay pinned to the natural
        /// width (`layout().lane_x`), so dragging never rescales topology.
        fn current_graph_width(&self) -> f64 {
            let layout = self.layout();
            dom::current_graph_width(&layout, &self.col_widths)
        }

        /// The per-row SVG graph cell geometry: pinned natural lane centers,
        /// the compressed dot radius, the rendered column width (divider
        /// override or natural), and the fixed `ROW_H` cell height.
        fn graph_cell_spec(&self) -> dom::GraphCellSpec {
            let layout = self.layout();
            dom::GraphCellSpec {
                lane_x: layout.lane_x.clone(),
                dot_radius: layout.dot_radius,
                width: self.current_graph_width(),
                height: dom::i64_to_f64(ROW_H),
            }
        }

        fn col_style(&self) -> String {
            let layout = self.layout();
            dom::col_style(
                dom::current_graph_width(&layout, &self.col_widths),
                self.dom.window_inner_width_css(),
                &self.col_widths,
            )
        }

        fn pane_status(&self) -> dom::PaneStatus {
            dom::PaneStatus {
                search_mode: self.state.view_flags.search_mode,
                search_query: self.state.search_query.clone(),
                total: self.state.visible_total(),
                open_warnings: self.state.open_warnings.clone(),
            }
        }

        /// The state-aware per-row context (selection/find/expansion/roving).
        fn row_context(&self, abs_index: i64, is_group_start: bool) -> RowContext {
            self.state
                .row_context(self.profile_view(), abs_index, is_group_start)
        }

        fn spacer_height_px(&self) -> i64 {
            self.state.visible_total().saturating_mul(ROW_H).max(1)
        }

        fn profile_view(&self) -> ViewMode {
            match self.state.profile {
                Profile::Activity => ViewMode::Activity,
                Profile::Raw => ViewMode::Raw,
            }
        }

        fn reanchor_window(&mut self, top: i64, bottom: i64) -> Result<(), JsValue> {
            let specs = dom::window_specs(&self.state, top, bottom);
            let col_style = self.col_style();
            let graph = self.graph_cell_spec();
            let options = dom::RebuildOptions {
                spacer_height_px: self.spacer_height_px(),
                wrap_top_px: top.saturating_mul(ROW_H),
                aria_rowcount: self.state.visible_total(),
                graph_width_css: self.current_graph_width(),
                graph,
                status: self.pane_status(),
            };
            self.dom.reanchor(&specs, &col_style, &options)?;
            self.install_resize_handles()?;
            Ok(())
        }

        /// Re-create the column-divider handles after a full rebuild
        /// (`setupColumnResizeHandles`).
        fn install_resize_handles(&mut self) -> Result<(), JsValue> {
            self.dom
                .install_resize_handles(self.dom.window_inner_width_css())
        }

        /// `applyRovingTabindex` — enforce exactly one tabbable row over the
        /// rendered window; the anchor falls back to the first rendered row
        /// when the current one was scrolled/trimmed away.
        fn apply_roving_tabindex(&mut self) {
            let Some(wrap) = self.dom.wrap() else {
                return;
            };
            let Ok(list) = wrap.query_selector_all(".row") else {
                return;
            };
            if list.length() == 0 {
                return;
            }
            let mut anchor_abs = None;
            let mut first_abs = None;
            for index in 0..list.length() {
                let item = list.item(index);
                let Some(node) = item else {
                    continue;
                };
                let Some(element) = node.dyn_ref::<web_sys::Element>() else {
                    continue;
                };
                let Some(abs) = element
                    .get_attribute("data-row")
                    .and_then(|raw| raw.trim().parse::<i64>().ok())
                else {
                    continue;
                };
                if first_abs.is_none() {
                    first_abs = Some(abs);
                }
                if abs == self.state.roving_abs() {
                    anchor_abs = Some(abs);
                }
            }
            let anchor = anchor_abs.or(first_abs);
            let Some(anchor) = anchor else {
                return;
            };
            if anchor != self.state.roving_abs() {
                self.state.roving_abs = anchor;
            }
            let anchor_text = anchor.to_string();
            for index in 0..list.length() {
                let item = list.item(index);
                let Some(node) = item else {
                    continue;
                };
                let Some(element) = node.dyn_ref::<web_sys::Element>() else {
                    continue;
                };
                let Some(abs) = element
                    .get_attribute("data-row")
                    .and_then(|raw| raw.trim().parse::<i64>().ok())
                else {
                    continue;
                };
                drop(element.set_attribute(
                    "tabindex",
                    if abs.to_string() == anchor_text {
                        "0"
                    } else {
                        "-1"
                    },
                ));
            }
        }

        /// `syncFindNavButtons` — Previous/Next are shown/enabled only when a
        /// settled, navigable find session matches the exact input text.
        fn sync_find_nav(&mut self) {
            let value = self.dom.search_input_value();
            let enabled = self.state.find_navigation_enabled(&value);
            self.dom.set_find_nav(enabled);
        }

        /// Production click/chevron semantics: select the row, then toggle
        /// disclosure for any expandable (non-sub-op, has-children) row.
        fn row_select_and_toggle(&mut self, abs: i64, viewport: &Viewport, step: &mut Step) {
            self.state.select_row(abs);
            if let Err(error) = self.dom.apply_selection(abs) {
                record_error(&format!(
                    "selection apply failed: {}",
                    js_value_text(&error)
                ));
            }
            let expandable = self
                .state
                .cache
                .get(&abs)
                .is_some_and(|row| !host::row::bool(row, "is_subop") && rows::has_sub_ops(row));
            if expandable {
                self.state.toggle_expanded_ui(abs, viewport, step);
            }
        }

        /// `openRawJson` — post the exact `openJson` identity envelope
        /// (`git_oid`+`repository` or `op_id`), or announce the absence.
        fn open_json_for_abs(&mut self, abs: i64, step: &mut Step) {
            let Some(row) = self.state.cache.get(&abs) else {
                return;
            };
            if let Some(envelope) = rows::open_json_envelope(row) {
                step.sends.push(Send::OpenJson(envelope));
            } else {
                step.sends.push(Send::StatusText(
                    "No raw record is available for this row".to_owned(),
                ));
            }
        }

        /// `focusSearchResult` — legacy flat-list result focus: roving anchor,
        /// selection, minimal reveal, then focus without a second scroll.
        fn focus_search_result(&mut self, abs: i64) {
            self.state.set_roving_abs(abs);
            self.apply_roving_tabindex();
            self.state.select_row(abs);
            if let Err(error) = self.dom.apply_selection(abs) {
                record_error(&format!(
                    "selection apply failed: {}",
                    js_value_text(&error)
                ));
            }
            if let Err(error) = self.dom.reveal_row(abs) {
                record_error(&format!("reveal failed: {}", js_value_text(&error)));
            }
            let selector = format!(".row[data-row=\"{abs}\"]");
            if let Ok(Some(target)) = self.dom.rows().query_selector(&selector) {
                if let Ok(target) = target.dyn_into::<web_sys::HtmlElement>() {
                    let options = web_sys::FocusOptions::new();
                    options.set_prevent_scroll(true);
                    drop(target.focus_with_options(&options));
                }
            }
        }

        fn append_window(&mut self, from: i64, to: i64) -> Result<(), JsValue> {
            let last_group = self.last_rendered_group();
            let specs = dom::window_rows_from(&self.state, from, to, last_group.as_deref())
                .into_iter()
                .map(|row| row.spec)
                .collect::<Vec<_>>();
            let col_style = self.col_style();
            let graph = self.graph_cell_spec();
            self.dom.append_rows(&specs, &col_style, &graph)
        }

        fn prepend_window(&mut self, from: i64, to: i64) -> Result<(), JsValue> {
            let planned = dom::window_rows_from(&self.state, from, to, None);
            let specs = planned
                .iter()
                .map(|row| row.spec.clone())
                .collect::<Vec<_>>();
            let col_style = self.col_style();
            let graph = self.graph_cell_spec();
            self.dom.prepend_rows(
                &specs,
                &col_style,
                self.state.render_top.saturating_mul(ROW_H),
                &graph,
            )?;
            // Production re-evaluates the old first-row chip against its new
            // previous sibling after a prepend crosses a group boundary.
            let boundary_vis = to.saturating_add(1);
            let Some(prev_group) = planned
                .iter()
                .rev()
                .find(|row| !row.spec.placeholder)
                .map(|row| row.spec.group.clone())
            else {
                return Ok(());
            };
            let boundary =
                dom::window_rows_from(&self.state, boundary_vis, boundary_vis, Some(&prev_group));
            if let Some(row) = boundary.first() {
                let abs = row.spec.identity.abs_index;
                self.dom
                    .replace_row_abs(abs, &row.spec, &col_style, &graph)?;
            }
            Ok(())
        }

        fn trim_top(&mut self, keep_top: i64) -> Result<(), JsValue> {
            // Trim by scanning the rendered DOM and mapping each rendered
            // absolute id back through the collapsed-mode mapping (production
            // `trimTop`): visible bounds never compare directly to `data-row`
            // absolute values, and rows added by a prepend/append during this
            // same transition are covered too. The wrap then shifts to the
            // state's advanced visible top (`setWrapTop(renderTop)`).
            let rendered = self.dom.rendered_row_abs();
            let remove = dom::rows_outside_visible(&self.state, &rendered, keep_top, i64::MAX);
            self.dom.remove_abs(&remove)?;
            self.dom
                .set_wrap_top(self.state.render_top.saturating_mul(ROW_H))
        }

        fn trim_bottom(&mut self, keep_bottom: i64) -> Result<(), JsValue> {
            let rendered = self.dom.rendered_row_abs();
            let remove = dom::rows_outside_visible(&self.state, &rendered, i64::MIN, keep_bottom);
            self.dom.remove_abs(&remove)
        }

        fn fill_placeholders(&mut self) -> Result<(), JsValue> {
            let col_style = self.col_style();
            let graph = self.graph_cell_spec();
            for abs in self.dom.placeholder_abs() {
                let Some(row) = self.state.cache.get(&abs) else {
                    continue;
                };
                let prev_group = self.previous_rendered_group(abs);
                let group = host::row::str(row, "group");
                let is_group_start = prev_group.as_deref().is_none_or(|prev| prev != group);
                let context = self.row_context(abs, is_group_start);
                let spec = RowSpec::from_value(row, &context);
                self.dom.replace_row_abs(abs, &spec, &col_style, &graph)?;
            }
            Ok(())
        }

        fn previous_rendered_group(&self, abs: i64) -> Option<String> {
            let prev = self.dom.previous_row_abs(abs)?;
            self.state
                .cache
                .get(&prev)
                .map(|row| host::row::owned_str(row, "group"))
        }

        fn last_rendered_group(&self) -> Option<String> {
            let abs = self.dom.last_row_abs()?;
            self.state
                .cache
                .get(&abs)
                .map(|row| host::row::owned_str(row, "group"))
        }

        fn refresh_header(&mut self) -> Result<(), JsValue> {
            let col_style = self.col_style();
            let graph_width = self.current_graph_width();
            self.dom.refresh_header(&col_style, graph_width)
        }

        /// Apply one reducer DOM op.
        fn apply_op(&mut self, op: &DomOp) -> Result<(), JsValue> {
            match op {
                DomOp::ShowMessage { text, error } => self.dom.show_message(text, *error),
                DomOp::ShowRequestError { text, retry } => {
                    let button = self.dom.show_request_error(text)?;
                    let retry = *retry;
                    let closure = Closure::<dyn FnMut()>::wrap(Box::new(move || {
                        on_retry(retry);
                    }));
                    button.add_event_listener_with_callback(
                        "click",
                        closure.as_ref().unchecked_ref(),
                    )?;
                    closure.forget();
                    Ok(())
                }
                DomOp::Reanchor { top, bottom } => self.reanchor_window(*top, *bottom),
                DomOp::AppendBelow { from, to } => self.append_window(*from, *to),
                DomOp::PrependAbove { from, to } => self.prepend_window(*from, *to),
                DomOp::TrimTop { keep_top } => self.trim_top(*keep_top),
                DomOp::TrimBottom { keep_bottom } => self.trim_bottom(*keep_bottom),
                DomOp::FillPlaceholders => self.fill_placeholders(),
                DomOp::RefreshHeader => self.refresh_header(),
                DomOp::SetScrollTop(px) => {
                    self.dom.set_scroll_top(*px);
                    Ok(())
                }
                DomOp::RestoreScrollTop { row_index } => {
                    let spacer = self.spacer_height_px();
                    self.dom.restore_scroll_top(*row_index, spacer);
                    Ok(())
                }
                DomOp::ProgressiveLoader(active) => {
                    if *active {
                        self.start_progressive_loader();
                    } else {
                        self.stop_progressive_loader();
                    }
                    Ok(())
                }
                DomOp::FindCounter(state) => {
                    self.dom.set_find_counter_state(state);
                    self.sync_find_nav();
                    Ok(())
                }
                DomOp::RevealRow { abs } => self.dom.reveal_row(*abs),
                DomOp::SetFindHighlight { abs } => {
                    let Some(row) = self.state.cache.get(abs) else {
                        return Ok(());
                    };
                    let node_key = host::row::owned_str(row, "node_key");
                    self.state.selected_key = Some(node_key);
                    self.dom.apply_selection(*abs)?;
                    self.dom.set_find_highlight(*abs)
                }
                DomOp::ClearFindHighlight => {
                    self.state.clear_selection();
                    self.dom.clear_selection_ui()?;
                    self.dom.clear_find_highlight()
                }
            }
        }

        /// Apply all reducer DOM ops in order (called while the shell borrow is
        /// held, BEFORE the step's sends are posted). The roving-tabindex
        /// invariant and the find-nav enabled state are re-reconciled after
        /// every step so they survive any DOM mutation.
        fn apply_step_ops(&mut self, step: &Step) {
            for op in &step.ops {
                let result = self.apply_op(op);
                if let Err(error) = result {
                    let message = format!("DOM op failed: {}", js_value_text(&error));
                    record_error(&message);
                }
            }
            self.apply_roving_tabindex();
            self.sync_find_nav();
        }

        fn start_progressive_loader(&mut self) {
            if self.progressive_timer.is_some() {
                return;
            }
            let Some(window) = web_sys::window() else {
                return;
            };
            let closure = Closure::<dyn FnMut()>::wrap(Box::new(on_progressive_tick));
            let result = window.set_interval_with_callback_and_timeout_and_arguments_0(
                closure.as_ref().unchecked_ref(),
                250,
            );
            match result {
                Ok(handle) => {
                    self.progressive_timer = Some(handle);
                    closure.forget();
                }
                Err(error) => {
                    let message = format!(
                        "progressive loader failed to start: {}",
                        js_value_text(&error)
                    );
                    record_error(&message);
                }
            }
        }

        fn stop_progressive_loader(&mut self) {
            if let Some(handle) = self.progressive_timer.take() {
                if let Some(window) = web_sys::window() {
                    window.clear_interval_with_handle(handle);
                }
            }
        }

        /// Publish the SVG render state after DOM rows changed. The per-row SVG
        /// graph cells are painted at row-build time inside the scrolling DOM
        /// subtree, so there is no canvas surface to size, position, or chase
        /// the scroll offset. This pass keeps the debug facade's frame-row
        /// mirror (`#gpu-rows`), counters (`renderCount`/`generation`),
        /// metrics, and status text aligned with the old GPU frame contract.
        fn publish_render_state(&mut self) {
            let host_height = self.dom.client_height_css();
            let rows = dom::frame_rows(&self.state, self.dom.scroll_top(), host_height);
            let started = performance_now();
            self.last_frame_rows.clone_from(&rows);
            self.render_count = self.render_count.saturating_add(1);
            self.generation = self.generation.saturating_add(1);
            self.last_render_ms = Some(performance_now() - started);
            if self.first_window_ms.is_none() {
                self.first_window_ms = Some(performance_now() - self.started_at_ms);
            }
            self.last_error = None;
            set_window_prop("__editchainLastError", &JsValue::NULL);
            if let Err(error) = self.dom.mirror_rows(&rows) {
                let message = format!("gpu row mirror failed: {}", js_value_text(&error));
                record_error(&message);
            }
            let total = self.state.total.unwrap_or(0);
            self.dom
                .set_status(&format!("{} / {} rows", rows.len(), total));
        }

        /// Persist the step's save-state through the narrow binding.
        fn persist(&self, state: &Value) {
            let Some(js) = js_sys::JSON::parse(&state.to_string()).ok() else {
                return;
            };
            let vscode = self.vscode.clone();
            let result = set_state_(&vscode, &js);
            if let Err(error) = result {
                record_error(&format!("setState failed: {}", js_value_text(&error)));
            }
        }
    }

    /// The window handle (the shell runs inside a browser context).
    fn window_handle() -> Option<web_sys::Window> {
        web_sys::window()
    }

    /// Current performance clock in ms.
    fn performance_now() -> f64 {
        window_handle()
            .and_then(|window| window.performance())
            .map_or(0.0, |performance| performance.now())
    }

    /// Set a property on the JS window (debug hooks the harness reads).
    fn set_window_prop(name: &str, value: &JsValue) {
        let Some(window) = window_handle() else {
            return;
        };
        let window_value: JsValue = window.into();
        drop(js_sys::Reflect::set(
            &window_value,
            &JsValue::from_str(name),
            value,
        ));
    }

    /// Record a shell error: console + `__editchainLastError` + shell field.
    fn record_error(message: &str) {
        web_sys::console::error_1(&JsValue::from_str(message));
        set_window_prop("__editchainLastError", &JsValue::from_str(message));
        SHELL_DATA.with(|cell| {
            if let Some(shell) = cell.borrow_mut().as_mut() {
                shell.last_error = Some(message.to_owned());
            }
        });
    }

    /// Parse one host message's structured-clone data into JSON.
    fn message_value(data: &JsValue) -> Value {
        let string = js_sys::JSON::stringify(data)
            .map(|js| js.as_string().unwrap_or_default())
            .unwrap_or_default();
        serde_json::from_str(&string).unwrap_or(Value::Null)
    }

    /// Render a JS value as diagnostic text.
    fn js_value_text(value: &JsValue) -> String {
        js_sys::JSON::stringify(value)
            .map(|js| js.as_string().unwrap_or_default())
            .unwrap_or_default()
    }

    /// Call one method on the acquired VS Code API through `Reflect` (the
    /// narrow binding surface: acquisition is wasm-bindgen, dispatch is
    /// structural).
    fn vscode_call(api: &JsValue, method: &str, args: &[JsValue]) -> Result<JsValue, JsValue> {
        let function: js_sys::Function =
            js_sys::Reflect::get(api, &JsValue::from_str(method))?.dyn_into()?;
        let arguments: js_sys::Array = args.iter().collect();
        js_sys::Reflect::apply(&function, api, &arguments)
    }

    /// `vscode.postMessage(message)`.
    fn post_message_(api: &JsValue, message: &JsValue) -> Result<(), JsValue> {
        vscode_call(api, "postMessage", std::slice::from_ref(message)).map(|_| ())
    }

    /// `vscode.getState()` (best-effort: returns `undefined` on failure).
    fn get_state_(api: &JsValue) -> JsValue {
        vscode_call(api, "getState", &[]).unwrap_or(JsValue::UNDEFINED)
    }

    /// `vscode.setState(state)`.
    fn set_state_(api: &JsValue, state: &JsValue) -> Result<(), JsValue> {
        vscode_call(api, "setState", std::slice::from_ref(state)).map(|_| ())
    }

    /// Post one host envelope through the narrow binding.
    fn post_envelope(envelope: &Value) {
        POST_QUEUE.with(|queue| queue.borrow_mut().push_back(envelope.clone()));
        schedule_post_flush();
    }

    /// Queue the deferred host-post flush on the microtask queue. Each
    /// `postMessage` runs in its own microtask, so the fixture bridge's
    /// synchronous response dispatch can never re-enter a wasm-bindgen
    /// `Closure` that is still on the stack.
    fn schedule_post_flush() {
        if FLUSH_ACTIVE.with(Cell::get) {
            return;
        }
        FLUSH_ACTIVE.with(|cell| cell.set(true));
        wasm_bindgen_futures::spawn_local(async {
            loop {
                // Yield first: the flush must never post from inside the
                // closure stack that queued the envelope.
                let yielded = js_sys::Promise::resolve(&JsValue::UNDEFINED);
                let _resolved: Result<JsValue, JsValue> =
                    wasm_bindgen_futures::JsFuture::from(yielded).await;
                let envelope = POST_QUEUE.with(|queue| queue.borrow_mut().pop_front());
                let Some(envelope) = envelope else {
                    FLUSH_ACTIVE.with(|cell| cell.set(false));
                    break;
                };
                let parsed = js_sys::JSON::parse(&envelope.to_string()).ok();
                let Some(message) = parsed else {
                    continue;
                };
                let vscode = SHELL_DATA.with(|cell| {
                    cell.borrow_mut()
                        .as_mut()
                        .map_or(JsValue::UNDEFINED, |shell| shell.vscode.clone())
                });
                let posted = post_message_(&vscode, &message);
                if let Err(error) = posted {
                    record_error(&format!("postMessage failed: {}", js_value_text(&error)));
                }
            }
        });
    }

    fn js_value_error(message: &str) -> JsValue {
        JsValue::from_str(message)
    }

    /// Execute one host send (called with NO shell borrow held — the fixture
    /// bridge may re-enter the listener synchronously).
    fn execute_send(send: &Send) {
        match send {
            Send::Request { id, body } => {
                let envelope = json!({ "id": *id, "body": body });
                post_envelope(&envelope);
            }
            Send::Log(text) => {
                web_sys::console::info_1(&JsValue::from_str(text));
                post_envelope(&json!({ "type": "log", "text": text }));
            }
            Send::Status { loaded, total } => {
                post_envelope(&json!({ "type": "status", "loaded": *loaded, "total": *total }));
            }
            Send::StatusText(text) => {
                SHELL_DATA.with(|cell| {
                    if let Some(shell) = cell.borrow_mut().as_mut() {
                        shell.dom.announce(text);
                    }
                });
                post_envelope(&json!({ "type": "statusText", "text": text }));
            }
            Send::OpenJson(body) => {
                post_envelope(body);
            }
            Send::WebviewReady(instance_id) => {
                post_envelope(&json!({ "type": "webviewReady", "instanceId": instance_id }));
            }
        }
    }

    /// Execute a step's sends (no shell borrow held).
    fn execute_sends(sends: Vec<Send>) {
        for send in sends {
            execute_send(&send);
        }
    }

    /// One bounded transition's host output.
    #[derive(Debug, Default)]
    struct TransitionOutput {
        sends: Vec<Send>,
        save_state: Option<Value>,
    }

    /// Run one bounded transition: borrow the shell, capture the pre-window,
    /// mutate state, apply DOM ops, and return the sends to post. The sends
    /// execute with no borrow held (responses may re-enter synchronously), the
    /// step's save-state persists after them, and a GPU frame is scheduled
    /// once the DOM settled.
    fn run_transition(transition: impl FnOnce(&mut ShellData) -> TransitionOutput) {
        if TRANSITION_ACTIVE.with(Cell::get) {
            // A synchronous DOM event (e.g. `focusin` from `element.focus()`)
            // fired inside another transition; the outer transition already
            // owns the state change, so a nested borrow would panic.
            return;
        }
        TRANSITION_ACTIVE.with(|cell| cell.set(true));
        let mut output = SHELL_DATA.with(|cell| {
            let mut borrow = cell.borrow_mut();
            let Some(shell) = borrow.as_mut() else {
                return TransitionOutput::default();
            };
            transition(shell)
        });
        let sends = std::mem::take(&mut output.sends);
        execute_sends(sends);
        if let Some(save_state) = output.save_state.take() {
            SHELL_DATA.with(|cell| {
                if let Some(shell) = cell.borrow_mut().as_mut() {
                    shell.persist(&save_state);
                }
            });
        }
        after_transition();
        TRANSITION_ACTIVE.with(|cell| cell.set(false));
    }

    /// Host message listener: queue the payload and drain the pump. The
    /// fixture bridge dispatches responses synchronously inside `postMessage`,
    /// so this may re-enter while sends are executing; the queue keeps the
    /// ordering deterministic.
    fn on_message_event(event: web_sys::Event) {
        let message: web_sys::MessageEvent = event.unchecked_into();
        MSG_QUEUE.with(|queue| queue.borrow_mut().push_back(message.data()));
        pump_messages();
    }

    /// Scroll handler: keep the bounded window in sync with the viewport.
    fn on_scroll() {
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

    /// Activity/Raw profile control click.
    fn on_profile(next: Profile) {
        run_transition(|shell| {
            let viewport = shell.dom.viewport();
            let mut step = Step::new();
            shell
                .state
                .set_profile(next, ProfileAction::Reset, &viewport, &mut step);
            shell.apply_step_ops(&step);
            shell.dom.set_profile_ui(next);
            TransitionOutput {
                sends: std::mem::take(&mut step.sends),
                save_state: step.save_state.take(),
            }
        });
    }

    /// Terminal request-error Retry button (named recovery per the error's
    /// `RetryAction`: reset the history, or re-issue the legacy Search query).
    fn on_retry(action: RetryAction) {
        run_transition(|shell| {
            let viewport = shell.dom.viewport();
            let mut step = Step::new();
            match action {
                RetryAction::ResetHistory => shell.state.reset_history(&viewport, &mut step),
                RetryAction::ResubmitSearch => {
                    let query = shell.state.search_query.clone();
                    shell.state.submit_legacy_search(&query, &mut step);
                }
            }
            shell.apply_step_ops(&step);
            TransitionOutput {
                sends: std::mem::take(&mut step.sends),
                save_state: step.save_state.take(),
            }
        });
    }

    /// Progressive loader tick: buffer history ahead of the scroll position.
    fn on_progressive_tick() {
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

    /// Exit the search interaction: legacy flat-list mode resets to the full
    /// history; an in-place find session just clears (chain untouched).
    fn exit_search() {
        run_transition(|shell| {
            let viewport = shell.dom.viewport();
            let mut step = Step::new();
            if shell.state.view_flags.search_mode {
                shell.state.reset_history(&viewport, &mut step);
            } else {
                shell.state.clear_find(&mut step);
            }
            shell.apply_step_ops(&step);
            TransitionOutput {
                sends: std::mem::take(&mut step.sends),
                save_state: step.save_state.take(),
            }
        });
    }

    /// Find-in-chain / legacy-search keyboard behaviour (production keydown):
    /// Enter submits / advances the same settled query; Shift+Enter steps
    /// back; Escape/empty clears; ArrowDown/Up navigate settled matches for
    /// the exact submitted query (or the flat result list in legacy mode).
    fn on_search_keydown(event: &web_sys::KeyboardEvent) {
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
                    shell.state.find_active() && trimmed == shell.state.search_query
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
                return;
            }
            let legacy = SHELL_DATA.with(|cell| {
                cell.borrow().as_ref().is_some_and(|shell| {
                    shell.state.view_flags.search_mode
                        && shell.state.total.unwrap_or(0) > 0
                        && trimmed == shell.state.search_query
                        && shell.state.current_search_epoch().is_none()
                })
            });
            if legacy {
                event.prevent_default();
                let target = SHELL_DATA.with(|cell| {
                    cell.borrow().as_ref().map_or(0, |shell| {
                        let total = shell.state.total.unwrap_or(0);
                        if key == "ArrowDown" {
                            0
                        } else {
                            total.saturating_sub(1).max(0)
                        }
                    })
                });
                run_transition(|shell| {
                    shell.focus_search_result(target);
                    TransitionOutput::default()
                });
            }
        }
    }

    /// `input` handler: an emptied input exits the find/search interaction;
    /// edited-but-unsubmitted text disables the nav buttons (the guard lives
    /// in the shell's `sync_find_nav`).
    fn on_search_input() {
        run_transition(|shell| {
            let viewport = shell.dom.viewport();
            let mut step = Step::new();
            if shell.dom.search_input_value().is_empty() {
                if shell.state.view_flags.search_mode {
                    shell.state.reset_history(&viewport, &mut step);
                } else {
                    shell.state.clear_find(&mut step);
                }
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
    fn on_search_nav(delta: isize) {
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
    fn on_row_click(event: &web_sys::Event) {
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

    /// Double-click on a row: select and open its raw JSON (explicit gesture).
    fn on_row_dblclick(event: &web_sys::Event) {
        let Some(target) = event.target() else {
            return;
        };
        if target_inside(&target, "button") {
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
    fn on_row_focusin(event: &web_sys::Event) {
        let Some(target) = event.target() else {
            return;
        };
        let Some(abs) = closest_row_abs(&target) else {
            return;
        };
        run_transition(|shell| {
            if abs != shell.state.roving_abs {
                shell.state.set_roving_abs(abs);
                shell.apply_roving_tabindex();
            }
            TransitionOutput::default()
        });
    }

    /// Roving keyboard navigation over the rendered rows: ArrowUp/Down move
    /// focus (wrapping in legacy search mode), Home/End jump to the window
    /// edges, ArrowRight/ArrowLeft toggle expandable rows, Enter/Space
    /// activate (disclosure for expandable rows, raw JSON for ordinary ones).
    fn on_row_keydown(event: &web_sys::KeyboardEvent) {
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
                        if shell.state.view_flags.search_mode {
                            moved.filter(|index| *index < len).unwrap_or_else(|| {
                                if direction > 0 {
                                    0
                                } else {
                                    len.saturating_sub(1)
                                }
                            })
                        } else if let Some(moved) = moved.filter(|index| *index < len) {
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
                    if shell.state.view_flags.search_mode {
                        shell.state.set_roving_abs(*target_abs);
                        shell.apply_roving_tabindex();
                        shell.state.select_row(*target_abs);
                        if let Err(error) = shell.dom.apply_selection(*target_abs) {
                            record_error(&format!(
                                "selection apply failed: {}",
                                js_value_text(&error)
                            ));
                        }
                        if let Err(error) = shell.dom.reveal_row(*target_abs) {
                            record_error(&format!("reveal failed: {}", js_value_text(&error)));
                        }
                        let options = web_sys::FocusOptions::new();
                        options.set_prevent_scroll(true);
                        drop(target.focus_with_options(&options));
                    } else {
                        shell.state.set_roving_abs(*target_abs);
                        shell.apply_roving_tabindex();
                        drop(target.focus());
                    }
                    TransitionOutput::default()
                });
            }
            "ArrowRight" | "ArrowLeft" => {
                let expanded = SHELL_DATA.with(|cell| {
                    cell.borrow().as_ref().is_some_and(|shell| {
                        shell
                            .state
                            .block_index_of_abs(abs)
                            .is_some_and(|block| shell.state.is_block_expanded(block))
                    })
                });
                let wants_toggle = SHELL_DATA.with(|cell| {
                    cell.borrow().as_ref().is_some_and(|shell| {
                        let row = shell.state.cache.get(&abs);
                        let Some(row) = row else {
                            return false;
                        };
                        if host::row::bool(row, "is_subop") || !rows::has_sub_ops(row) {
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
                        shell.state.cache.get(&abs).is_some_and(|row| {
                            !host::row::bool(row, "is_subop") && rows::has_sub_ops(row)
                        })
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
                    if key == "Enter" {
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
    fn on_rows_mousedown(event: &web_sys::Event) {
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
        start_column_drag(col, f64::from(mouse.client_x()));
    }

    /// The current effective width of a resizable column (drag start).
    fn column_start_width(shell: &ShellData, col: ColKey) -> f64 {
        match col {
            ColKey::Graph => shell.current_graph_width(),
            ColKey::Content | ColKey::Date | ColKey::Author | ColKey::Commit => {
                shell.dom.header_cell_width(col).max(col.min_width())
            }
        }
    }

    /// Begin a divider drag: pin the start geometry, mark the body, and
    /// install window-level move/up listeners (removed on mouseup).
    fn start_column_drag(col: ColKey, client_x: f64) {
        let start_w = SHELL_DATA.with(|cell| {
            cell.borrow()
                .as_ref()
                .map_or(0.0, |shell| column_start_width(shell, col))
        });
        let Some(window) = web_sys::window() else {
            return;
        };
        let move_closure =
            Closure::<dyn FnMut(web_sys::Event)>::wrap(Box::new(|event: web_sys::Event| {
                on_column_move(&event);
            }));
        let up_closure =
            Closure::<dyn FnMut(web_sys::Event)>::wrap(Box::new(|event: web_sys::Event| {
                on_column_up(&event);
            }));
        drop(
            window.add_event_listener_with_callback(
                "mousemove",
                move_closure.as_ref().unchecked_ref(),
            ),
        );
        drop(
            window.add_event_listener_with_callback("mouseup", up_closure.as_ref().unchecked_ref()),
        );
        let body = web_sys::window()
            .and_then(|window| window.document())
            .and_then(|document| document.body());
        if let Some(body) = body {
            drop(body.class_list().add_1("col-resizing"));
        }
        COLUMN_DRAG.with(|cell| {
            *cell.borrow_mut() = Some(ColumnDrag {
                col,
                start_x: client_x,
                start_w,
                move_closure: Some(move_closure),
                up_closure: Some(up_closure),
            });
        });
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
            let (top, bottom) = if shell.state.view_flags.search_mode {
                (0, shell.state.visible_total().saturating_sub(1).max(0))
            } else {
                (shell.state.render_top, shell.state.render_bottom)
            };
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
        let Some(window) = web_sys::window() else {
            return;
        };
        if let Some(move_closure) = &drag.move_closure {
            drop(window.remove_event_listener_with_callback(
                "mousemove",
                move_closure.as_ref().unchecked_ref(),
            ));
        }
        if let Some(up_closure) = &drag.up_closure {
            drop(window.remove_event_listener_with_callback(
                "mouseup",
                up_closure.as_ref().unchecked_ref(),
            ));
        }
        let body = web_sys::window()
            .and_then(|window| window.document())
            .and_then(|document| document.body());
        if let Some(body) = body {
            drop(body.class_list().remove_1("col-resizing"));
        }
    }

    /// `ResizeObserver` / window-resize entry: ignore height-only changes, then
    /// debounce the full layout re-render (`onViewportResize`, 150ms).
    fn on_resize_observed() {
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
            if let Some(handle) = shell.resize_timer.take() {
                if let Some(window) = web_sys::window() {
                    window.clear_timeout_with_handle(handle);
                }
            }
            true
        });
        if !changed {
            return;
        }
        let Some(window) = web_sys::window() else {
            return;
        };
        let closure = Closure::<dyn FnMut()>::wrap(Box::new(on_resize_debounced));
        let scheduled = window.set_timeout_with_callback_and_timeout_and_arguments_0(
            closure.as_ref().unchecked_ref(),
            150,
        );
        match scheduled {
            Ok(handle) => {
                closure.forget();
                SHELL_DATA.with(|cell| {
                    if let Some(shell) = cell.borrow_mut().as_mut() {
                        shell.resize_timer = Some(handle);
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
            if shell.state.view_flags.search_mode {
                step.ops.push(DomOp::Reanchor {
                    top: 0,
                    bottom: shell.state.visible_total().saturating_sub(1).max(0),
                });
            } else {
                step.ops.push(DomOp::Reanchor {
                    top: shell.state.render_top,
                    bottom: shell.state.render_bottom,
                });
                shell.state.sync_window(&viewport, &mut step);
                shell.state.fetch_window(&viewport, &mut step);
            }
            shell.apply_step_ops(&step);
            TransitionOutput {
                sends: std::mem::take(&mut step.sends),
                save_state: step.save_state.take(),
            }
        });
    }

    /// Install the read-only harness parity hooks (`__editchainGetProfile`,
    /// `__editchainGetTotal`, `__editchainRowAt`, `__editchainSetProfile`).
    /// These are pure facades over the live shell — no JS app state or
    /// business logic lives here.
    fn install_parity_hooks() {
        let get_profile = Closure::<dyn FnMut() -> String>::wrap(Box::new(|| {
            SHELL_DATA.with(|cell| {
                cell.borrow().as_ref().map_or_else(
                    || "activity".to_owned(),
                    |shell| match shell.state.profile {
                        Profile::Activity => "activity".to_owned(),
                        Profile::Raw => "raw".to_owned(),
                    },
                )
            })
        }));
        let get_total = Closure::<dyn FnMut() -> f64>::wrap(Box::new(|| {
            SHELL_DATA.with(|cell| {
                cell.borrow().as_ref().map_or(-1.0, |shell| {
                    dom::i64_to_f64(shell.state.total.unwrap_or(-1))
                })
            })
        }));
        let row_at = Closure::<dyn FnMut(f64) -> JsValue>::wrap(Box::new(|abs: f64| {
            SHELL_DATA.with(|cell| {
                let shell_ref = cell.borrow();
                let Some(shell) = shell_ref.as_ref() else {
                    return JsValue::NULL;
                };
                let index = dom::f64_round_to_i64(abs);
                let Some(row) = shell.state.cache.get(&index) else {
                    return JsValue::NULL;
                };
                js_sys::JSON::parse(&row.to_string()).unwrap_or(JsValue::NULL)
            })
        }));
        let set_profile = Closure::<dyn FnMut(String)>::wrap(Box::new(|name: String| {
            let next = if name == "raw" {
                Profile::Raw
            } else {
                Profile::Activity
            };
            on_profile(next);
        }));
        set_window_prop(
            "__editchainGetProfile",
            get_profile.as_ref().unchecked_ref(),
        );
        set_window_prop("__editchainGetTotal", get_total.as_ref().unchecked_ref());
        set_window_prop("__editchainRowAt", row_at.as_ref().unchecked_ref());
        set_window_prop(
            "__editchainSetProfile",
            set_profile.as_ref().unchecked_ref(),
        );
        // The window props hold the JS functions; the wasm closures leak
        // deliberately for the shell's lifetime.
        get_profile.forget();
        get_total.forget();
        row_at.forget();
        set_profile.forget();
    }

    /// Drain queued host messages and any synchronous responses they trigger.
    fn pump_messages() {
        if PUMP_ACTIVE.with(Cell::get) || TRANSITION_ACTIVE.with(Cell::get) {
            return;
        }
        PUMP_ACTIVE.with(|cell| cell.set(true));
        TRANSITION_ACTIVE.with(|cell| cell.set(true));
        loop {
            let message = MSG_QUEUE.with(|queue| queue.borrow_mut().pop_front());
            let Some(message) = message else {
                break;
            };
            let mut output = SHELL_DATA.with(|cell| {
                let mut borrow = cell.borrow_mut();
                let Some(shell) = borrow.as_mut() else {
                    return TransitionOutput::default();
                };
                let Some(parsed) = host::HostMessage::parse(&message_value(&message)) else {
                    return TransitionOutput::default();
                };
                let viewport = shell.dom.viewport();
                let mut step = Step::new();
                shell
                    .state
                    .handle_host_message(parsed, &viewport, &mut step);
                shell.apply_step_ops(&step);
                TransitionOutput {
                    sends: std::mem::take(&mut step.sends),
                    save_state: step.save_state.take(),
                }
            });
            let sends = std::mem::take(&mut output.sends);
            execute_sends(sends);
            if let Some(save_state) = output.save_state.take() {
                SHELL_DATA.with(|cell| {
                    if let Some(shell) = cell.borrow_mut().as_mut() {
                        shell.persist(&save_state);
                    }
                });
            }
        }
        PUMP_ACTIVE.with(|cell| cell.set(false));
        TRANSITION_ACTIVE.with(|cell| cell.set(false));
        after_transition();
    }

    /// Post-step sync: mirror readiness flags to the window debug properties
    /// and publish the SVG render state after rows changed.
    fn after_transition() {
        SHELL_DATA.with(|cell| {
            let mut borrow = cell.borrow_mut();
            let Some(shell) = borrow.as_mut() else {
                return;
            };
            sync_debug_props_locked(shell);
            shell.publish_render_state();
        });
    }

    /// Mirror shell/state flags onto the window (debug contract).
    fn sync_debug_props() {
        SHELL_DATA.with(|cell| {
            if let Some(shell) = cell.borrow_mut().as_mut() {
                sync_debug_props_locked(shell);
            }
        });
    }

    fn sync_debug_props_locked(shell: &mut ShellData) {
        let ready = shell.state.view_flags.data_ready;
        set_window_prop("__editchainDataReady", &JsValue::from_bool(ready));
        set_window_prop(
            "__editchainInFlightCount",
            &js_sys::Number::from(u32::try_from(shell.state.in_flight.len()).unwrap_or(u32::MAX))
                .into(),
        );
        set_window_prop(
            "__editchainViewGen",
            &js_sys::Number::from(u32::try_from(shell.state.view_gen).unwrap_or(u32::MAX)).into(),
        );
        set_window_prop(
            "__editchainGeneration",
            &js_sys::Number::from(u32::try_from(shell.generation).unwrap_or(u32::MAX)).into(),
        );
        set_window_prop(
            "__editchainRenderCount",
            &js_sys::Number::from(u32::try_from(shell.render_count).unwrap_or(u32::MAX)).into(),
        );
        set_window_prop(
            "__editchainRendererReady",
            &JsValue::from_bool(shell.flags.renderer_ready),
        );
        set_window_prop(
            "__editchainWasmReady",
            &JsValue::from_bool(shell.flags.wasm_ready),
        );
        set_window_prop(
            "__editchainRendererInstanceId",
            &JsValue::from_str(&shell.instance_id),
        );
        let profile = match shell.state.profile {
            Profile::Activity => "activity",
            Profile::Raw => "raw",
        };
        set_window_prop("__editchainProfile", &JsValue::from_str(profile));
    }

    /// Install the shell, listeners, and webviewReady handshake.
    fn install_shell() -> Result<(), JsValue> {
        // Surface Rust panic messages before the wasm abort trap: without a
        // hook, a panic in a wasm32-unknown-unknown release build is a silent
        // "unreachable" and leaves the loader's marker stuck on loading.
        std::panic::set_hook(Box::new(|info| {
            let message = format!("{info}");
            web_sys::console::error_1(&JsValue::from_str(&message));
            set_window_prop("__editchainLastError", &JsValue::from_str(&message));
        }));
        let vscode = acquire_vscode_api()?;
        let dom = HistoryDom::new()?;
        let initial_rows_width = f64::from(dom.rows().client_width());
        let mut state = HistoryAppState::default();
        let restored = get_state_(&vscode);
        if let Ok(value) = js_sys::JSON::stringify(&restored) {
            if let Some(text) = value.as_string() {
                state.persisted = serde_json::from_str(&text).ok();
            }
        }
        let shell = ShellData {
            flags: ShellFlags::default(),
            vscode,
            dom,
            state,
            instance_id: new_instance_id(),
            render_count: 0,
            generation: 0,
            started_at_ms: performance_now(),
            first_window_ms: None,
            last_render_ms: None,
            last_frame_rows: Vec::new(),
            last_error: None,
            progressive_timer: None,
            resize_timer: None,
            last_rows_width: initial_rows_width,
            resize_observer: None,
            col_widths: dom::ColWidths::default(),
        };
        shell.dom.set_profile_ui(shell.state.profile);
        shell.dom.set_status("idle");
        shell.dom.set_find_nav(false);
        let rows_el = shell.dom.rows();
        let search_input = shell.dom.search_input();
        let search_prev = shell.dom.search_prev_button();
        let search_next = shell.dom.search_next_button();
        let profile_activity = shell.dom.profile_activity_button();
        let profile_raw = shell.dom.profile_raw_button();
        SHELL_DATA.with(|cell| drop(cell.borrow_mut().replace(shell)));

        let window =
            web_sys::window().ok_or_else(|| js_value_error("browser window is unavailable"))?;
        let message_closure =
            Closure::<dyn FnMut(web_sys::Event)>::wrap(Box::new(on_message_event));
        window.add_event_listener_with_callback(
            "message",
            message_closure.as_ref().unchecked_ref(),
        )?;
        message_closure.forget();
        let scroll_closure = Closure::<dyn FnMut()>::wrap(Box::new(on_scroll));
        rows_el
            .add_event_listener_with_callback("scroll", scroll_closure.as_ref().unchecked_ref())?;
        scroll_closure.forget();
        let activity_closure =
            Closure::<dyn FnMut()>::wrap(Box::new(|| on_profile(Profile::Activity)));
        profile_activity
            .add_event_listener_with_callback("click", activity_closure.as_ref().unchecked_ref())?;
        activity_closure.forget();
        let raw_closure = Closure::<dyn FnMut()>::wrap(Box::new(|| on_profile(Profile::Raw)));
        profile_raw
            .add_event_listener_with_callback("click", raw_closure.as_ref().unchecked_ref())?;
        raw_closure.forget();

        // Search controls: keyboard (Enter/Escape/Arrow), input-clearing, and
        // the Previous/Next buttons (mousedown keeps focus in the input).
        let search_keydown_closure = Closure::<dyn FnMut(web_sys::KeyboardEvent)>::wrap(Box::new(
            |event: web_sys::KeyboardEvent| on_search_keydown(&event),
        ));
        search_input.add_event_listener_with_callback(
            "keydown",
            search_keydown_closure.as_ref().unchecked_ref(),
        )?;
        search_keydown_closure.forget();
        let search_input_closure =
            Closure::<dyn FnMut(web_sys::Event)>::wrap(Box::new(|_| on_search_input()));
        search_input.add_event_listener_with_callback(
            "input",
            search_input_closure.as_ref().unchecked_ref(),
        )?;
        search_input_closure.forget();
        let prev_mousedown =
            Closure::<dyn FnMut(web_sys::Event)>::wrap(Box::new(|event: web_sys::Event| {
                let mouse: web_sys::MouseEvent = event.unchecked_into();
                mouse.prevent_default();
            }));
        search_prev.add_event_listener_with_callback(
            "mousedown",
            prev_mousedown.as_ref().unchecked_ref(),
        )?;
        prev_mousedown.forget();
        let next_mousedown =
            Closure::<dyn FnMut(web_sys::Event)>::wrap(Box::new(|event: web_sys::Event| {
                let mouse: web_sys::MouseEvent = event.unchecked_into();
                mouse.prevent_default();
            }));
        search_next.add_event_listener_with_callback(
            "mousedown",
            next_mousedown.as_ref().unchecked_ref(),
        )?;
        next_mousedown.forget();
        let prev_click =
            Closure::<dyn FnMut(web_sys::Event)>::wrap(Box::new(|_| on_search_nav(-1)));
        search_prev
            .add_event_listener_with_callback("click", prev_click.as_ref().unchecked_ref())?;
        prev_click.forget();
        let next_click = Closure::<dyn FnMut(web_sys::Event)>::wrap(Box::new(|_| on_search_nav(1)));
        search_next
            .add_event_listener_with_callback("click", next_click.as_ref().unchecked_ref())?;
        next_click.forget();

        // Delegated row interactions: click (with detail guard + chevron),
        // double-click (raw JSON), focusin (roving anchor), keydown (roving
        // navigation / disclosure / activation), and mousedown (divider drag).
        let rows_click =
            Closure::<dyn FnMut(web_sys::Event)>::wrap(Box::new(|event: web_sys::Event| {
                on_row_click(&event);
            }));
        rows_el.add_event_listener_with_callback("click", rows_click.as_ref().unchecked_ref())?;
        rows_click.forget();
        let rows_dblclick =
            Closure::<dyn FnMut(web_sys::Event)>::wrap(Box::new(|event: web_sys::Event| {
                on_row_dblclick(&event);
            }));
        rows_el
            .add_event_listener_with_callback("dblclick", rows_dblclick.as_ref().unchecked_ref())?;
        rows_dblclick.forget();
        let rows_focusin =
            Closure::<dyn FnMut(web_sys::Event)>::wrap(Box::new(|event: web_sys::Event| {
                on_row_focusin(&event);
            }));
        rows_el
            .add_event_listener_with_callback("focusin", rows_focusin.as_ref().unchecked_ref())?;
        rows_focusin.forget();
        let rows_keydown = Closure::<dyn FnMut(web_sys::KeyboardEvent)>::wrap(Box::new(
            |event: web_sys::KeyboardEvent| on_row_keydown(&event),
        ));
        rows_el
            .add_event_listener_with_callback("keydown", rows_keydown.as_ref().unchecked_ref())?;
        rows_keydown.forget();
        let rows_mousedown =
            Closure::<dyn FnMut(web_sys::Event)>::wrap(Box::new(|event: web_sys::Event| {
                on_rows_mousedown(&event);
            }));
        rows_el.add_event_listener_with_callback(
            "mousedown",
            rows_mousedown.as_ref().unchecked_ref(),
        )?;
        rows_mousedown.forget();

        // Viewport resize: window resize + a ResizeObserver over #rows
        // (production `onViewportResize`, debounced).
        let window_resize =
            Closure::<dyn FnMut(web_sys::Event)>::wrap(Box::new(|_| on_resize_observed()));
        window
            .add_event_listener_with_callback("resize", window_resize.as_ref().unchecked_ref())?;
        window_resize.forget();
        let resize_callback = Closure::<
            dyn FnMut(Vec<web_sys::ResizeObserverEntry>, web_sys::ResizeObserver),
        >::wrap(Box::new(|_, _| on_resize_observed()));
        let observer_fn: js_sys::Function = resize_callback
            .as_ref()
            .unchecked_ref::<js_sys::Function>()
            .clone();
        resize_callback.forget();
        match web_sys::ResizeObserver::new(&observer_fn) {
            Ok(resize_observer) => {
                resize_observer.observe(&rows_el);
                SHELL_DATA.with(|cell| {
                    if let Some(shell) = cell.borrow_mut().as_mut() {
                        shell.resize_observer = Some(resize_observer);
                    }
                });
            }
            Err(_) => {
                record_error(
                    "ResizeObserver is unavailable; viewport resize falls back to window resize",
                );
            }
        }

        install_parity_hooks();

        // Readiness handshake AFTER the listener exists (synchronous fixture
        // replies must correlate; see the Send::WebviewReady contract).
        let instance_id = SHELL_DATA.with(|cell| {
            cell.borrow_mut()
                .as_mut()
                .map(|shell| shell.instance_id.clone())
                .unwrap_or_default()
        });
        execute_send(&Send::WebviewReady(instance_id));
        SHELL_DATA.with(|cell| {
            if let Some(shell) = cell.borrow_mut().as_mut() {
                shell.flags.wasm_ready = true;
            }
        });
        sync_debug_props();
        Ok(())
    }

    /// A per-view renderer-instance id (production `rendererInstanceId`).
    fn new_instance_id() -> String {
        let now = js_sys::Number::from(js_sys::Date::now())
            .to_string_with_radix(36)
            .unwrap_or_default();
        let random = js_sys::Number::from(js_sys::Math::random())
            .to_string_with_radix(36)
            .unwrap_or_default();
        let suffix = random.slice(2, random.length());
        format!("{now}-{suffix}")
    }

    /// WASM startup entry: install the Rust shell (listener before
    /// `webviewReady`) and mark the per-row SVG renderer ready. No wgpu
    /// surface is created on this path — the graph lives inside the scrolling
    /// row DOM, so there is nothing to position or chase.
    ///
    /// # Errors
    ///
    /// Returns an error when the shell scaffold or the VS Code API is
    /// unavailable.
    #[wasm_bindgen(js_name = "startHistoryView")]
    pub fn start_history_view() -> Result<(), JsValue> {
        install_shell()?;
        SHELL_DATA.with(|cell| {
            let mut borrow = cell.borrow_mut();
            let Some(shell) = borrow.as_mut() else {
                return;
            };
            shell.flags.renderer_ready = true;
            shell.last_error = None;
            shell.dom.set_backend("svg", "svg");
            shell.publish_render_state();
        });
        set_window_prop("__editchainLastError", &JsValue::NULL);
        sync_debug_props();
        Ok(())
    }

    /// `__editchainDataReady` mirror: content has rendered for the view.
    #[wasm_bindgen(js_name = "debugDataReady")]
    #[must_use]
    pub fn debug_data_ready() -> bool {
        SHELL_DATA.with(|cell| {
            cell.borrow()
                .as_ref()
                .is_some_and(|shell| shell.state.view_flags.data_ready)
        })
    }

    /// `__editchainGetTotal` data source: the authoritative history total, or
    /// `-1` while unknown.
    #[wasm_bindgen(js_name = "debugTotal")]
    #[must_use]
    pub fn debug_total() -> f64 {
        SHELL_DATA.with(|cell| {
            cell.borrow()
                .as_ref()
                .and_then(|shell| shell.state.total)
                .map_or(-1.0, dom::i64_to_f64)
        })
    }

    /// `__editchainGetProfile` data source: `activity` or `raw`.
    #[wasm_bindgen(js_name = "debugProfile")]
    #[must_use]
    pub fn debug_profile() -> String {
        SHELL_DATA.with(|cell| {
            let profile = cell
                .borrow()
                .as_ref()
                .map_or(Profile::Activity, |shell| shell.state.profile);
            match profile {
                Profile::Activity => "activity".to_owned(),
                Profile::Raw => "raw".to_owned(),
            }
        })
    }

    /// `__editchainGpuDebug.findState()` data source: the settled find session
    /// (read-only parity facade; never app state).
    #[wasm_bindgen(js_name = "debugFindState")]
    #[must_use]
    pub fn debug_find_state() -> String {
        SHELL_DATA.with(|cell| {
            let borrow = cell.borrow();
            let Some(shell) = borrow.as_ref() else {
                return String::new();
            };
            json!({
                "active": shell.state.find_active(),
                "index": shell.state.find_index(),
                "total": shell.state.find_total(),
                "more": shell.state.find_more(),
                "epoch": shell.state.current_search_epoch(),
                "currentRow": shell.state.current_find_match().map(|found| found.row),
            })
            .to_string()
        })
    }

    /// `__editchainRendererInstanceId` data source.
    #[wasm_bindgen(js_name = "debugRendererInstanceId")]
    #[must_use]
    pub fn debug_renderer_instance_id() -> String {
        SHELL_DATA.with(|cell| {
            cell.borrow()
                .as_ref()
                .map(|shell| shell.instance_id.clone())
                .unwrap_or_default()
        })
    }

    /// `__editchainGraphState` data source (render window + lane geometry).
    #[wasm_bindgen(js_name = "debugGraphState")]
    #[must_use]
    pub fn debug_graph_state() -> String {
        SHELL_DATA.with(|cell| {
            let borrow = cell.borrow();
            let Some(shell) = borrow.as_ref() else {
                return String::new();
            };
            json!({
                "renderTop": shell.state.render_top,
                "renderBottom": shell.state.render_bottom,
                "maxLane": shell.state.max_lane,
                "layoutReady": shell.state.session_flags.layout_ready,
                "graphWidth": shell.current_graph_width(),
            })
            .to_string()
        })
    }

    /// `__editchainGraphAdapter.laneXAll` data source (fixed CSS-px centers).
    #[wasm_bindgen(js_name = "debugLaneXAll")]
    #[must_use]
    pub fn debug_lane_x_all() -> String {
        SHELL_DATA.with(|cell| {
            let borrow = cell.borrow();
            let Some(shell) = borrow.as_ref() else {
                return String::new();
            };
            json!(shell.layout().lane_x).to_string()
        })
    }

    /// `__editchainGpuDebug.snapshot()` data source.
    #[wasm_bindgen(js_name = "debugSnapshot")]
    #[must_use]
    pub fn debug_snapshot() -> String {
        SHELL_DATA.with(|cell| {
            let borrow = cell.borrow();
            let Some(shell) = borrow.as_ref() else {
                return String::new();
            };
            let rows: Vec<Value> = shell
                .last_frame_rows
                .iter()
                .map(|row| {
                    json!({
                        "index": row.index,
                        "key": row.key,
                        "node_key": row.key,
                        "lane": row.lane,
                        "above": row.above,
                        "below": row.below,
                        "transitions": row.transitions,
                        "top": row.top,
                        "bottom": row.bottom,
                        "middle": row.middle,
                        "is_subop": row.is_subop,
                        "is_bundle": row.is_bundle,
                    })
                })
                .collect();
            json!({
                "rows": rows,
                "total": shell.state.total.unwrap_or(-1),
                "backend": "svg",
            })
            .to_string()
        })
    }

    /// `__editchainGpuDebug.metrics()` data source (bootstrap field names).
    #[wasm_bindgen(js_name = "debugMetrics")]
    #[must_use]
    pub fn debug_metrics() -> String {
        SHELL_DATA.with(|cell| {
            let borrow = cell.borrow();
            let Some(shell) = borrow.as_ref() else {
                return String::new();
            };
            json!({
                "initMs": (performance_now() - shell.started_at_ms).max(0.0),
                "firstWindowMs": shell.first_window_ms,
                "lastRenderMs": shell.last_render_ms,
                "renderCount": shell.render_count,
                "vertexCount": 0,
                "domRows": shell.last_frame_rows.len(),
                "generation": shell.generation,
                "rendererReady": shell.flags.renderer_ready,
                "dataReady": shell.state.view_flags.data_ready,
            })
            .to_string()
        })
    }

    /// `__editchainInFlightCount` data source.
    #[wasm_bindgen(js_name = "debugInFlightCount")]
    #[must_use]
    pub fn debug_in_flight_count() -> u64 {
        SHELL_DATA.with(|cell| {
            cell.borrow()
                .as_ref()
                .and_then(|shell| u64::try_from(shell.state.in_flight.len()).ok())
                .unwrap_or(u64::MAX)
        })
    }

    /// `__editchainViewGen` data source.
    #[wasm_bindgen(js_name = "debugViewGen")]
    #[must_use]
    pub fn debug_view_gen() -> u64 {
        SHELL_DATA.with(|cell| {
            cell.borrow()
                .as_ref()
                .map_or(0, |shell| shell.state.view_gen)
        })
    }

    /// `__editchainRowAt` data source: the cached row JSON at an absolute index.
    #[wasm_bindgen(js_name = "debugRowAt")]
    #[must_use]
    pub fn debug_row_at(abs: i64) -> String {
        SHELL_DATA.with(|cell| {
            cell.borrow()
                .as_ref()
                .and_then(|shell| shell.state.cache.get(&abs))
                .map_or_else(|| "null".to_owned(), Value::to_string)
        })
    }

    /// The active graph renderer: the per-row SVG cells (`svg`).
    #[wasm_bindgen(js_name = "debugBackend")]
    #[must_use]
    pub fn debug_backend() -> String {
        "svg".to_owned()
    }

    /// DOM generation counter (render passes) for `whenIdle` stability checks.
    #[wasm_bindgen(js_name = "debugGeneration")]
    #[must_use]
    pub fn debug_generation() -> u64 {
        SHELL_DATA.with(|cell| cell.borrow().as_ref().map_or(0, |shell| shell.generation))
    }

    /// Successful per-row SVG render passes.
    #[wasm_bindgen(js_name = "debugRenderCount")]
    #[must_use]
    pub fn debug_render_count() -> u64 {
        SHELL_DATA.with(|cell| cell.borrow().as_ref().map_or(0, |shell| shell.render_count))
    }
}

/// Re-export the wasm shell entry points so the `#[wasm_bindgen]` exports stay
/// reachable (and importable by the generated JS bindings).
#[cfg(target_arch = "wasm32")]
pub use shell::{
    debug_backend, debug_data_ready, debug_find_state, debug_generation, debug_graph_state,
    debug_in_flight_count, debug_lane_x_all, debug_metrics, debug_profile, debug_render_count,
    debug_renderer_instance_id, debug_row_at, debug_snapshot, debug_total, debug_view_gen,
    start_history_view,
};
