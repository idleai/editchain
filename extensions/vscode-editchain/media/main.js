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

// Show an explicit loading state until the extension host finishes `Open` and
// the first window arrives (or surfaces the open error).
showViewMessage('Loading history…', false);

// Harness data-readiness signal. Set to `true` only once the webview has
// processed a terminal event correlated with actual content: an `open` error,
// a GetWindow response, or search results. The layout probe waits on this plus
// the absence of placeholder rows, so "idle" never means a DOM full of
// placeholders waiting on an in-flight fetch.
window.__editchainDataReady = false;

// Rows fetched per request. Larger than the visible viewport so each fetch
// buffers well ahead of the scroll position.
const PAGE = 500;
// How many rows to keep rendered past each edge of the viewport. The rendered
// DOM stays bounded at roughly `viewport + 2*BUFFER` rows regardless of total.
const BUFFER = 400;
let total = 0;             // global row count (server-reported)
let pendingWindowReqId = -1; // request id of the in-flight GetWindow, or -1

// Sparse window cache: absolute row index -> HistoryRow. Only windows near the
// scroll position are retained; far-offscreen windows are evicted.
let cache = new Map();
// Monotonic count of distinct rows ever fetched (across all windows), so the
// status bar shows how much history the user has actually loaded — not just the
// bounded viewport cache size.
let totalFetched = 0;

// Search-result view mode. While active, #rows renders a flat list of search
// hits (from the service's lexical index) instead of the virtual-scrolled
// history window; fetch/scroll/progressive-load machinery is suspended.
let searchMode = false;
let searchQuery = '';

// Latest-query-wins correlation for search. A search response is rendered ONLY
// if it carries the CURRENT epoch. Two rapid searches share the same view
// generation (rendering a search bumps viewGen to reject stale history
// windows), so generation alone cannot tell which response belongs to the
// latest query: without a per-query epoch, the first response to arrive bumps
// the generation and the second query's response is dropped as "stale" — the
// UI then shows query B's input with query A's results (or, if B lands first,
// A's late response overwrites B). The epoch makes the LATEST issued query win
// regardless of response order.
let searchEpoch = 0;          // monotonically increasing issue counter
let currentSearchEpoch = -1;  // epoch of the latest issued search; -1 = none

// --- Request correlation ----------------------------------------------------
//
// Every service request carries a client-generated id, and the extension host
// echoes that id back with the response. Responses are correlated by id (not by
// response shape), and tagged with the VIEW GENERATION they were issued under.
// When the view changes (open, filter reset, search, clear), the generation
// increments; any in-flight response from an older generation is dropped so it
// can never poison cache/total/filter state (e.g. a GetWindow issued before a
// resetAndRefetch landing after it, or a search overlapping an in-flight
// window).
let nextReqId = 1;
const inFlight = new Map();
let viewGen = 0;
// Whether the per-filter expansion snapshot (sub_op_counts) has been received
// for the CURRENT view generation. The service ships it only with the offset-0
// window, so a deep jump must first fetch offset 0 to establish visible/absolute
// index mapping before paging the deep window.
let snapshotEstablished = false;

// Non-blocking chain-data warnings surfaced from the Open response (e.g. blob
// payloads missing from the durable store). Shown as a banner above the table
// while rows still render; never silently discarded.
let openWarnings = [];

let lastRenderKey = '';    // cache key of the last rendered slice (avoid redundant rebuilds)
// Global maximum graph lane across ALL rows, reported by the server with each
// GetWindow response. Used to size the graph column stably regardless of which
// window is loaded (lanes don't jump on scroll).
// Initial lane count used before the first GetWindow response arrives. Set to
// a small non-zero default (3 lanes) so the sticky header's graph column isn't
// collapsed on open — it snaps to the real lane count once max_lane is known.
let maxLane = 2;

// Additive (UIKit-style) window state. The DOM always holds a CONTIGUOUS run of
// rows [renderTop, renderBottom] (inclusive), in order, with no gaps. The
// .table-wrap is positioned at `renderTop * ROW_H`. Scrolling extends/trims this
// window at its edges and shifts the wrap by exact multiples of ROW_H — existing
// nodes are never rebuilt during a scroll-through-loaded-content.
let renderTop = 0;         // absolute row index of first rendered row
let renderBottom = -1;     // absolute row index of last rendered row (inclusive); -1 = empty

const ROW_H = 34;

// --- Inline sub-op reveal ---------------------------------------------------
//
// The server emits a FIXED fully-expanded flat list (`total` = fully-expanded
// count): every combined op always occupies its stable 1+N absolute slots
// (parent + one per bundled sub-op), so fetch/cache indices never move.
// Collapse/expand is purely a rendering decision driven by reveal state below.
//
// Virtual scroll therefore operates in TWO spaces:
//   - CACHE & FETCH use ABSOLUTE indices (server returns stable windows).
//   - RENDER & SCROLL & SPACER use VISIBLE indices — how many uniform ROW_H
//     slots are actually drawn after collapsing hidden sub-op slots.
//
// Mapping between them uses prefix sums over per-node sub-op counts (`subOpCounts`,
// shipped once per filter state from the server when offset==0).

/** Number of bundled sub-ops per top-level node (global; empty until received). */
let subOpCounts = [];
/** Absolute start slot of each top-level node's block (= parent row index). */
let blockStarts = [];
/** Top-level node indices currently expanded (reveal state). Default all collapsed. */
const expandedBlocks = new Set();
/** Prefix sum over blocks of "hidden contribution" — recomputed on toggle/counts change.
 * contrib[b] = count[b] if block b is collapsed else 0; prefixContrib[k] = sum_{b<k} contrib[b]. */
let prefixContrib = [];

/** Recompute blockStarts + prefixContrib from subOpCounts + expandedBlocks.
 * Call whenever either changes so all mapping helpers stay consistent. */
function recomputeExpansion() {
  const n = subOpCounts.length;
  const starts = new Array(n);
  let acc = 0;
  for (let i = 0; i < n; i++) {
    starts[i] = acc;
    acc += 1 + subOpCounts[i];
  }
  blockStarts = starts;
  const contrib = new Array(n);
  let pacc = 0;
  for (let i = 0; i < n; i++) {
    contrib[i] = pacc;
    pacc += expandedBlocks.has(i) ? 0 : subOpCounts[i];
  }
  prefixContrib = contrib;
}

/** Number of hidden slots strictly before any slot inside block b.
 * For a VISIBLE slot of block b this equals prefixContrib[b]. */
function hiddenBeforeBlock(b) {
  return b < prefixContrib.length ? prefixContrib[b] : 0;
}

/** Total number of VISIBLE rows given current reveal state (= spacer height / ROW_H). */
function visibleTotal() {
  let hidden = 0;
  for (let i = 0; i < subOpCounts.length; i++) {
    if (!expandedBlocks.has(i)) hidden += subOpCounts[i];
  }
  return Math.max(1, total - hidden);
}

/** Map a VISIBLE index back to its ABSOLUTE slot index.
 * Returns null if vis is out of range or lands on nothing drawable.
 *
 * Before the first GetWindow response arrives, `subOpCounts`/`blockStarts` are
 * empty and no sub-op expansion is known yet — the mapping is identity (every
 * absolute slot is its own visible slot). This keeps the initial `reanchorTo`
 * from rendering a blank grid while the offset==0 window is in flight.
 */
function absIndexForVisible(vis) {
  if (!blockStarts.length) return vis; // no expansion known yet — identity
  // Binary search blocks by their VISIBLE start (= start[b] - hiddenBeforeBlock(b)).
  let lo = 0;
  let hi = blockStarts.length;
  while (lo < hi) {
    const mid = (lo + hi) >> 1;
    const vStartMid = blockStarts[mid] - hiddenBeforeBlock(mid);
    if (vStartMid <= vis) lo = mid + 1;
    else hi = mid;
  }
  const b = lo - 1;
  if (b < 0 || b >= blockStarts.length) return null;
  const vStartB = blockStarts[b] - hiddenBeforeBlock(b);
  const offInBlockVis = vis - vStartB;
  const countB = b < subOpCounts.length ? subOpCounts[b] : 0;
  if (!expandedBlocks.has(b)) {
    // Collapsed: only one visible slot → parent row.
    return offInBlockVis === 0 ? blockStarts[b] : null;
  }
  if (offInBlockVis > countB) return null;
  return blockStarts[b] + offInBlockVis;
}

/** Toggle expansion of a top-level node by its ABSOLUTE parent-row index.
 * Returns true if anything changed. */
function toggleExpanded(absParentRow) {
  const b = blockIndexOfAbs(absParentRow);
  if (b < 0 || b >= subOpCounts.length || !subOpCounts[b]) return false;
  if (expandedBlocks.has(b)) expandedBlocks.delete(b);
  else expandedBlocks.add(b);
  recomputeExpansion();
  return true;
}

/** Index of the top-level node whose block starts at `absParentRow`, else -1. */
function blockIndexOfAbs(absParentRow) {
  let lo = 0;
  let hi = blockStarts.length;
  while (lo < hi) {
    const mid = (lo + hi) >> 1;
    if (blockStarts[mid] <= absParentRow) lo = mid + 1;
    else hi = mid;
  }
  const b = lo - 1;
  return b >= 0 && b < blockStarts.length && blockStarts[b] === absParentRow ? b : -1;
}

/** Visible row index of the top of the viewport (= floor(scrollTop / ROW_H)). */
function viewportVisibleTop() {
  return Math.max(0, Math.floor(rowsEl.scrollTop / ROW_H));
}

/** Visible row index just below the bottom of the viewport. */
function viewportVisibleBottom() {
  return Math.min(visibleTotal() - 1, Math.max(viewportVisibleTop(), Math.floor((rowsEl.scrollTop + rowsEl.clientHeight) / ROW_H)));
}

/** Persist only viewport state across recreations. We never persist row payloads:
 * they can blow past VS Code's webview state size limit for large chains, and a
 * recreated webview can refetch its window cheaply. */
function saveState() {
  vscode.setState({
    total,
    // Persist the visible TOP ROW INDEX, not raw pixel scrollTop: the webview
    // JS context is destroyed when hidden behind an editor preview, and on
    // restore the spacer/scaffold doesn't exist until after `reanchorTo`, so a
    // raw pixel offset is meaningless (it clamps to 0). A row index survives
    // expansion differences and is reapplied as `topRow * ROW_H` once rows load.
    topRow: viewportVisibleTop(),
    hideSubmodules: hideSubmodules(),
    showMessagesOnly: showMessagesOnly(),
    hideUndated: hideUndated(),
    filterPattern: filterEl ? filterEl.value : '',
  });
}

/** Restore filter checkboxes/input from state; return the saved top row index
 * (or -1 if none) WITHOUT touching scrollTop — the spacer isn't built yet here,
 * so applying scroll must wait until the open/reveal handler has reanchored. */
function restoreState() {
  const s = vscode.getState();
  let topRow = -1;
  if (s && (typeof s.topRow === 'number' || s.filterPattern || s.hideUndated ||
      s.showMessagesOnly || typeof s.hideSubmodules === 'boolean')) {
    // Do NOT restore `total` here — the chain may have been reimported since the
    // last session, so the persisted node count can be stale. The server's
    // `total` is authoritative and is applied by the open handler before this
    // runs. Only the top row index and filter state survive a session.
    if (typeof s.topRow === 'number' && s.topRow > 0) topRow = s.topRow;
    if (hideSubmodulesEl && typeof s.hideSubmodules === 'boolean') {
      // saveState stores the "hide submodules" boolean (inverted from the
      // "Show git submodules" checkbox); restore the checkbox to match.
      hideSubmodulesEl.checked = !s.hideSubmodules;
    }
    if (hideUndatedEl && typeof s.hideUndated === 'boolean') {
      hideUndatedEl.checked = s.hideUndated;
    }
    if (hideSystemEl && typeof s.showMessagesOnly === 'boolean') {
      hideSystemEl.checked = s.showMessagesOnly;
    }
    if (filterEl && typeof s.filterPattern === 'string') {
      filterEl.value = s.filterPattern;
    }
  }
  return topRow;
}

/** Apply a restored visible top row index as a pixel scroll offset. Must be
 * called AFTER the scaffold (`.scroll-spacer`) is in place so the scroll range
 * is real; clamp to the bottom so an old deep offset never over-scrolls. */
function restoreScrollTop(rowIndex) {
  const target = rowIndex * ROW_H;
  const maxScroll = Math.max(0, rowsEl.scrollHeight - rowsEl.clientHeight);
  rowsEl.scrollTop = Math.min(target, maxScroll);
}

// Branch colours for graph lanes (indexed by lane).
const COLORS = ['#e6194b', '#3cb44b', '#ffe119', '#4363d8', '#f58231', '#911eb4', '#46f0f0', '#f032e6', '#bcf60c', '#fabebe'];

const LANE_W = 18;
const DOT_R = 4;

/** Send a request body to the extension host, correlating the response.
 *
 * Returns the request id. The extension host echoes `{ id, body }` back; the
 * message handler matches responses to requests by id and drops responses whose
 * view generation no longer matches.
 */
function send(body) {
  const id = nextReqId++;
  inFlight.set(id, { body, gen: viewGen });
  vscode.postMessage({ id, body });
  return id;
}

/** Send a request tagged with the current search epoch (search requests only). */
function sendSearch(body, epoch) {
  const id = nextReqId++;
  inFlight.set(id, { body, gen: viewGen, searchEpoch: epoch });
  vscode.postMessage({ id, body });
  return id;
}

/** Render a full-pane message (loading, open error) into #rows. */
function showViewMessage(text, isError) {
  clearDetail();
  rowsEl.innerHTML = '<div class="view-message' + (isError ? ' error' : '') + '">' +
    esc(text) + '</div>';
}

/** Show a full-pane, user-visible request error with an explicit Retry action.
 *
 * Terminal GetWindow/Search failures (dead service, timed-out request) used to
 * land only in the detail pane — invisible when no inspector was open — while
 * the progressive loader retried the dead service forever. This replaces the
 * table with the error, SUSPENDS the progressive loader, and requires an
 * explicit recovery: the Retry button (or re-running the open command, which
 * re-establishes the service) re-runs the failed operation.
 */
function showRequestError(text, retryAction) {
  stopProgressiveLoader();
  clearDetail();
  rowsEl.innerHTML =
    '<div class="view-message error">' +
      '<div class="request-error-text">' + esc(text) + '</div>' +
      '<button class="retry-btn" type="button">Retry</button>' +
    '</div>';
  const btn = rowsEl.querySelector('.retry-btn');
  if (btn) {
    btn.addEventListener('click', () => {
      // Explicit recovery: clear the error state and re-run the failed op.
      window.__editchainDataReady = false;
      retryAction();
      startProgressiveLoader();
    });
  }
  window.__editchainDataReady = true;
  vscode.postMessage({ type: 'log', text: 'request error shown; loader suspended until explicit recovery' });
}

/** Collect user-facing chain warnings from an Open response.
 *
 * The service reports integrity issues in `warnings` (strings) and as
 * structured `diagnostics` (missing/corrupt/unresolved blob payloads). Both
 * are surfaced so data-integrity problems are never silently discarded.
 */
function collectOpenWarnings(value) {
  const out = [];
  if (!value || typeof value !== 'object') return out;
  if (Array.isArray(value.warnings)) {
    for (const w of value.warnings) {
      if (typeof w === 'string' && w.trim()) out.push(w.trim());
    }
  }
  const d = value.diagnostics;
  if (d && typeof d === 'object') {
    const blobs = d.blobs;
    if (blobs && typeof blobs === 'object') {
      const missing = Number(blobs.missing) || 0;
      const corrupt = Number(blobs.corrupt) || 0;
      const unresolved = Number(blobs.unresolved) || 0;
      const summary = (missing ? missing + ' missing' : '') +
        (missing && (corrupt || unresolved) ? ', ' : '') +
        (corrupt ? corrupt + ' corrupt' : '') +
        (corrupt && unresolved ? ', ' : '') +
        (unresolved ? unresolved + ' unresolved' : '');
      if (summary) {
        const already = out.some((w) => w.includes(String(Math.max(missing, corrupt, unresolved))));
        if (!already) out.push('Chain data integrity: ' + summary + ' blob payload(s) in the durable store');
      }
    }
  }
  return out;
}

/** Report the current scroll depth / total node counts to the extension host so
 * it can update the status bar. `depth` is the absolute row index at the top of
 * the viewport — how deep into the chain the user currently is (0 = newest,
 * growing toward `total` = inception). This reflects scroll position, not how
 * many rows have been fetched. */
function reportStatus() {
  vscode.postMessage({ type: 'status', loaded: viewportVisibleTop(), total: visibleTotal() });
}

/** Unwrap a service response body ({ Ok: v } | { Error: msg }). */
function unwrap(body) {
  if (body && body.Ok !== undefined) return { ok: true, value: body.Ok };
  if (body && body.Error !== undefined) return { ok: false, error: body.Error };
  // Legacy lowercase envelope: older extension-host builds posted transport
  // exceptions as { error: ... }. Treat it as an error too so failures still
  // surface instead of being rendered as data.
  if (body && body.error !== undefined) return { ok: false, error: body.error };
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
 * ALWAYS sends an explicit filter: the service's `ChainFilter::default()`
 * hides undated nodes, so sending no filter at all would silently keep hiding
 * them even when "Hide undated" is unchecked. An explicit `hide_undated: false`
 * makes the checkbox authoritative in both directions. The filter pattern is
 * treated as a regex server-side (with literal fallback) and HIDES matching
 * rows (the service preserves chain endpoints, so the pattern never removes
 * the oldest root or newest leaf); "Show messages only" maps to an INCLUSIVE
 * kind pattern that keeps message/command rows plus structural branch/reconnect
 * anchors required for continuity, so the window, layout, and sub-op counts
 * stay coherent with the filtered row set.
 */
function filterPayload() {
  const pattern = (filterEl && filterEl.value.trim()) || '';
  return {
    summary_pattern: pattern,
    kind_pattern: '',
    include_kind_pattern: showMessagesOnly() ? '^(message|command)$' : '',
    hide_undated: hideUndated(),
    splice: true,
  };
}

/** Whether only messages should be shown (from the "Show messages only" checkbox).
 *
 * Filtering is server-side: the checkbox maps to an `include_kind_pattern`
 * (an inclusive constraint, not a hide pattern) so the service re-windows the
 * filtered set (and re-emits per-filter sub-op counts). Non-message rows are
 * excluded except structural branch/reconnect anchors required to keep the
 * graph connected. This keeps the visible/absolute index mapping coherent
 * instead of hiding rows client-side after they were fetched.
 */
function showMessagesOnly() {
  return !!(hideSystemEl && hideSystemEl.checked);
}

/** Whether a search hit's summary matches the active chain-filter HIDE regex.
 *
 * The chain-filter input is a regex with a literal fallback (same matching as
 * the service's ChainFilter matcher). SearchFilters cannot carry a summary
 * pattern, so the active chain filter is enforced client-side on search hits
 * to keep search results consistent with the visible chain window. The chain
 * filter HIDES matches (with server-side endpoint preservation), so a flat
 * search result list REMOVES hits whose summary matches. An empty pattern
 * never matches (nothing is hidden).
 */
function matchesActiveChainFilter(text) {
  const pattern = (filterEl && filterEl.value.trim()) || '';
  if (!pattern) return false;
  try {
    return new RegExp(pattern).test(String(text));
  } catch {
    return String(text).indexOf(pattern) !== -1;
  }
}

/** Build the SearchFilters payload for a Search request from the active UI.
 *
 * The service's SearchFilters covers kinds / sources / actors / paths / time —
 * not submodules or a summary regex. Map what it CAN express:
 *   - "Show messages only" -> kind tags (Message, Command);
 *   - "Hide undated"       -> earliest-timestamp bound (undated rows have 0).
 * Submodule hiding and the chain-filter pattern are enforced client-side on
 * the result list (see renderSearchResults), which matches the visible chain.
 */
function searchFiltersPayload() {
  const filters = {};
  if (showMessagesOnly()) {
    filters.kinds = ['Message', 'Command'];
  }
  if (hideUndated()) {
    filters.after = 1; // undated nodes carry timestamp_ms == 0
  }
  return filters;
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

// Minimum widths for each resizable column.
const MIN_COL_W = { graph: 40, content: 60, date: 90, author: 60, commit: 60 };
// User-dragged per-column width overrides (null = default behavior):
//   graph   -> natural lane-based width (numLanes * LANE_W, bounded by budget)
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

/** The ABSOLUTE index range we want cached: the visible viewport (plus BUFFER on
 * each side) mapped back to absolute slots. Fetch/cache always use absolute
 * indices; only render/scroll/spacer use visible indices. */
function desiredCacheRange() {
  const vTop = Math.max(0, viewportVisibleTop() - BUFFER);
  const vBottom = Math.min(visibleTotal() - 1, viewportVisibleBottom() + BUFFER);
  const topAbs = absIndexForVisible(vTop);
  const bottomAbs = absIndexForVisible(vBottom);
  return {
    top: topAbs === null ? 0 : topAbs,
    bottom: bottomAbs === null ? total - 1 : bottomAbs,
  };
}

/** The VISIBLE index range we want rendered: the viewport plus BUFFER on each
 * side, in visible space. Render functions (reanchorTo/trim/append/prepend)
 * consume VISIBLE indices; only fetch/evict use absolute indices. */
function desiredVisibleRange() {
  return {
    top: Math.max(0, viewportVisibleTop() - BUFFER),
    bottom: Math.min(visibleTotal() - 1, viewportVisibleBottom() + BUFFER),
  };
}

/**
 * Fetch a window of rows around an absolute offset into the cacheable range.
 *
 * Random-access: we request whatever slice of `[top,bottom]` is missing from
 * `cache`, rather than appending sequentially from offset 0. This lets deep
 * scrolls jump straight to their target without loading everything before it.
 */
function fetchWindow() {
  // total === 0 means the server reported an empty result for the CURRENT
  // view (nothing to fetch); a reset (history/filter/search clear) marks the
  // total as unknown (-1) so a fresh window is requested under the new view.
  if (searchMode || pendingWindowReqId !== -1 || total === 0) return;
  const { top, bottom } = desiredCacheRange();
  if (top > bottom) return;

  // The service ships the expansion snapshot (sub_op_counts) only with the
  // offset-0 window. Until that arrives for the current view generation, force
  // the first page to start at offset 0 so visible/absolute index mapping is
  // established before any deep window is requested (deep restore included).
  const forceSnapshot = !snapshotEstablished;
  const rangeTop = forceSnapshot ? 0 : top;
  const rangeBottom = forceSnapshot ? Math.min(PAGE - 1, bottom) : bottom;
  if (rangeTop > rangeBottom) return;

  // Find the first missing row inside [top,bottom] to fetch next. The range is
  // bounded by BUFFER on each side of the viewport, so this scan is cheap.
  let start = -1;
  for (let i = rangeTop; i <= rangeBottom; i++) {
    if (!cache.has(i)) { start = i; break; }
  }
  if (start === -1) return; // everything we want is already cached

  const limit = Math.min(PAGE, rangeBottom - start + 1);
  pendingWindowReqId = send({
    GetWindow: { offset: start, limit, hide_submodules: hideSubmodules(), filter: filterPayload() },
  });
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

// Minimum width for the content (summary) column: wide enough for the
// "Content" header and a readable run of summary text at any viewport.
const MIN_CONTENT_W = 160;
// The graph column never exceeds this fraction of the viewport, so the
// Content/Date/Author/Commit columns always stay visible at desktop and narrow
// widths (with many concurrent lanes, lane X positions compress into this
// capped region instead of the graph hogging the table).
const GRAPH_MAX_FRACTION = 0.5;
// Lane-spacing floor in px when lane count exceeds the natural budget.
const MIN_LANE_W = 1.5;

/** Pixel budget for the graph column.
 *
 * Fixed columns (date/author/commit) and a readable content column are
 * reserved first; the graph gets the remainder, capped at `GRAPH_MAX_FRACTION`
 * of the viewport. High-lane real chains therefore compress lane positions
 * into a bounded region (see `graphLaneWidth`) instead of pushing the fixed
 * columns off-screen.
 */
function graphWidthBudget() {
  const fixedW = DEFAULT_COL_W.date + DEFAULT_COL_W.author + DEFAULT_COL_W.commit;
  const rowsW = Math.max(1, rowsEl.clientWidth);
  const graphCap = Math.max(MIN_COL_W.graph, Math.floor(rowsW * GRAPH_MAX_FRACTION));
  const avail = Math.max(MIN_COL_W.graph, rowsW - fixedW - MIN_CONTENT_W);
  return Math.min(graphCap, avail);
}

/** Effective per-lane pixel width.
 *
 * Normally `LANE_W`; when the lane count would exceed the graph budget, lanes
 * compress (down to `MIN_LANE_W`). There is NO hard lane-count cap: every
 * service lane is drawn, with spacing compressing inside the graph budget
 * (and, beyond the compression floor, laneX distributes centres proportionally
 * across the full column so no lane is ever clipped). User-dragged graph
 * widths are unaffected.
 */
function graphLaneWidth() {
  const numLanes = maxLane + 1;
  return Math.max(MIN_LANE_W, Math.min(LANE_W, graphWidthBudget() / (numLanes + 1)));
}

/** X pixel position of a lane's centre within the graph column.
 *
 * Natural placement for ordinary lane counts. If the lane count exceeds what
 * the column can fit at `MIN_LANE_W` (extreme chains), every lane centre is
 * distributed proportionally across the full column width instead — distinct
 * lanes stay monotonic and no lane is drawn outside the graph cell.
 */
function laneX(lane) {
  const w = graphLaneWidth();
  const width = currentGraphWidth();
  const numLanes = maxLane + 1;
  if (numLanes * w <= width) {
    return w / 2 + lane * w + w / 2;
  }
  return (lane + 0.5) * (width / numLanes);
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
  // A sub-op row draws NO dot — it is not a graph node. Its `above`/`below` are
  // the pass-through lanes spanning this region, drawn as full-height straight
  // lines (both halves meet at midY). Only top-level rows get a node dot.
  if (!row.is_subop) {
    // The node's own dot at its lane.
    const lane = row.lane || 0;
    const colour = COLORS[lane % COLORS.length];
    s += `<circle class="graphDot" cx="${laneX(lane)}" cy="${midY}" r="${DOT_R}" fill="${colour}"/>`;
  }
  s += '</svg>';
  return s;
}

/** Whether a row carries bundled metadata sub-ops (revealed on click). */
function hasSubOps(row) {
  return !!(row.sub_ops && row.sub_ops.length);
}

/** Map a sub-op semantic class to a VS Code Codicon glyph name.
 *
 * Codicons are injected into VS Code webviews automatically (no font file).
 * In the standalone harness they render as empty glyphs but never break text or
 * geometry checks. Falls back to a generic glyph for unknown classes.
 */
function subopIcon(subopKind) {
  switch (subopKind) {
    case 'edit': return 'edit';
    case 'msg': return 'comment';
    case 'tool_result': return 'output';
    case 'meta':
    default: return 'info';
  }
}

/** Labels for the provider-neutral structural relationship kinds.
 *
 * The service tags a row's parent edges with the kinds the projection derives
 * from `SubagentOf` / `ReconnectsTo` / `ForkOf` structural notes (see
 * crates/editchain-protocol — `HistoryRow.parent_relations`). A badge marks
 * the row itself: a row that STARTS a subagent branch, a row that RETURNS
 * into a subagent branch (completion result), or a row that forks off a
 * trunk.
 *
 * Every rendered attribute (CSS class, glyph, text, title) comes from the
 * constant labels below — the wire value is only used as a lookup key — so an
 * unknown or hostile kind can never inject a class name or markup. Unknown
 * kinds (the protocol's forward-compatible `Unknown` variant) are ignored.
 * Glyphs are basic-block arrows (U+2190–U+21FF) so they render in the
 * webview's default font stack across platforms.
 */
const REL_LABELS = {
  subagent: { cls: 'rel-subagent', glyph: '↳', text: 'subagent', title: 'Starts a subagent branch' },
  reconnect: { cls: 'rel-reconnect', glyph: '↩', text: 'return', title: 'Completion returns into the subagent branch' },
  fork: { cls: 'rel-fork', glyph: '⇉', text: 'fork', title: 'Branches off the target row at a fork boundary' },
};

/** Compact badges for a row's structural parent relations, or ''.
 *
 * Rendered inline in the content cell so the branch/start and return/completion
 * semantics survive semantic collapsing. Inline (not block) so badges never
 * affect row height — virtual scroll keeps every row exactly ROW_H and the
 * content cell clips overflow, so long summaries simply ellipsize past the
 * badges. Each badge carries a hover title and an aria-label for assistive
 * tech.
 */
function relationBadges(row) {
  const rels = Array.isArray(row.parent_relations) ? row.parent_relations : [];
  if (!rels.length) return '';
  const seen = new Set();
  let html = '';
  for (const rel of rels) {
    const kind = rel && typeof rel.kind === 'string' ? rel.kind : '';
    const label = Object.prototype.hasOwnProperty.call(REL_LABELS, kind)
      ? REL_LABELS[kind]
      : null;
    if (!label || seen.has(kind)) continue;
    seen.add(kind);
    html += '<span class="rel-badge ' + label.cls + '" title="' + esc(label.title) +
      '" aria-label="' + esc(label.title) + '">' +
      label.glyph + ' ' + label.text + '</span>';
  }
  return html;
}

/** Build one row's HTML from its cached HistoryRow. `absIdx` is its absolute index.
 *
 * Two kinds of rows:
 *   - Top-level rows carrying bundled sub-ops get a chevron affordance in their
 *     content cell; clicking toggles inline expansion (revealing one uniform
 *     ROW_H row per sub-op directly below).
 *   - Sub-op rows (`row.is_subop`) render indented with a small Codicon; clicking
 *     opens their JSON editor.
 *
 * Every `.row` stays exactly ROW_H tall so virtual-scroll math is undisturbed.
 */
function buildRowHtml(row, absIdx, isGroupStart) {
  const groupClass = isGroupStart ? ' row-group-start' : '';
  const groupLabel = isGroupStart
    ? '<div class="group-label">' + esc(groupLabelText(row.group)) + '</div>'
    : '';
  const kindClass = row.is_system ? 'row-tool'
    : (row.kind === 'message' || row.kind === 'command') ? ''
    : 'row-dim';
  const humanClass = row.author === 'human' ? ' row-human' : '';
  const subopClass = row.is_subop ? ' row-subop' : '';
  const badges = relationBadges(row);
  // Badge rows are graph-topology-critical; the CSS override lifts their text
  // cells out of the tool/dim opacity dimming so the badge stays readable at
  // full strength (row height is untouched — the class only affects opacity).
  const relClass = badges ? ' row-has-badges' : '';
  let content;
  if (row.is_subop) {
    // A bundled sub-op expanded inline: small Codicon + indented summary.
    const icon = subopIcon(row.subop_kind);
    content = '<span class="subop-icon codicon codicon-' + icon + '" aria-hidden="true"></span>' +
      '<span class="subop-summary">' + esc(row.summary || '(no summary)') + '</span>';
  } else if (hasSubOps(row)) {
    // Top-level combined op: chevron toggles inline expansion.
    const expanded = expandedBlocks.has(blockIndexOfAbs(absIdx));
    const chevron = expanded ? '▾' : '▸';
    content = '<span class="subop-chevron" title="Expand metadata records">' + chevron + '</span>' +
      badges + esc(row.summary || '(no summary)');
  } else {
    content = badges + esc(row.summary || '(no summary)');
  }
  return '<div class="row ' + kindClass + humanClass + subopClass + relClass + groupClass +
    '" data-key="' + esc(row.node_key) +
    '" data-row="' + absIdx + '" style="' + colStyle() + '">' +
    groupLabel +
    '<div class="graph-cell">' + buildGraphCell(row) + '</div>' +
    '<div class="text-cell"><div class="summary">' + content + '</div></div>' +
    '<div class="date-cell">' + esc(formatDate(row.timestamp_ms)) + '</div>' +
    '<div class="author-cell">' + esc(row.author || '') + '</div>' +
    '<div class="commit-cell">' + esc(row.commit_id || '') + '</div>' +
    '</div>';
}

/** The .table-wrap element currently in #rows, or null if not built yet. */
function wrapEl() {
  return rowsEl.querySelector('.table-wrap');
}

/** Set .table-wrap.top to position it at VISIBLE row `top`. */
function setWrapTop(top) {
  const w = wrapEl();
  if (w) w.style.top = (top * ROW_H) + 'px';
}

/** Build the sticky header row HTML. The graph column's width is derived from
 * the current `maxLane`, so this must be re-run whenever `maxLane` changes
 * (e.g. when the first GetWindow response arrives after `open`). */
function buildHeaderHtml() {
  return '<div class="tbl-header" style="' + colStyle() + '">' +
    '<div class="th graph">Graph</div>' +
    '<div class="th content">Content</div>' +
    '<div class="th date">Date</div>' +
    '<div class="th author">Author</div>' +
    '<div class="th commit">Commit/ID</div>' +
    '</div>';
}

/** Rebuild just the sticky header in place (no row rebuild) so its column
 * widths track a changed `maxLane`. The header is a direct child of #rows and
 * must NOT be rebuilt by touching .table-wrap. Re-wires resize handles since
 * their positions depend on header cell boundaries. */
function refreshHeader() {
  const header = rowsEl.querySelector('.tbl-header');
  if (!header) return;
  header.outerHTML = buildHeaderHtml();
  setupColumnResizeHandles();
}

/** Rebuild the entire window from cache in one pass. Used for initial load,
 * far jumps, column resize, filter changes, and reveal toggles — NOT for normal
 * scrolling. `top`/`bottom` are VISIBLE indices; each maps to an absolute slot,
 * and hidden (collapsed sub-op) slots are skipped so only drawable rows appear.
 */
function reanchorTo(top, bottom) {
  renderTop = top; renderBottom = bottom;
  let html = '';
  let lastGroup = null;
  for (let vis = top; vis <= bottom; vis++) {
    const absIdx = absIndexForVisible(vis);
    if (absIdx === null) continue; // hidden slot — skip entirely
    const row = cache.get(absIdx);
    if (!row) { html += '<div class="row row-placeholder" data-row="' + absIdx + '"></div>'; continue; }
    const isGroupStart = row.group !== lastGroup;
    if (isGroupStart) lastGroup = row.group;
    html += buildRowHtml(row, absIdx, isGroupStart);
  }
  // Build the sticky header + spacer + wrap in one innerHTML pass. There is a
  // SINGLE header, a direct child of #rows, so its `position: sticky; top: 0`
  // sticks to the #rows viewport and stays at the top while scrolling. It must
  // NOT live inside .table-wrap (which is positioned at renderTop*ROW_H and moves
  // with scroll) — a header there would scroll with content and appear mid-table.
  const spacerH = Math.max(1, visibleTotal() * ROW_H);
  const headerHtml = buildHeaderHtml();
  // Non-blocking chain-data warnings (Open response) sit above the table.
  const warningHtml = (!searchMode && openWarnings.length)
    ? '<div class="open-warning" role="status">' +
      openWarnings.map((w) => '<div class="open-warning-line">' + esc(w) + '</div>').join('') +
      '</div>'
    : '';
  // Search results get a compact banner so the mode is explicit and the user
  // can tell the flat result list apart from the full history window.
  const bannerHtml = searchMode
    ? '<div class="search-banner">' + esc(String(total)) + ' result' +
      (total === 1 ? '' : 's') + ' for "' + esc(searchQuery) + '"</div>'
    : '';
  // Preserve the scroll position across the DOM rebuild (setting innerHTML
  // resets scrollTop to 0).
  const prevScrollTop = rowsEl.scrollTop;
  rowsEl.innerHTML =
    warningHtml +
    bannerHtml +
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
      if (row) inspect(row, absIdx);
    });
  });
}

/** Append rows [renderBottom+1, renderBottom+n] to the bottom of the window.
 * Only extends CONTIGUOUSLY: if the immediate next VISIBLE row isn't cached yet,
 * nothing is appended (fetchWindow will deliver it and syncWindow will retry).
 * This keeps the DOM gap-free. */
function appendRowsBelow(n) {
  if (n <= 0 || renderBottom >= visibleTotal() - 1) return;
  const w = wrapEl();
  if (!w) return;
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
  for (let vis = renderBottom + 1; vis <= Math.min(renderBottom + n, visibleTotal() - 1); vis++) {
    const absIdx = absIndexForVisible(vis);
    if (absIdx === null) continue; // hidden slot — skip
    const row = cache.get(absIdx);
    if (!row) break; // stop at first gap — keep contiguous
    const isGroupStart = row.group !== lastGroup;
    if (isGroupStart) lastGroup = row.group;
    html += buildRowHtml(row, absIdx, isGroupStart);
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
  for (let vis = renderTop - 1; vis >= Math.max(0, renderTop - n); vis--) {
    const absIdx = absIndexForVisible(vis);
    if (absIdx === null) continue; // hidden slot — skip
    const row = cache.get(absIdx);
    if (!row) break; // stop at first gap — keep contiguous
    const isGroupStart = prevGroup === null || row.group !== prevGroup;
    html += buildRowHtml(row, absIdx, isGroupStart);
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
 * up by the number removed so remaining content stays put. `keepTop` is a VISIBLE
 * index. */
function trimTop(keepTop) {
  const w = wrapEl();
  if (!w || renderTop >= keepTop) return;
  let removed = Math.min(keepTop - renderTop, renderBottom - renderTop + 1);
  for (let i = renderTop; i < renderTop + removed; i++) {
    const absIdx = absIndexForVisible(i);
    if (absIdx === null) continue;
    const el = w.querySelector('.row[data-row="' + absIdx + '"]');
    if (el) el.remove();
  }
  renderTop += removed;
  setWrapTop(renderTop);
}

/** Remove rows below `keepBottom` from the bottom of the window. `keepBottom` is a
 * VISIBLE index. */
function trimBottom(keepBottom) {
  const w = wrapEl();
  if (!w || renderBottom <= keepBottom) return;
  let removed = Math.min(renderBottom - keepBottom, renderBottom - renderTop + 1);
  for (let i = renderBottom; i > renderBottom - removed; i--) {
    const absIdx = absIndexForVisible(i);
    if (absIdx === null) continue;
    const el = w.querySelector('.row[data-row="' + absIdx + '"]');
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
  if (searchMode || total <= 0) return;
  const { top: wantTop, bottom: wantBottom } = desiredVisibleRange();
  // If the window is empty or no longer covers the viewport (e.g. after a fast
  // fling evicted rows and we've scrolled back into them), reanchor cleanly from
  // cache rather than trying to incrementally patch a misaligned window. This is
  // the accepted "quick render penalty" for scrolling into unfilled territory.
  const coversViewport =
    renderTop <= viewportVisibleTop() && renderBottom >= viewportVisibleBottom();
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
  if (renderTop > wantTop || cache.has(absIndexForVisible(renderTop - 1))) {
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
  // Harness-only pause switch: lets the stale-response race test hold the
  // loader while a delayed (stale) window response lands.
  if (window.__editchainPauseLoader === true) return;
  fetchWindow();
  // Sync the window so newly-fetched rows appear even without a scroll event.
  syncWindow();
}

function inspect(row, absIdx) {
  console.log('[editchain] inspect', row && row.node_key);
  // A sub-op row opens its JSON editor directly.
  if (row.is_subop) {
    if (row.op_id) vscode.postMessage({ type: 'openJson', op_id: row.op_id });
    return;
  }
  // A top-level combined op toggles inline expansion (revealing one uniform
  // ROW_H row per bundled sub-op directly below).
  if (hasSubOps(row)) {
    if (toggleExpanded(absIdx)) {
      // Reveal state changed — rebuild the FULL desired visible window. Using
      // the old [renderTop, renderBottom] here only re-renders the pre-expansion
      // slice, so just the first sub-op slot(s) appear and the rest of the
      // viewport stays blank (ensureFilled finds nothing to fetch — the rows
      // are already cached). The desired range is in visible space, so it
      // expands to cover every newly revealed sub-op row.
      reanchorTo(desiredVisibleRange().top, desiredVisibleRange().bottom);
      ensureFilled();
    }
    return;
  }
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
    if (msg.body === null || msg.body === undefined) {
      // Explicit loading signal (e.g. the extension hasn't finished opening the
      // workspace yet). Keep the loading message on screen.
      showViewMessage('Loading history…', false);
      return;
    }
    if (r.ok && r.value) {
      window.__editchainDataReady = false;
      searchMode = false;
      searchQuery = '';
      // A fresh chain is a new view: drop any responses from a previous chain
      // (or a replayed open into a surviving context) and re-establish the
      // offset-0 expansion snapshot. The cache is cleared too: a replayed open
      // into a SURVIVING JS context must refetch against the authoritative
      // open body (otherwise fetchWindow sees every row cached and skips the
      // request, leaving stale rows under a possibly-changed total).
      viewGen++;
      inFlight.clear();
      cache.clear();
      totalFetched = 0;
      pendingWindowReqId = -1;
      currentSearchEpoch = -1;
      snapshotEstablished = false;
      subOpCounts = [];
      recomputeExpansion();
      // Surface integrity warnings (e.g. missing blob payloads) as a
      // non-blocking banner — rows still render below it.
      openWarnings = collectOpenWarnings(r.value);
      if (openWarnings.length) {
        vscode.postMessage({ type: 'log', text: 'open warnings: ' + openWarnings.join(' | ') });
      }
      vscode.postMessage({ type: 'log', text: `open: ${r.value.nodes} nodes, ${r.value.repos} repos` });
      // The server's node count is authoritative. The persisted `total` from a
      // previous session can go stale whenever the chain is reimported or
      // regenerated (node count changes), so never let `restoreState` override
      // this fresh value — otherwise the first window maps to stale offsets and
      // renders placeholders (the "newly generated chain doesn't render" bug).
      total = r.value.nodes;
      if (total === 0) {
        // Nothing to load: show an explicit empty state instead of a single
        // unfilled placeholder row that the loader can never populate.
        window.__editchainDataReady = true;
        showViewMessage('No history found in this workspace', false);
        return;
      }
      const restoredTopRow = restoreState();
      // Reanchor to the viewport window for the CURRENT (un-scrolled) position
      // first, so the spacer exists and has real height. Rows may not be cached
      // yet — GetWindow responses will append them in.
      fetchWindow();
      reanchorTo(desiredVisibleRange().top, desiredVisibleRange().bottom);
      // Now that the scaffold is built, restore the persisted scroll offset
      // (previously this set scrollTop before reanchor — before the spacer
      // existed — so it clamped to 0 and the position was lost).
      if (restoredTopRow > 0) {
        restoreScrollTop(restoredTopRow);
      } else {
        rowsEl.scrollTop = 0;
      }
      // Start the background progressive loader so history buffers ahead of the
      // scroll position without waiting for scroll events.
      startProgressiveLoader();
      reportStatus();
    } else {
      // Explicit, visible open error (spawn failure, bad chain dir, timeout,
      // service crash) — never a silent blank panel.
      const errText = String(r.error || 'unknown error');
      vscode.postMessage({ type: 'log', text: `open error: ${errText}` });
      window.__editchainDataReady = true;
      // showViewMessage escapes its text once; do NOT escape again here or the
      // message renders with literal entities (e.g. `&lt;`) for any error text
      // containing HTML characters.
      showViewMessage('Failed to open history: ' + errText, true);
    }
    return;
  }

  // The panel was revealed again (e.g. after navigating to a JSON editor and
  // back). The webview's JS context is reset when hidden, so restore the
  // persisted viewport state from vscode.setState before rendering. A short
  // delay lets the webview finish transitioning from hidden to visible so it
  // has real dimensions to measure.
  if (msg.id === 'reveal') {
    const restoredTopRow = restoreState();
    // The revealed webview is a fresh context, but be safe: start a new view
    // generation and force the offset-0 snapshot so the restored filter state's
    // expansion counts are established before any deep restore window loads.
    viewGen++;
    snapshotEstablished = false;
    subOpCounts = [];
    recomputeExpansion();
    vscode.postMessage({ type: 'log', text: 'reveal: topRow=' + restoredTopRow + ' total=' + total });
    setTimeout(() => {
      fetchWindow();
      reanchorTo(desiredVisibleRange().top, desiredVisibleRange().bottom);
      // Restore the persisted scroll offset after the scaffold is rebuilt so the
      // scroll range is real (same fix as the open handler).
      if (restoredTopRow > 0) {
        restoreScrollTop(restoredTopRow);
      }
      startProgressiveLoader();
    }, 50);
    return;
  }

  // Every other message is the correlated response to a request issued via
  // send(). Match by id; unknown ids (e.g. responses from a replayed open)
  // are dropped.
  const req = typeof msg.id === 'number' ? inFlight.get(msg.id) : undefined;
  if (!req) return;
  inFlight.delete(msg.id);
  const wasPendingWindow = msg.id === pendingWindowReqId;
  if (wasPendingWindow) pendingWindowReqId = -1;

  // Stale-view rejection: the response was issued under an older view
  // generation (open/filter/search changed while it was in flight). Applying it
  // would cache rows/totals from the old view into the new one, so drop it. If
  // it was the in-flight window, immediately request the current view's window
  // so the UI self-heals without waiting for the progressive loader.
  if (req.gen !== viewGen) {
    if (wasPendingWindow) fetchWindow();
    return;
  }

  // Latest-query-wins for search: the response is only rendered if it carries
  // the CURRENT search epoch. Both rapid searches share a view generation
  // (rendering a search bumps it), so without this check an older query's late
  // response would be treated as current — or, when the newer response lands
  // first, the older one would clobber it. A stale-epoch response is dropped
  // before any state (error rendering included) is applied.
  if (req.searchEpoch !== undefined && req.searchEpoch !== currentSearchEpoch) {
    vscode.postMessage({
      type: 'log',
      text: 'dropping stale search response (epoch ' + req.searchEpoch +
        ' of ' + currentSearchEpoch + ')',
    });
    return;
  }

  if (!r.ok) {
    const errText = String(r.error || 'unknown error');
    vscode.postMessage({ type: 'log', text: 'request error: ' + errText });
    const reqBody = req.body || {};
    // Terminal GetWindow/Search failures (dead service, hung request that
    // timed out) must be VISIBLE and must suspend retries: the 300ms
    // progressive loader retrying a dead service spins forever with no visible
    // state change. Show a full-pane error with an explicit Retry action.
    if (wasPendingWindow || reqBody.GetWindow !== undefined) {
      showRequestError('Failed to load history rows: ' + errText, () => {
        resetAndRefetch();
      });
      return;
    }
    if (reqBody.Search !== undefined) {
      showRequestError('Search failed: ' + errText, () => {
        // Re-issue the current query with a fresh epoch (explicit recovery).
        const q = searchQuery;
        currentSearchEpoch = ++searchEpoch;
        sendSearch(
          { Search: { query: q, mode: 'Lexical', top_k: 50, filters: searchFiltersPayload() } },
          currentSearchEpoch
        );
      });
      return;
    }
    // Non-window/non-search request errors (e.g. detail fetches) keep the
    // detail-pane behaviour.
    if (layoutEl.classList.contains('has-detail')) {
      detailEl.innerHTML = '<div class="detail-title">Error</div>' +
        '<pre class="detail-body">' + esc(errText) + '</pre>';
    }
    return;
  }
  if (!r.value || typeof r.value !== 'object') return;

  // Search response — a bare array of hits (harness fixtures) or the service's
  // `{ results: [...] }` envelope of scored chunks. Rendered as a flat result
  // list that navigates to the JSON editor on click.
  const searchHits = Array.isArray(r.value)
    ? r.value
    : (Array.isArray(r.value.results) ? r.value.results : null);
  if (searchHits !== null) {
    renderSearchResults(searchHits);
    return;
  }

  // GetWindow response — has a `rows` array plus `total`. Rows are placed at the
  // offset THIS request was issued with (never a global, never the request that
  // happens to be pending now), so a response can only ever write to the
  // absolute indices it asked for.
  if (Array.isArray(r.value.rows)) {
    total = r.value.total;
    // Global max lane for stable graph-column width (per-row graph cells).
    if (typeof r.value.max_lane === 'number' && r.value.max_lane !== maxLane) {
      maxLane = r.value.max_lane;
      // The header's graph-column width derives from maxLane. On `open` the
      // header is built before the first GetWindow response, so it starts
      // collapsed; refresh it now that the real lane count is known, or the
      // sticky header stays narrower than the rows it labels.
      refreshHeader();
    }
    // The service ships the expansion snapshot (sub_op_counts) only with the
    // offset-0 window. Rebuild prefix sums so visible/absolute index mapping
    // stays consistent; until this arrives for the current view, the renderer
    // keeps requesting offset 0 first (see fetchWindow).
    if (Array.isArray(r.value.sub_op_counts)) {
      subOpCounts = r.value.sub_op_counts;
      snapshotEstablished = true;
      recomputeExpansion();
      // The initial `open` render used identity mapping (counts not yet known),
      // so its rows/spacer are stale once real counts arrive. Force a re-render
      // so hidden sub-op slots collapse and the spacer height is correct.
      reanchorTo(renderTop, renderBottom);
    }
    const base = req.body.GetWindow.offset;
    for (let i = 0; i < r.value.rows.length; i++) {
      const absIdx = base + i;
      if (!cache.has(absIdx)) totalFetched++;
      cache.set(absIdx, r.value.rows[i]);
    }
    evictFarWindows();
    // Newly cached rows may extend the rendered window at either edge. Sync the
    // window to the current viewport so newly-loaded rows appear without a full
    // rebuild.
    syncWindow();
    // Correlated data-ready: rows for the requested window have arrived and
    // been rendered (placeholders replaced). The harness waits on this signal
    // so "idle" never means a placeholder-filled DOM.
    window.__editchainDataReady = true;
    if (cache.size === 0 && total === 0) {
      // A filter matched nothing (e.g. messages-only with no message rows).
      // Replace the unfillable placeholder with an explicit empty state.
      showViewMessage('No rows match the current filter', false);
    }
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

/** Normalize one search hit into a renderable HistoryRow.
 *
 * The service returns protocol `SearchHit` objects (flat, JSON-safe:
 * `{ op_id, chunk_id, score, text, source, session_id, actor_id, git_oid,
 * repository, kind, is_submodule, ... }` with every identifier an exact
 * string); the harness fixture bridge returns the same shape. HistoryRow-shaped
 * fixture rows are still accepted as passthrough.
 */
function normalizeSearchHit(hit, index) {
  if (hit && typeof hit === 'object' && typeof hit.text === 'string') {
    // Real Git hits are identified by (git_oid, repository): the service
    // indexes git commits as synthetic ops whose op_id (`0:0:generation`)
    // exists ONLY inside the search index — it is not a projection node, so
    // GetNodeDetails can never resolve it. Prefer the git identity for every
    // Git hit and drop the synthetic op_id so clicks form ResolveObject, not
    // GetNodeDetails. EditChain hits keep their exact op_id navigation.
    const isGit = hit.source === 'Git' || hit.kind === 'git' || !!hit.git_oid;
    return {
      op_id: isGit ? null : (hit.op_id || null),
      git_oid: isGit ? (hit.git_oid || null) : null,
      repository: isGit ? (hit.repository || null) : null,
      summary: hit.text,
      timestamp_ms: typeof hit.timestamp_ms === 'number' ? hit.timestamp_ms : 0,
      group: hit.session_id
        ? 'session:' + hit.session_id
        : (isGit
            ? (hit.repository ? 'repo:' + hit.repository : 'repo:search')
            : 'search'),
      node_key: isGit
        ? (hit.git_oid || ('search:' + index))
        : (hit.op_id || ('search:' + index)),
      parents: [],
      is_submodule: !!hit.is_submodule,
      is_system: false,
      author: typeof hit.actor_id === 'string' ? hit.actor_id : '',
      commit_id: '',
      kind: hit.kind || (isGit ? 'git' : 'message'),
      lane: 0,
      above: [],
      below: [],
      transitions: [],
      sub_ops: [],
      is_subop: false,
    };
  }
  return hit;
}

/** Render search hits as a flat, navigable result list in #rows.
 *
 * Suspends the virtual-scroll machinery (fetch/sync/progressive loader) — the
 * result set is small and fully local. Clicking a result opens its JSON editor
 * via the extension host. Clearing the search input restores the full history
 * window.
 */
function renderSearchResults(hits) {
  searchMode = true;
  // SearchFilters cannot express submodule exclusion, so enforce the active
  // "Show git submodules" setting client-side (mirrors GetWindow's server-side
  // hide_submodules) to keep search consistent with the visible chain. The
  // active chain-filter regex is enforced the same way, and it HIDES matches
  // (the service preserves chain endpoints for the window; a flat result list
  // simply REMOVES matching hits — see matchesActiveChainFilter). Git hits
  // carry is_submodule from the service's identity map, so this filter applies
  // to real scored search results too.
  if (hideSubmodules()) {
    hits = hits.filter((h) => !h.is_submodule);
  }
  // The chain-filter pattern HIDES matching rows; a flat search result list
  // therefore REMOVES matches (no endpoint preservation in search results).
  // No-op when the input is empty (matchesActiveChainFilter accepts
  // everything), so this only ever narrows results.
  hits = hits.filter((h) => !matchesActiveChainFilter(h.summary || h.text || ''));
  // Search replaces the view: bump the generation so any in-flight history
  // window response is rejected as stale instead of clobbering the result list,
  // and release the window slot (the result list is fully local).
  viewGen++;
  pendingWindowReqId = -1;
  snapshotEstablished = false;
  currentSearchEpoch = -1;
  const rows = hits.map(normalizeSearchHit);
  // Search results are a flat list: drop any sub-op expansion mapping from the
  // history view so visible/absolute indices are identity.
  subOpCounts = [];
  expandedBlocks.clear();
  recomputeExpansion();
  cache.clear();
  totalFetched = 0;
  lastRenderKey = '';
  total = Math.max(0, rows.length);
  rows.forEach((row, i) => cache.set(i, row));
  renderTop = 0;
  renderBottom = -1;
  rowsEl.scrollTop = 0;
  clearDetail();
  if (total === 0) {
    showViewMessage('No results for "' + esc(searchQuery) + '"', false);
  } else {
    reanchorTo(0, Math.max(0, total - 1));
  }
  window.__editchainDataReady = true;
  vscode.postMessage({ type: 'log', text: `search: ${total} result(s)` });
  reportStatus();
}

/** Reset to the full history view and reload from the top. */
function resetHistory() {
  searchMode = false;
  searchQuery = '';
  currentSearchEpoch = -1;
  // The previous view (e.g. a 0-result search) may have left `total` at 0;
  // the full-history window must be re-requested, not treated as empty.
  total = -1;
  // Returning to the full history is a new view: stale in-flight responses
  // (e.g. a Search issued before the clear) are rejected via the generation.
  viewGen++;
  snapshotEstablished = false;
  subOpCounts = [];
  recomputeExpansion();
  pendingWindowReqId = -1;
  cache.clear();
  totalFetched = 0;
  lastRenderKey = '';
  renderTop = 0;
  renderBottom = -1;
  rowsEl.scrollTop = 0;
  clearDetail();
  fetchWindow();
}

/** Clear cached rows/layout and refetch from the top (used when the chain
 * filter changes, since filtering is server-side). */
function resetAndRefetch() {
  searchMode = false;
  searchQuery = '';
  currentSearchEpoch = -1;
  // A previous filter may have matched nothing (total === 0). The new filter
  // state must re-request its own window instead of being blocked by the old
  // empty total.
  total = -1;
  // Filter changes are a new view: responses issued under the previous filter
  // state are stale once this runs and must be dropped (see the message
  // handler's generation check). The offset-0 snapshot is re-established by the
  // next fetch.
  viewGen++;
  snapshotEstablished = false;
  cache.clear();
  totalFetched = 0;
  lastRenderKey = '';
  pendingWindowReqId = -1;
  renderTop = 0;
  renderBottom = -1;
  rowsEl.scrollTop = 0;
  // Reveal state and global counts are filter-specific — reset them so the
  // next offset==0 GetWindow response rebuilds prefix sums cleanly.
  expandedBlocks.clear();
  subOpCounts = [];
  recomputeExpansion();
  fetchWindow();
}

// Search on Enter; empty query resets back to the full history.
searchEl.addEventListener('keydown', (e) => {
  if (e.key === 'Enter') {
    const q = searchEl.value.trim();
    if (q) {
      searchQuery = q;
      // Latest-query-wins: tag this search with a fresh epoch so its response
      // is rendered even if an earlier search's (older-epoch) response lands
      // first, and so a late older response can never overwrite it. Results
      // render/navigate as a flat list on the Search response; the filters
      // carry the active chain/hide-undated/messages-only semantics.
      currentSearchEpoch = ++searchEpoch;
      sendSearch(
        { Search: { query: q, mode: 'Lexical', top_k: 50, filters: searchFiltersPayload() } },
        currentSearchEpoch
      );
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

// Re-fetch when the messages-only toggle changes. Filtering is server-side
// (kind_pattern in the filter payload), so the window, layout, and sub-op
// counts all stay coherent with the filtered row set.
if (hideSystemEl) {
  hideSystemEl.addEventListener('change', () => {
    resetAndRefetch();
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
  window.__editchainProgressiveTimerActive = true;
}
function stopProgressiveLoader() {
  if (progressiveTimer) {
    clearInterval(progressiveTimer);
    progressiveTimer = null;
  }
  window.__editchainProgressiveTimerActive = false;
}

// Infinite scroll: extend/trim the rendered window at its edges so content
// follows the cursor in both directions without a full rebuild. Throttled to a
// frame so we don't run syncWindow on every scroll event.
let syncTimer = null;
rowsEl.addEventListener('scroll', () => {
  if (searchMode) {
    reportStatus();
    return;
  }
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
  resizeTimer = setTimeout(onViewportResize, 150);
});

/** Rebuild the layout after a viewport resize.
 *
 * Every piece of graph geometry derives from the viewport width: the graph
 * column's pixel budget (graphWidthBudget), the per-lane compression
 * (graphLaneWidth), each row SVG cell's width (buildGraphCell), and the
 * header's track widths (buildHeaderHtml). syncWindow only trims/appends rows
 * — it never rebuilds cells with new widths — so a resize must re-render the
 * current window (and the header, which reanchorTo rebuilds).
 */
function onViewportResize() {
  if (total <= 0) return;
  if (searchMode) {
    // Search results are a flat local list: re-render with the new widths.
    reanchorTo(0, Math.max(0, total - 1));
    return;
  }
  reanchorTo(renderTop, renderBottom);
  syncWindow();
  ensureFilled();
}

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
 * on the final lane boundary) isn't clipped by the column's overflow. The
 * column never exceeds the viewport graph budget (GRAPH_MAX_FRACTION): lanes
 * compress (graphLaneWidth) and, beyond the compression floor, distribute
 * proportionally inside the budget (laneX) — so NO lane count is ever clipped
 * or allowed to push the content column off-screen. A user drag overrides the
 * natural width entirely.
 */
function currentGraphWidth() {
  // Use the GLOBAL max lane (reported by the server) so the graph column width
  // is stable regardless of which window is loaded — lanes don't jump on scroll.
  if (colWidths.graph !== null) return colWidths.graph;
  const numLanes = maxLane + 1;
  const w = graphLaneWidth();
  const natural = (numLanes + 1) * w;
  return Math.round(Math.min(natural, graphWidthBudget()) * 100) / 100;
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

// Harness-only debug hooks (not production behaviour): expose the cached row at
// an absolute index and the current authoritative total so a text-only probe can
// assert on real row payloads (e.g. deterministic date rendering).
window.__editchainRowAt = function (absIdx) {
  return cache.get(absIdx) || null;
};
window.__editchainGetTotal = function () {
  return total;
};

// Harness-only debug hooks (not production behaviour): expose the renderer's
// REAL in-flight request count and progressive-loader state so the layout
// probe's whenIdle can wait on actual renderer state even in real VS Code
// (where `window.vscode` is not exposed and the probe's own postMessage hook
// is a no-op).
window.__editchainInFlightCount = function () {
  return inFlight.size;
};
window.__editchainProgressiveTimerActive = false;
