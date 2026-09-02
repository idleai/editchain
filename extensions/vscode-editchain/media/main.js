// Webview renderer for the EditChain History explorer.
// Renders a git-graph-style visualization of unified history as one small SVG
// per row (the node dot, vertical lane segments, and smooth cross-lane
// transition paths) over a real table with columns: Graph | Content | Date |
// Author | Commit/ID.
//
// The per-row graph geometry (lane, above, below, transitions) is computed
// server-side by the Rust service and shipped over stdio with each `GetWindow`
// response; this file only draws it.
//
// The webview is a THIN VIEWPORT over a server-owned graph. It renders only the
// visible slice of rows plus a buffer on each side, and requests windowed
// layouts around the scroll position. It does NOT accumulate the whole history:
// far-offscreen windows are evicted from a sparse cache. This keeps DOM size,
// JS heap, and per-scroll serialization bounded regardless of chain size (the
// design target is ~1M nodes).
//

// @ts-ignore — vscode provides this global in webviews.
const vscode = acquireVsCodeApi();
// Distinguishes this concrete main.js context from an older one owned by the
// same WebviewPanel. The host uses it to replay Open only after this instance's
// message listener is installed, never during an ordinary retained reveal.
const rendererInstanceId = Date.now().toString(36) + '-' +
  Math.random().toString(36).slice(2);

const rowsEl = document.getElementById('rows');
const searchEl = document.getElementById('search');
const searchCounterEl = document.getElementById('search-counter');
const searchPrevBtn = document.getElementById('search-prev');
const searchNextBtn = document.getElementById('search-next');
const statusLiveEl = document.getElementById('status-live');
const profileActivityBtn = document.getElementById('profile-activity');
const profileRawBtn = document.getElementById('profile-raw');

// Temporary fixed view while the filtering experience is redesigned. The
// Activity/Raw profile control toggles `hide_trace`: Activity (the default
// pregenerated view) hides internal trace records server-side; Raw shows the
// complete record stream. The flag rides inside the chain filter on every
// GetWindow/layout follow-up so the whole view (rows + geometry) stays
// coherent under the active profile.
const FIXED_HIDE_SUBMODULES = true;
const FIXED_FILTER = Object.freeze({
  summary_pattern: '',
  kind_pattern: '',
  include_kind_pattern: '',
  hide_undated: false,
  splice: true,
  hide_trace: true,
});

// Active profile: 'activity' (hide_trace=true) or 'raw' (hide_trace=false).
let profile = 'activity';
function profileLabel() {
  return profile === 'activity' ? 'Activity' : 'Raw';
}
/** Whether the current view profile hides trace records. */
function hideTrace() {
  return profile === 'activity';
}

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
// Whether the service has built global lane geometry for the current view.
// The first page deliberately requests row data without it, paints, then
// repeats the same page with layout enabled.
let layoutReady = false;

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

// Inline selection state. The history remains a single surface: selecting a
// row never opens a secondary pane. Enter or double-click is the explicit path
// to the existing read-only raw JSON editor.
let selectedRowKey = null;
// Roving-tabindex anchor: the ABSOLUTE index of the single tabbable row in the
// rendered window. Only that row is in the tab order (Tab enters/exits the
// grid as a unit); ArrowUp/Down move focus between rendered rows instead of
// tabbing through every virtualized row. Falls back to the first rendered row
// whenever the anchor is trimmed away by virtual scrolling (applyRovingTabindex).
let rovingAbs = -1;
// Announced the initial history load once (aria-live), not on every page.
let announcedInitialLoad = false;

// Show an explicit loading state until the extension host finishes `Open` and
// the first window arrives (or surfaces the open error). Runs after the state
// declarations above are initialized.
showViewMessage('Loading history…', false);

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

// Find-in-chain state (in-place find over the real history view). Unlike the
// legacy flat-list Search, a find NEVER replaces the history DOM: the backend
// resolves every hit to a real top-level row of the ACTIVE view, and the
// session only scrolls, highlights, and updates the adjacent counter. All
// other view state (profile, expansion, cache, layout, scroll) stays intact.
const FIND_TOP_K = 50;        // candidate cap; the response reports more=true
let findActive = false;       // a find session has settled
let findMatches = [];         // settled ranked matches (FindInHistoryMatch)
let findTotal = 0;            // navigable distinct match count
let findMore = false;         // response.more — more may exist past the cap
let findIndex = 0;            // current match (0-based)
let pendingFindTarget = null; // { abs, index } awaiting its window in cache

// --- Request correlation ----------------------------------------------------
//
// Every service request carries a client-generated id, and the extension host
// echoes that id back with the response. Responses are correlated by id (not by
// response shape), and tagged with the VIEW GENERATION they were issued under.
// When the view changes (open, history reset, search, clear), the generation
// increments; any in-flight response from an older generation is dropped so it
// can never poison cache/total/view state (e.g. a GetWindow issued before a
// history reset landing after it, or a search overlapping an in-flight
// window).
let nextReqId = 1;
const inFlight = new Map();
let viewGen = 0;
// Whether the current view's expansion snapshot (sub_op_counts) has been received
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
// shipped once per view generation from the server when offset==0).

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

/** Persist only safe profile + viewport state for genuine context recreation.
 * Raw JSON editor navigation retains the live bounded cache. We never
 * serialize row payloads, search state, filters, or totals: they can exceed
 * VS Code's webview-state size limit or go stale when the chain is reimported,
 * and a recreated webview can refetch its bounded window cheaply. The profile
 * choice is safe (it only changes which filter flag the first fetch sends) and
 * the visible TOP ROW INDEX survives expansion differences as `topRow * ROW_H`. */
function saveState() {
  vscode.setState({
    profile,
    // Persist the visible TOP ROW INDEX, not raw pixel scrollTop: after a real
    // context recreation the spacer/scaffold doesn't exist until `reanchorTo`,
    // so a raw pixel offset clamps to 0. A row index survives expansion
    // differences and is reapplied as `topRow * ROW_H` once rows load.
    topRow: viewportVisibleTop(),
  });
}

/** Return the saved `{ topRow, profile }` WITHOUT touching scrollTop — the
 * spacer isn't built yet here, so applying scroll must wait until the
 * open/reveal handler has reanchored. Legacy persisted filter/total keys are
 * ignored; an absent or unknown profile defaults to Activity. */
function restoreState() {
  const s = vscode.getState();
  let topRow = -1;
  if (s && typeof s.topRow === 'number') {
    // Do NOT restore `total` here — the chain may have been reimported since the
    // last session, so the persisted node count can be stale. The server's
    // `total` is authoritative and is applied by the open handler before this
    // runs. Only the top row index survives a recreated webview.
    if (s.topRow > 0) topRow = s.topRow;
  }
  const savedProfile = s && s.profile === 'raw' ? 'raw' : 'activity';
  return { topRow, profile: savedProfile };
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
const COLORS = ['#48f1dc', '#a18aff', '#6ee7a2', '#5ca8ff', '#ffc86a', '#ff70a6', '#72ddf7', '#c77dff', '#64dfdf', '#ff8fa3'];

const LANE_W = 18;
const DOT_R = 4;
// Execute-run bundle node glyph: typed Activity bundle rows replace the
// ordinary single graph dot with a compact vertical capsule with separate
// entry (above the row midpoint) and exit (below it) terminals. The terminal
// radius follows the dot radius so dense lane compression shrinks the whole
// glyph proportionally; the half-span (terminal distance from the row
// midpoint) is fixed so the glyph keeps a stable compact footprint inside the
// 34px row. The capsule is `BUNDLE_CAPSULE_MARGIN` px wider than the terminal
// diameter on each side.
const BUNDLE_TERMINAL_RATIO = 0.75;
const BUNDLE_HALF_SPAN = 7;
const BUNDLE_CAPSULE_MARGIN = 1;

/** Send a GetWindow request and mark it as the in-flight window BEFORE posting.
 *
 * The fixture bridge responds inside postMessage, so assigning the pending id
 * only after posting would happen after the response had already been processed
 * re-entrantly — the response's
 * `wasPendingWindow` correlation would miss, and the late assignment would leave
 * a PHANTOM pending id that blocks every later fetchWindow (the harness scroll
 * race). Registering the id up front keeps `wasPendingWindow` correct in both
 * the synchronous harness and the asynchronous real service.
 */
function sendWindow(body) {
  const id = nextReqId++;
  inFlight.set(id, { body, gen: viewGen });
  pendingWindowReqId = id;
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
  clearSelection();
  rowsEl.innerHTML = '<div class="view-message' + (isError ? ' error' : '') + '" role="' +
    (isError ? 'alert' : 'status') + '">' +
    esc(text) + '</div>';
}

/** Show a full-pane, user-visible request error with an explicit Retry action.
 *
 * Terminal GetWindow/Search failures (dead service, timed-out request) replace
 * the table with an explicit error, SUSPEND the progressive loader, and require
 * explicit recovery: the Retry button (or re-running the open command, which
 * re-establishes the service) re-runs the failed operation.
 */
function showRequestError(text, retryAction) {
  stopProgressiveLoader();
  clearSelection();
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

/** Remove Markdown decoration while preserving the readable label text.
 *
 * This is intentionally small and display-oriented rather than a second full
 * Markdown parser. It is used for tooltips, accessible names, and semantic
 * headings such as work-unit titles, where literal `**`, backticks, link
 * destinations, or raw HTML would add noise. Inline code keeps its contents,
 * links keep their labels, escaped punctuation is restored, and imported HTML
 * is omitted rather than interpreted inside the privileged webview. */
function markdownPlainInline(value) {
  let source = String(value);
  source = source.replace(/\\([\\`*_[\]{}()#+\-.!~>])/g, '$1');
  source = source.replace(/!\[([^\]\n]*)\]\([^\n)]*\)/g, '$1');
  source = source.replace(/\[([^\]\n]+)\]\([^\n)]*\)/g, '$1');
  source = source.replace(/`+([^`\n]*?)`+/g, '$1');
  source = source.replace(/\*\*([^*\n]+)\*\*/g, '$1');
  source = source.replace(/__([^_\n]+)__/g, '$1');
  source = source.replace(/~~([^~\n]+)~~/g, '$1');
  source = source.replace(/(^|[\s([{<:;,.!?-])\*([^*\n]+)\*(?=$|[\s)\]}>:;,.!?-])/g, '$1$2');
  source = source.replace(/(^|[\s([{<:;,.!?-])_([^_\n]+)_(?=$|[\s)\]}>:;,.!?-])/g, '$1$2');
  source = source.replace(/<\/?[A-Za-z][^>\n]*>/g, '');
  // Unmatched decoration runs are never useful in a compact display label.
  // Preserve single `*` / `_` characters so commands and identifiers are not
  // mangled, but remove unmistakable Markdown delimiter runs and backticks.
  source = source.replace(/\*{2,}|~{2,}|`+/g, '');
  return source.replace(/\s+/g, ' ').trim();
}

/** Plain-text form of one Markdown source line. Block prefixes are presentation
 * syntax, so headings/lists/tasks/quotes/fences lose their punctuation while
 * retaining the actual sentence. */
function markdownPlainLine(value) {
  let line = String(value).trim();
  line = line.replace(/^#{1,6}\s+/, '');
  line = line.replace(/^[-+*]\s+\[[ xX]\]\s+/, '');
  line = line.replace(/^[-+*]\s+/, '');
  line = line.replace(/^\d+[.)]\s+/, '');
  line = line.replace(/^>\s*/, '');
  line = line.replace(/^\[![A-Za-z]+\]\s*/, '');
  line = line.replace(/^`{3,}\s*[A-Za-z0-9_+.-]*\s*/, '');
  return markdownPlainInline(line);
}

/** Readable plain-text summary for tooltips/ARIA. Unlike the visible preview,
 * this keeps every meaningful source line so truncation never removes context. */
function markdownPlainSummary(value) {
  return String(value).replace(/\r\n?/g, '\n').split('\n')
    .map(markdownPlainLine).filter(Boolean).join(' · ');
}

/** Text-bearing fields commonly found in provider-neutral structured tool
 * envelopes. Ordering matters: prose/output wins over request metadata. */
const TOOL_PAYLOAD_TEXT_KEYS = [
  'text', 'output_text', 'input_text', 'message', 'summary', 'output',
  'content', 'status', 'completed', 'failed', 'error',
  'cmd', 'command', 'query', 'path',
];

/** Find the first useful string in a parsed tool envelope without displaying
 * serializer fields such as `type: input_text`. The small visit budget keeps
 * imported, unexpectedly deep payloads from monopolizing the webview. */
function firstToolPayloadText(value) {
  const pending = [value];
  let visits = 0;
  while (pending.length && visits < 48) {
    const current = pending.shift();
    visits++;
    if (typeof current === 'string' && current.trim()) return current;
    if (Array.isArray(current)) {
      pending.push(...current);
      continue;
    }
    if (!current || typeof current !== 'object') continue;
    TOOL_PAYLOAD_TEXT_KEYS.forEach((key) => {
      if (Object.prototype.hasOwnProperty.call(current, key)) pending.push(current[key]);
    });
  }
  return '';
}

/** Decode a JSON tool payload, with a narrow recovery path for summaries that
 * were truncated after serialization. Raw source is still retained in the row
 * tooltip and the JSON editor; this only chooses the visible preview text. */
function decodedToolPayloadText(value) {
  const source = String(value).trim();
  try {
    let parsed = JSON.parse(source);
    // Some adapters serialize a JSON envelope as a JSON string. Unwrap at most
    // once so malformed or recursive input cannot create unbounded work.
    if (typeof parsed === 'string' && /^(?:\[\s*(?:\{|"|\])|\{\s*(?:"|\}))/.test(parsed.trim())) {
      parsed = JSON.parse(parsed);
    }
    return firstToolPayloadText(parsed);
  } catch (_) {
    const textField = /"(?:text|output_text|input_text|message|summary|output|completed|failed|error|cmd|command)"\s*:\s*"((?:\\.|[^"\\])*)/.exec(source);
    if (!textField) return '';
    const encoded = textField[1].replace(/\\$/, '');
    try {
      return JSON.parse('"' + encoded + '"');
    } catch (_) {
      // A summary may be cut in the middle of a JSON string. Recover only the
      // harmless display escapes needed by the first-line preview.
      return encoded.replace(/\\r\\n|\\n|\\r/g, '\n')
        .replace(/\\"/g, '"').replace(/\\\\/g, '\\');
    }
  }
}

/** Reduce multi-line execution envelopes to their meaningful first status or
 * output line. Standard script lifecycle wording is normalized so wall time,
 * cell IDs, and serializer details do not become the row's primary content. */
function conciseToolText(value) {
  const lines = String(value).replace(/\r\n?/g, '\n').split('\n')
    .map((line) => line.trim()).filter(Boolean);
  const status = lines.find((line) => /^Script\s+(?:completed|failed|running)\b/i.test(line));
  if (/^Script\s+completed\b/i.test(status || '')) return 'Script completed';
  if (/^Script\s+failed\b/i.test(status || '')) return 'Script failed';
  if (/^Script\s+running\b/i.test(status || '')) return 'Script running';
  return lines.length ? lines[0].replace(/\s+/g, ' ') : '';
}

/** Presentation-only compaction for action/result payloads. Human narrative is
 * left untouched for Markdown rendering. Obvious JSON/tool wrappers become a
 * readable line; undecodable structured payloads receive a calm generic label
 * instead of leaking brackets, escaped newlines, and serializer field names. */
function displaySummaryForRow(row, value) {
  const source = String(value);
  const toolishRow = row && (row.record_role === 'action' || row.record_role === 'result' ||
    row.kind === 'tool' || row.kind === 'command') &&
    (row.activity_kind === 'execute' || row.is_system || row.kind === 'tool' || row.kind === 'command');
  const trimmed = source.trim();
  const container = /^<[A-Za-z][A-Za-z0-9_.:-]*>\s*/.exec(trimmed);
  // Service summaries can be length-capped before an outer notification's
  // closing tag. Removing only a leading, inert container is enough to detect
  // the JSON body while leaving ordinary wrapped prose on its original path.
  const candidate = container ? trimmed.slice(container[0].length).trim() : trimmed;
  const wrapper = /^tool:\s*[A-Za-z0-9_.:-]+(?:\s+|$)/i.exec(candidate);
  const payload = wrapper ? candidate.slice(wrapper[0].length).trim() : candidate;
  const jsonish = /^(?:\[\s*(?:\{|"|\])|\{\s*(?:"|\})|")/.test(payload);
  if (!toolishRow && !jsonish) return source;
  if (toolishRow && !wrapper && !jsonish) return source;

  const decoded = jsonish ? decodedToolPayloadText(payload) : payload;
  const concise = conciseToolText(decoded);
  if (concise) return concise;
  // A narrative JSON sample with no recognized text-bearing envelope remains
  // authored content. Generic fallback labels are reserved for operational
  // rows whose payload is known to be presentation metadata.
  if (!toolishRow) return source;
  if (row.outcome === 'success') return 'Completed';
  if (row.outcome === 'failure') return 'Failed';
  if (row.outcome === 'warning') return 'Completed with warnings';
  if (row.outcome === 'cancelled') return 'Cancelled';
  if (row.record_role === 'action' || row.kind === 'command') return 'Tool request';
  return 'Tool result';
}

/** Split a Git conventional prefix from the first colon. This is presentation
 * only: the service summary and raw commit message retain their exact bytes. */
function gitSummaryParts(row, value) {
  if (!row || !row.git_oid) return null;
  const source = String(value);
  const colon = source.indexOf(':');
  if (colon <= 0) return null;
  const prefix = source.slice(0, colon).trim();
  if (!prefix) return null;
  return {
    prefix,
    content: source.slice(colon + 1).trimStart(),
  };
}

/** Render Git's leading prefix as a chip and omit the delimiter. Ordinary
 * summaries keep the same Markdown-safe compositor. */
function renderRowSummary(row, value) {
  const git = gitSummaryParts(row, value);
  if (!git) {
    return '<span class="summary-text">' + renderMarkdownSummary(value) + '</span>';
  }
  const content = git.content
    ? '<span class="summary-text git-summary-text">' +
      renderMarkdownSummary(git.content) + '</span>'
    : '';
  return '<span class="git-prefix-chip" title="' + esc('Commit prefix: ' + git.prefix) +
    '" aria-label="' + esc('Commit prefix: ' + git.prefix) + '">' +
    esc(git.prefix) + '</span>' + content;
}

/** Plain-text equivalent of [`renderRowSummary`] for labels and de-duplication
 * comparisons. The visible colon is intentionally absent here too. */
function plainRowSummary(row, value) {
  const git = gitSummaryParts(row, value);
  if (!git) return markdownPlainSummary(value);
  return markdownPlainSummary(git.prefix + (git.content ? ' ' + git.content : ''));
}

/** Find an unescaped closing Markdown delimiter. */
function markdownClosing(source, delimiter, start) {
  let at = source.indexOf(delimiter, start);
  while (at >= 0) {
    if (source[at - 1] !== '\\' && at > start) return at;
    at = source.indexOf(delimiter, at + delimiter.length);
  }
  return -1;
}

/** Render the small inline Markdown subset that can remain legible in one
 * fixed-height history row. Raw HTML is never accepted: every text fragment
 * and tooltip is escaped, and Markdown links are visual spans rather than
 * navigable anchors. This keeps imported session content inert inside the
 * privileged VS Code webview while preserving its reading hierarchy. */
function renderMarkdownInline(value, depth) {
  const source = String(value);
  const level = depth || 0;
  if (level > 4) return '<span class="md-text">' + esc(markdownPlainInline(source)) + '</span>';

  let html = '';
  let plain = '';
  const flushPlain = () => {
    if (!plain) return;
    const cleaned = markdownPlainInline(plain);
    if (cleaned) {
      // markdownPlainInline trims labels by design. Restore one collapsed edge
      // space for an inline fragment so `text **strong** text` does not become
      // `textstrongtext` when separate FLEX items meet in the DOM. Non-breaking
      // spaces survive flex-item boundary whitespace trimming.
      const leading = /^\s/.test(plain) ? '\u00a0' : '';
      const trailing = /\s$/.test(plain) ? '\u00a0' : '';
      html += '<span class="md-text">' + esc(leading + cleaned + trailing) + '</span>';
    } else if (/\s/.test(plain)) {
      // A whitespace-only fragment can sit between adjacent formatted spans.
      html += '<span class="md-space" aria-hidden="true">\u00a0</span>';
    }
    plain = '';
  };
  let i = 0;
  while (i < source.length) {
    // Markdown escapes: show the escaped punctuation without the backslash.
    if (source[i] === '\\' && i + 1 < source.length && /[\\`*_[\]{}()#+\-.!~>]/.test(source[i + 1])) {
      plain += source[i + 1];
      i += 2;
      continue;
    }

    // Images and links stay non-navigable in the dense row. The destination is
    // available as an escaped tooltip, while the label keeps inline emphasis.
    const link = /^(!?)\[([^\]\n]+)\]\(([^)\n]+)\)/.exec(source.slice(i));
    if (link) {
      flushPlain();
      const image = link[1] === '!';
      const label = renderMarkdownInline(link[2], level + 1);
      const target = link[3].trim();
      html += '<span class="' + (image ? 'md-image' : 'md-link') + '" title="' +
        esc(target) + '">' + (image ? '<span aria-hidden="true">image · </span>' : '') + label + '</span>';
      i += link[0].length;
      continue;
    }

    // Code spans are literal: no Markdown is interpreted inside them.
    if (source[i] === '`') {
      const run = /^`+/.exec(source.slice(i))[0];
      const close = markdownClosing(source, run, i + run.length);
      if (close >= 0) {
        flushPlain();
        const code = source.slice(i + run.length, close).replace(/^ | $/g, '');
        html += '<code class="md-code">' + esc(code) + '</code>';
        i = close + run.length;
        continue;
      }
    }

    const paired = [
      { delimiter: '**', open: '<strong class="md-strong">', close: '</strong>' },
      { delimiter: '__', open: '<strong class="md-strong">', close: '</strong>' },
      { delimiter: '~~', open: '<span class="md-strike">', close: '</span>' },
    ].find((token) => source.startsWith(token.delimiter, i));
    if (paired) {
      const close = markdownClosing(source, paired.delimiter, i + paired.delimiter.length);
      if (close >= 0) {
        flushPlain();
        html += paired.open + renderMarkdownInline(
          source.slice(i + paired.delimiter.length, close), level + 1
        ) + paired.close;
        i = close + paired.delimiter.length;
        continue;
      }
    }

    // Conservative single-emphasis handling avoids treating identifiers such
    // as `work_unit_id` as italics while still respecting prose emphasis.
    if (source[i] === '*' || source[i] === '_') {
      const delimiter = source[i];
      const previous = i > 0 ? source[i - 1] : '';
      const next = source[i + 1] || '';
      const boundaryBefore = i === 0 || /[\s([{<:;,.!?-]/.test(previous);
      const close = markdownClosing(source, delimiter, i + 1);
      const after = close >= 0 ? source[close + 1] || '' : '';
      const boundaryAfter = close >= 0 && (!after || /[\s)\]}>:;,.!?-]/.test(after));
      if (boundaryBefore && next && !/\s/.test(next) && boundaryAfter) {
        flushPlain();
        html += '<em class="md-em">' + renderMarkdownInline(source.slice(i + 1, close), level + 1) + '</em>';
        i = close + 1;
        continue;
      }
    }

    plain += source[i];
    i++;
  }
  flushPlain();
  return html;
}

/** Render one Markdown source line with a compact semantic prefix. */
function renderMarkdownLine(value) {
  const line = String(value).trim();
  if (!line) return '';

  const heading = /^(#{1,6})\s+(.+?)\s*#*$/.exec(line);
  if (heading) {
    return '<span class="md-line md-heading md-h' + heading[1].length + '">' +
      renderMarkdownInline(heading[2]) + '</span>';
  }

  const task = /^[-+*]\s+\[([ xX])\]\s+(.+)$/.exec(line);
  if (task) {
    const done = task[1].toLowerCase() === 'x';
    return '<span class="md-line md-list md-task' + (done ? ' md-task-done' : '') + '">' +
      '<span class="md-marker" aria-hidden="true">' + (done ? '✓' : '○') + '</span>' +
      renderMarkdownInline(task[2]) + '</span>';
  }

  const unordered = /^[-+*]\s+(.+)$/.exec(line);
  if (unordered) {
    return '<span class="md-line md-list"><span class="md-marker" aria-hidden="true">•</span>' +
      renderMarkdownInline(unordered[1]) + '</span>';
  }

  const ordered = /^(\d+[.)])\s+(.+)$/.exec(line);
  if (ordered) {
    return '<span class="md-line md-list"><span class="md-marker" aria-hidden="true">' +
      esc(ordered[1]) + '</span>' + renderMarkdownInline(ordered[2]) + '</span>';
  }

  const quote = /^>\s*(.+)$/.exec(line);
  if (quote) {
    const callout = /^\[!([A-Za-z]+)\]\s*(.*)$/.exec(quote[1]);
    if (callout) {
      return '<span class="md-line md-quote"><span class="md-callout">' + esc(callout[1]) + '</span>' +
        renderMarkdownInline(callout[2]) + '</span>';
    }
    return '<span class="md-line md-quote"><span class="md-marker" aria-hidden="true">›</span>' +
      renderMarkdownInline(quote[1]) + '</span>';
  }

  const fence = /^`{3,}\s*([A-Za-z0-9_+.-]*)\s*(.*)$/.exec(line);
  if (fence) {
    const language = fence[1] || 'code';
    return '<span class="md-line md-fence"><span class="md-callout">' + esc(language) + '</span>' +
      renderMarkdownInline(fence[2]) + '</span>';
  }

  return '<span class="md-line">' + renderMarkdownInline(line) + '</span>';
}

/** Convert Markdown into a safe, restrained one-line read-through.
 *
 * A fixed-height history row is a preview, not a document viewport. Render the
 * first meaningful source line with simple Markdown semantics and summarize
 * the remainder as a quiet `+N lines` tail. This avoids the previous horizontal
 * parade of three miniature paragraphs while preserving the full plain source
 * in the row tooltip and accessible name. */
function renderMarkdownSummary(value) {
  const lines = String(value).replace(/\r\n?/g, '\n').split('\n')
    .map((line) => line.trim())
    .filter((line) => line && markdownPlainLine(line));
  const rendered = lines.length ? renderMarkdownLine(lines[0]) : '';
  if (!rendered) return '<span class="md-line md-empty">Structured content</span>';
  if (lines.length > 1) {
    const hidden = lines.length - 1;
    return rendered + '<span class="md-more" aria-hidden="true">+' + hidden +
      (hidden === 1 ? ' line' : ' lines') + '</span>';
  }
  return rendered;
}


/** Whether nested Git repositories/submodules are hidden in the fixed view. */
function hideSubmodules() {
  return FIXED_HIDE_SUBMODULES;
}

/** Explicit fixed filter sent to avoid the service's legacy implicit default. */
function filterPayload() {
  return { ...FIXED_FILTER, hide_trace: hideTrace() };
}

/** Search-filters DTO for FindInHistory (kinds, sources, sessions, actors,
 * paths, times). The UI has no filter controls yet, so the defaults apply; the
 * profile/chain filter is carried separately via filterPayload() so the
 * backend resolves hits against the exact active view. */
function searchFiltersPayload() {
  return {};
}

// --- Find-in-chain ---------------------------------------------------------
//
// In-place find over the real history view. A settled FindInHistory response
// carries ranked matches resolved to real top-level rows of the ACTIVE view
// (absolute expanded-history parent-row offsets, GetWindow coordinates). The
// find never rebuilds the chain: it only scrolls, highlights the current
// match, and updates the adjacent counter. Navigation keeps focus in the
// search input so query editing stays easy.

/** Normalize one FindInHistoryMatch into the shape the find session consumes:
 * the stable real node key plus the absolute expanded-history parent-row
 * offset the backend resolved under the active view. A row coordinate that is
 * absent (null/undefined/empty) or not a finite, whole, in-range offset
 * (negative, fractional, NaN, or beyond the view total) normalizes to null, so
 * the session only ever navigates to coordinates the cache can hold — a
 * malformed backend row can never stall a pending jump, and a missing row is
 * never silently mapped to row 0. */
function normalizeFindMatch(m) {
  const raw = m.row;
  const row = typeof raw === 'number' ? raw : Number(raw);
  if (raw === null || raw === undefined || raw === '' ||
      !Number.isFinite(row) || !Number.isInteger(row) || row < 0 || row >= total) {
    return null;
  }
  return {
    row,
    node_key: String(m.node_key || ''),
    summary: String(m.summary || ''),
    op_id: m.op_id || null,
    git_oid: m.git_oid || null,
    repository: m.repository || null,
    source: m.source || 'EditChain',
    is_submodule: !!m.is_submodule,
  };
}

/** Render the adjacent find counter. Every state is compact and none replaces
 * the chain: pending "…", settled "i of N" (or "N+" when the response was
 * truncated), "0 of 0", and a styled "error" with the reason as a tooltip. */
function updateFindCounter(state, detail) {
  if (!searchCounterEl) return;
  searchCounterEl.classList.remove(
    'search-counter-pending', 'search-counter-zero', 'search-counter-error');
  let text = '';
  if (state === 'pending') {
    text = '…';
    searchCounterEl.classList.add('search-counter-pending');
    searchCounterEl.setAttribute('aria-label', 'Searching…');
    searchCounterEl.removeAttribute('title');
  } else if (state === 'zero') {
    text = '0 of 0';
    searchCounterEl.classList.add('search-counter-zero');
    searchCounterEl.removeAttribute('aria-label');
    searchCounterEl.removeAttribute('title');
  } else if (state === 'error') {
    text = 'error';
    searchCounterEl.classList.add('search-counter-error');
    searchCounterEl.setAttribute('aria-label', 'Find failed' + (detail ? ': ' + detail : ''));
    searchCounterEl.title = detail || '';
  } else if (state === 'settled') {
    text = (findIndex + 1) + ' of ' + findTotal + (findMore ? '+' : '');
    searchCounterEl.removeAttribute('aria-label');
    searchCounterEl.removeAttribute('title');
  } else {
    searchCounterEl.removeAttribute('aria-label');
    searchCounterEl.removeAttribute('title');
  }
  searchCounterEl.textContent = text;
  syncFindNavButtons();
}

/** Whether the exact submitted query has a settled, navigable find: matches
 * exist, no replacement is in flight, and the input text still matches the
 * submitted query (edited-but-unsubmitted text never navigates stale
 * matches). Mirrors the ArrowUp/ArrowDown guard in the search keydown
 * handler, so the buttons can only ever drive the same wrap path. */
function findNavigationEnabled() {
  return !!searchEl && findActive && findMatches.length > 0 &&
    currentSearchEpoch === -1 && searchEl.value.trim() === searchQuery;
}

/** Sync Previous/Next visibility and enabled state with the find session. The
 * buttons are collapsed from the initial/pending/zero/error/cleared/profile-
 * reset and edited-query states, then shown and enabled only once valid
 * matches settle (including a single match, where a click wraps in place). */
function syncFindNavButtons() {
  const enabled = findNavigationEnabled();
  if (searchPrevBtn) {
    searchPrevBtn.hidden = !enabled;
    searchPrevBtn.disabled = !enabled;
  }
  if (searchNextBtn) {
    searchNextBtn.hidden = !enabled;
    searchNextBtn.disabled = !enabled;
  }
}

/** Drop every piece of find-in-chain state except the rendered highlight. */
function resetFindState() {
  findActive = false;
  findMatches = [];
  findTotal = 0;
  findMore = false;
  findIndex = 0;
  pendingFindTarget = null;
}

/** End the find session: state, highlight, and counter are cleared WITHOUT
 * reloading history or moving the scroll position (the chain was never
 * replaced, so there is nothing to restore). */
function clearFind() {
  resetFindState();
  searchQuery = '';
  currentSearchEpoch = -1;
  clearFindHighlight();
  updateFindCounter('hidden');
}

/** Clear the find-induced row highlight (inline selection + current marker). */
function clearFindHighlight() {
  clearSelection();
  const w = wrapEl();
  if (w) {
    const cur = w.querySelector('.row-find-current');
    if (cur) cur.classList.remove('row-find-current');
  }
}

/** Mark `absIdx` as the current find match: the row-selected inline selection
 * plus a left-edge current-marker accent. Both derive from real cached rows —
 * nothing is fabricated. */
function setFindHighlight(absIdx) {
  const row = cache.get(absIdx);
  if (!row) return;
  const w = wrapEl();
  if (w) {
    const prev = w.querySelector('.row-find-current');
    if (prev && Number(prev.getAttribute('data-row')) !== absIdx) {
      prev.classList.remove('row-find-current');
    }
    const cur = w.querySelector('.row[data-row="' + absIdx + '"]');
    if (cur) cur.classList.add('row-find-current');
  }
  selectRow(row, absIdx);
}

/** Map an ABSOLUTE slot index back to its VISIBLE index under the current
 * expansion state (inverse of absIndexForVisible). A hidden collapsed sub-op
 * slot maps to null — it is drawn as nothing. */
function visibleIndexForAbs(abs) {
  if (!blockStarts.length) return abs; // no expansion known yet — identity
  let lo = 0;
  let hi = blockStarts.length;
  while (lo < hi) {
    const mid = (lo + hi) >> 1;
    if (blockStarts[mid] <= abs) lo = mid + 1;
    else hi = mid;
  }
  const b = lo - 1;
  if (b < 0 || b >= blockStarts.length) return abs;
  const offInBlock = abs - blockStarts[b];
  if (offInBlock > 0 && !expandedBlocks.has(b)) return null; // hidden sub-op
  return blockStarts[b] - hiddenBeforeBlock(b) + offInBlock;
}

/** Move the viewport to find match `index`, selecting/revealing its row once
 * the window around the backend-resolved absolute row is cached. Matches are
 * ranked top-level rows of the ACTIVE view, so no expansion toggle is ever
 * needed. */
function jumpToFindMatch(index) {
  if (index < 0 || index >= findMatches.length) return;
  findIndex = index;
  const abs = findMatches[index].row;
  updateFindCounter('settled');
  pendingFindTarget = { abs, index };
  if (cache.has(abs)) {
    completeFindJump();
  } else {
    fetchWindowAround(abs);
  }
}

/** Complete a find jump once the target row's window is cached: scroll the
 * target into view under the CURRENT expansion state, render the cached
 * window, then select/reveal the real row. */
function completeFindJump() {
  const t = pendingFindTarget;
  if (!t) return;
  // Defensive: a match coordinate outside the active view (a malformed backend
  // row, or a view/total that changed after the response) can never be cached;
  // refresh the window instead of stalling the jump with a phantom pending
  // target. The check runs BEFORE the cache probe so a non-finite coordinate
  // (which cache.has() can never satisfy) still recovers.
  if (!Number.isFinite(t.abs) || t.abs < 0 || t.abs >= total) {
    pendingFindTarget = null;
    fetchWindow();
    return;
  }
  if (!cache.has(t.abs)) return;
  pendingFindTarget = null;
  const vis = visibleIndexForAbs(t.abs);
  if (vis === null) return; // hidden slot — backend only targets top-level rows
  const targetTop = Math.max(0,
    vis * ROW_H - Math.floor(rowsEl.clientHeight / ROW_H / 2) * ROW_H);
  rowsEl.scrollTop = targetTop;
  // Render the cached window around the target immediately (the scroll event
  // would do it asynchronously; the harness needs the jump complete before
  // idle settles).
  syncWindow();
  setFindHighlight(t.abs);
  const el = wrapEl() ? wrapEl().querySelector('.row[data-row="' + t.abs + '"]') : null;
  if (el) revealRow(el);
  announce('Match ' + (findIndex + 1) + ' of ' + findTotal + (findMore ? '+' : ''));
}

/** Move the find cursor by `delta` (+1 next / -1 previous), wrapping at both
 * ends. Only invoked for a SETTLED session matching the exact submitted
 * query. */
function navigateFind(delta) {
  if (!findActive || findMatches.length === 0) return;
  findIndex = (findIndex + delta + findMatches.length) % findMatches.length;
  jumpToFindMatch(findIndex);
}

/** Issue a FindInHistory request for the trimmed query text. The response is
 * correlated by search epoch so only the LATEST query's matches can land.
 * The request mirrors the active view exactly: the same ChainFilterDto the
 * view was fetched with (filterPayload carries the profile's hide_trace), the
 * same hide_submodules flag, and the search filters (no controls yet, so the
 * defaults). */
function submitFind(q) {
  searchQuery = q;
  resetFindState();
  clearFindHighlight();
  currentSearchEpoch = ++searchEpoch;
  updateFindCounter('pending');
  announce('Searching for "' + q + '"');
  sendSearch({
    FindInHistory: {
      query: q,
      top_k: FIND_TOP_K,
      filters: searchFiltersPayload(),
      filter: filterPayload(),
      hide_submodules: hideSubmodules(),
    },
  }, currentSearchEpoch);
}

/** Apply a settled FindInHistory response in place: the history DOM, profile,
 * expansion state, virtualization cache, and layout are untouched — the find
 * only selects/highlights, scrolls, and updates the counter. */
function applyFindResponse(response) {
  findMatches = (Array.isArray(response.matches) ? response.matches : [])
    .map(normalizeFindMatch)
    .filter((m) => m !== null);
  // The counter reflects the NAVIGABLE set — rows the view can actually jump
  // to. A malformed backend row (or one beyond a changed view's total) is
  // filtered above, so the session never claims a count it cannot cycle.
  findTotal = findMatches.length;
  findMore = !!response.more;
  findActive = true;
  findIndex = 0;
  currentSearchEpoch = -1;
  pendingFindTarget = null;
  clearFindHighlight();
  if (findMatches.length === 0) {
    updateFindCounter('zero');
    announce('No matches for "' + searchQuery + '"');
    vscode.postMessage({ type: 'log', text: 'find: 0 match(es) for "' + searchQuery + '"' });
    return;
  }
  vscode.postMessage({ type: 'log', text: `find: ${findTotal} match(es) for "${searchQuery}"` });
  // On a settled response, immediately scroll to and highlight match 1.
  jumpToFindMatch(0);
}

/** Compact, non-disruptive find error: the chain stays fully visible, the
 * counter shows "error" with the reason, and the session ends (the next Enter
 * re-searches). */
function showFindError(errText) {
  resetFindState();
  currentSearchEpoch = -1;
  clearFindHighlight();
  updateFindCounter('error', errText);
  announce('Find failed: ' + errText);
  vscode.postMessage({ type: 'log', text: 'find error: ' + errText });
}

/** Announce a status change to assistive tech (and the status bar) without
 * stealing focus. Best-effort: the live region may be absent in embedded
 * contexts, which is fine. */
function announce(text) {
  if (statusLiveEl) {
    statusLiveEl.textContent = text;
  }
  if (typeof vscode.postMessage === 'function') {
    vscode.postMessage({ type: 'statusText', text });
  }
}

/** Update the segmented control UI to reflect the active profile. */
function syncProfileButtons() {
  const active = profile === 'activity';
  if (profileActivityBtn && profileRawBtn) {
    profileActivityBtn.classList.toggle('active', active);
    profileRawBtn.classList.toggle('active', !active);
    profileActivityBtn.setAttribute('aria-pressed', String(active));
    profileRawBtn.setAttribute('aria-pressed', String(!active));
  }
}

/** Switch the Activity/Raw profile.
 *
 * With `reset` (user action), the view resets coherently: search mode exits,
 * the view generation bumps (in-flight windows from the old profile are
 * rejected), the expansion snapshot and cache are dropped, and history
 * refetches from offset 0 under the new profile. `reset:false` only updates
 * the in-memory profile + control (used on open/reveal before the first
 * fetch so the initial window already carries the persisted profile).
 */
function setProfile(next, opts) {
  opts = opts || {};
  if (next !== 'activity' && next !== 'raw') return;
  if (profile === next && !opts.force) return;
  profile = next;
  syncProfileButtons();
  if (opts.reset) {
    announce('Showing ' + profileLabel() + ' history');
    // Persist the profile immediately; the viewport index is saved once the
    // first window of the new profile arrives.
    vscode.setState({ profile, topRow: 0 });
    resetHistory();
  } else if (opts.persist !== false) {
    // Restore paths pass `persist:false`: at that point the scaffold isn't
    // built yet, so saveState() would write the pre-restore scrollTop (0)
    // and clobber the persisted topRow a real context recreation is about to
    // restore. The caller persists once the restored position is applied.
    saveState();
  }
}

/** Shorten a raw 64-bit identifier for display (never show the full string).
 *
 * Protocol identifiers (op ids, repository ids, oids) are exact strings that
 * can exceed 64 bits; showing them raw makes rows unreadable. Keeps the tail
 * so the short form still disambiguates within a session.
 */
function shortId(id) {
  if (!id) return '';
  const s = String(id);
  return s.length <= 12 ? s : s.slice(-12);
}

/** Human-readable label for a block-separator group key.
 *
 * `repo:*` groups are git repositories; `session:*` groups are Claude Code
 * sessions; anything else falls back to "EditChain ops". Identifiers are
 * shortened — never the full raw 64-bit string.
 */
function groupLabelText(group) {
  return group.startsWith('repo:') ? 'Git · repo ' + shortId(group.slice(5))
    : group.startsWith('session:') ? 'Session ' + shortId(group.slice(8))
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

// Narrow-width media-query breakpoints (must match media/main.css). Below each
// threshold a fixed column is DROPPED from the grid in priority order (commit,
// author, date) so Content keeps its readable width before it is ever
// squeezed. These drive the JS width math (graph budget, inline vars, resize
// handles) so it agrees with the CSS grid.
const HIDE_COMMIT_MAX = 617;
const HIDE_AUTHOR_MAX = 480;
const HIDE_DATE_MAX = 400;

/** Fixed columns hidden at the current viewport width. */
function hiddenColumns() {
  const w = window.innerWidth || rowsEl.clientWidth || 0;
  // Pulse is narrative-first: author and exact identity stay available through
  // explicit raw JSON activation instead of competing with Content. Date is
  // retained until the narrowest breakpoint so the read-through stays temporal.
  const hidden = new Set(['author', 'commit']);
  if (w <= HIDE_COMMIT_MAX) hidden.add('commit');
  if (w <= HIDE_AUTHOR_MAX) hidden.add('author');
  if (w <= HIDE_DATE_MAX) hidden.add('date');
  return hidden;
}

function isColumnHidden(col) {
  return hiddenColumns().has(col);
}

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
  for (const col of ['date', 'author', 'commit']) {
    if (isColumnHidden(col)) continue;
    parts.push('--' + col + '-w:' + (colWidths[col] !== null ? colWidths[col] : DEFAULT_COL_W[col]) + 'px');
  }
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
  // view (nothing to fetch); a history/search reset marks the
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
  sendWindow({
    GetWindow: {
      offset: start,
      limit,
      hide_submodules: hideSubmodules(),
      filter: filterPayload(),
      include_layout: layoutReady,
    },
  });
}

/**
 * Fetch the sparse window around an arbitrary ABSOLUTE row (a find-in-chain
 * target) so far matches are revealed WITHOUT scanning or loading the chain
 * before them. Like fetchWindow, the offset-0 window is forced until the
 * expansion snapshot establishes visible/absolute mapping.
 */
function fetchWindowAround(absRow) {
  if (pendingWindowReqId !== -1 || total <= 0) return;
  const forceSnapshot = !snapshotEstablished;
  const top = Math.max(0, absRow - BUFFER);
  const bottom = Math.min(total - 1, absRow + BUFFER);
  const rangeTop = forceSnapshot ? 0 : top;
  const rangeBottom = forceSnapshot ? Math.min(PAGE - 1, bottom) : bottom;
  if (rangeTop > rangeBottom) return;
  let start = -1;
  for (let i = rangeTop; i <= rangeBottom; i++) {
    if (!cache.has(i)) { start = i; break; }
  }
  if (start === -1) return; // the target window is already cached
  const limit = Math.min(PAGE, rangeBottom - start + 1);
  sendWindow({
    GetWindow: {
      offset: start,
      limit,
      hide_submodules: hideSubmodules(),
      filter: filterPayload(),
      include_layout: layoutReady,
    },
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
 * genuinely changes (initial load, far jump, column resize, or search reset).
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
// Compact graph-rail cap for narrow panels (must match the <=480px CSS media
// query that drops the Author column). Below this width the rail switches to a
// fixed compact width instead of taking half the viewport, so Content keeps a
// readable budget — every service lane is still drawn (laneX distributes lane
// centres across the full column, see graphLaneWidth/laneX). The rail is
// SHRUNK, never hidden: it stays >= MIN_COL_W.graph at all widths.
const GRAPH_MAX_W_NARROW = 120;
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
  // Only count fixed columns still visible at this width: at narrow viewports
  // the CSS grid drops commit/author/date (priority order), freeing their
  // budget for the graph rail and Content instead of reserving phantom tracks.
  const hidden = hiddenColumns();
  let fixedW = 0;
  for (const col of ['date', 'author', 'commit']) {
    if (hidden.has(col)) continue;
    fixedW += colWidths[col] !== null ? colWidths[col] : DEFAULT_COL_W[col];
  }
  const rowsW = Math.max(1, rowsEl.clientWidth);
  const graphCap = Math.max(MIN_COL_W.graph, Math.floor(rowsW * GRAPH_MAX_FRACTION));
  const avail = Math.max(MIN_COL_W.graph, rowsW - fixedW - MIN_CONTENT_W);
  const budget = Math.min(graphCap, avail);
  // Compact fixed-width rail at <=480px: the CSS media query drops the Author
  // column there, and the graph stops competing with Content for the freed
  // space. All lanes still compress inside this cap (graphLaneWidth/laneX).
  if ((window.innerWidth || rowsEl.clientWidth || 0) <= 480) {
    return Math.min(budget, GRAPH_MAX_W_NARROW);
  }
  return budget;
}

/** Node-dot radius, compressed when the graph rail is dense.
 *
 * High-lane chains shrink lane spacing inside the fixed graph budget; the dot
 * shrinks with it (down to a readable floor) so 20+ lanes read as a compact
 * rail of distinct marks instead of an overlapping smear. Topology is
 * untouched — lane centres still distribute monotonically across the column.
 */
function dotRadius() {
  const spacing = graphLaneWidth();
  return Math.max(1.5, Math.min(DOT_R, spacing / 2));
}

/** Terminal radius for the typed Activity-bundle glyph.
 *
 * Scales with the dot radius (which already compresses with dense lanes) so
 * the bundle glyph shrinks proportionally on crowded rails, down to the same
 * readable floor as ordinary dots. Terminals stay slightly smaller than the
 * node dot so the capsule reads as a distinct mark, not a double dot.
 */
function bundleTerminalRadius() {
  return Math.max(1.5, dotRadius() * BUNDLE_TERMINAL_RATIO);
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
  // Pulse compresses topology into a quiet navigation rail. A manually resized
  // Graph column remains authoritative and therefore uses normal lane spacing.
  const pulseScale = colWidths.graph !== null ? 1 : 0.82;
  return Math.max(
    MIN_LANE_W,
    Math.min(LANE_W * pulseScale, graphWidthBudget() / (numLanes + 1))
  );
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
 * any smooth cross-lane transition paths at this row.
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
 *
 * A cross-lane transition replaces the old hard three-line jog (source-lane
 * vertical half + horizontal connector + destination-lane vertical half) with
 * a tangent-continuous Bézier curve split into two exact colour halves. When
 * this row owns either endpoint node, the complete transition is one convex
 * quadratic: it leaves or enters the node smoothly and bows outward toward the
 * other lane without an inward hook. A boundary-to-boundary transition uses
 * two convex quadratic halves sharing one horizontal tangent, so a long edge
 * remains smooth while preserving vertical continuity with adjacent rows.
 * Each side is anchored to whatever actually connects it:
 *
 *   - a transition whose child node lives on THIS row (`row.lane === fromLane`)
 *     begins exactly at the node dot (xFrom, midY) — never at y=0, which would
 *     leave an open top stub; any legitimate `above` line on that lane is a
 *     separate edge and is still drawn into the dot;
 *   - otherwise it begins at y=0 on the from-lane ONLY when `above` lists that
 *     lane (the child's lane ran down from the row above, and this path owns
 *     that top half);
 *   - a transition whose parent node lives on THIS row (`row.lane === toLane`,
 *     and `below` does not list the lane) ends exactly at the node dot;
 *   - otherwise it ends at y=height on the to-lane only when `below` lists
 *     that lane (the parent's lane continues into the row below, and this path
 *     owns that bottom half).
 *
 * A transition whose endpoint would be neither a dot nor a connected boundary
 * is a dangling stub and is not drawn. Generic vertical halves are skipped
 * exactly when a rendered path owns them, so dot-anchored transitions never
 * suppress the neighbouring legitimate segment (the path would not cover it).
 *
 * Recognized typed Activity bundle rows (`execute-run` and `plan-repeat`)
 * render a compact vertical capsule instead of the node dot:
 * the entry terminal sits above the row midpoint and the exit terminal below
 * it, both on the node lane, and the capsule spans between them. Incoming
 * geometry on the node lane terminates at the entry terminal and outgoing
 * geometry starts at the exit terminal; other pass-through lanes keep the
 * ordinary midpoint geometry. Transitions touching the node lane re-anchor
 * their dot-anchored side to the matching terminal (the shared seam moves
 * with it); transitions that do not touch the node lane are unchanged.
 */
function buildGraphCell(row) {
  const width = currentGraphWidth();
  const height = ROW_H;
  const midY = ROW_H / 2;
  const nodeLane = row.lane || 0;
  const isBundle = isActivityBundle(row);
  const bundle = isBundle
    ? {
        termR: bundleTerminalRadius(),
        entryY: midY - BUNDLE_HALF_SPAN,
        exitY: midY + BUNDLE_HALF_SPAN,
      }
    : null;
  // Decorative graph marks — never exposed to the accessibility tree.
  let s = `<svg class="graphCell" width="${width}" height="${height}" xmlns="http://www.w3.org/2000/svg" aria-hidden="true">`;
  // `transitions` entries are (from_lane, to_lane) = production's
  // (child_lane, parent_lane) order.
  const transitions = row.transitions || [];
  const below = row.below || [];
  const above = row.above || [];
  // Resolve each transition's real anchors before drawing anything. A side is
  // DOT-anchored when the transition starts/ends on this row's own node; a
  // BOUNDARY-anchored side must be backed by the adjacent row's geometry
  // (`above` for a top start, `below` for a bottom end). Any other combination
  // would leave an open stub, so the transition is dropped.
  const rendered = [];
  for (const [fromLane, toLane] of transitions) {
    const startAtDot = row.lane === fromLane;
    // Prefer the destination node when this row owns it, even if another edge
    // continues down the same lane. The transition then enters the node on a
    // smooth bottom-right/bottom-left curve while the ordinary `below` segment
    // independently leaves the dot toward its own parent. Choosing the bottom
    // boundary first made forks render from the opposite corner.
    const endAtDot = row.lane === toLane;
    const endAtBoundary = !endAtDot && below.indexOf(toLane) !== -1;
    const startConnected = startAtDot || above.indexOf(fromLane) !== -1;
    if (!startConnected || (!endAtBoundary && !endAtDot)) continue;
    rendered.push({ fromLane, toLane, startAtDot, endAtDot });
  }
  // The halves a rendered transition path actually covers: the from-lane's top
  // half (only when the path begins at the boundary) and the to-lane's bottom
  // half (only when the path ends at the boundary). Dot-anchored sides leave
  // the neighbouring generic half in place — e.g. a legitimate `above` line
  // continuing into the dot.
  const ownsTop = new Set();
  const ownsBottom = new Set();
  for (const t of rendered) {
    if (!t.startAtDot) ownsTop.add(t.fromLane);
    if (!t.endAtDot) ownsBottom.add(t.toLane);
  }
  // Top-half vertical segments: lanes entering from above (y=0 → midY). On a
  // bundle row the node lane's incoming line terminates at the ENTRY terminal
  // instead of running on to the row midpoint.
  for (const lane of above) {
    if (ownsTop.has(lane)) continue;
    const x = laneX(lane);
    const colour = COLORS[lane % COLORS.length];
    const endY = bundle && lane === nodeLane ? bundle.entryY : midY;
    s += `<line class="graphLine" x1="${x}" y1="0" x2="${x}" y2="${endY}" style="stroke:${colour}"/>`;
  }
  // Bottom-half vertical segments: lanes leaving downward (midY → height). On
  // a bundle row the node lane's outgoing line starts at the EXIT terminal
  // instead of the row midpoint.
  for (const lane of below) {
    if (ownsBottom.has(lane)) continue;
    const x = laneX(lane);
    const colour = COLORS[lane % COLORS.length];
    const startY = bundle && lane === nodeLane ? bundle.exitY : midY;
    s += `<line class="graphLine" x1="${x}" y1="${startY}" x2="${x}" y2="${height}" style="stroke:${colour}"/>`;
  }
  // Smooth cross-lane transition paths at this row (drawn after the verticals
  // so the curves sit on top; the node dot is still painted last).
  for (const t of rendered) {
    s += buildTransitionPaths(t.fromLane, t.toLane, height, t.startAtDot, t.endAtDot, bundle);
  }
  // A sub-op row draws NO node — it is not a graph node. Its `above`/`below`
  // are the pass-through lanes spanning this region, drawn as full-height
  // straight lines (both halves meet at midY). Only top-level rows get a node
  // mark: the ordinary dot, or — on a recognized typed bundle row — the
  // capsule with its entry/exit terminals.
  if (!row.is_subop) {
    const colour = COLORS[nodeLane % COLORS.length];
    if (bundle) {
      // The bundle glyph: one capsule spanning the entry/exit terminals, which
      // share the node lane x and sit symmetrically around the row midpoint.
      const x = laneX(nodeLane);
      const { termR, entryY, exitY } = bundle;
      const capW = termR * 2 + BUNDLE_CAPSULE_MARGIN * 2;
      const capH = (exitY - entryY) + termR * 2;
      s += `<rect class="graphBundleCapsule" x="${fmt(x - capW / 2)}" y="${fmt(entryY - termR)}"` +
        ` width="${fmt(capW)}" height="${fmt(capH)}" rx="${fmt(capW / 2)}" fill="${colour}"/>` +
        `<circle class="graphBundleTerminal graphBundleEntry" cx="${fmt(x)}" cy="${fmt(entryY)}" r="${fmt(termR)}" fill="${colour}"/>` +
        `<circle class="graphBundleTerminal graphBundleExit" cx="${fmt(x)}" cy="${fmt(exitY)}" r="${fmt(termR)}" fill="${colour}"/>`;
    } else {
      // The node's own dot at its lane.
      s += `<circle class="graphDot" cx="${laneX(nodeLane)}" cy="${midY}" r="${dotRadius()}" fill="${colour}"/>`;
    }
  }
  s += '</svg>';
  return s;
}

/** Format a coordinate for SVG path output (keeps `d` compact and stable). */
function fmt(v) {
  return Math.round(v * 100) / 100;
}

/**
 * Build the two exact path halves for one cross-lane transition.
 *
 * The transition runs from `fromLane` (production's child lane) to `toLane`
 * (production's parent lane) inside one row cell. Each side is anchored by
 * `buildGraphCell`: either at the row's own node dot or at the row boundary
 * (y=0 / y=height). A node-to-boundary transition is one convex quadratic
 * Bézier, split at t=0.5 with de Casteljau subdivision. The two emitted path
 * halves therefore reproduce exactly the same curve and share the same tangent
 * at their colour seam. A boundary-to-boundary transition cannot be globally
 * convex while retaining vertical tangents at both ends, so it uses two convex
 * quadratics that meet at the lane midpoint with one shared horizontal tangent.
 * There are no straight elbows, corner-radius fallbacks, or concave hooks.
 *
 * Both halves reuse the exact same formatted seam coordinates (butt caps, no
 * gradients/defs), so the categorical colour handoff is sharp and gap-free.
 *
 * On a recognized typed bundle row (`bundle` is non-null) the node spans
 * from the entry terminal (above the row midpoint) to the exit terminal
 * (below it), so a dot-anchored side is re-anchored to the matching terminal
 * and the shared seam moves with it: an outgoing (child) side starts at the
 * exit terminal, an incoming (parent) side ends at the entry terminal. Sides
 * that do not touch the bundle node keep the ordinary row-midpoint seam.
 */
function buildTransitionPaths(fromLane, toLane, height, startAtDot, endAtDot, bundle) {
  const x1 = laneX(fromLane);
  const x2 = laneX(toLane);
  const midY = height / 2;
  const start = [x1, startAtDot ? (bundle ? bundle.exitY : midY) : 0];
  const end = [x2, endAtDot ? (bundle ? bundle.entryY : midY) : height];
  const srcColour = COLORS[fromLane % COLORS.length];
  const dstColour = COLORS[toLane % COLORS.length];
  let srcControl;
  let dstControl;
  let seam;

  if (startAtDot !== endAtDot) {
    // One endpoint is the row's node: construct a single convex quadratic and
    // split it at t=0.5. The control point gives the boundary endpoint a
    // vertical tangent and the node endpoint an outward horizontal tangent.
    const control = startAtDot ? [x2, start[1]] : [x1, end[1]];
    srcControl = midpoint(start, control);
    dstControl = midpoint(control, end);
    seam = midpoint(srcControl, dstControl);
  } else if (!startAtDot) {
    // Both endpoints are row boundaries. Two convex halves meet with an exact
    // horizontal tangent at the geometric centre; adjacent row lines remain
    // vertical at both external anchors.
    seam = [(x1 + x2) / 2, (start[1] + end[1]) / 2];
    srcControl = [x1, seam[1]];
    dstControl = [x2, seam[1]];
  } else {
    // Defensive fallback for the impossible ordinary-row case where both
    // different lanes claim the same node: retain a smooth straight quadratic.
    const control = midpoint(start, end);
    srcControl = midpoint(start, control);
    dstControl = midpoint(control, end);
    seam = midpoint(srcControl, dstControl);
  }

  const srcD = quadraticPath(start, srcControl, seam);
  const dstD = quadraticPath(seam, dstControl, end);
  return '<path class="graphTransition graphTransitionSrc" d="' + srcD +
    '" style="stroke:' + srcColour + '"/>' +
    '<path class="graphTransition graphTransitionDst" d="' + dstD +
    '" style="stroke:' + dstColour + '"/>';
}

/** Midpoint of two SVG coordinates. */
function midpoint(a, b) {
  return [(a[0] + b[0]) / 2, (a[1] + b[1]) / 2];
}

/** One compact quadratic SVG path with stable two-decimal coordinates. */
function quadraticPath(start, control, end) {
  return 'M ' + fmt(start[0]) + ' ' + fmt(start[1]) +
    ' Q ' + fmt(control[0]) + ' ' + fmt(control[1]) +
    ' ' + fmt(end[0]) + ' ' + fmt(end[1]);
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

/** Whitelisted concise labels for the `activity_kind` field.
 *
 * Keys are EXACTLY the Rust wire enum (crates/editchain-project/taxonomy.rs):
 * conversation/plan/explore/execute/change/verify/diagnose/coordinate/
 * source_control/external/system/unknown. `conversation` is the default
 * activity and gets NO badge (keeps the content cell clean); `unknown` and
 * any unrecognized wire value are ignored. Every rendered attribute (CSS
 * class, text, title) comes from these constant labels — the wire value is
 * only a lookup key — so an unknown or hostile activity_kind can never inject
 * a class name or markup.
 */
const ACTIVITY_LABELS = {
  plan: { cls: 'act-plan', text: 'plan' },
  explore: { cls: 'act-explore', text: 'explore' },
  execute: { cls: 'act-execute', text: 'run' },
  change: { cls: 'act-change', text: 'change' },
  verify: { cls: 'act-verify', text: 'verify' },
  diagnose: { cls: 'act-diagnose', text: 'diagnose' },
  coordinate: { cls: 'act-coordinate', text: 'coordinate' },
  source_control: { cls: 'act-source-control', text: 'git' },
  external: { cls: 'act-external', text: 'external' },
  system: { cls: 'act-system', text: 'system' },
};

/** Central visibility switches for deliberately optional, high-frequency row
 * chrome. Keep the label/style implementations available, but default common
 * success and source-control signals off so they do not repeat on every clean
 * Git/tool row. Exceptional outcomes and all other meaningful activities stay
 * visible. */
const ROW_BADGE_OPTIONS = Object.freeze({
  showSuccessOutcome: false,
  showSourceControlActivity: false,
});

/** Whitelisted concise labels for the `outcome` field.
 *
 * Keys are EXACTLY the Rust wire enum: success/warning/failure/cancelled/
 * unknown. `unknown` and any unrecognized wire value are ignored (never
 * inferred from absence of evidence).
 */
const OUTCOME_LABELS = {
  success: { cls: 'outcome-success', text: 'ok' },
  warning: { cls: 'outcome-warning', text: 'warn' },
  failure: { cls: 'outcome-failure', text: '✕', aria: 'failed' },
  cancelled: { cls: 'outcome-neutral', text: 'cancelled' },
};

/** Compact semantic activity badge ('' when the row is a plain message). */
function activityBadge(row) {
  const kind = row.activity_kind;
  if (kind === 'source_control' && !ROW_BADGE_OPTIONS.showSourceControlActivity) return '';
  const label = Object.prototype.hasOwnProperty.call(ACTIVITY_LABELS, kind)
    ? ACTIVITY_LABELS[kind]
    : null;
  if (!label) return '';
  return '<span class="act-badge ' + label.cls + '" title="activity: ' + esc(kind) +
    '" aria-label="activity: ' + esc(label.text) + '">' + esc(label.text) + '</span>';
}

/** Compact semantic outcome badge ('' when the row has no reported outcome). */
function outcomeBadge(row) {
  const outcome = row.outcome;
  if (outcome === 'success' && !ROW_BADGE_OPTIONS.showSuccessOutcome) return '';
  const label = Object.prototype.hasOwnProperty.call(OUTCOME_LABELS, outcome)
    ? OUTCOME_LABELS[outcome]
    : null;
  if (!label) return '';
  return '<span class="out-badge ' + label.cls + '" title="outcome: ' + esc(outcome) +
    '" aria-label="outcome: ' + esc(label.aria || label.text) + '">' + esc(label.text) + '</span>';
}

// --- Activity work-unit / bundle / promotion layer --------------------------
//
// The Activity profile renders a stable, additive semantic layer over the flat
// row list: work-unit section headers, typed Activity-bundle chrome, and
// conservative promotion rails. Raw stays the exact flat UI — every helper
// below no-ops outside `profile === 'activity'`, so Raw rows never carry the
// classes/descendants/data attributes the semantic layer introduces.

/** Short human fallback labels for work-unit headers with no narrative title.
 * Keys are whitelisted activity kinds; unknown kinds fall back to type/group
 * labels. The opaque unit id is NEVER used as visible primary text. */
const WORK_UNIT_FALLBACK_LABELS = {
  execute: 'Run',
  change: 'Change',
  verify: 'Verify',
  plan: 'Plan',
  explore: 'Explore',
  diagnose: 'Diagnose',
  coordinate: 'Coordinate',
  source_control: 'Git',
  external: 'External',
  system: 'System',
};

/** Whether the Activity profile is active (Raw renders none of the semantic
 * work-unit/bundle/promotion layer). */
function activityView() {
  return profile === 'activity';
}

/** The row's Activity work-unit payload, or null outside the Activity profile
 * or on sub-op rows (the service ships `None` there; sub-ops never carry a
 * unit header of their own). */
function workUnitOf(row) {
  if (!activityView() || !row || row.is_subop || !row.work_unit) return null;
  return row.work_unit;
}

/** Title for a work-unit header: the DTO title when present, else a short
 * human label derived from the row's activity kind, then type, then group —
 * never the full opaque unit id as primary text. */
function workUnitTitle(row) {
  const wu = workUnitOf(row);
  if (wu && typeof wu.title === 'string' && wu.title) {
    const title = wu.title.replace(/\r\n?/g, '\n').split('\n')
      .map(markdownPlainLine).find(Boolean);
    if (title) return title;
  }
  const fallback = WORK_UNIT_FALLBACK_LABELS[row.activity_kind];
  if (fallback) return fallback;
  if (row.kind === 'message' || row.kind === 'command') return 'Request';
  return groupLabelText(row.group);
}

/** Human count text for a work-unit header, from the exact DTO count. "Entry"
 * describes what is actually counted without exposing the wire-level record
 * vocabulary in the UI. */
function workUnitCountText(count) {
  return String(count) + (count === 1 ? ' entry' : ' entries');
}

/** Keep grouping counts sparse. A source-control section already reads as a
 * Git history and the native VS Code graph does not append a commit count to
 * its section title; single-entry units likewise need no annotation. */
function showWorkUnitCount(row, wu) {
  return !!wu && Number.isFinite(wu.count) && wu.count > 1 &&
    row.activity_kind !== 'source_control';
}

/** Tooltip explaining exactly what a work-unit count measures. */
function workUnitCountTitle(count) {
  return workUnitCountText(count) + ' grouped in this activity';
}

/** The small, display-safe session provenance supplied by the service. */
function sessionMetaValues(row) {
  const meta = row && row.session_meta;
  if (!meta || typeof meta !== 'object') return [];
  const values = [];
  if (typeof meta.model_provider === 'string' && meta.model_provider.trim()) {
    values.push({ cls: 'session-chip-model', label: meta.model_provider.trim(), title: 'Model provider' });
  }
  if (typeof meta.agent_nickname === 'string' && meta.agent_nickname.trim()) {
    values.push({ cls: 'session-chip-agent', label: meta.agent_nickname.trim(), title: 'Agent' });
  }
  return values;
}

/** Session chips are inserted only at a rendered session boundary, matching
 * native Git ref labels instead of repeating the same provenance on every row. */
function sessionMetaChips(row) {
  return sessionMetaValues(row).map((item) =>
    '<span class="session-chip ' + item.cls + '" title="' + esc(item.title + ': ' + item.label) +
    '" aria-label="' + esc(item.title + ': ' + item.label) + '">' + esc(item.label) + '</span>'
  ).join('');
}

/** Accessible prose corresponding to the visible session chips. */
function sessionMetaDescription(row) {
  return sessionMetaValues(row).map((item) => item.title + ' ' + item.label).join(', ');
}

/** CSS classes for a row's work-unit boundary role: start (unit header),
 * subtle end closure, or none. */
function workUnitClasses(row) {
  const wu = workUnitOf(row);
  if (!wu) return '';
  if (wu.is_start) return ' row-work-unit-start';
  if (wu.is_end) return ' row-work-unit-end';
  return '';
}

/** The recognized typed bundle kind for one top-level Activity row. Unknown
 * and future kinds deliberately return an empty string and stay flat. */
function activityBundleKind(row) {
  if (!activityView() || !row || row.is_subop || !row.activity_bundle) return '';
  const kind = row.activity_bundle.kind;
  return kind === 'execute-run' || kind === 'plan-repeat' ? kind : '';
}

/** Whether a row is any bundle kind this renderer understands. */
function isActivityBundle(row) {
  return activityBundleKind(row) !== '';
}

/** Whether a row is a top-level Activity execute-run bundle. */
function isExecuteRunBundle(row) {
  return activityBundleKind(row) === 'execute-run';
}

/** Whether a row is an adjacent repeated-Plan bundle. */
function isPlanRepeatBundle(row) {
  return activityBundleKind(row) === 'plan-repeat';
}

/** Whether a row is promoted in the Activity view (sub-ops are never). */
function isPromotedRow(row) {
  return !!(activityView() && row && !row.is_subop && row.promoted === true);
}

/** CSS classes for a promoted row: a strong accent for failure/warning/
 * cancelled outcomes and change/verify activity; a quiet rail for promoted
 * narrative. No badge is added — the rail carries the signal. */
function promotedClasses(row) {
  if (row.outcome === 'failure' || row.outcome === 'warning' || row.outcome === 'cancelled') {
    return ' row-promoted row-promoted-' + row.outcome;
  }
  if (row.activity_kind === 'change' || row.activity_kind === 'verify') {
    return ' row-promoted row-promoted-' + row.activity_kind;
  }
  return ' row-promoted row-promoted-rail';
}

/** One concise label for a typed bundle row. The count comes ONLY from
 * the DTO's `member_count` (never parsed from the summary or flattened sub-op
 * count). */
function bundleCountText(row) {
  const bundle = row.activity_bundle;
  const mc = bundle ? bundle.member_count : undefined;
  if (typeof mc !== 'number') return '';
  if (isPlanRepeatBundle(row)) {
    return mc + (mc === 1 ? ' update' : ' updates');
  }
  const command = row.kind === 'command';
  return mc + (command
    ? (mc === 1 ? ' command' : ' commands')
    : (mc === 1 ? ' tool step' : ' tool steps'));
}

function bundleChrome(row) {
  const countText = bundleCountText(row);
  if (!countText) return '';
  // A check is execute-specific structured outcome evidence. Plan-repeat
  // bundles are narrative updates and intentionally carry no status glyph.
  const success = isExecuteRunBundle(row) && row.outcome === 'success';
  const title = countText + (success ? ', completed' : '');
  return '<span class="bundle-count" title="' + esc(title) + '">' + esc(countText) + '</span>' +
    (success
      ? '<span class="bundle-status bundle-status-success" title="completed" aria-label="completed">✓</span>'
      : '');
}

/** Restrained leading metadata for one row.
 *
 * Execute bundles own their complete compact label; Plan bundles keep their
 * narrative heading beside the updates count. Structural relations
 * outrank generic activity; when one exists, only a negative/cancelled outcome
 * may accompany it. Common success and source-control chips are governed by
 * `ROW_BADGE_OPTIONS` and default off. */
function rowSemanticChrome(row, isBundle) {
  if (isBundle) return bundleChrome(row);
  const relations = relationBadges(row);
  if (relations) {
    const consequential = row.outcome === 'warning' || row.outcome === 'failure' ||
      row.outcome === 'cancelled';
    return relations + (consequential ? outcomeBadge(row) : '');
  }
  const activity = activityBadge(row);
  return activity + outcomeBadge(row);
}

/** Whitelisted record-role class used for typographic hierarchy. Older rows
 * without the provider-neutral field keep the existing kind-based fallback. */
const RECORD_ROLE_CLASSES = new Set([
  'narrative', 'action', 'result', 'artifact', 'lifecycle', 'echo', 'unknown',
]);

/** Short commit/ID display value for the Commit/ID column.
 *
 * Op IDs are removed from the default visual priority: an op row shows only a
 * short turn id (or the tail of its op id) instead of the full raw string, so
 * the Content column carries the visual weight. Git rows keep their
 * abbreviated OID (already short).
 */
function shortCommitId(row) {
  if (row.git_oid) return shortId(row.commit_id || row.git_oid);
  if (row.is_subop) return shortId(row.op_id);
  if (row.turn_id) return shortId(row.turn_id);
  return shortId(row.commit_id || row.op_id);
}

/** Build one row's HTML from its cached HistoryRow. `absIdx` is its absolute index.
 *
 * Two kinds of rows:
 *   - Top-level rows carrying bundled sub-ops get a native-style disclosure
 *     chevron. The whole row is the disclosure target (revealing one uniform
 *     ROW_H row per sub-op directly below), while double-click still opens its
 *     raw JSON editor.
 *   - Sub-op rows (`row.is_subop`) render indented with a small Codicon; clicking
 *     selects them inline.
 *
 * The Activity profile additionally layers stable semantic chrome over the flat
 * row (see the work-unit/bundle/promotion helpers above): work-unit starts get
 * a compact two-line unit header inside the SAME fixed ROW_H (never the opaque
 * unit id as primary text), typed bundles get one count label plus
 * disclosure semantics, and promoted rows get a quiet rail (restrained
 * narrative) or a strong accent (failure/warning/cancelled and change/verify).
 * Raw renders the exact flat row with none of it.
 *
 * Accessibility: rows are focusable grid rows with aria-selected/aria-expanded,
 * the chevron is a labelled button, truncated cells carry title tooltips, and
 * the group boundary label is a visible (non-hover) short-ID chip. Every `.row`
 * stays exactly ROW_H tall so virtual-scroll math is undisturbed.
 */
function buildRowHtml(row, absIdx, isGroupStart) {
  const wu = workUnitOf(row);
  const isWuStart = !!wu && !!wu.is_start;
  const isWuEnd = !!wu && !!wu.is_end;
  const isBundle = isActivityBundle(row);
  const promotedCls = isPromotedRow(row) ? promotedClasses(row) : '';
  const groupClass = isGroupStart && !isWuStart ? ' row-group-start' : '';
  // Work-unit starts render their own unit header; suppress the group chip on
  // those rows so labels never stack (the separator border stays).
  const groupLabel = isGroupStart && !isWuStart
    ? '<div class="group-label" aria-hidden="true">' + esc(groupLabelText(row.group)) + '</div>'
    : '';
  const kindClass = row.is_system ? 'row-tool'
    : (row.kind === 'message' || row.kind === 'command') ? ''
    : 'row-dim';
  const humanClass = row.author === 'human' ? ' row-human' : '';
  const subopClass = row.is_subop ? ' row-subop' : '';
  const roleClass = RECORD_ROLE_CLASSES.has(row.record_role)
    ? ' row-role-' + row.record_role
    : '';
  const semanticChrome = rowSemanticChrome(row, isBundle);
  const hasSessionMeta = !row.is_subop && sessionMetaValues(row).length > 0;
  const sessionChrome = hasSessionMeta && isGroupStart ? sessionMetaChips(row) : '';
  const sessionSlot = hasSessionMeta
    ? '<span class="session-meta-slot">' + sessionChrome + '</span>'
    : '';
  // Badge rows are graph-topology-critical; the CSS override lifts their text
  // cells out of the tool/dim opacity dimming so the badge stays readable at
  // full strength (row height is untouched — the class only affects opacity).
  const relClass = semanticChrome || sessionChrome ? ' row-has-badges' : '';
  const selectedClass = row.node_key === selectedRowKey ? ' row-selected' : '';
  // The current find-in-chain match is re-marked on every rebuild (selection
  // survives by node key; the accent class is keyed to the match's row).
  const findCurrentClass = findActive && findMatches.length > 0 &&
    findMatches[findIndex] && findMatches[findIndex].row === absIdx
    ? ' row-find-current' : '';
  const summaryText = row.summary || '(no summary)';
  const displaySummary = displaySummaryForRow(row, summaryText);
  const plainSummary = plainRowSummary(row, displaySummary) || '(no summary)';
  const detailSummary = markdownPlainSummary(summaryText) || '(no summary)';
  const unitTitle = isWuStart ? workUnitTitle(row) : '';
  const hasSubs = !row.is_subop && hasSubOps(row);
  const subOpCount = Array.isArray(row.sub_ops) ? row.sub_ops.length : 0;
  const expanded = hasSubs && expandedBlocks.has(blockIndexOfAbs(absIdx));
  // Every row with children is one full-width disclosure target. Typed bundles
  // normally carry children; `hasSubs` remains authoritative if a partial or
  // forward-compatible payload arrives without them.
  const expandable = hasSubs;
  const expandableAttr = expandable
    ? ' aria-expanded="' + (expanded ? 'true' : 'false') + '"'
    : '';
  const chevron = expanded ? '▾' : '▸';
  const childLabel = isBundle && bundleCountText(row)
    ? bundleCountText(row)
    : String(subOpCount) + (subOpCount === 1 ? ' detail' : ' details');
  const disclosureLabel = (expanded ? 'Collapse ' : 'Expand ') + childLabel;
  const chevronHtml = hasSubs
    ? '<button type="button" class="subop-chevron" title="' + esc(disclosureLabel) +
      '" aria-label="' + esc(disclosureLabel) +
      '"' + expandableAttr + '>' + chevron + '</button>'
    : '';
  const semanticHtml = semanticChrome
    ? '<span class="row-meta">' + semanticChrome + '</span>'
    : '';
  let content;
  let workUnitTitleOnly = false;
  if (row.is_subop) {
    // A bundled sub-op expanded inline: small Codicon + indented summary.
    const icon = subopIcon(row.subop_kind);
    content = '<span class="subop-icon codicon codicon-' + icon + '" aria-hidden="true"></span>' +
      '<span class="subop-summary">' + renderMarkdownSummary(displaySummary) + '</span>';
  } else {
    // Top-level row: the chevron makes disclosure obvious and the whole row
    // toggles it. Execute bundles use their one structured label as the whole
    // compact summary; Plan bundles retain their narrative heading. Session
    // chips appear once at the session boundary.
    content = chevronHtml + (isWuStart ? '' : sessionSlot) + semanticHtml +
      (isExecuteRunBundle(row) ? '' : renderRowSummary(row, displaySummary));
  }
  if (isWuStart) {
    // Compact two-line unit header inside the fixed ROW_H: the unit title +
    // count sit above the row's own summary. When the initiating request is
    // itself the start row, the title and summary are identical; render it once
    // and vertically center the header instead of duplicating the sentence.
    workUnitTitleOnly = unitTitle === plainSummary;
    const countHtml = showWorkUnitCount(row, wu)
      ? '<span class="work-unit-count" title="' + esc(workUnitCountTitle(wu.count)) + '">' +
        esc(workUnitCountText(wu.count)) + '</span>'
      : '';
    content = '<span class="work-unit-ribbon-line">' +
      '<span class="work-unit-ribbon" title="' + esc(unitTitle) + '">' + esc(unitTitle) + '</span>' +
      sessionSlot +
      countHtml + '</span>' + (workUnitTitleOnly
      ? ''
      : '<span class="work-unit-row-line">' + content + '</span>');
  }
  const dateText = formatDate(row.timestamp_ms);
  const authorText = row.author || '';
  // Descriptive accessible name: bundle rows read as disclosures ("5-step
  // execute run: summary"); work-unit starts lead with their unit header.
  let ariaLabel = plainSummary;
  if (isBundle) {
    const mc = row.activity_bundle.member_count;
    const memberNoun = isPlanRepeatBundle(row)
      ? (mc === 1 ? ' update' : ' updates')
      : (mc === 1 ? ' step' : ' steps');
    ariaLabel = (isPlanRepeatBundle(row) ? 'Plan group' : 'Execute run') +
      (typeof mc === 'number' ? ', ' + mc + memberNoun : '') +
      (isExecuteRunBundle(row) && row.outcome === 'success' ? ', completed' : '') +
      (isPlanRepeatBundle(row) ? ': ' + plainSummary : '');
  } else if (isWuStart) {
    ariaLabel = workUnitTitleOnly ? unitTitle : unitTitle + ': ' + plainSummary;
  }
  const baseAriaLabel = ariaLabel;
  if (isGroupStart && hasSessionMeta) {
    ariaLabel += ', ' + sessionMetaDescription(row);
  }
  // Roving tabindex: exactly one row per rendered window is tabbable (the rest
  // are focusable-but-not-tabbable so keyboard users step through the grid as
  // a unit; see applyRovingTabindex and the ArrowUp/Down handling).
  const rovingTab = absIdx === rovingAbs ? '0' : '-1';
  const wuAttrs = isWuStart
    ? ' data-work-unit-id="' + esc(wu.id) + '"'
    : '';
  const bundleAttrs = isBundle
    ? ' data-activity-bundle="' + activityBundleKind(row) + '"' +
      (typeof row.activity_bundle.member_count === 'number'
        ? ' data-bundle-count="' + row.activity_bundle.member_count + '"'
        : '')
    : '';
  return '<div class="row ' + kindClass + humanClass + subopClass + roleClass + relClass + selectedClass + findCurrentClass + groupClass +
    workUnitClasses(row) + (isBundle ? ' row-activity-bundle' : '') +
    (expandable ? ' row-expandable' : '') + promotedCls +
    '" role="row" tabindex="' + rovingTab + '" aria-selected="' + (row.node_key === selectedRowKey ? 'true' : 'false') + '"' +
    expandableAttr +
    ' aria-label="' + esc(ariaLabel) + '" title="' + esc(detailSummary) + '"' +
    ' data-base-aria-label="' + esc(baseAriaLabel) + '"' +
    ' data-key="' + esc(row.node_key) +
    '" data-row="' + absIdx + '"' + wuAttrs + bundleAttrs + ' style="' + colStyle() + '">' +
    groupLabel +
    '<div class="graph-cell" role="gridcell">' + buildGraphCell(row) + '</div>' +
    '<div class="text-cell" role="gridcell"><div class="summary' +
      (isWuStart ? ' work-unit-block' : '') +
      (workUnitTitleOnly ? ' work-unit-title-only' : '') +
      '" title="' + esc(detailSummary) + '">' + content + '</div></div>' +
    '<div class="date-cell" role="gridcell"' + (dateText ? ' title="' + esc(dateText) + '"' : '') + '>' + esc(dateText) + '</div>' +
    '<div class="author-cell" role="gridcell"' + (authorText ? ' title="' + esc(authorText) + '"' : '') + '>' + esc(authorText) + '</div>' +
    '<div class="commit-cell" role="gridcell" title="' + esc(row.commit_id || row.op_id || '') + '">' + esc(shortCommitId(row)) + '</div>' +
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

/** Sync an existing row element's group-start chip (class + label) with
 * `isGroupStart`, matching what buildRowHtml would produce. */
function setGroupStart(el, row, isGroupStart) {
  if (!el || !row) return;
  // Work-unit starts render their own unit header and separator — never stack
  // a group chip or retain the separate group-boundary class on top of it
  // (matches buildRowHtml's suppression).
  const wu = workUnitOf(row);
  const suppressChip = !!(wu && wu.is_start);
  const has = el.classList.contains('row-group-start');
  if (isGroupStart && !suppressChip && !has) {
    el.classList.add('row-group-start');
    const label = document.createElement('div');
    label.className = 'group-label';
    label.setAttribute('aria-hidden', 'true');
    label.textContent = groupLabelText(row.group);
    el.insertBefore(label, el.firstChild);
  } else if ((!isGroupStart || suppressChip) && has) {
    el.classList.remove('row-group-start');
    const label = el.querySelector('.group-label');
    if (label) label.remove();
  }
  const sessionSlot = el.querySelector('.session-meta-slot');
  if (sessionSlot) {
    sessionSlot.innerHTML = isGroupStart ? sessionMetaChips(row) : '';
    const hasOtherBadges = !!el.querySelector('.row-meta');
    el.classList.toggle('row-has-badges', isGroupStart || hasOtherBadges);
    const baseAria = el.getAttribute('data-base-aria-label') || '';
    const sessionDescription = isGroupStart ? sessionMetaDescription(row) : '';
    el.setAttribute('aria-label', baseAria + (sessionDescription ? ', ' + sessionDescription : ''));
  }
}

/** Build the sticky header row HTML. The graph column's width is derived from
 * the current `maxLane`, so this must be re-run whenever `maxLane` changes
 * (e.g. when the first GetWindow response arrives after `open`). */
function buildHeaderHtml() {
  return '<div class="tbl-header" role="row" style="' + colStyle() + '">' +
    '<div class="th graph" role="columnheader">' + graphColumnHeaderLabel() + '</div>' +
    '<div class="th content" role="columnheader">Content</div>' +
    '<div class="th date" role="columnheader">Date</div>' +
    '<div class="th author" role="columnheader">Author</div>' +
    '<div class="th commit" role="columnheader">Commit/ID</div>' +
    '</div>';
}

// Smallest graph-column width that renders the "Graph" columnheader label
// without ellipsizing. Measured once from the first rendered header (after
// fonts are loaded) so the threshold tracks the real webview font instead of a
// hardcoded pixel guess.
let graphLabelMinW = null;
function graphColumnHeaderLabel() {
  if (graphLabelMinW === null) {
    const probe = document.createElement('div');
    probe.className = 'th graph';
    probe.style.cssText = 'position:absolute;visibility:hidden;left:-9999px;top:0;width:auto;' +
      // Mirror .tbl-header .th exactly: a detached probe outside .tbl-header
      // misses the cell padding, under-measures the label, and leaves the
      // real narrow track clipping "Graph" to "G…" (scrollWidth includes the
      // cell's horizontal padding, so it equals the smallest column width
      // that shows the label unclipped).
      'padding:6px 8px;box-sizing:border-box;font-weight:700;white-space:nowrap;';
    probe.textContent = 'Graph';
    document.body.appendChild(probe);
    // scrollWidth includes the cell's horizontal padding, so it equals the
    // smallest column width that shows the label unclipped.
    graphLabelMinW = probe.scrollWidth;
    probe.remove();
  }
  return currentGraphWidth() >= graphLabelMinW
    ? 'Graph'
    // A lane-narrow graph rail cannot fit the label (a 2-lane column renders
    // ~36px while bold "Graph" needs ~65px); the text would clip to "G…".
    // Render it as visually-hidden text instead so the columnheader keeps its
    // accessible name with zero clipped visual text; once lanes widen the
    // header rebuilds (maxLane change / viewport resize) and the label returns.
    : '<span class="visually-hidden">Graph</span>';
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
 * far jumps, column resize, view changes, and reveal toggles — NOT for normal
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
  // Build the grid wrapper (sticky header + spacer + wrap) in one innerHTML
  // pass. There is a SINGLE header, inside the labelled .tbl-grid wrapper but
  // OUTSIDE .table-wrap, so `position: sticky; top: 0` still sticks to the
  // #rows viewport and stays at the top while scrolling. It must NOT live
  // inside .table-wrap (which is positioned at renderTop*ROW_H and moves with
  // scroll) — a header there would scroll with content and appear mid-table.
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
    ? '<div class="search-banner" role="status" aria-live="polite">' + esc(String(total)) + ' result' +
      (total === 1 ? '' : 's') + ' for "' + esc(searchQuery) + '"</div>'
    : '';
  // The table is ONE labelled grid containing BOTH the sticky header row (its
  // columnheaders must live inside the same role=grid as the data rows —
  // ARIA forbids orphaned row/columnheader roles) and the virtualized rows.
  // The header stays a sibling of the scroll spacer inside the grid wrapper so
  // position:sticky keeps working exactly as before, and .table-wrap is demoted
  // to role=presentation (pure positioning layer between the grid and its rows).
  const gridHtml =
    '<div class="tbl-grid" role="grid" aria-label="History rows" aria-rowcount="' +
      visibleTotal() + '">' +
      headerHtml +
      '<div class="scroll-spacer" role="presentation" style="height:' + spacerH + 'px">' +
        '<div class="table-wrap" role="presentation" style="top:' + (top * ROW_H) + 'px;' + colStyle() + '">' +
          html +
        '</div>' +
      '</div>' +
    '</div>';
  // Preserve the scroll position across the DOM rebuild (setting innerHTML
  // resets scrollTop to 0).
  const prevScrollTop = rowsEl.scrollTop;
  // Preserve focus too: rebuilds happen not only on jumps but on the debounced
  // width recompute (host layout changes can resize #rows), and silently
  // blurring the focused row mid-interaction is a keyboard-UX regression. If a
  // row (or a control inside it) had focus, restore focus to the rebuilt row
  // at the same absolute index; a scrolled-away or absent row is skipped
  // (applyRovingTabindex re-arms the anchor to the first rendered row).
  const activeEl = document.activeElement;
  const activeRowEl = activeEl && activeEl.closest ? activeEl.closest('.row') : null;
  const focusedAbs = activeRowEl && rowsEl.contains(activeRowEl)
    ? parseInt(activeRowEl.getAttribute('data-row'), 10)
    : null;
  rowsEl.innerHTML =
    warningHtml +
    bannerHtml +
    gridHtml;
  rowsEl.scrollTop = prevScrollTop;
  // Rows were just (re)built from scratch — re-apply the single-tabbable-row
  // invariant, restore focus to the previously-focused row when it is still
  // rendered, then wire the new DOM's row interactions.
  applyRovingTabindex();
  if (focusedAbs !== null) {
    const restored = rowsEl.querySelector('.row[data-row="' + focusedAbs + '"]');
    if (restored) {
      rovingAbs = focusedAbs;
      applyRovingTabindex();
      // preventScroll: this is a REBUILD, not navigation — restoring focus to
      // the previously-focused row must never scroll it into view (that would
      // yank the user's scroll position during a host resize).
      restored.focus({ preventScroll: true });
    }
  }
  // No graph refresh needed: each row's graph cell is built into its HTML, so
  // the rebuilt DOM already contains the correct per-row graph.
  attachRowClicks();
}

/** Enforce the roving-tabindex invariant over the currently rendered window:
 * exactly one row is tabbable, all others are focusable but skipped in tab
 * order (tabindex -1). Runs after every render mutation so the invariant
 * survives virtualization (reanchorTo rebuilds the whole window, the additive
 * append/prepend/trim helpers mutate its edges, and far jumps replace it).
 *
 * The anchor is tracked by ABSOLUTE row index (it must survive DOM rebuilds).
 * When the anchor isn't rendered — the very first render, or after virtual
 * scrolling trimmed/scrolled it away — it falls back to the first rendered row
 * so the grid always has exactly one tab stop to land on.
 */
function applyRovingTabindex() {
  const w = wrapEl();
  if (!w) return;
  const rows = Array.from(w.querySelectorAll('.row'));
  if (!rows.length) return;
  let current = rows.find((r) => parseInt(r.getAttribute('data-row'), 10) === rovingAbs);
  if (!current) {
    current = rows[0];
    rovingAbs = parseInt(current.getAttribute('data-row'), 10);
  }
  for (const r of rows) {
    const absIdx = parseInt(r.getAttribute('data-row'), 10);
    r.tabIndex = absIdx === rovingAbs ? 0 : -1;
  }
}

/** Toggle inline sub-op expansion for a top-level combined row, rebuilding the
 * bounded visible window so newly revealed sub-op slots fill the viewport. */
function toggleExpandFor(row, absIdx) {
  if (row.is_subop || !hasSubOps(row)) return;
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
}

/** Keyboard toggle of a row's reveal state (disclosure semantics).
 * After the reanchor rebuilds the window, re-focus the fresh parent row so
 * keyboard focus survives the DOM replacement. */
function toggleDisclosureKeyboard(row, absIdx) {
  toggleExpandFor(row, absIdx);
  const w = wrapEl();
  if (w) {
    const fresh = w.querySelector('.row[data-row="' + absIdx + '"]');
    if (fresh) fresh.focus();
  }
}

function attachRowClicks() {
  const w = wrapEl();
  if (!w) return;
  w.querySelectorAll('.row').forEach((el) => {
    // The chevron remains the explicit disclosure affordance, but the complete
    // parent row is the click target, matching VS Code's native Git graph.
    // Double-click remains the explicit pointer gesture for raw JSON.
    const chevron = el.querySelector('.subop-chevron');
    if (chevron) {
      chevron.addEventListener('click', (e) => {
        e.preventDefault();
        e.stopPropagation();
        const absIdx = parseInt(el.getAttribute('data-row'), 10);
        const row = cache.get(absIdx);
        if (row) {
          selectRow(row, absIdx);
          toggleExpandFor(row, absIdx);
        }
      });
    }
    el.addEventListener('click', (e) => {
      if (e.target.closest && e.target.closest('button')) return;
      // A real double-click emits two click events first. Let the first one
      // toggle once, but ignore the second so the disclosure does not snap
      // closed again immediately before raw JSON opens.
      if (e.detail > 1) return;
      const absIdx = parseInt(el.getAttribute('data-row'), 10);
      const row = cache.get(absIdx);
      if (!row) return;
      selectRow(row, absIdx);
      if (!row.is_subop && hasSubOps(row)) toggleExpandFor(row, absIdx);
    });
    el.addEventListener('dblclick', (e) => {
      if (e.target.closest && e.target.closest('button')) return;
      const absIdx = parseInt(el.getAttribute('data-row'), 10);
      const row = cache.get(absIdx);
      if (!row) return;
      selectRow(row, absIdx);
      openRawJson(row);
    });
  });
}

// Keyboard activation: ArrowUp/Down move focus between rendered rows (roving
// tabindex — only the current row is in the tab order, so Tab enters/exits the
// grid as a unit instead of tabbing through every virtualized row); Enter opens
// raw JSON for an ordinary row; Enter/Space toggle any row with children. The
// chevron button handles its
// own Enter/Space via native button activation (we skip events originating
// inside it to avoid a double toggle).
rowsEl.addEventListener('keydown', (e) => {
  if (e.key === 'ArrowDown' || e.key === 'ArrowUp') {
    // Move the roving focus to the adjacent rendered row. Stops at the rendered
    // window edge (no wrap/auto-scroll): scrolling beyond it re-arms the roving
    // anchor via applyRovingTabindex, so large-list navigation is unchanged.
    // Search results are a flat, fully rendered list, so arrows wrap at the
    // ends (last -> first / first -> last) and ALSO move the inline selection,
    // keeping roving tabindex/focus/aria-selected/selectedRowKey in sync.
    const w = wrapEl();
    if (!w) return;
    const rows = Array.from(w.querySelectorAll('.row'));
    if (!rows.length) return;
    const cur = e.target.closest('.row');
    const curIdx = cur ? rows.indexOf(cur) : -1;
    let nextIdx = e.key === 'ArrowDown' ? curIdx + 1 : curIdx - 1;
    if (searchMode) {
      if (nextIdx < 0) nextIdx = rows.length - 1;
      else if (nextIdx >= rows.length) nextIdx = 0;
    } else if (nextIdx < 0 || nextIdx >= rows.length) {
      return;
    }
    e.preventDefault();
    const target = rows[nextIdx];
    const absIdx = parseInt(target.getAttribute('data-row'), 10);
    if (searchMode) {
      focusSearchResult(absIdx);
    } else {
      rovingAbs = absIdx;
      applyRovingTabindex();
      target.focus();
    }
    return;
  }
  if (e.key === 'ArrowRight' || e.key === 'ArrowLeft') {
    // Expandable rows use standard tree semantics: ArrowRight expands a
    // collapsed row and ArrowLeft collapses an expanded one.
    const el = e.target.closest('.row');
    if (!el) return;
    const absIdx = parseInt(el.getAttribute('data-row'), 10);
    const row = cache.get(absIdx);
    if (!row || row.is_subop || !hasSubOps(row)) return;
    const expanded = expandedBlocks.has(blockIndexOfAbs(absIdx));
    const wantsToggle = (e.key === 'ArrowRight' && !expanded) ||
      (e.key === 'ArrowLeft' && expanded);
    if (wantsToggle) {
      e.preventDefault();
      toggleDisclosureKeyboard(row, absIdx);
    }
    return;
  }
  if (e.key !== 'Enter' && e.key !== ' ') return;
  if (e.target.closest('button')) return; // native button activation handles it
  const el = e.target.closest('.row');
  if (!el) return;
  e.preventDefault();
  const absIdx = parseInt(el.getAttribute('data-row'), 10);
  const row = cache.get(absIdx);
  if (!row) return;
  if (!row.is_subop && hasSubOps(row)) {
    // Enter/Space toggle the row's reveal state (disclosure semantics) and
    // re-focus the rebuilt row so focus survives the reanchor.
    toggleDisclosureKeyboard(row, absIdx);
    return;
  }
  selectRow(row, absIdx);
  if (e.key === 'Enter') openRawJson(row);
});

// Any row that receives focus (Tab entry, programmatic focus, pointer) becomes
// the roving anchor: re-pin the single tab stop to it so the invariant tracks
// where the user actually is in the virtualized list.
rowsEl.addEventListener('focusin', (e) => {
  const el = e.target.closest ? e.target.closest('.row') : null;
  if (!el) return;
  const absIdx = parseInt(el.getAttribute('data-row'), 10);
  if (Number.isFinite(absIdx) && absIdx !== rovingAbs) {
    rovingAbs = absIdx;
    applyRovingTabindex();
  }
});

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
  // NOTE: `.row:last-child` is NOT reliable here — .table-wrap's last children
  // are the absolutely-positioned column-resize handles, so a row is never the
  // last child and the lookup returns null (making the first appended row look
  // like a group start). Take the last element that is actually a row instead.
  const rowEls = w.querySelectorAll('.row');
  const lastRowEl = rowEls.length ? rowEls[rowEls.length - 1] : null;
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
    applyRovingTabindex();
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
  // Build TOP-DOWN, exactly like reanchorTo: a row is a group-start when its
  // group differs from the row ABOVE it, so the chip lands on the FIRST
  // (newest) row of a run. Iterating bottom-up instead made the chip stick to
  // the run's LAST (oldest) row — prepending across a boundary moved a group's
  // label from its true start (e.g. row 100) down to the row below it (99).
  // The topmost prepended row is marked unconditionally (prevGroup starts
  // null), matching reanchorTo's "first rendered row of the window" rule, so
  // chip positions are identical before/after a full rebuild.
  let prevGroup = null;
  let html = '';
  let added = 0;
  const oldTop = renderTop;
  const topVis = renderTop - 1;
  for (let vis = Math.max(0, topVis - n + 1); vis <= topVis; vis++) {
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
    // The row that used to be the window's FIRST rendered row may carry a
    // group-start chip that was only justified by the old window edge (a
    // mid-group reanchor marks the top row unconditionally). With real rows
    // now above it, re-evaluate that chip against its new previous sibling so
    // prepending same-group rows never leaves a duplicate stale boundary.
    const boundaryAbs = absIndexForVisible(oldTop);
    const boundaryEl = w.querySelector('.row[data-row="' + boundaryAbs + '"]');
    if (boundaryEl) {
      const prevEl = boundaryEl.previousElementSibling;
      const prevRow = prevEl ? cache.get(parseInt(prevEl.getAttribute('data-row'), 10)) : null;
      const boundaryRow = cache.get(boundaryAbs);
      if (prevRow && boundaryRow) {
        setGroupStart(boundaryEl, boundaryRow, boundaryRow.group !== prevRow.group);
      }
    }
    applyRovingTabindex();
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
    applyRovingTabindex();
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
  applyRovingTabindex();
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
  applyRovingTabindex();
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

/** Mark a row as the inline selection (DOM + state). Rows are rebuilt by
 * virtual scroll, so `buildRowHtml` also re-applies the selected class from
 * `selectedRowKey` on every render. Selection never changes the table width or
 * opens a secondary surface. */
function selectRow(row, absIdx) {
  const w = wrapEl();
  if (w) {
    const prev = w.querySelector('.row-selected');
    if (prev && prev !== w.querySelector('.row[data-row="' + absIdx + '"]')) {
      prev.classList.remove('row-selected');
      prev.setAttribute('aria-selected', 'false');
    }
    const cur = w.querySelector('.row[data-row="' + absIdx + '"]');
    if (cur) {
      cur.classList.add('row-selected');
      cur.setAttribute('aria-selected', 'true');
    }
  }
  selectedRowKey = row.node_key;
}

/** Open the selected record in the existing read-only JSON editor. This is an
 * explicit activation only (Enter or double-click); ordinary reading and row
 * selection stay entirely inside the single history surface. */
function openRawJson(row) {
  if (!row) return;
  if (row.git_oid) {
    vscode.postMessage({ type: 'openJson', git_oid: row.git_oid, repository: row.repository });
  } else if (row.op_id) {
    vscode.postMessage({ type: 'openJson', op_id: row.op_id });
  } else {
    announce('No raw record is available for this row');
  }
}

/** Clear the inline selection when replacing or resetting the history view. */
function clearSelection() {
  selectedRowKey = null;
  const w = wrapEl();
  if (w) {
    const prev = w.querySelector('.row-selected');
    if (prev) {
      prev.classList.remove('row-selected');
      prev.setAttribute('aria-selected', 'false');
    }
  }
}

/** Focus a search-result row, keeping every arrow-navigation invariant
 * synchronized: roving tabindex (single tab stop), DOM focus, aria-selected /
 * visual row selection, and selectedRowKey. The target is revealed with a
 * minimal header-aware scroll (see revealRow) so a wrap from the input to the
 * last hit (or last -> first) never jars the scroller, then focus is applied
 * without a second scroll. */
function focusSearchResult(absIdx) {
  const w = wrapEl();
  if (!w) return;
  const target = w.querySelector('.row[data-row="' + absIdx + '"]');
  const row = cache.get(absIdx);
  if (!target || !row) return;
  rovingAbs = absIdx;
  applyRovingTabindex();
  selectRow(row, absIdx);
  revealRow(target);
  target.focus({ preventScroll: true });
}

/** Reveal `el` inside the #rows scroller with the minimal scroll needed, so
 * the sticky column header never overlaps the target and content below the
 * fold is brought in by exactly its overflow. No scroll happens when the row
 * is already fully visible, which keeps keyboard-driven wraps non-jarring. */
function revealRow(el) {
  const viewport = rowsEl.getBoundingClientRect();
  const header = rowsEl.querySelector('.tbl-header');
  const headerH = header ? header.getBoundingClientRect().height : 0;
  const rect = el.getBoundingClientRect();
  const top = rect.top - viewport.top;
  const bottom = rect.bottom - viewport.top;
  if (top < headerH) {
    rowsEl.scrollTop -= (headerH - top);
  } else if (bottom > rowsEl.clientHeight) {
    rowsEl.scrollTop += (bottom - rowsEl.clientHeight);
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
      resetFindState();
      updateFindCounter('hidden');
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
      layoutReady = false;
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
      const restored = restoreState();
      // Apply the persisted profile (Activity/Raw) BEFORE the first fetch so
      // the offset-0 window is requested with the right hide_trace flag.
      // `persist:false` keeps this from writing the still-unrestored viewport
      // (scrollTop is 0 here — the spacer isn't built yet), which would
      // clobber the persisted topRow this restore is about to apply. The
      // actually-restored position is persisted right after restoreScrollTop.
      setProfile(restored.profile, { reset: false, persist: false });
      // Reanchor to the viewport window for the CURRENT (un-scrolled) position
      // first, so the spacer exists and has real height. Rows may not be cached
      // yet — GetWindow responses will append them in.
      fetchWindow();
      reanchorTo(desiredVisibleRange().top, desiredVisibleRange().bottom);
      // Now that the scaffold is built, restore the persisted scroll offset
      // (previously this set scrollTop before reanchor — before the spacer
      // existed — so it clamped to 0 and the position was lost).
      if (restored.topRow > 0) {
        restoreScrollTop(restored.topRow);
      } else {
        rowsEl.scrollTop = 0;
      }
      // Persist the actually-restored viewport now that the scaffold is real.
      // This is the ONLY open-path save: any earlier saveState() would write
      // the pre-restore scrollTop (0) and lose the saved position on a real
      // context recreation.
      saveState();
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

  // Compatibility path for older extension hosts that send `reveal` after a
  // recreated context. Current hosts retain ordinary hidden contexts and use
  // the instance-aware `webviewReady` handshake for genuine recreation.
  if (msg.id === 'reveal') {
    const restored = restoreState();
    // persist:false — same contract as the open handler (never write the
    // pre-restore viewport into persisted state during initialization).
    setProfile(restored.profile, { reset: false, persist: false });
    // The revealed webview is a fresh context, but be safe: start a new view
    // generation and force the offset-0 snapshot so expansion counts are
    // established before any deep restore window loads.
    viewGen++;
    snapshotEstablished = false;
    subOpCounts = [];
    recomputeExpansion();
    vscode.postMessage({ type: 'log', text: 'reveal: topRow=' + restored.topRow + ' total=' + total });
    setTimeout(() => {
      fetchWindow();
      reanchorTo(desiredVisibleRange().top, desiredVisibleRange().bottom);
      // Restore the persisted scroll offset after the scaffold is rebuilt so the
      // scroll range is real (same fix as the open handler), then persist the
      // actually-restored viewport.
      if (restored.topRow > 0) {
        restoreScrollTop(restored.topRow);
      } else {
        rowsEl.scrollTop = 0;
      }
      saveState();
      startProgressiveLoader();
    }, 50);
    return;
  }

  // Every other message is the correlated response to a request issued via
  // Match by id; unknown ids (e.g. responses from a replayed open)
  // are dropped.
  const req = typeof msg.id === 'number' ? inFlight.get(msg.id) : undefined;
  if (!req) return;
  inFlight.delete(msg.id);
  const wasPendingWindow = msg.id === pendingWindowReqId;
  if (wasPendingWindow) pendingWindowReqId = -1;

  // Stale-view rejection: the response was issued under an older view
  // generation (open/history/search changed while it was in flight). Applying it
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
        resetHistory();
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
    if (reqBody.FindInHistory !== undefined) {
      // Find-in-chain errors are compact and non-disruptive: the chain stays
      // fully visible and the counter reports the failure (next Enter retries).
      showFindError(errText);
      return;
    }
    return;
  }
  if (!r.value || typeof r.value !== 'object') return;

  // FindInHistory response — an in-place find: `{ matches, returned, more }`.
  // Unlike the legacy flat-list Search, this NEVER replaces the history view:
  // matches resolve to real top-level rows of the current view, and the find
  // only scrolls, highlights, and updates the counter (applyFindResponse).
  if (Array.isArray(r.value.matches)) {
    applyFindResponse(r.value);
    return;
  }

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
    const responseLayoutReady = r.value.layout_ready !== false;
    if (responseLayoutReady) {
      layoutReady = true;
    }
    total = r.value.total;
    // Global max lane for stable graph-column width (per-row graph cells).
    if (responseLayoutReady && typeof r.value.max_lane === 'number' && r.value.max_lane !== maxLane) {
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
    // A find-in-chain jump's target window has arrived: move the viewport to
    // it BEFORE eviction (eviction keys off the current viewport) and
    // select/reveal the real row.
    if (pendingFindTarget && cache.has(pendingFindTarget.abs)) {
      completeFindJump();
    }
    // The layout hydration response overwrites already-rendered provisional
    // rows. Rebuild the bounded visible window once so lane SVGs update; the
    // initial row-only response has already delivered the first paint.
    if (responseLayoutReady && req.body.GetWindow.include_layout === true) {
      reanchorTo(renderTop, renderBottom);
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
    if (!announcedInitialLoad && total > 0) {
      announcedInitialLoad = true;
      announce('Loaded ' + visibleTotal() + ' history rows');
    }
    if (cache.size === 0 && total === 0) {
      // Replace the unfillable placeholder with an explicit empty state.
      showViewMessage('No history rows', false);
    }
    vscode.postMessage({ type: 'log', text: `cached ${cache.size}/${total} nodes (fetched ${totalFetched})` });
    reportStatus();
    saveState();
    if (!responseLayoutReady && req.body.GetWindow.include_layout === false) {
      // Paint is complete. Now ask the service to perform the O(V) geometry
      // pass and replace exactly this bounded page when it returns.
      sendWindow({
        GetWindow: {
          offset: req.body.GetWindow.offset,
          limit: req.body.GetWindow.limit,
          hide_submodules: req.body.GetWindow.hide_submodules,
          filter: req.body.GetWindow.filter,
          include_layout: true,
        },
      });
      return;
    }
    if (pendingFindTarget) {
      // Keep driving the jump: fetch the target's window (or the offset-0
      // snapshot first) until the row is cached.
      fetchWindowAround(pendingFindTarget.abs);
    } else {
      // Keep loading until the content fills the viewport so scrolling works.
      fetchWindow();
    }
    return;
  }

});

// Announce readiness only after the host-message listener above exists. This
// closes the race where a recreated webview could miss the reveal replay and
// remain permanently on its initial "Loading history…" message.
vscode.postMessage({ type: 'webviewReady', instanceId: rendererInstanceId });

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
  // The legacy flat-list view supersedes any in-place find session.
  resetFindState();
  updateFindCounter('hidden');
  // SearchFilters cannot express submodule exclusion, so apply the same fixed
  // default client-side. Git hits carry `is_submodule` from the service.
  if (hideSubmodules()) {
    hits = hits.filter((h) => !h.is_submodule);
  }
  // Search replaces the view: bump the generation so any in-flight history
  // window response is rejected as stale instead of clobbering the result list,
  // and release the window slot (the result list is fully local).
  viewGen++;
  pendingWindowReqId = -1;
  snapshotEstablished = false;
  layoutReady = false;
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
  clearSelection();
  if (total === 0) {
    showViewMessage('No results for "' + esc(searchQuery) + '"', false);
  } else {
    reanchorTo(0, Math.max(0, total - 1));
  }
  announce(String(total) + ' result' + (total === 1 ? '' : 's') + ' for "' + searchQuery + '"');
  window.__editchainDataReady = true;
  vscode.postMessage({ type: 'log', text: `search: ${total} result(s)` });
  reportStatus();
}

/** Reset to the full history view and reload from the top. */
function resetHistory() {
  searchMode = false;
  searchQuery = '';
  currentSearchEpoch = -1;
  resetFindState();
  updateFindCounter('hidden');
  // The previous view (e.g. a 0-result search) may have left `total` at 0;
  // the full-history window must be re-requested, not treated as empty.
  total = -1;
  // Returning to the full history is a new view: stale in-flight responses
  // (e.g. a Search issued before the clear) are rejected via the generation.
  viewGen++;
  snapshotEstablished = false;
  layoutReady = false;
  subOpCounts = [];
  recomputeExpansion();
  pendingWindowReqId = -1;
  cache.clear();
  totalFetched = 0;
  lastRenderKey = '';
  renderTop = 0;
  renderBottom = -1;
  rowsEl.scrollTop = 0;
  clearSelection();
  // The previous view's DOM rows belong to the OLD generation: leaving them in
  // place until the new window arrives would let them stay interactive (and
  // satisfy harness/e2e readiness) while the cache/total no longer back them —
  // e.g. a profile switch with a delayed GetWindow: readiness sees rows with no
  // placeholders, then Enter targets a stale `.row` whose absolute index is
  // absent from the cleared cache and raw activation is swallowed. Drop the grid
  // for an explicit loading state and clear readiness BEFORE the new fetch, so
  // no stale row is visible, focusable, or selectable while the new view is in
  // flight.
  window.__editchainDataReady = false;
  rovingAbs = -1;
  showViewMessage('Loading history…', false);
  fetchWindow();
}

// Find-in-chain keyboard behaviour. Enter submits a NEW query; Enter/Shift+Enter
// on the SAME settled query advance / step back through matches (wrapping).
// ArrowDown/ArrowUp move next/previous through SETTLED matches for the exact
// submitted query and wrap; while a newer search is in flight or the input text
// has been edited without submitting, navigation stays in the input (stale
// matches are never entered). Escape clears the find session without reloading
// history or changing the scroll position. Focus ALWAYS remains in the input
// during find navigation so query editing stays easy.
searchEl.addEventListener('keydown', (e) => {
  if (e.key === 'Enter') {
    const q = searchEl.value.trim();
    if (!q) {
      // Empty query: exit find (or restore the chain from the legacy flat-list
      // view) without reloading anything.
      if (searchMode) resetHistory();
      else clearFind();
      return;
    }
    if (q === searchQuery && findActive) {
      // Same-query Enter advances to the next match; Shift+Enter steps back.
      e.preventDefault();
      navigateFind(e.shiftKey ? -1 : 1);
    } else {
      submitFind(q);
    }
    return;
  }
  if (e.key === 'Escape') {
    // Escape exits the find session but keeps the typed text so the query
    // stays editable; nothing refetches and the scroll position is unchanged.
    e.preventDefault();
    if (searchMode) resetHistory();
    else clearFind();
    return;
  }
  if (e.key === 'ArrowDown' || e.key === 'ArrowUp') {
    const q = searchEl.value.trim();
    // In-place find: navigate the SETTLED matches for the exact submitted
    // query only. Edited-but-unsubmitted text and an in-flight replacement
    // keep the arrows in the input.
    if (findActive && findMatches.length > 0 && q === searchQuery &&
        currentSearchEpoch === -1) {
      e.preventDefault();
      navigateFind(e.key === 'ArrowDown' ? 1 : -1);
      return;
    }
    // Legacy flat-list search results (older services): leave the input and
    // focus+select the first (ArrowDown) or last (ArrowUp) result. Kept only
    // for compatibility — production navigation stays in the input above.
    if (searchMode && total > 0 && q === searchQuery &&
        currentSearchEpoch === -1) {
      e.preventDefault();
      focusSearchResult(e.key === 'ArrowDown' ? 0 : total - 1);
    }
  }
});

// Clearing the search input exits find-in-chain without reloading history or
// moving the scroll position (the chain was never replaced); the legacy
// flat-list search view still resets to restore the history DOM.
searchEl.addEventListener('input', () => {
  if (!searchEl.value.trim()) {
    if (searchMode) resetHistory();
    else clearFind();
  }
  // Edited-but-unsubmitted text must disable the buttons (and restore them
  // when the exact submitted query is retyped), mirroring the arrow guard.
  syncFindNavButtons();
});

// Previous/Next find navigation buttons: the exact same wrapping
// navigateFind(-1/+1) path ArrowUp/ArrowDown use, so a click moves the cursor
// through the SAME settled matches, updates "i of N", and reveals/selects/
// highlights the real chain row without ever replacing or rebuilding the
// chain. mousedown is prevented from stealing focus so a mouse click never
// blurs the search input; the click then re-focuses the input so editing and
// keyboard navigation stay immediate. Disabled buttons never fire clicks, so
// the disabled-state guard in syncFindNavButtons is the only gate needed.
if (searchPrevBtn) {
  searchPrevBtn.addEventListener('mousedown', (e) => e.preventDefault());
  searchPrevBtn.addEventListener('click', () => {
    navigateFind(-1);
    searchEl.focus();
  });
}
if (searchNextBtn) {
  searchNextBtn.addEventListener('mousedown', (e) => e.preventDefault());
  searchNextBtn.addEventListener('click', () => {
    navigateFind(1);
    searchEl.focus();
  });
}
syncFindNavButtons();

// Activity/Raw profile control. Switching resets the view coherently: the
// search exits, the view generation bumps (stale windows from the old profile
// are rejected), the expansion snapshot and cache drop, and history refetches
// from offset 0 under the new hide_trace flag.
if (profileActivityBtn) {
  profileActivityBtn.addEventListener('click', () => setProfile('activity', { reset: true }));
}
if (profileRawBtn) {
  profileRawBtn.addEventListener('click', () => setProfile('raw', { reset: true }));
}
syncProfileButtons();

// Harness-only debug hooks (not production behaviour): let probes/e2e switch
// the profile through the real control path and read the active profile.
window.__editchainSetProfile = function (name) {
  setProfile(name, { reset: true });
};
window.__editchainGetProfile = function () {
  return profile;
};
window.__editchainHideTrace = function () {
  return hideTrace();
};

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

// Host layout changes can resize #rows without a window resize event. Observe
// its content-box width and re-run the same debounced recompute whenever it
// changes so graph and inline column widths cannot go stale. Height-only
// notifications are ignored: the rows viewport height is flex-fixed, and
// re-rendering on height changes would loop (reanchorTo rebuilds the spacer,
// which can alter scrollbar presence and nudge clientWidth once — that one
// real width change is exactly what we want to react to).
let lastRowsWidth = rowsEl.clientWidth;
if (typeof ResizeObserver === 'function') {
  new ResizeObserver((entries) => {
    const entry = entries && entries[0];
    const w = entry ? entry.contentRect.width : rowsEl.clientWidth;
    if (Math.abs(w - lastRowsWidth) < 0.5) return;
    lastRowsWidth = w;
    clearTimeout(resizeTimer);
    resizeTimer = setTimeout(onViewportResize, 150);
  }).observe(rowsEl);
}

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
  // Keep even Pulse's compressed one-lane rail visibly present at narrow
  // widths (the layout contract treats topology as quiet, never absent).
  return Math.round(Math.min(Math.max(32, natural), graphWidthBudget()) * 100) / 100;
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
  // Center the 6px handle on the column's right boundary, but never let it
  // extend past the container's right edge: the last visible column's boundary
  // sits exactly at the container edge, so centering there would hang 3px past
  // it and add phantom horizontal scroll (scrollW = clientW + 3) with no real
  // overflow. When the boundary is BEYOND the container (a column dragged wider
  // than the viewport), the handle pins to the edge while the columns keep
  // their genuine overflow — nothing is masked.
  const wrapW = wrapEl.clientWidth;
  handle.style.left = Math.min(boundaryX - 3, Math.max(0, wrapW - 6)) + 'px';
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
    // Columns dropped at narrow widths have no visible boundary — a handle
    // there would pile onto the adjacent column's edge and mislead the drag.
    if (isColumnHidden(col)) continue;
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
    layoutReady,
    graphWidth: currentGraphWidth(),
  };
};
// Lets the real-VS-Code lifecycle test prove that raw JSON -> Back reused the
// same retained JS context rather than recreating a fast-looking replacement.
window.__editchainRendererInstanceId = rendererInstanceId;

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
window.__editchainViewGen = function () {
  return viewGen;
};
window.__editchainProgressiveTimerActive = false;
