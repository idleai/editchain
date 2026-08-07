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

const statusEl = document.getElementById('status');
const rowsEl = document.getElementById('rows');
const searchEl = document.getElementById('search');
const detailEl = document.getElementById('detail');
const layoutEl = document.getElementById('layout');
const hideSubmodulesEl = document.getElementById('hideSubmodules');

let offset = 0;
const PAGE = 200;
let total = 0;
let pending = false;
let allRows = [];          // accumulated rows across pages
let layout = null;         // { rows: [{node,lane}], edges: [{child,parent,points}] }
let layoutOffset = 0;      // history offset of the first layout row
let pendingLayoutOffset = 0; // offset requested for the in-flight layout

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
  const naturalW = (maxLane + 1) * LANE_W + LANE_W;
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

  // Draw each edge as one continuous path, clipped to the rendered rows.
  for (const edge of layout.edges) {
    const childRow = rowOf[edge.child];
    if (childRow === undefined) continue; // child not yet loaded

    // Collect points that fall within the rendered rows. Edge points are
    // layout-relative; convert to actual row index by adding layoutOffset.
    const pts = [];
    for (const p of edge.points) {
      const actualRow = p.row + layoutOffset;
      if (actualRow < rows.length) pts.push({ ...p, row: actualRow });
    }
    if (!pts.length) continue;

    // If the parent isn't rendered yet, extend the path down to window bottom.
    const parentLoaded = rowOf[edge.parent] !== undefined;
    let d = '';
    for (let i = 0; i < pts.length; i++) {
      const x = LANE_W / 2 + pts[i].lane * LANE_W + LANE_W / 2;
      const y = (rowYs[pts[i].row] !== undefined ? rowYs[pts[i].row] : pts[i].row * ROW_H) + ROW_H / 2;
      d += (i === 0 ? 'M' : 'L') + x.toFixed(1) + ',' + y.toFixed(1);
    }
    if (!parentLoaded && pts.length) {
      // Extend vertically from last point down to window bottom.
      const lastX = LANE_W / 2 + pts[pts.length - 1].lane * LANE_W + LANE_W / 2;
      d += 'L' + lastX.toFixed(1) + ',' + height.toFixed(1);
    }

    // Shadow path (thick, background-coloured) for contrast against text.
    svg += `<path class="graphShadow" d="${d}"/>`;
    // Line path — colour comes from CSS (.graphLine) so it stays visible on
    // both light and dark themes.
    svg += `<path class="graphLine" d="${d}"/>`;
  }

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

// Maximum width of the graph column, so the description/content column always
// stays visible even when there are many concurrent lanes.
const MAX_GRAPH_W = 320;

/** Render all accumulated rows into a table with graph column + text columns. */
function renderRows() {
  // Apply the submodule filter (hidden unless "Show git submodules" is checked).
  const visible = hideSubmodules() ? allRows.filter((r) => !r.is_submodule) : allRows;

  const maxLane = layout && layout.rows.length ? Math.max(...layout.rows.map((r) => r.lane), 0) : 0;
  const naturalGraphW = (maxLane + 1) * LANE_W + LANE_W;
  // Cap the graph column so content stays visible; lanes beyond the cap are
  // clipped by overflow-hidden on the graph cell.
  const graphWidth = Math.min(naturalGraphW, MAX_GRAPH_W);

  // Build the sticky table header.
  let html =
    '<div class="tbl-header" style="--graph-w:' + graphWidth + 'px">' +
      '<div class="th">Graph</div>' +
      '<div class="th">Content</div>' +
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

    html += '<div class="row" data-key="' + esc(row.node_key) + '" style="--graph-w:' + graphWidth + 'px">' +
      '<div class="graph-cell"></div>' +
      '<div class="text-cell"><div class="summary">' + esc(row.summary || '(no summary)') + '</div></div>' +
      '<div class="date-cell">' + esc(formatDate(row.timestamp_ms)) + '</div>' +
      '<div class="author-cell">' + esc(row.author || '') + '</div>' +
      '<div class="commit-cell">' + esc(row.commit_id || '') + '</div>' +
      '</div>';
  });

  rowsEl.innerHTML =
    '<div class="table-wrap" style="--graph-w:' + graphWidth + 'px">' +
      html +
    '</div>';

  // Measure each rendered row's real offsetTop within the table-wrap so block
  // separators don't misalign the graph overlay.
  const wrapEl = rowsEl.querySelector('.table-wrap');
  const rowEls = wrapEl.querySelectorAll('.row');
  const rowYs = [];
  rowEls.forEach((el) => { rowYs.push(el.offsetTop); });

  // Insert the graph overlay after measuring.
  wrapEl.insertAdjacentHTML('beforeend', buildGraphSvg(rowYs, visible, graphWidth));

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

// Layout window size as a multiple of the visible viewport rows. The graph
// layout is recomputed dynamically as the user scrolls; a window of K× the
// viewport gives prefetch headroom so lines extend before the next recompute.
const WINDOW_MULT = 4;

/** Estimate how many rows fit in the current viewport. */
function viewportRows() {
  const h = rowsEl.clientHeight || 600;
  return Math.max(1, Math.floor(h / ROW_H));
}

/** Request the graph layout for the current window range. */
function requestLayout() {
  const win = layoutWindow();
  pendingLayoutOffset = win.offset;
  send({ GetLayout: { hide_submodules: hideSubmodules(), offset: win.offset, limit: win.limit } });
}

/** Compute the layout window range centered on the current scroll position. */
function layoutWindow() {
  const vp = viewportRows();
  const size = vp * WINDOW_MULT;
  // Center on the first visible row.
  const firstVisible = Math.floor(rowsEl.scrollTop / ROW_H);
  let off = Math.max(0, firstVisible - Math.floor(size / 2));
  off = Math.min(off, Math.max(0, total - size));
  return { offset: off, limit: size };
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

/** Inspect a node's details. */
function inspect(row) {
  console.log('[editchain] inspect', row && row.node_key);
  // Show the detail pane and populate it with the node's details.
  layoutEl.classList.add('has-detail');
  detailEl.innerHTML = '<div class="detail-title">Loading…</div>';
  if (row.git_oid) {
    send({ ResolveObject: { repository: row.repository, oid: row.git_oid } });
  } else if (row.op_id) {
    send({ GetNodeDetails: { op_id: row.op_id } });
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
      statusEl.textContent = `Open: ${r.value.nodes} nodes, ${r.value.repos} repos`;
      allRows = [];
      offset = 0;
      total = r.value.nodes;
      loadMore();
      requestLayout();
    } else {
      statusEl.textContent = `Error: ${r.error}`;
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
    renderRows();
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
    statusEl.textContent = `${allRows.length}/${total} nodes`;
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

// Infinite scroll: load more rows near the bottom, and recompute the graph
// layout dynamically as the user scrolls so lines stay continuous.
rowsEl.addEventListener('scroll', () => {
  if (rowsEl.scrollTop + rowsEl.clientHeight >= rowsEl.scrollHeight - 40) {
    loadMore();
  }
  // Recompute layout when the visible window drifts toward the edge of the
  // current layout window.
  const win = layoutWindow();
  const firstVisible = Math.floor(rowsEl.scrollTop / ROW_H);
  const nearEdge =
    firstVisible < win.offset + viewportRows() ||
    firstVisible > win.offset + win.limit - viewportRows() * 2;
  if (nearEdge && (win.offset !== layoutOffset || !layout)) {
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