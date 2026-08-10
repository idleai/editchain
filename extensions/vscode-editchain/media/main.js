// Webview renderer for the EditChain History explorer.
// Renders a git-graph-style visualization of unified history using a single
// full-height SVG overlay (continuous branch lines) over a real table with
// columns: Graph | Content | Date | Author | Commit/ID.
//
// The graph geometry (lanes + edge point paths) is computed server-side by the
// Rust service (`GetLayout`) and shipped over stdio; this file only draws it.
// This mirrors vscode-git-graph's architecture: one SVG positioned absolutely
// over the table container, with pointer-events disabled on the SVG and
// re-enabled on node dots.
//
// Rows are paged via infinite scroll; as more rows load both the table and the
// SVG grow together. Edges whose parent has not yet been loaded are drawn down
// to the bottom of the currently-loaded window (they extend further once more
// rows arrive), matching how vscode-git-graph handles "Load More".
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
const hideSubmodulesEl = document.getElementById('hideSubmodules');
const hideSystemEl = document.getElementById('hideSystem');

let offset = 0;
// Rows fetched per request. Larger than the visible viewport so each fetch
// buffers well ahead of the scroll position, keeping the user from ever
// waiting on an in-flight fetch.
const PAGE = 500;
// How far ahead (in rows) of the current scroll position to keep buffered.
// The background loader keeps fetching until this much content is loaded past
// the viewport, so scrolling never blocks on a fetch.
const PROGRESSIVE_BUFFER = 1000;
let total = 0;
let pending = false;
let allRows = [];          // accumulated rows across pages
let layout = null;         // { rows: [{node,lane}], edges: [{child,parent,points}] }
let layoutOffset = 0;      // history offset of the first layout row
let layoutLimit = 0;       // number of rows covered by the loaded layout window
let pendingLayoutOffset = 0; // offset requested for the in-flight layout

// Persist webview state across recreations (e.g. when the user navigates to a
// JSON editor and back). Without this, the webview is recreated from scratch on
// every reveal and shows "Loading…" until it refetches.
function saveState() {
  // Cap the persisted rows to a reasonable window so we stay under VS Code's
  // webview state size limit (setState fails silently if exceeded). Persisting
  // the full loaded history can blow past it for large chains.
  const MAX_SAVED_ROWS = 2000;
  const savedRows = allRows.slice(0, MAX_SAVED_ROWS);
  vscode.setState({
    allRows: savedRows,
    offset: Math.min(offset, savedRows.length),
    total,
    layout,
    layoutOffset,
    layoutLimit,
    pendingLayoutOffset,
  });
  console.log('[editchain] saveState: ' + savedRows.length + ' rows');
}

function restoreState() {
  const s = vscode.getState();
  console.log('[editchain] restoreState: ' + (s && Array.isArray(s.allRows) ? s.allRows.length : 'none'));
  if (s && Array.isArray(s.allRows) && s.allRows.length) {
    allRows = s.allRows;
    offset = s.offset || 0;
    total = s.total || 0;
    layout = s.layout || null;
    layoutOffset = s.layoutOffset || 0;
    layoutLimit = s.layoutLimit || 0;
    pendingLayoutOffset = s.pendingLayoutOffset || 0;
    return true;
  }
  return false;
}

// Branch colours for graph lanes (indexed by lane).
const COLORS = ['#e6194b', '#3cb44b', '#ffe119', '#4363d8', '#f58231', '#911eb4', '#46f0f0', '#f032e6', '#bcf60c', '#fabebe'];

const ROW_H = 34;
const LANE_W = 18;
const DOT_R = 4;

/** Send a request body to the extension host. */
function send(body) {
  vscode.postMessage({ body });
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
 * `rowYs` maps visible-row index → pixel y-offset within the table-wrap (the
 * measured `offsetTop` of each rendered `.row`). `loadedCount` is how many of
 * those rows are currently shown. Each edge is drawn as one continuous <path>
 * from its ordered grid points (child → parent), so lines run unbroken across
 * many rows.
 */
function buildGraphSvg(rowYs, rows, maxWidth) {
  if (!layout || !layout.rows.length || rows.length === 0) return '';
  const maxLane = Math.max(...layout.rows.map((r) => r.lane), 0);
  const numLanes = Math.min(maxLane + 1, MAX_GRAPH_LANES);
  // One extra lane of padding so the last lane's dot isn't clipped.
  const naturalW = numLanes * LANE_W + LANE_W;
  // Cap the SVG width so lanes beyond the cap are clipped (content stays visible).
  const width = Math.min(naturalW, maxWidth || naturalW);
  // Height spans from the first row's top to just below the last row.
  const height = (rowYs[rows.length - 1] || 0) + ROW_H;

  // Map node key -> actual row index in the rendered rows.
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
    const childRow = rowOf[edge.child];
    const parentRow = rowOf[edge.parent];

    // Convert every point to an absolute row index. Points beyond the loaded
    // rows are kept so we can still extend lines to the SVG bottom; they are
    // clamped when computing y.
    const pts = [];
    for (const p of edge.points) {
      pts.push({ ...p, row: p.row + layoutOffset });
    }
    if (!pts.length) { skippedEdges++; continue; }

    const childLoaded = childRow !== undefined;
    const parentLoaded = parentRow !== undefined;

    // Helper: pixel y for an absolute row, clamped into [0, height].
    const yOf = (r) => {
      const raw = (rowYs[r] !== undefined ? rowYs[r] : r * ROW_H) + ROW_H / 2;
      return Math.max(0, Math.min(height, raw));
    };

    let d = '';
    if (!childLoaded && pts.length) {
      // Child is above the rendered rows: start at SVG top on the child's lane,
      // then follow points downward.
      const firstX = LANE_W / 2 + pts[0].lane * LANE_W + LANE_W / 2;
      d += 'M' + firstX.toFixed(1) + ',' + '0';
      for (let i = 0; i < pts.length; i++) {
        const x = LANE_W / 2 + pts[i].lane * LANE_W + LANE_W / 2;
        d += 'L' + x.toFixed(1) + ',' + yOf(pts[i].row).toFixed(1);
      }
    } else {
      for (let i = 0; i < pts.length; i++) {
        const x = LANE_W / 2 + pts[i].lane * LANE_W + LANE_W / 2;
        d += (i === 0 ? 'M' : 'L') + x.toFixed(1) + ',' + yOf(pts[i].row).toFixed(1);
      }
    }

    if (!parentLoaded && pts.length) {
      // Parent is below the rendered rows: extend vertically down to SVG bottom.
      const lastX = LANE_W / 2 + pts[pts.length - 1].lane * LANE_W + LANE_W / 2;
      d += 'L' + lastX.toFixed(1) + ',' + height.toFixed(1);
    }

    // Shadow path (thick, background-coloured) for contrast against text.
    svg += `<path class="graphShadow" d="${d}"/>`;
    // Line path — colour comes from CSS (.graphLine) so it stays visible on
    // both light and dark themes.
    svg += `<path class="graphLine" d="${d}"/>`;
    drawnEdges++;
  }
  console.log('[editchain] buildGraphSvg layoutOffset=' + layoutOffset +
    ' edges=' + layout.edges.length + ' drawn=' + drawnEdges + ' skipped=' + skippedEdges);

  // Draw node dots at each rendered row's lane centre.
  for (let i = layoutOffset; i < rows.length; i++) {
    const key = rows[i].node_key;
    const lane = laneOf[key];
    if (lane === undefined) continue;
    const x = LANE_W / 2 + lane * LANE_W + LANE_W / 2;
    const y = rowYs[i] + ROW_H / 2;
    const colour = COLORS[lane % COLORS.length];
    svg += `<circle class="graphDot" data-row="${i}" cx="${x}" cy="${y}" r="${DOT_R}" fill="${colour}"/>`;
  }

  svg += '</svg>';
  return svg;
}

/** Whether submodules should be hidden (inverted from the "Show" checkbox). */
function hideSubmodules() {
  // "Show git submodules" is off by default → submodules hidden by default.
  return !(hideSubmodulesEl && hideSubmodulesEl.checked);
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

/** Render all accumulated rows into a table with graph column + text columns. */
function renderRows() {
  // Apply the submodule filter (hidden unless "Show git submodules" is checked)
  // and the messages-only filter (when "Show messages only" is checked).
  let visible = hideSubmodules() ? allRows.filter((r) => !r.is_submodule) : allRows;
  if (showMessagesOnly()) {
    visible = visible.filter(isMessageRow);
  }

  // Build the sticky table header.
  let html =
    '<div class="tbl-header" style="' + colStyle() + '">' +
      '<div class="th graph">Graph</div>' +
      '<div class="th content">Content</div>' +
      '<div class="th date">Date</div>' +
      '<div class="th author">Author</div>' +
      '<div class="th commit">Commit/ID</div>' +
    '</div>';

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

    html += '<div class="row ' + kindClass + agentClass + '" data-key="' + esc(row.node_key) + '" style="' + colStyle() + '">' +
      '<div class="graph-cell"></div>' +
      '<div class="text-cell"><div class="summary">' + esc(row.summary || '(no summary)') + '</div></div>' +
      '<div class="date-cell">' + esc(formatDate(row.timestamp_ms)) + '</div>' +
      '<div class="author-cell">' + esc(row.author || '') + '</div>' +
      '<div class="commit-cell">' + esc(row.commit_id || '') + '</div>' +
      '</div>';
  });

  // Preserve the scroll position across the DOM rebuild. Setting innerHTML
  // destroys and recreates all rows, which would otherwise reset scrollTop to 0
  // on every layout recompute — snapping the user back to the top and
  // preventing any further recomputation as they scroll down.
  const prevScrollTop = rowsEl.scrollTop;

  rowsEl.innerHTML =
    '<div class="table-wrap" style="' + colStyle() + '">' +
      html +
    '</div>';

  // Restore the scroll position after rebuilding.
  rowsEl.scrollTop = prevScrollTop;

  // Measure each rendered row's real offsetTop within the table-wrap so block
  // separators don't misalign the graph overlay.
  const wrapEl = rowsEl.querySelector('.table-wrap');
  const rowEls = wrapEl.querySelectorAll('.row');
  const rowYs = [];
  rowEls.forEach((el) => { rowYs.push(el.offsetTop); });

  // Insert the graph overlay after measuring.
  wrapEl.insertAdjacentHTML('beforeend', buildGraphSvg(rowYs, visible, currentGraphWidth()));

  // Attach click handlers to rows.
  rowEls.forEach((el) => {
    el.addEventListener('click', () => {
      const key = el.getAttribute('data-key');
      const row = allRows.find((r) => r.node_key === key);
      if (row) inspect(row);
    });
  });
}

/** Load the next window of history. */
function loadMore() {
  if (pending || (total > 0 && offset >= total)) return;
  pending = true;
  send({ GetWindow: { offset, limit: PAGE, hide_submodules: hideSubmodules() } });
}

/** Load more rows until the content fills the viewport (so scrolling works). */
function ensureFilled() {
  if (pending || (total > 0 && offset >= total)) return;
  const contentH = rowsEl.scrollHeight;
  const viewH = rowsEl.clientHeight;
  if (contentH < viewH) {
    loadMore();
  }
}

/**
 * Progressively load history ahead of the scroll position so the user never
 * waits on an in-flight fetch.
 *
 * Keeps fetching in the background until either all rows are loaded or there is
 * at least `PROGRESSIVE_BUFFER` rows of content buffered past the bottom of the
 * current viewport. Because it is driven by a timer (not by scroll events), it
 * runs continuously and independently of how fast the user scrolls.
 */
function progressiveLoad() {
  if (pending || (total > 0 && offset >= total)) return;
  // Rows buffered past the bottom of the viewport.
  const loadedRows = allRows.length;
  const visibleRows = Math.ceil(rowsEl.clientHeight / ROW_H);
  const bufferedAhead = loadedRows - visibleRows - Math.floor(rowsEl.scrollTop / ROW_H);
  if (bufferedAhead < PROGRESSIVE_BUFFER) {
    loadMore();
  }
}

/** Request the graph layout for all currently loaded rows. */
function requestLayout() {
  // Request a layout covering everything loaded so far. This keeps the graph
  // stable as you scroll — lines don't recenter or pop in/out — and only grows
  // when more rows load.
  const limit = Math.max(1, allRows.length);
  pendingLayoutOffset = 0;
  console.log('[editchain] requestLayout offset=0 limit=' + limit);
  send({ GetLayout: { hide_submodules: hideSubmodules(), offset: 0, limit } });
}

/** Reset to the full history view and reload from the top. */
function resetHistory() {
  allRows = [];
  offset = 0;
  total = 0;
  pending = false;
  clearDetail();
  loadMore();
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
      // Restore previously cached rows if this webview was recreated (e.g. after
      // navigating to a JSON editor and back), so history renders immediately
      // instead of showing "Loading…" until a refetch.
      if (!restoreState()) {
        allRows = [];
        offset = 0;
        total = r.value.nodes;
        loadMore();
        requestLayout();
      } else {
        // The webview was just recreated; its viewport height (`100vh`) and DOM
        // layout are not settled yet. Render synchronously first (the data is
        // already in memory), then re-render on a timer so columns and rows get
        // settled dimensions. A timer is used instead of requestAnimationFrame
        // because rAF may not fire until the webview is painted/visible, which
        // would leave the view blank until the user interacts.
        console.log('[editchain] restore: ' + allRows.length + ' rows, layout=' + (layout ? layout.rows.length : 0));
        renderRows();
        ensureFilled();
        setTimeout(() => {
          renderRows();
          ensureFilled();
        }, 100);
      }
      // Start the background progressive loader so history buffers ahead of the
      // scroll position without waiting for scroll events.
      startProgressiveLoader();
    } else {
      vscode.postMessage({ type: 'log', text: `open error: ${r.error}` });
    }
    return;
  }

  // The panel was revealed again (e.g. after navigating to a JSON editor and
  // back). The webview's JS context is reset when hidden (allRows=0), so restore
  // the persisted state from vscode.setState before rendering. A short delay
  // lets the webview finish transitioning from hidden to visible so it has real
  // dimensions to measure.
  if (msg.id === 'reveal') {
    // The webview's JS context is reset when hidden (allRows=0), so restore the
    // persisted state from vscode.setState before rendering.
    const restored = restoreState();
    vscode.postMessage({ type: 'log', text: 'reveal: restored=' + restored + ' allRows=' + allRows.length + ' layout=' + (layout ? layout.rows.length : 0) });
    if (allRows.length) {
      setTimeout(() => {
        renderRows();
        ensureFilled();
      }, 50);
    }
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
    layoutOffset = pendingLayoutOffset;
    layoutLimit = layout.rows.length;
    renderRows();
    saveState();
    return;
  }

  // GetWindow response — has a `rows` array plus `total`.
  if (Array.isArray(r.value.rows)) {
    total = r.value.total;
    for (const row of r.value.rows) {
      allRows.push(row);
    }
    offset += r.value.rows.length;
    pending = false;
    renderRows();
    vscode.postMessage({ type: 'log', text: `loaded ${allRows.length}/${total} nodes` });
    // Persist the loaded rows so a recreated webview can restore them.
    saveState();
    // Recompute the layout to cover the newly loaded rows so lines extend.
    requestLayout();
    // Keep loading until the content fills the viewport so scrolling works.
    ensureFilled();
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

// Re-fetch when the show-submodules toggle changes (filtering is server-side).
if (hideSubmodulesEl) {
  hideSubmodulesEl.addEventListener('change', () => {
    allRows = [];
    offset = 0;
    total = 0;
    pending = false;
    layout = null;
    loadMore();
    requestLayout();
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

// Infinite scroll: load more rows near the bottom, and recompute the graph
// layout dynamically as the user scrolls so lines stay continuous. The scroll
// handler is a fallback; the background loader above is the primary driver.
rowsEl.addEventListener('scroll', () => {
  if (rowsEl.scrollTop + rowsEl.clientHeight >= rowsEl.scrollHeight - 40) {
    loadMore();
  }
  // Recompute layout only when more rows have loaded than the current layout
  // covers. The layout spans all loaded rows (offset 0), so scrolling alone
  // never triggers a recompute — lines stay stable.
  if (!layout || allRows.length > layoutLimit) {
    requestLayout();
  }
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
  const maxLane = layout && layout.rows.length ? Math.max(...layout.rows.map((r) => r.lane), 0) : 0;
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