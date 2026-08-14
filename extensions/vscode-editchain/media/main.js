// Webview renderer for the EditChain History explorer.
// Renders a git-graph-style visualization of unified history using a single
// full-height SVG overlay (continuous branch lines) over a real table with
// columns: Graph | Content | Date | Author | Commit/ID.
//
// The graph geometry (lanes + edge point paths) is computed server-side by the
// Rust service (`GetLayout`) and shipped over stdio; this file only draws it.
//
// The webview is a THIN VIEWPORT over a server-owned graph. It renders only the
// visible slice of rows plus a buffer on each side, and requests windowed
// layouts around the scroll position. It does NOT accumulate the whole history:
// far-offscreen windows are evicted from a sparse cache. This keeps DOM size,
// JS heap, and per-scroll serialization bounded regardless of chain size (the
// design target is ~1M nodes).
//
// Edge points from `GetLayout` are ABSOLUTE canonical row indices (not relative
// to the requested offset), so they map directly onto absolute row positions.
//
// Alignment note: block separators shift rows down from a uniform grid, so we
// measure each rendered row's real `offsetTop` after rendering and use those
// pixel positions for both node dots and edge paths.

// @ts-ignore — vscode provides this global in webviews.
const vscode = acquireVsCodeApi();

const rowsEl = document.getElementById('rows');
const searchEl = document.getElementById('search');
const detailEl = document.getElementById('detail');
const layoutEl = document.getElementById('layout');
const filterEl = document.getElementById('filter');
const hideUndatedEl = document.getElementById('hideUndated');
const hideSubmodulesEl = document.getElementById('hideSubmodules');
const hideSystemEl = document.getElementById('hideSystem');

// Rows fetched per request. Larger than the visible viewport so each fetch
// buffers well ahead of the scroll position.
const PAGE = 500;
// How many rows to keep rendered past each edge of the viewport. The rendered
// DOM stays bounded at roughly `viewport + 2*BUFFER` rows regardless of total.
const BUFFER = 400;
let total = 0;             // global row count (server-reported)
let pendingWindow = false; // a GetWindow request is in flight
let pendingWindowOffset = 0; // absolute offset of the in-flight GetWindow
let pendingLayoutId = 0;   // monotonically increasing id for in-flight GetLayout

// Sparse window cache: absolute row index -> HistoryRow. Only windows near the
// scroll position are retained; far-offscreen windows are evicted.
let cache = new Map();
// Monotonic count of distinct rows ever fetched (across all windows), so the
// status bar shows how much history the user has actually loaded — not just the
// bounded viewport cache size.
let totalFetched = 0;

let layout = null;         // { rows: [{node,lane}], edges: [{child,parent,points}] }
let layoutLo = Infinity;   // absolute row index of first layout row
let layoutHi = -1;         // absolute row index of last layout row
let lastRenderKey = '';    // cache key of the last rendered slice (avoid redundant rebuilds)

const ROW_H = 34;

/** Absolute row index of the top of the viewport, clamped to [0, total-1] so
 * overscrolling below the bottom never reports a depth beyond the chain. */
function viewportTopRow() {
  return Math.max(0, Math.min(total - 1, Math.floor(rowsEl.scrollTop / ROW_H)));
}

/** Absolute row index just below the bottom of the viewport. */
function viewportBottomRow() {
  return Math.min(total - 1, Math.ceil((rowsEl.scrollTop + rowsEl.clientHeight) / ROW_H));
}

/** Persist only viewport state across recreations. We never persist row payloads:
 * they can blow past VS Code's webview state size limit for large chains, and a
 * recreated webview can refetch its window cheaply. */
function saveState() {
  vscode.setState({
    total,
    scrollTop: rowsEl.scrollTop,
    hideSubmodules: hideSubmodules(),
    showMessagesOnly: showMessagesOnly(),
    hideUndated: hideUndated(),
    filterPattern: filterEl ? filterEl.value : '',
  });
}

function restoreState() {
  const s = vscode.getState();
  if (s && typeof s.total === 'number') {
    total = s.total || 0;
    if (typeof s.scrollTop === 'number') rowsEl.scrollTop = s.scrollTop;
    if (hideUndatedEl && typeof s.hideUndated === 'boolean') {
      hideUndatedEl.checked = s.hideUndated;
    }
    if (filterEl && typeof s.filterPattern === 'string') {
      filterEl.value = s.filterPattern;
    }
    return true;
  }
  return false;
}

// Branch colours for graph lanes (indexed by lane).
const COLORS = ['#e6194b', '#3cb44b', '#ffe119', '#4363d8', '#f58231', '#911eb4', '#46f0f0', '#f032e6', '#bcf60c', '#fabebe'];

const LANE_W = 18;
const DOT_R = 4;

/** Send a request body to the extension host. */
function send(body) {
  vscode.postMessage({ body });
}

/** Report the current scroll depth / total node counts to the extension host so
 * it can update the status bar. `depth` is the absolute row index at the top of
 * the viewport — how deep into the chain the user currently is (0 = newest,
 * growing toward `total` = inception). This reflects scroll position, not how
 * many rows have been fetched. */
function reportStatus() {
  vscode.postMessage({ type: 'status', loaded: viewportTopRow(), total });
}

/** Unwrap a service response body ({ Ok: v } | { Error: msg }). */
function unwrap(body) {
  if (body && body.Ok !== undefined) return { ok: true, value: body.Ok };
  if (body && body.Error !== undefined) return { ok: false, error: body.Error };
  return { ok: true, value: body };
}

/** Format a Unix-ms timestamp as a readable date. */
function formatDate(ms) {
  if (!ms) return '';
  const d = new Date(ms);
  return d.toLocaleDateString(undefined, { year: 'numeric', month: 'short', day: 'numeric' }) +
    ' ' + d.toLocaleTimeString(undefined, { hour: '2-digit', minute: '2-digit' });
}

/** Escape HTML in a string for safe injection into the table. */
function esc(s) {
  return String(s).replace(/[&<>"']/g, (c) => ({
    '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;',
  }[c]));
}

/**
 * Build the single full-height SVG overlay for the graph.
 *
 * `rowYs` maps rendered-row position → pixel y-offset within the table-wrap
 * (the measured `offsetTop` of each rendered `.row`). `rows` is the array of
 * rows currently rendered (a viewport slice). `absToPos` maps each rendered
 * row's ABSOLUTE canonical index → its position in `rows`, so we can translate
 * absolute edge-point indices to pixel positions even when cached rows have
 * gaps. Each edge is drawn as one continuous <path> from its ordered grid
 * points (child → parent), so lines run unbroken across many rows.
 *
 * Edge points are ABSOLUTE canonical row indices.
 */
function buildGraphSvg(rowYs, rows, absToPos, maxWidth) {
  if (!layout || !layout.rows.length || rows.length === 0) return '';
  // Use the GLOBAL max lane (reported by the server) so the graph column width
  // is stable regardless of which window is loaded — lanes don't jump on scroll.
  const maxLane = layout.max_lane || 0;
  const numLanes = Math.min(maxLane + 1, MAX_GRAPH_LANES);
  // One extra lane of padding so the last lane's dot isn't clipped.
  const naturalW = numLanes * LANE_W + LANE_W;
  // Cap the SVG width so lanes beyond the cap are clipped (content stays visible).
  const width = Math.min(naturalW, maxWidth || naturalW);
  // Height spans from the first row's top to just below the last row.
  const height = (rowYs[rows.length - 1] || 0) + ROW_H;

  // Map node key -> rendered-row position in `rows`.
  const rowOf = {};
  for (let i = 0; i < rows.length; i++) {
    rowOf[rows[i].node_key] = i;
  }
  // Map node key -> lane from the windowed layout rows.
  const laneOf = {};
  for (const r of layout.rows) {
    laneOf[r.node] = r.lane;
  }

  let svg = `<svg id="graphOverlay" width="${width}" height="${height}" xmlns="http://www.w3.org/2000/svg">`;

  // Draw each edge as one continuous path over all rendered rows. An edge is
  // never dropped: if its child is offscreen above or its parent offscreen
  // below, the path extends to the SVG top/bottom so lines stay continuous as
  // you scroll instead of popping in and out.
  let drawnEdges = 0;
  let skippedEdges = 0;
  for (const edge of layout.edges) {
    const childPos = rowOf[edge.child];
    const parentPos = rowOf[edge.parent];

    // Points are absolute canonical row indices already.
    const pts = edge.points;
    if (!pts.length) { skippedEdges++; continue; }

    const childLoaded = childPos !== undefined;
    const parentLoaded = parentPos !== undefined;

    // Helper: pixel y for an absolute row, clamped into [0, height].
    const yOfAbs = (r) => {
      const pos = absToPos[r];
      const raw =
        (pos !== undefined && pos < rowYs.length && rowYs[pos] !== undefined)
          ? rowYs[pos]
          : r * ROW_H;
      return Math.max(0, Math.min(height, raw + ROW_H / 2));
    };

    let d = '';
    if (!childLoaded && pts.length) {
      // Child is above the rendered rows: start at SVG top on the child's lane,
      // then follow points downward.
      const firstX = LANE_W / 2 + pts[0].lane * LANE_W + LANE_W / 2;
      d += 'M' + firstX.toFixed(1) + ',' + '0';
      for (let i = 0; i < pts.length; i++) {
        const x = LANE_W / 2 + pts[i].lane * LANE_W + LANE_W / 2;
        d += 'L' + x.toFixed(1) + ',' + yOfAbs(pts[i].row).toFixed(1);
      }
    } else {
      for (let i = 0; i < pts.length; i++) {
        const x = LANE_W / 2 + pts[i].lane * LANE_W + LANE_W / 2;
        d += (i === 0 ? 'M' : 'L') + x.toFixed(1) + ',' + yOfAbs(pts[i].row).toFixed(1);
      }
    }

    if (!parentLoaded && pts.length) {
      // Parent is below the rendered rows: extend vertically down to SVG bottom.
      const lastX = LANE_W / 2 + pts[pts.length - 1].lane * LANE_W + LANE_W / 2;
      d += 'L' + lastX.toFixed(1) + ',' + height.toFixed(1);
    }

    // Colour each edge by its child's lane so lines match their node dots.
    const colour = COLORS[pts[0].lane % COLORS.length];
    // Shadow path (thick, background-coloured) for contrast against text.
    svg += `<path class="graphShadow" d="${d}"/>`;
    // Line path — stroke set via inline style so it overrides the CSS rule
    // (a presentation attribute would lose to the stylesheet).
    svg += `<path class="graphLine" d="${d}" style="stroke:${colour}"/>`;
    drawnEdges++;
  }
  console.log('[editchain] buildGraphSvg edges=' + layout.edges.length +
    ' drawn=' + drawnEdges + ' skipped=' + skippedEdges);

  // Draw node dots at each rendered row's lane centre. Dots are only drawn for
  // rows that fall inside the loaded layout window AND are currently rendered.
  for (let pos = 0; pos < rows.length; pos++) {
    const absIdx = rows[pos]._absIndex;
    if (absIdx === undefined || absIdx < layoutLo || absIdx > layoutHi) continue;
    const lane = laneOf[rows[pos].node_key];
    if (lane === undefined) continue;
    const x = LANE_W / 2 + lane * LANE_W + LANE_W / 2;
    const y = rowYs[pos] + ROW_H / 2;
    const colour = COLORS[lane % COLORS.length];
    svg += `<circle class="graphDot" data-row="${absIdx}" cx="${x}" cy="${y}" r="${DOT_R}" fill="${colour}"/>`;
  }

  svg += '</svg>';
  return svg;
}

/** Whether submodules should be hidden (inverted from the "Show" checkbox). */
function hideSubmodules() {
  // "Show git submodules" is off by default → submodules hidden by default.
  return !(hideSubmodulesEl && hideSubmodulesEl.checked);
}

/** Whether undated nodes should be hidden (from the "Hide undated" checkbox). */
function hideUndated() {
  return !!(hideUndatedEl && hideUndatedEl.checked);
}

/** The current chain-filter payload to send with window/layout requests.
 *
 * `null` when no filtering is active, so the service skips the filter entirely.
 * The filter pattern is treated as a regex server-side (with literal fallback).
 */
function filterPayload() {
  const pattern = (filterEl && filterEl.value.trim()) || '';
  const undated = hideUndated();
  if (!pattern && !undated) return null;
  return {
    summary_pattern: pattern,
    kind_pattern: '',
    hide_undated: undated,
    splice: true,
  };
}

/** Whether a row is user-facing text (message or command) — the rows kept when
 * "Show messages only" is checked. */
function isMessageRow(row) {
  return row.kind === 'message' || row.kind === 'command';
}

/** Whether only messages should be shown (from the "Show messages only" checkbox). */
function showMessagesOnly() {
  return !!(hideSystemEl && hideSystemEl.checked);
}

// Maximum number of graph lanes shown before clipping. The graph column's
// initial width is `numLanes * LANE_W`, capped at this many lanes so the
// description/content column always stays visible even with many concurrent
// branches.
const MAX_GRAPH_LANES = 64;
// Minimum widths for each resizable column.
const MIN_COL_W = { graph: 40, content: 60, date: 90, author: 60, commit: 60 };
// User-dragged per-column width overrides (null = default behavior):
//   graph   -> natural lane-based width (numLanes * LANE_W, capped at MAX_GRAPH_LANES)
//   content -> flexible minmax(0,1fr)
//   date / author / commit -> fixed defaults
// Dragging can exceed the natural cap so lanes beyond it stay visible.
const colWidths = { graph: null, content: null, date: null, author: null, commit: null };

// Default (pre-drag) widths for the fixed columns. These are applied from the
// first render so the layout is stable and left-aligned — without them the
// date/author/commit tracks are `auto`-sized to content, which cramps them and
// misaligns rows with the header (whose cells carry a min-width). A user drag
// overrides these via `colWidths`.
const DEFAULT_COL_W = { content: 0, date: 140, author: 100, commit: 100 };

/** Build an inline style string carrying every column width as a CSS var.
 *
 * Every column gets an explicit width so nothing is auto-sized: the graph uses
 * its natural/capped lane width, and the fixed columns use their defaults until
 * the user drags a boundary. The content column stays flexible (`1fr`) unless
 * dragged, so it absorbs leftover space.
 */
function colStyle() {
  const parts = ['--graph-w:' + currentGraphWidth() + 'px'];
  if (colWidths.content !== null) parts.push('--content-w:' + colWidths.content + 'px');
  parts.push('--date-w:' + (colWidths.date !== null ? colWidths.date : DEFAULT_COL_W.date) + 'px');
  parts.push('--author-w:' + (colWidths.author !== null ? colWidths.author : DEFAULT_COL_W.author) + 'px');
  parts.push('--commit-w:' + (colWidths.commit !== null ? colWidths.commit : DEFAULT_COL_W.commit) + 'px');
  return parts.join(';');
}

/** The absolute index range we want cached: viewport plus BUFFER on each side. */
function desiredCacheRange() {
  const top = Math.max(0, viewportTopRow() - BUFFER);
  const bottom = Math.min(total - 1, viewportBottomRow() + BUFFER);
  return { top, bottom };
}

/**
 * Fetch a window of rows around an absolute offset into the cacheable range.
 *
 * Random-access: we request whatever slice of `[top,bottom]` is missing from
 * `cache`, rather than appending sequentially from offset 0. This lets deep
 * scrolls jump straight to their target without loading everything before it.
 */
function fetchWindow() {
  if (pendingWindow || total <= 0) return;
  const { top, bottom } = desiredCacheRange();
  if (top > bottom) return;

  // Find the first missing row inside [top,bottom] to fetch next. The range is
  // bounded by BUFFER on each side of the viewport, so this scan is cheap.
  let start = -1;
  for (let i = top; i <= bottom; i++) {
    if (!cache.has(i)) { start = i; break; }
  }
  if (start === -1) return; // everything we want is already cached

  const limit = Math.min(PAGE, bottom - start + 1);
  pendingWindow = true;
  pendingWindowOffset = start;
  send({ GetWindow: { offset: start, limit, hide_submodules: hideSubmodules(), filter: filterPayload() } });
}

/** Evict cached rows far outside the desired range so memory stays bounded. */
function evictFarWindows() {
  const { top, bottom } = desiredCacheRange();
  for (const key of cache.keys()) {
    if (key < top - BUFFER || key > bottom + BUFFER) {
      cache.delete(key);
    }
    if (cache.size > PAGE * 4) break; // hard cap on retained rows
  }
}

/**
 * Render only the viewport slice plus BUFFER on each side into a table with
 * graph column + text columns. Rows outside this slice are not in the DOM, so
 * DOM size stays bounded regardless of chain size.
 */
function renderRows() {
  const { top, bottom } = desiredCacheRange();

  // Count how many rows in [top,bottom] are actually cached, so a window that
  // fills gaps within an unchanged range still triggers a re-render.
  let cachedInRange = 0;
  if (top <= bottom && total > 0) {
    for (let i = top; i <= bottom; i++) {
      if (cache.has(i)) cachedInRange++;
    }
  }

  // Skip a redundant rebuild when neither the visible slice content nor the
  // layout has changed. This avoids the double-render (window response + layout
  // response) that otherwise causes a visible flicker/bounce while scrolling.
  // Column widths are included so a column resize (which mutates `colWidths`)
  // forces a rebuild instead of being swallowed by the early return.
  const renderKey =
    `${top}:${bottom}:${cachedInRange}:${layoutLo}:${layoutHi}:${layout ? layout.max_lane : -1}` +
    `:${colWidths.graph}:${colWidths.content}:${colWidths.date}:${colWidths.author}:${colWidths.commit}`;
  if (renderKey === lastRenderKey) return;
  lastRenderKey = renderKey;

  // Build the sticky table header (fixed at the top of #rows, outside the
  // scroll-spacer so it stays visible while scrolling). Always rendered so the
  // column labels show even when no rows are loaded yet.
  let html =
    '<div class="tbl-header" style="' + colStyle() + '">' +
      '<div class="th graph">Graph</div>' +
      '<div class="th content">Content</div>' +
      '<div class="th date">Date</div>' +
      '<div class="th author">Author</div>' +
      '<div class="th commit">Commit/ID</div>' +
    '</div>';

  // Build the rendered slice from cached rows in absolute order. Each row is
  // tagged with its absolute index (used for graph wiring and click handling).
  const visible = [];
  if (top <= bottom && total > 0) {
    for (let i = top; i <= bottom; i++) {
      const row = cache.get(i);
      if (!row) continue; // gap — will be filled by fetchWindow
      visible.push(row);
      visible[visible.length - 1]._absIndex = i;
    }
  }

  // Map absolute index -> rendered position, for translating edge points.
  const absToPos = {};
  for (let pos = 0; pos < visible.length; pos++) {
    absToPos[visible[pos]._absIndex] = pos;
  }

  let lastGroup = null;

  visible.forEach((row) => {
    // Block separator spans all columns.
    if (row.group !== lastGroup) {
      lastGroup = row.group;
      const label = row.group.startsWith('repo:') ? 'Git · repo ' + row.group.slice(5)
        : row.group.startsWith('session:') ? 'Session ' + row.group.slice(8)
        : 'EditChain ops';
      html += '<div class="block-sep">' + esc(label) + '</div>';
    }

    // Apply a row-level opacity class based on the node's kind (reported by the
    // service): system nodes (tool results / import records) are dimmed most
    // (0.3), other non-text rows (e.g. "tool: NAME") are dimmed moderately
    // (0.7), and text rows (messages/commands) stay full opacity.
    const kindClass = row.is_system ? 'row-tool'
      : (row.kind === 'message' || row.kind === 'command') ? ''
      : 'row-dim';
    // Agent-authored text gets extra left padding to read as a distinct voice.
    const agentClass = row.author === 'agent' ? ' row-agent' : '';

    html += '<div class="row ' + kindClass + agentClass + '" data-key="' + esc(row.node_key) +
      '" data-row="' + row._absIndex + '" style="' + colStyle() + '">' +
      '<div class="graph-cell"></div>' +
      '<div class="text-cell"><div class="summary">' + esc(row.summary || '(no summary)') + '</div></div>' +
      '<div class="date-cell">' + esc(formatDate(row.timestamp_ms)) + '</div>' +
      '<div class="author-cell">' + esc(row.author || '') + '</div>' +
      '<div class="commit-cell">' + esc(row.commit_id || '') + '</div>' +
      '</div>';
  });

  // Preserve the scroll position across the DOM rebuild. Setting innerHTML
  // destroys and recreates all rows, which would otherwise reset scrollTop to
  // snap back to wherever it was — but since we render a window anchored at the
  // current scroll position, we must restore it exactly.
  const prevScrollTop = rowsEl.scrollTop;

  // The scroll-spacer sets #rows's scrollHeight to the FULL history height
  // (total * ROW_H), so the scrollbar spans the whole chain even though only a
  // slice is rendered. The table-wrap is positioned at the slice's top offset.
  const spacerH = Math.max(1, total * ROW_H);
  rowsEl.innerHTML =
    '<div class="tbl-header" style="' + colStyle() + '"></div>' +
    '<div class="scroll-spacer" style="height:' + spacerH + 'px">' +
      '<div class="table-wrap" style="top:' + (top * ROW_H) + 'px;' + colStyle() + '">' +
        html +
      '</div>' +
    '</div>';

  rowsEl.scrollTop = prevScrollTop;

  // Measure each rendered row's real offsetTop within the table-wrap so block
  // separators don't misalign the graph overlay.
  const wrapEl = rowsEl.querySelector('.table-wrap');
  const rowEls = wrapEl.querySelectorAll('.row');
  const rowYs = [];
  rowEls.forEach((el) => { rowYs.push(el.offsetTop); });

  // Insert the graph overlay after measuring.
  wrapEl.insertAdjacentHTML('beforeend', buildGraphSvg(rowYs, visible, absToPos, currentGraphWidth()));

  // Attach click handlers to rows.
  rowEls.forEach((el) => {
    el.addEventListener('click', () => {
      const key = el.getAttribute('data-key');
      const absIdx = parseInt(el.getAttribute('data-row'), 10);
      const row = cache.get(absIdx);
      if (row) inspect(row);
    });
  });
}

/** Load more rows until the content fills the viewport (so scrolling works). */
function ensureFilled() {
  fetchWindow();
}

/**
 * Progressively load history ahead of the scroll position so the user never
 * waits on an in-flight fetch.
 *
 * Keeps fetching in the background until either all desired rows are cached or
 * there is at least `BUFFER` rows of content buffered past each edge of the
 * current viewport. Because it is driven by a timer (not by scroll events), it
 * runs continuously and independently of how fast the user scrolls.
 */
function progressiveLoad() {
  fetchWindow();
}

/**
 * Request a windowed graph layout around the current scroll position.
 *
 * Sends `GetLayout{offset≈viewportTop−BUFFER, limit≈viewport+2*BUFFER}` so only
 * lanes/edges near what's on screen are serialized — not everything loaded so
 * far. Debounced so rapid scrolling doesn't spam requests; only one layout is
 * in flight at a time (`pendingLayoutId`).
 */
let layoutTimer = null;
function requestLayout() {
  clearTimeout(layoutTimer);
  layoutTimer = setTimeout(requestLayoutNow, 80);
}

/** Send the windowed layout request immediately (no debounce). */
function requestLayoutNow() {
  const { top, bottom } = desiredCacheRange();
  if (top > bottom || total <= 0) return;
  const offset = Math.max(0, top);
  const limit = Math.max(1, bottom - top + 1);
  pendingLayoutId++;
  pendingLayoutOffset = offset;
  console.log('[editchain] requestLayout offset=' + offset + ' limit=' + limit);
  send({ GetLayout: { hide_submodules: hideSubmodules(), offset, limit, filter: filterPayload() } });
}

/** Inspect a node's details — opens a read-only JSON editor in VS Code. */
function inspect(row) {
  console.log('[editchain] inspect', row && row.node_key);
  // Ask the extension host to open a read-only JSON editor for this node.
  if (row.git_oid) {
    vscode.postMessage({ type: 'openJson', git_oid: row.git_oid, repository: row.repository });
  } else if (row.op_id) {
    vscode.postMessage({ type: 'openJson', op_id: row.op_id });
  }
}

/** Hide the detail pane (e.g. on reset/search). */
function clearDetail() {
  layoutEl.classList.remove('has-detail');
  detailEl.innerHTML = '';
}

/** Render node details in the inspector pane. */
function renderDetails(details) {
  try {
    detailEl.innerHTML = '';
    const titleEl = document.createElement('div');
    titleEl.className = 'detail-title';
    titleEl.textContent = details.summary || '(no summary)';
    detailEl.appendChild(titleEl);

    if (details.body) {
      const bodyEl = document.createElement('pre');
      bodyEl.className = 'detail-body';
      bodyEl.textContent = details.body;
      detailEl.appendChild(bodyEl);
    }
  } catch (e) {
    console.error('[editchain] renderDetails error:', e);
    detailEl.innerHTML = '<div class="detail-title">Error rendering details</div>' +
      '<pre class="detail-body">' + esc(String(e)) + '</pre>';
  }
}

// Surface any uncaught exception in the webview so we can diagnose.
window.addEventListener('error', (e) => {
  console.error('[editchain] uncaught error:', e.message, e.error);
});

// Handle messages from the extension host.
window.addEventListener('message', (event) => {
  const msg = event.data;
  const r = unwrap(msg.body);

  if (msg.id === 'open') {
    if (r.ok) {
      vscode.postMessage({ type: 'log', text: `open: ${r.value.nodes} nodes, ${r.value.repos} repos` });
      total = r.value.nodes;
      const restored = restoreState();
      if (!restored) {
        rowsEl.scrollTop = 0;
      }
      // Fetch the window around the current scroll position and request its
      // layout. The webview is a thin viewport; it never accumulates history.
      fetchWindow();
      requestLayoutNow();
      renderRows();
      // Start the background progressive loader so history buffers ahead of the
      // scroll position without waiting for scroll events.
      startProgressiveLoader();
      reportStatus();
    } else {
      vscode.postMessage({ type: 'log', text: `open error: ${r.error}` });
    }
    return;
  }

  // The panel was revealed again (e.g. after navigating to a JSON editor and
  // back). The webview's JS context is reset when hidden, so restore the
  // persisted viewport state from vscode.setState before rendering. A short
  // delay lets the webview finish transitioning from hidden to visible so it
  // has real dimensions to measure.
  if (msg.id === 'reveal') {
    const restored = restoreState();
    vscode.postMessage({ type: 'log', text: 'reveal: restored=' + restored + ' total=' + total });
    setTimeout(() => {
      fetchWindow();
      requestLayoutNow();
      renderRows();
      startProgressiveLoader();
    }, 50);
    return;
  }

  if (!r.ok) {
    // Show request errors (e.g. timeout) in the detail pane so they're visible.
    if (layoutEl.classList.contains('has-detail')) {
      detailEl.innerHTML = '<div class="detail-title">Error</div>' +
        '<pre class="detail-body">' + esc(r.error || 'unknown error') + '</pre>';
    }
    return;
  }
  if (!r.value || typeof r.value !== 'object') return;

  // GetLayout response — has both `rows` and `edges` arrays.
  if (Array.isArray(r.value.edges)) {
    layout = r.value;
    // The server's LayoutRow has only {node,lane}; row index is implicit by
    // position, starting at the offset we requested.
    layoutLo = pendingLayoutOffset;
    layoutHi = pendingLayoutOffset + layout.rows.length - 1;
    renderRows();
    saveState();
    return;
  }

  // GetWindow response — has a `rows` array plus `total`.
  if (Array.isArray(r.value.rows)) {
    total = r.value.total;
    const base = pendingWindowOffset;
    for (let i = 0; i < r.value.rows.length; i++) {
      const absIdx = base + i;
      if (!cache.has(absIdx)) totalFetched++;
      cache.set(absIdx, r.value.rows[i]);
    }
    pendingWindow = false;
    evictFarWindows();
    renderRows();
    vscode.postMessage({ type: 'log', text: `cached ${cache.size}/${total} nodes (fetched ${totalFetched})` });
    reportStatus();
    saveState();
    // Recompute the layout to cover the newly loaded rows so lines extend.
    requestLayoutNow();
    // Keep loading until the content fills the viewport so scrolling works.
    fetchWindow();
    return;
  }

  // NodeDetails response (GetNodeDetails) — has summary + body.
  if (typeof r.value.summary === 'string') {
    console.log('[editchain] got details', r.value.summary.slice(0, 40));
    renderDetails(r.value);
    return;
  }
  // Git commit response (ResolveObject) — has message + oid.
  if (r.value && r.value.message !== undefined) {
    console.log('[editchain] got commit');
    const msg = typeof r.value.message === 'string' ? r.value.message : '';
    renderDetails({
      summary: msg || '(no message)',
      body: msg,
      refs: [],
      changed_paths: [],
    });
    return;
  }
});

/** Reset to the full history view and reload from the top. */
function resetHistory() {
  cache.clear();
  totalFetched = 0;
  lastRenderKey = '';
  layout = null;
  layoutLo = Infinity;
  layoutHi = -1;
  pendingWindow = false;
  rowsEl.scrollTop = 0;
  clearDetail();
  fetchWindow();
  requestLayoutNow();
}

/** Clear cached rows/layout and refetch from the top (used when the chain
 * filter changes, since filtering is server-side). */
function resetAndRefetch() {
  cache.clear();
  totalFetched = 0;
  lastRenderKey = '';
  layout = null;
  layoutLo = Infinity;
  layoutHi = -1;
  pendingWindow = false;
  rowsEl.scrollTop = 0;
  fetchWindow();
  requestLayoutNow();
}

// Search on Enter; empty query resets back to the full history.
searchEl.addEventListener('keydown', (e) => {
  if (e.key === 'Enter') {
    const q = searchEl.value.trim();
    if (q) {
      send({ Search: { query: q, mode: 'Lexical', top_k: 20, filters: {} } });
    } else {
      resetHistory();
    }
  }
});

// Clearing the search input resets back to the full history.
searchEl.addEventListener('input', () => {
  if (!searchEl.value.trim()) {
    resetHistory();
  }
});

// Apply the chain filter on Enter; clearing it resets to the full history.
if (filterEl) {
  filterEl.addEventListener('keydown', (e) => {
    if (e.key === 'Enter') {
      resetAndRefetch();
    }
  });
}

// Re-fetch when the hide-undated toggle changes (filtering is server-side).
if (hideUndatedEl) {
  hideUndatedEl.addEventListener('change', () => {
    resetAndRefetch();
  });
}

// Re-fetch when the show-submodules toggle changes (filtering is server-side).
if (hideSubmodulesEl) {
  hideSubmodulesEl.addEventListener('change', () => {
    resetAndRefetch();
  });
}

// Re-render when the messages-only toggle changes (filtering is client-side
// over already-loaded rows, so no refetch is needed). Reset scroll to the top
// (the visible set changes significantly) and keep loading until the content
// fills the viewport so scrolling still works.
if (hideSystemEl) {
  hideSystemEl.addEventListener('change', () => {
    rowsEl.scrollTop = 0;
    renderRows();
    ensureFilled();
  });
}

// Background progressive loader: keeps fetching history ahead of the scroll
// position on a timer, so the user never waits on an in-flight fetch. It runs
// continuously and independently of scroll events.
let progressiveTimer = null;
function startProgressiveLoader() {
  if (progressiveTimer) return;
  progressiveTimer = setInterval(() => {
    progressiveLoad();
  }, 300);
}

// Infinite scroll: fetch windows around the cursor and recompute the graph
// layout as the user scrolls so lines stay continuous. The scroll handler is a
// fallback; the background loader above is the primary driver.
rowsEl.addEventListener('scroll', () => {
  fetchWindow();
  requestLayout();
  // Update the status bar depth as the user scrolls.
  reportStatus();
});

// Re-render and re-request layout when the webview resizes so columns stretch
// and the graph window tracks the new viewport size.
let resizeTimer = null;
window.addEventListener('resize', () => {
  clearTimeout(resizeTimer);
  resizeTimer = setTimeout(() => {
    renderRows();
    requestLayout();
    ensureFilled();
  }, 150);
});

// --- Draggable column widths ------------------------------------------------
//
// A thin vertical handle sits on the right edge of every resizable column
// (Graph, Content, Date, Author, Commit/ID). Dragging a handle overrides that
// column's width via a CSS var (`--graph-w`, `--content-w`, etc.) so the user
// can adjust any column, not just the graph. Handles are re-created on every
// render because `renderRows` rebuilds the table DOM.

/** Current effective graph column width (override or natural).
 *
 * The natural width is `numLanes * LANE_W` (each lane is a fixed `LANE_W`-wide
 * column) plus one extra lane of padding, so the last lane's node dot (centred
 * on the final lane boundary) isn't clipped by the column's overflow. Capped at
 * `MAX_GRAPH_LANES` lanes so the content column stays visible. A user drag
 * overrides the natural width entirely.
 */
function currentGraphWidth() {
  // Use the GLOBAL max lane (reported by the server) so the graph column width
  // is stable regardless of which window is loaded — lanes don't jump on scroll.
  const maxLane = layout ? (layout.max_lane || 0) : 0;
  const numLanes = Math.min(maxLane + 1, MAX_GRAPH_LANES);
  const naturalGraphW = numLanes * LANE_W + LANE_W;
  return colWidths.graph !== null ? colWidths.graph : naturalGraphW;
}

/**
 * Create and wire a drag handle on a column's right edge.
 *
 * `col` is one of "graph" | "content" | "date" | "author" | "commit". The
 * handle is positioned at the column's current right boundary and, while
 * dragging, updates `colWidths[col]` and re-renders so the grid tracks the
 * mouse.
 */
function setupColumnResizeHandle(wrapEl, col, boundaryX) {
  // Remove any stale handle for this column from a previous render.
  const old = wrapEl.querySelector('.col-resize-handle[data-col="' + col + '"]');
  if (old) old.remove();

  const handle = document.createElement('div');
  handle.className = 'col-resize-handle';
  handle.dataset.col = col;
  handle.title = 'Drag to resize ' + col + ' column';
  // Center the 6px handle on the column's right boundary.
  handle.style.left = (boundaryX - 3) + 'px';
  wrapEl.appendChild(handle);

  let dragging = false;
  let startX = 0;
  let startW = 0;

  handle.addEventListener('mousedown', (e) => {
    e.preventDefault();
    e.stopPropagation();
    dragging = true;
    startX = e.clientX;
    startW = currentColumnWidth(col);
    document.body.classList.add('col-resizing');
    window.addEventListener('mousemove', onMove);
    window.addEventListener('mouseup', onUp, { once: true });
  });

  function onMove(e) {
    if (!dragging) return;
    const delta = e.clientX - startX;
    const next = Math.max(MIN_COL_W[col], startW + delta);
    colWidths[col] = next;
    renderRows();
  }

  function onUp() {
    dragging = false;
    document.body.classList.remove('col-resizing');
    window.removeEventListener('mousemove', onMove);
  }
}

/** Current effective width of a resizable column.
 *
 * For the graph column this is computed from the lane count (or the user's
 * override). For every other column it is measured from the rendered header
 * cell so a drag starts from the column's real on-screen width.
 */
function currentColumnWidth(col) {
  if (col === 'graph') return currentGraphWidth();
  if (colWidths[col] !== null) return colWidths[col];
  const th = rowsEl.querySelector('.tbl-header .th.' + col);
  return th ? th.offsetWidth : MIN_COL_W[col];
}

/** Create and wire drag handles for every resizable column.
 *
 * Each handle is positioned at the right edge of its header cell. The header
 * cells are laid out by the same grid template as the rows, so measuring their
 * `offsetLeft + offsetWidth` gives the exact column boundary regardless of
 * flexible/auto track sizing.
 */
function setupColumnResizeHandles() {
  const wrapEl = rowsEl.querySelector('.table-wrap');
  if (!wrapEl) return;
  // The column headers live inside the table-wrap (the outer `.tbl-header`
  // directly under #rows is an empty sticky spacer). Look up the header that
  // actually holds the `.th` cells so handle positions match column boundaries.
  const header = wrapEl.querySelector('.tbl-header');
  if (!header) return;
  const cols = ['graph', 'content', 'date', 'author', 'commit'];
  for (const col of cols) {
    const th = header.querySelector('.th.' + col);
    if (!th) continue;
    setupColumnResizeHandle(wrapEl, col, th.offsetLeft + th.offsetWidth);
  }
}

// Wire the handles after each render. `renderRows` is called from many places,
// so hook it here rather than duplicating calls.
const _origRenderRows = renderRows;
renderRows = function () {
  _origRenderRows.apply(this, arguments);
  setupColumnResizeHandles();
};