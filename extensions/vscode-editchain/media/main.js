// Webview renderer for the EditChain History explorer.
// Renders a git-graph-style SVG visualization of unified history.

// @ts-ignore — vscode provides this global in webviews.
const vscode = acquireVsCodeApi();

const statusEl = document.getElementById('status');
const rowsEl = document.getElementById('rows');
const searchEl = document.getElementById('search');
const detailEl = document.getElementById('detail');
const hideSubmodulesEl = document.getElementById('hideSubmodules');

let offset = 0;
const PAGE = 50;
let total = 0;
let pending = false;
let allRows = [];          // accumulated rows across pages
let nodeIndex = {};        // node_key -> row index in allRows

// Branch colors for the graph lanes.
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

/** Assign a lane to each row using a git-log lane algorithm. */
function computeLanes(rows) {
  const laneOf = {};
  const active = []; // lane -> node_key
  const lanes = [];

  for (let i = rows.length - 1; i >= 0; i--) {
    const row = rows[i];
    const key = row.node_key;
    let lane;
    if (laneOf[key] !== undefined) {
      lane = laneOf[key];
    } else {
      lane = active.length;
      active.push(key);
      laneOf[key] = lane;
    }
    active[lane] = null;
    const parents = row.parents || [];
    if (parents.length > 0) {
      active[lane] = parents[0];
      if (laneOf[parents[0]] === undefined) laneOf[parents[0]] = lane;
      for (let p = 1; p < parents.length; p++) {
        if (laneOf[parents[p]] === undefined) {
          const pl = findSpareLane(active, lane);
          active[pl] = parents[p];
          laneOf[parents[p]] = pl;
        }
      }
    }
    lanes[i] = lane;
  }
  return lanes;
}

function findSpareLane(active, preferred) {
  if (active[preferred] === null || active[preferred] === undefined) return preferred;
  for (let i = 0; i < active.length; i++) {
    if (active[i] === null || active[i] === undefined) return i;
  }
  return active.length;
}

/**
 * Build the per-row SVG graph cell.
 *
 * Each row's SVG is ROW_H tall and shows:
 *  - vertical lines for every lane that has a line passing through this row
 *    (i.e. an ancestor relationship spans across this row),
 *  - a horizontal connector from this node's lane to its parent's lane,
 *  - the node dot.
 */
function buildRowSvg(rows, lanes, i) {
  const maxLane = lanes.length ? Math.max(...lanes) : 0;
  const width = (maxLane + 1) * LANE_W + LANE_W;

  // Map node_key -> row index.
  const idx = {};
  rows.forEach((r, j) => { idx[r.node_key] = j; });

  const row = rows[i];
  const yMid = ROW_H / 2;
  const x1 = LANE_W / 2 + lanes[i] * LANE_W + LANE_W / 2;
  const color = COLORS[lanes[i] % COLORS.length];

  // Determine which lanes have a line passing through this row.
  // A line passes through row i in lane L if some node at or above i has a
  // parent at or below i in lane L (i.e. the edge spans across this row).
  const passThroughLanes = new Set();
  for (let j = i; j >= 0; j--) {
    for (const parentKey of (rows[j].parents || [])) {
      const pi = idx[parentKey];
      if (pi === undefined || pi >= j) continue; // parent not loaded or not above
      // Edge from j down to pi spans rows pi..j. It passes through row i if pi <= i <= j.
      if (pi <= i && i <= j) {
        passThroughLanes.add(lanes[j]);
        passThroughLanes.add(lanes[pi]);
      }
    }
  }

  let svg = `<svg width="${width}" height="${ROW_H}" xmlns="http://www.w3.org/2000/svg">`;

  // Vertical lines for lanes passing through this row.
  for (const lane of passThroughLanes) {
    const xc = LANE_W / 2 + lane * LANE_W + LANE_W / 2;
    svg += `<line x1="${xc}" y1="0" x2="${xc}" y2="${ROW_H}" stroke="${COLORS[lane % COLORS.length]}" stroke-width="2" opacity="0.6"/>`;
  }

  // Horizontal connector from this node to its parent(s).
  for (const parentKey of (row.parents || [])) {
    const pi = idx[parentKey];
    if (pi === undefined || pi >= i) continue; // parent not loaded or not above
    const x2 = LANE_W / 2 + lanes[pi] * LANE_W + LANE_W / 2;
    svg += `<line x1="${x1}" y1="${yMid}" x2="${x2}" y2="${yMid}" stroke="${color}" stroke-width="2" opacity="0.8"/>`;
    // Draw the dot at the parent's position too (so it's visible at its own row).
    svg += `<circle cx="${x2}" cy="${yMid}" r="${DOT_R}" fill="${COLORS[lanes[pi] % COLORS.length]}"/>`;
  }

  // The node dot.
  svg += `<circle cx="${x1}" cy="${yMid}" r="${DOT_R}" fill="${color}"/>`;
  svg += '</svg>';
  return svg;
}

/** Render all accumulated rows into a table with graph column + blocks. */
function renderRows() {
  // Apply the hide-submodules filter.
  const hideSubmodules = hideSubmodulesEl && hideSubmodulesEl.checked;
  const visible = hideSubmodules ? allRows.filter((r) => !r.is_submodule) : allRows;

  const lanes = computeLanes(visible);
  const maxLane = lanes.length ? Math.max(...lanes) : 0;
  const graphWidth = (maxLane + 1) * LANE_W + LANE_W;

  rowsEl.innerHTML = '';
  let lastGroup = null;

  visible.forEach((row, i) => {
    // Block separator spans both columns.
    if (row.group !== lastGroup) {
      lastGroup = row.group;
      const sep = document.createElement('div');
      sep.className = 'block-sep';
      sep.textContent = row.group.startsWith('repo:') ? 'Git · repo ' + row.group.slice(5)
        : row.group.startsWith('session:') ? 'Session ' + row.group.slice(8)
        : 'EditChain ops';
      rowsEl.appendChild(sep);
    }

    // Each row is a grid with two columns: graph | text.
    const div = document.createElement('div');
    div.className = 'row' + (row.git_oid ? ' git' : '');
    div.style.gridTemplateColumns = `${graphWidth}px minmax(0,1fr)`;

    // Graph cell.
    const graphCell = document.createElement('div');
    graphCell.className = 'graph-cell';
    graphCell.innerHTML = buildRowSvg(allRows, lanes, i);
    div.appendChild(graphCell);

    // Text cell.
    const textCell = document.createElement('div');
    textCell.className = 'text-cell';

    const summaryEl = document.createElement('div');
    summaryEl.className = 'summary';
    summaryEl.textContent = row.summary || '(no summary)';
    textCell.appendChild(summaryEl);

    const metaEl = document.createElement('div');
    metaEl.className = 'meta';
    metaEl.textContent = [row.git_oid ? 'git' : 'op', formatDate(row.timestamp_ms)].filter(Boolean).join(' · ');
    textCell.appendChild(metaEl);

    div.appendChild(textCell);

    div.addEventListener('click', () => inspect(row));
    rowsEl.appendChild(div);
  });
}

/** Load the next window of history. */
function loadMore() {
  if (pending || (total > 0 && offset >= total)) return;
  pending = true;
  send({ GetWindow: { offset, limit: PAGE, hide_submodules: hideSubmodulesEl && hideSubmodulesEl.checked } });
}

/** Reset to the full history view and reload from the top. */
function resetHistory() {
  allRows = [];
  nodeIndex = {};
  offset = 0;
  total = 0;
  pending = false;
  loadMore();
}

/** Inspect a node's details. */
function inspect(row) {
  if (row.git_oid) {
    send({ ResolveObject: { repository: row.repository, oid: row.git_oid } });
  } else if (row.op_id) {
    send({ GetNodeDetails: { op_id: row.op_id } });
  }
}

/** Render node details in the inspector pane. */
function renderDetails(details) {
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
  if (details.refs && details.refs.length) {
    const refsEl = document.createElement('div');
    refsEl.className = 'detail-meta';
    refsEl.textContent = 'refs: ' + details.refs.join(', ');
    detailEl.appendChild(refsEl);
  }
  if (details.changed_paths && details.changed_paths.length) {
    const pathsEl = document.createElement('div');
    pathsEl.className = 'detail-meta';
    pathsEl.textContent = 'paths: ' + details.changed_paths.join(', ');
    detailEl.appendChild(pathsEl);
  }
}

// Handle messages from the extension host.
window.addEventListener('message', (event) => {
  const msg = event.data;
  const r = unwrap(msg.body);

  if (msg.id === 'open') {
    if (r.ok) {
      statusEl.textContent = `Open: ${r.value.nodes} nodes, ${r.value.repos} repos`;
      allRows = [];
      nodeIndex = {};
      offset = 0;
      total = r.value.nodes;
      loadMore();
    } else {
      statusEl.textContent = `Error: ${r.error}`;
    }
  } else if (msg.id === 'ready') {
    loadMore();
  }

  // Response to a GetWindow request.
  if (r.ok && r.value && Array.isArray(r.value.rows)) {
    total = r.value.total;
    for (const row of r.value.rows) {
      allRows.push(row);
      nodeIndex[row.node_key] = allRows.length - 1;
    }
    offset += r.value.rows.length;
    renderRows();
    statusEl.textContent = `${allRows.length}/${total} nodes`;
    pending = false;
  }
  // Response to a Search request — render results as rows.
  else if (r.ok && Array.isArray(r.value)) {
    const results = r.value.map((c) => ({
      op_id: c.metadata.source === 'EditChain' ? c.metadata.op_id : null,
      git_oid: c.metadata.source === 'Git' ? c.metadata.op_id : null,
      repository: null,
      summary: c.text,
      timestamp_ms: c.metadata.timestamp_ms,
      group: c.metadata.source === 'Git' ? 'search:git' : 'search:editchain',
      node_key: c.metadata.source === 'Git' ? 'git:' + c.metadata.op_id : 'op:' + c.metadata.op_id,
      parents: [],
    }));
    allRows = results;
    nodeIndex = {};
    renderRows();
    statusEl.textContent = `${results.length} search results`;
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

// Re-fetch when the hide-submodules toggle changes (filtering is server-side).
if (hideSubmodulesEl) {
  hideSubmodulesEl.addEventListener('change', () => {
    allRows = [];
    nodeIndex = {};
    offset = 0;
    total = 0;
    pending = false;
    loadMore();
  });
}

// Infinite scroll.
rowsEl.addEventListener('scroll', () => {
  if (rowsEl.scrollTop + rowsEl.clientHeight >= rowsEl.scrollHeight - 40) {
    loadMore();
  }
});