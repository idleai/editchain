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

// Sparse window cache: absolute row index -> HistoryRow. Only windows near the
// scroll position are retained; far-offscreen windows are evicted.
let cache = new Map();
// Monotonic count of distinct rows ever fetched (across all windows), so the
// status bar shows how much history the user has actually loaded — not just the
// bounded viewport cache size.
let totalFetched = 0;

let lastRenderKey = '';    // cache key of the last rendered slice (avoid redundant rebuilds)
// Global maximum graph lane across ALL rows, reported by the server with each
// GetWindow response. Used to size the graph column stably regardless of which
// window is loaded (lanes don't jump on scroll).
let maxLane = 0;

// Additive (UIKit-style) window state. The DOM always holds a CONTIGUOUS run of
// rows [renderTop, renderBottom] (inclusive), in order, with no gaps. The
// .table-wrap is positioned at `renderTop * ROW_H`. Scrolling extends/trims this
// window at its edges and shifts the wrap by exact multiples of ROW_H — existing
// nodes are never rebuilt during a scroll-through-loaded-content.
let renderTop = 0;         // absolute row index of first rendered row
let renderBottom = -1;     // absolute row index of last rendered row (inclusive); -1 = empty

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

/** Human-readable label for a block-separator group key.
 *
 * `repo:*` groups are git repositories; `session:*` groups are Claude Code
 * sessions; anything else falls back to "EditChain ops".
 */
function groupLabelText(group) {
  return group.startsWith('repo:') ? 'Git · repo ' + group.slice(5)
    : group.startsWith('session:') ? 'Session ' + group.slice(8)
    : 'EditChain ops';
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
 * Additive (UIKit-style) virtual scroll.
 *
 * The DOM holds a CONTIGUOUS run of rows [renderTop, renderBottom] inside a
 * .table-wrap positioned at `renderTop * ROW_H`. Scrolling extends/trims this
 * window at its edges and shifts the wrap by exact multiples of ROW_H — existing
 * nodes are never rebuilt during a scroll-through-loaded-content, so there is no
 * re-anchor moment and no layout jump. Full rebuilds happen only when content
 * genuinely changes (initial load, far jump, column resize, filter).
 *
 * Because every row is exactly ROW_H tall, shifting .table-wrap.top by `n*ROW_H`
 * moves content by exactly n rows with zero sub-pixel drift, and the fixed-height
 * .scroll-spacer keeps the scrollbar stable.
 */

/** X pixel position of a lane's centre within the graph column. */
function laneX(lane) {
  return LANE_W / 2 + lane * LANE_W + LANE_W / 2;
}

/**
 * Build one row's graph cell: a small inline SVG drawing the node's dot, the
 * vertical line segments for lanes entering from above and leaving below, and
 * any horizontal merge connectors at this row.
 *
 * This is the per-row replacement for the old full-height SVG overlay. Because
 * each row carries its own graph geometry (lane, above, below, transitions)
 * shipped with its GetWindow data, scrolling = moving rows = moving their cells
 * together — there is no separate overlay to reconcile, so no flash/jump/blank.
 *
 * Vertical segments are split into a TOP half (lanes in `above`, entering from
 * above) and a BOTTOM half (lanes in `below`, leaving downward). Adjacent rows'
 * halves meet at cell boundaries into continuous lines. A TIP (newest node, no
 * children) has no `above` lanes → no line above its dot; a ROOT (no parents)
 * has no `below` lanes → no line below. The dot sits at the row's own lane,
 * vertically centred.
 */
function buildGraphCell(row) {
  const width = currentGraphWidth();
  const height = ROW_H;
  const midY = ROW_H / 2;
  let s = `<svg class="graphCell" width="${width}" height="${height}" xmlns="http://www.w3.org/2000/svg">`;
  // Top-half vertical segments: lanes entering from above (y=0 → midY).
  const above = row.above || [];
  for (const lane of above) {
    const x = laneX(lane);
    const colour = COLORS[lane % COLORS.length];
    s += `<line class="graphLine" x1="${x}" y1="0" x2="${x}" y2="${midY}" style="stroke:${colour}"/>`;
  }
  // Bottom-half vertical segments: lanes leaving downward (midY → height).
  const below = row.below || [];
  for (const lane of below) {
    const x = laneX(lane);
    const colour = COLORS[lane % COLORS.length];
    s += `<line class="graphLine" x1="${x}" y1="${midY}" x2="${x}" y2="${height}" style="stroke:${colour}"/>`;
  }
  // Horizontal merge connectors at this row. Colour by the FROM lane (the chain
  // the connector originates from).
  const transitions = row.transitions || [];
  for (const [fromLane, toLane] of transitions) {
    const x1 = laneX(fromLane);
    const x2 = laneX(toLane);
    const colour = COLORS[fromLane % COLORS.length];
    s += `<line class="graphLine" x1="${x1}" y1="${midY}" x2="${x2}" y2="${midY}" style="stroke:${colour}"/>`;
  }
  // The node's own dot at its lane.
  const lane = row.lane || 0;
  const colour = COLORS[lane % COLORS.length];
  s += `<circle class="graphDot" cx="${laneX(lane)}" cy="${midY}" r="${DOT_R}" fill="${colour}"/>`;
  s += '</svg>';
  return s;
}

/** Build one row's HTML from its cached HistoryRow. `absIdx` is its absolute index. */
function buildRowHtml(row, absIdx, isGroupStart) {
  const groupClass = isGroupStart ? ' row-group-start' : '';
  const groupLabel = isGroupStart
    ? '<div class="group-label">' + esc(groupLabelText(row.group)) + '</div>'
    : '';
  const kindClass = row.is_system ? 'row-tool'
    : (row.kind === 'message' || row.kind === 'command') ? ''
    : 'row-dim';
  const agentClass = row.author === 'agent' ? ' row-agent' : '';
  return '<div class="row ' + kindClass + agentClass + groupClass + '" data-key="' + esc(row.node_key) +
    '" data-row="' + absIdx + '" style="' + colStyle() + '">' +
    groupLabel +
    '<div class="graph-cell">' + buildGraphCell(row) + '</div>' +
    '<div class="text-cell"><div class="summary">' + esc(row.summary || '(no summary)') + '</div></div>' +
    '<div class="date-cell">' + esc(formatDate(row.timestamp_ms)) + '</div>' +
    '<div class="author-cell">' + esc(row.author || '') + '</div>' +
    '<div class="commit-cell">' + esc(row.commit_id || '') + '</div>' +
    '</div>';
}

/** The .table-wrap element currently in #rows, or null if not built yet. */
function wrapEl() {
  return rowsEl.querySelector('.table-wrap');
}

/** Set .table-wrap.top to position it at absolute row `top`. */
function setWrapTop(top) {
  const w = wrapEl();
  if (w) w.style.top = (top * ROW_H) + 'px';
}

/** Rebuild the entire window from cache in one pass. Used for initial load,
 * far jumps, column resize, and filter changes — NOT for normal scrolling. */
function reanchorTo(top, bottom) {
  renderTop = top; renderBottom = bottom;
  let html = '';
  let lastGroup = null;
  for (let i = top; i <= bottom; i++) {
    const row = cache.get(i);
    if (!row) { html += '<div class="row row-placeholder" data-row="' + i + '"></div>'; continue; }
    const isGroupStart = row.group !== lastGroup;
    if (isGroupStart) lastGroup = row.group;
    html += buildRowHtml(row, i, isGroupStart);
  }
  // Build the sticky header + spacer + wrap in one innerHTML pass. There is a
  // SINGLE header, a direct child of #rows, so its `position: sticky; top: 0`
  // sticks to the #rows viewport and stays at the top while scrolling. It must
  // NOT live inside .table-wrap (which is positioned at renderTop*ROW_H and moves
  // with scroll) — a header there would scroll with content and appear mid-table.
  const spacerH = Math.max(1, total * ROW_H);
  const headerHtml =
    '<div class="tbl-header" style="' + colStyle() + '">' +
      '<div class="th graph">Graph</div>' +
      '<div class="th content">Content</div>' +
      '<div class="th date">Date</div>' +
      '<div class="th author">Author</div>' +
      '<div class="th commit">Commit/ID</div>' +
    '</div>';
  // Preserve the scroll position across the DOM rebuild (setting innerHTML
  // resets scrollTop to 0).
  const prevScrollTop = rowsEl.scrollTop;
  rowsEl.innerHTML =
    headerHtml +
    '<div class="scroll-spacer" style="height:' + spacerH + 'px">' +
      '<div class="table-wrap" style="top:' + (top * ROW_H) + 'px;' + colStyle() + '">' +
        html +
      '</div>' +
    '</div>';
  rowsEl.scrollTop = prevScrollTop;
  // No graph refresh needed: each row's graph cell is built into its HTML, so
  // the rebuilt DOM already contains the correct per-row graph.
  attachRowClicks();
}

function attachRowClicks() {
  const w = wrapEl();
  if (!w) return;
  w.querySelectorAll('.row').forEach((el) => {
    el.addEventListener('click', () => {
      const key = el.getAttribute('data-key');
      const absIdx = parseInt(el.getAttribute('data-row'), 10);
      const row = cache.get(absIdx);
      if (row) inspect(row);
    });
  });
}

/** Append rows [renderBottom+1, renderBottom+n] to the bottom of the window.
 * Only extends CONTIGUOUSLY: if the immediate next row isn't cached yet, nothing
 * is appended (fetchWindow will deliver it and syncWindow will retry). This keeps
 * the DOM gap-free. */
function appendRowsBelow(n) {
  if (n <= 0 || renderBottom >= total - 1) return;
  const w = wrapEl();
  if (!w) return;
  // Only extend if the edge row is cached (contiguity).
  if (!cache.has(renderBottom + 1)) return;
  let html = '';
  let lastGroup = null;
  // Group continuity from the last currently-rendered row.
  const lastRowEl = w.querySelector('.row:last-child');
  if (lastRowEl) {
    const lastAbs = parseInt(lastRowEl.getAttribute('data-row'), 10);
    const lastRow = cache.get(lastAbs);
    if (lastRow) lastGroup = lastRow.group;
  }
  let added = 0;
  for (let i = renderBottom + 1; i <= Math.min(renderBottom + n, total - 1); i++) {
    const row = cache.get(i);
    if (!row) break; // stop at first gap — keep contiguous
    const isGroupStart = row.group !== lastGroup;
    if (isGroupStart) lastGroup = row.group;
    html += buildRowHtml(row, i, isGroupStart);
    added++;
    renderBottom++;
  }
  if (added && html) {
    w.insertAdjacentHTML('beforeend', html);
    attachRowClicks();
  }
}

/** Prepend rows [renderTop-n, renderTop-1] to the top of the window, shifting
 * .table-wrap.top down by the number of rows actually added. Only extends
 * CONTIGUOUSLY from the edge. */
function prependRowsAbove(n) {
  if (n <= 0 || renderTop <= 0) return;
  const w = wrapEl();
  if (!w) return;
  // Only extend if the edge row above is cached (contiguity).
  if (!cache.has(renderTop - 1)) return;
  // Build bottom-up so group-start detection matches reanchorTo/appendRowsBelow:
  // a row is a group-start if its group differs from the row ABOVE it.
  let prevGroup = null;
  // Group continuity from the first currently-rendered row (the row just below
  // the new topmost prepended row).
  const firstRowEl = w.querySelector('.row:first-child');
  if (firstRowEl) {
    const firstAbs = parseInt(firstRowEl.getAttribute('data-row'), 10);
    const firstRow = cache.get(firstAbs);
    if (firstRow) prevGroup = firstRow.group;
  }
  let html = '';
  let added = 0;
  for (let i = renderTop - 1; i >= Math.max(0, renderTop - n); i--) {
    const row = cache.get(i);
    if (!row) break; // stop at first gap — keep contiguous
    const isGroupStart = prevGroup === null || row.group !== prevGroup;
    html += buildRowHtml(row, i, isGroupStart);
    added++;
    prevGroup = row.group;
    renderTop--;
  }
  if (added && html) {
    w.insertAdjacentHTML('afterbegin', html);
    // Shift the wrap down by the number of rows added so content stays put.
    setWrapTop(renderTop);
    attachRowClicks();
  }
}

/** Replace any `.row-placeholder` elements with real row HTML once their data
 * arrives in cache. Called after GetWindow delivers rows that reanchorTo had
 * rendered as placeholders (because they weren't cached yet). */
function fillPlaceholders() {
  const w = wrapEl();
  if (!w) return;
  const placeholders = w.querySelectorAll('.row-placeholder');
  if (!placeholders.length) return;
  let changed = false;
  placeholders.forEach((el) => {
    const absIdx = parseInt(el.getAttribute('data-row'), 10);
    const row = cache.get(absIdx);
    if (!row) return;
    // Group-start: compare to the row above (previous sibling).
    const prevEl = el.previousElementSibling;
    const prevRow = prevEl ? cache.get(parseInt(prevEl.getAttribute('data-row'), 10)) : null;
    const isGroupStart = !prevRow || row.group !== prevRow.group;
    el.outerHTML = buildRowHtml(row, absIdx, isGroupStart);
    changed = true;
  });
  if (changed) {
    attachRowClicks();
  }
}

/** Remove rows above `keepTop` from the top of the window, shifting .table-wrap.top
 * up by the number removed so remaining content stays put. */
function trimTop(keepTop) {
  const w = wrapEl();
  if (!w || renderTop >= keepTop) return;
  let removed = Math.min(keepTop - renderTop, renderBottom - renderTop + 1);
  for (let i = renderTop; i < renderTop + removed; i++) {
    const el = w.querySelector('.row[data-row="' + i + '"]');
    if (el) el.remove();
  }
  renderTop += removed;
  setWrapTop(renderTop);
}

/** Remove rows below `keepBottom` from the bottom of the window. */
function trimBottom(keepBottom) {
  const w = wrapEl();
  if (!w || renderBottom <= keepBottom) return;
  let removed = Math.min(renderBottom - keepBottom, renderBottom - renderTop + 1);
  for (let i = renderBottom; i > renderBottom - removed; i--) {
    const el = w.querySelector('.row[data-row="' + i + '"]');
    if (el) el.remove();
  }
  renderBottom -= removed;
}

/**
 * Sync the rendered window to the current scroll position — the additive scroll
 * driver. Extends/trims at the edges so content follows the cursor without a
 * full rebuild. Scrolling into unfilled territory just waits for fetchWindow to
 * deliver rows (a quick render penalty), never blanking or re-anchoring.
 */
function syncWindow() {
  if (total <= 0) return;
  const { top: wantTop, bottom: wantBottom } = desiredCacheRange();
  // If the window is empty or no longer covers the viewport (e.g. after a fast
  // fling evicted rows and we've scrolled back into them), reanchor cleanly from
  // cache rather than trying to incrementally patch a misaligned window. This is
  // the accepted "quick render penalty" for scrolling into unfilled territory.
  const coversViewport =
    renderTop <= viewportTopRow() && renderBottom >= viewportBottomRow();
  if (renderBottom < renderTop || !coversViewport) {
    reanchorTo(wantTop, wantBottom);
    fetchWindow();
    return;
  }
  // Window covers the viewport — extend/trim incrementally at the edges so
  // content follows the cursor without a full rebuild.
  if (renderBottom < wantBottom) {
    appendRowsBelow(wantBottom - renderBottom);
    fetchWindow(); // request more below
  }
  if (renderTop > wantTop || cache.has(renderTop - 1)) {
    prependRowsAbove(Math.max(1, renderTop - wantTop));
    fetchWindow(); // request more above
  }
  // Trim rows that have drifted far offscreen so the DOM stays bounded.
  trimTop(wantTop);
  trimBottom(wantBottom);
  // Replace any placeholders whose data has now arrived.
  fillPlaceholders();
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
  // Sync the window so newly-fetched rows appear even without a scroll event.
  syncWindow();
}

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
      // Fetch the window around the current scroll position. The webview is a thin
      // viewport; it never accumulates history.
      fetchWindow();
      // Reanchor to the current viewport window (rows may not be cached yet —
      // GetWindow responses will append them in).
      reanchorTo(desiredCacheRange().top, desiredCacheRange().bottom);
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
      reanchorTo(desiredCacheRange().top, desiredCacheRange().bottom);
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

  // GetWindow response — has a `rows` array plus `total`.
  if (Array.isArray(r.value.rows)) {
    total = r.value.total;
    // Global max lane for stable graph-column width (per-row graph cells).
    if (typeof r.value.max_lane === 'number') maxLane = r.value.max_lane;
    const base = pendingWindowOffset;
    for (let i = 0; i < r.value.rows.length; i++) {
      const absIdx = base + i;
      if (!cache.has(absIdx)) totalFetched++;
      cache.set(absIdx, r.value.rows[i]);
    }
    pendingWindow = false;
    evictFarWindows();
    // Newly cached rows may extend the rendered window at either edge. Sync the
    // window to the current viewport so newly-loaded rows appear without a full
    // rebuild.
    syncWindow();
    vscode.postMessage({ type: 'log', text: `cached ${cache.size}/${total} nodes (fetched ${totalFetched})` });
    reportStatus();
    saveState();
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
  pendingWindow = false;
  renderTop = 0;
  renderBottom = -1;
  rowsEl.scrollTop = 0;
  clearDetail();
  fetchWindow();
}

/** Clear cached rows/layout and refetch from the top (used when the chain
 * filter changes, since filtering is server-side). */
function resetAndRefetch() {
  cache.clear();
  totalFetched = 0;
  lastRenderKey = '';
  pendingWindow = false;
  renderTop = 0;
  renderBottom = -1;
  rowsEl.scrollTop = 0;
  fetchWindow();
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
    reanchorTo(desiredCacheRange().top, desiredCacheRange().bottom);
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

// Infinite scroll: extend/trim the rendered window at its edges so content
// follows the cursor in both directions without a full rebuild. Throttled to a
// frame so we don't run syncWindow on every scroll event.
let syncTimer = null;
rowsEl.addEventListener('scroll', () => {
  fetchWindow();
  clearTimeout(syncTimer);
  syncTimer = setTimeout(syncWindow, 0);
  // Update the status bar depth as the user scrolls.
  reportStatus();
});

// Re-render when the webview resizes so columns stretch and the graph cells
// track the new viewport size.
let resizeTimer = null;
window.addEventListener('resize', () => {
  clearTimeout(resizeTimer);
  resizeTimer = setTimeout(() => {
    syncWindow();
    ensureFilled();
  }, 150);
});

// --- Draggable column widths ------------------------------------------------
//
// A thin vertical handle sits on the right edge of every resizable column
// (Graph, Content, Date, Author, Commit/ID). Dragging a handle overrides that
// column's width via a CSS var (`--graph-w`, `--content-w`, etc.) so the user
// can adjust any column, not just the graph. Handles are re-created on every
// full rebuild because `reanchorTo` rebuilds the table DOM.

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
    // Column width changed — full rebuild so the grid tracks the mouse.
    reanchorTo(renderTop, renderBottom);
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
  // The single sticky header is a direct child of #rows (it must NOT live inside
  // .table-wrap, or it would scroll with content). Measure cell positions from
  // it so handle positions match column boundaries.
  const header = rowsEl.querySelector('.tbl-header');
  if (!header) return;
  const cols = ['graph', 'content', 'date', 'author', 'commit'];
  for (const col of cols) {
    const th = header.querySelector('.th.' + col);
    if (!th) continue;
    setupColumnResizeHandle(wrapEl, col, th.offsetLeft + th.offsetWidth);
  }
}

// Wire the handles after each full rebuild. Column resize mutates `colWidths`
// and needs a full rebuild (row widths change), so hook `reanchorTo` here rather
// than duplicating calls. Incremental append/prepend don't change column widths,
// so they don't need handle re-wiring.
const _origReanchorTo = reanchorTo;
reanchorTo = function () {
  _origReanchorTo.apply(this, arguments);
  setupColumnResizeHandles();
};

// Harness-only debug hook: expose render state so a text-only probe can sample
// the per-row graph cells across a scroll. This is NOT part of the production
// webview behaviour.
window.__editchainGraphState = function () {
  return {
    renderTop, renderBottom,
    maxLane,
    graphWidth: currentGraphWidth(),
  };
};