// Text-only layout probe for the EditChain history webview.
//
// Exposes window.__editchainDebug so a text-only agent can inspect the rendered
// layout as numbers/text instead of images:
//
//   whenIdle()      -> Promise<RenderGeneration>  resolves when the UI is settled
//   dumpLayout()    -> LayoutDump                 full geometry + DOM tree
//   assertLayout()  -> AssertionResult            textual checks (pass/fail)
//   getMetrics()    -> RenderMetrics              render timing / DOM counts
//
// This file is harness-only. It is loaded by test/harness/index.html and is NOT
// part of the production webview.

(function () {
  'use strict';

  // --- helpers ---------------------------------------------------------------

  function box(el) {
    const r = el.getBoundingClientRect();
    return {
      x: Math.round(r.x * 100) / 100,
      y: Math.round(r.y * 100) / 100,
      w: Math.round(r.width * 100) / 100,
      h: Math.round(r.height * 100) / 100,
    };
  }

  function scrollDims(el) {
    return {
      scrollW: el.scrollWidth,
      scrollH: el.scrollHeight,
      clientW: el.clientWidth,
      clientH: el.clientHeight,
      scrollTop: el.scrollTop,
      scrollLeft: el.scrollLeft,
    };
  }

  function visible(el) {
    const cs = getComputedStyle(el);
    if (cs.display === 'none' || cs.visibility === 'hidden' || +cs.opacity === 0) {
      return false;
    }
    const r = el.getBoundingClientRect();
    return r.width > 0 && r.height > 0;
  }

  // Format a timestamp the same way the renderer does (media/main.js
  // `formatDate`), using explicit Intl options. Both sides run in the same
  // engine, so the comparison is locale/timezone-explicit and deterministic —
  // never a hardcoded string that only matches one host environment.
  function fmtDateExplicit(ms) {
    const d = new Date(ms);
    return d.toLocaleDateString(undefined, { year: 'numeric', month: 'short', day: 'numeric' }) +
      ' ' + d.toLocaleTimeString(undefined, { hour: '2-digit', minute: '2-digit' });
  }

  // --- contrast helpers (WCAG relative luminance) ----------------------------

  function parseRgb(color) {
    if (!color) return null;
    const m = /rgba?\(\s*([\d.]+)[,\s]+([\d.]+)[,\s]+([\d.]+)(?:[,\s/]+([\d.]+))?/.exec(color);
    if (m) {
      return [Number(m[1]), Number(m[2]), Number(m[3]), m[4] !== undefined ? Number(m[4]) : 1];
    }
    const hex = /^#([0-9a-f]{6})$/i.exec(color.trim());
    if (hex) {
      const n = parseInt(hex[1], 16);
      return [(n >> 16) & 255, (n >> 8) & 255, n & 255, 1];
    }
    return null;
  }

  function luminance(rgb) {
    const [r, g, b] = rgb.map((v) => {
      const s = v / 255;
      return s <= 0.03928 ? s / 12.92 : Math.pow((s + 0.055) / 1.055, 2.4);
    });
    return 0.2126 * r + 0.7152 * g + 0.0722 * b;
  }

  function contrastRatio(a, b) {
    const l1 = luminance(a);
    const l2 = luminance(b);
    const hi = Math.max(l1, l2);
    const lo = Math.min(l1, l2);
    return (hi + 0.05) / (lo + 0.05);
  }

  // Blend a text color toward the background by the element's opacity chain, so
  // intentionally dimmed cells (row-tool/row-dim/sub-op) are measured as they
  // actually render.
  function effectiveTextColor(textRgb, bgRgb, opacity) {
    if (opacity >= 0.999) return textRgb;
    return textRgb.map((v, i) => Math.round(v * opacity + bgRgb[i] * (1 - opacity)));
  }

  // Nearest opaque background behind an element (rows have no background; the
  // body carries var(--vscode-editor-background)).
  function effectiveBackground(el) {
    let node = el;
    while (node && node !== document.documentElement) {
      const bg = parseRgb(getComputedStyle(node).backgroundColor);
      if (bg && bg[3] > 0.01) return bg;
      node = node.parentElement;
    }
    const root = getComputedStyle(document.documentElement);
    const fallback = parseRgb(root.backgroundColor) ||
      parseRgb(root.getPropertyValue('--vscode-editor-background'));
    return fallback || [30, 30, 30];
  }

  // Total opacity applied to an element by its own + ancestors' opacity styles.
  function cumulativeOpacity(el, stopAt) {
    let opacity = 1;
    let node = el;
    while (node && node !== stopAt) {
      const o = parseFloat(getComputedStyle(node).opacity);
      if (!isNaN(o)) opacity *= o;
      node = node.parentElement;
    }
    return opacity;
  }

  // Run a search through the real renderer path: fill the search input, press
  // Enter, wait for the result list to settle, then click the first result and
  // verify it requests a JSON editor (navigation coherence). Captures the
  // openJson postMessage the renderer emits on click and reports which identity
  // it navigated with: a Git hit must click by (git_oid, repository) — never by
  // its synthetic index-only op_id.
  async function runSearch(query, timeoutMs) {
    const searchInput = document.getElementById('search');
    const captured = [];
    const origPost = window.vscode.postMessage.bind(window.vscode);
    window.vscode.postMessage = function (msg) {
      if (msg && msg.type === 'openJson') captured.push(msg);
      return origPost(msg);
    };
    searchInput.value = query;
    searchInput.dispatchEvent(new KeyboardEvent('keydown', { key: 'Enter', bubbles: true }));
    await whenIdle(timeoutMs || 5000);
    const resultRows = document.querySelectorAll('.row').length;
    const banner = document.querySelector('.search-banner');
    const bannerText = banner ? (banner.textContent || '').trim() : '';
    const firstRow = document.querySelector('.row');
    const firstRowChevron = !!(firstRow && firstRow.querySelector('.subop-chevron'));
    if (firstRow) firstRow.click();
    await whenIdle(timeoutMs || 5000);
    window.vscode.postMessage = origPost;
    const clicked = captured.length ? captured[0] : null;
    return {
      resultRows,
      bannerText,
      navigated: captured.length > 0,
      navigatedGit: !!(clicked && clicked.git_oid && !clicked.op_id),
      navigatedOp: !!(clicked && clicked.op_id && !clicked.git_oid),
      clickedIdentity: clicked
        ? { op_id: clicked.op_id || null, git_oid: clicked.git_oid || null,
            repository: clicked.repository !== undefined ? clicked.repository : null }
        : null,
      firstRowChevron,
      captured,
      dataReady: dataReady(),
    };
  }

  // --- readiness -------------------------------------------------------------

  // Track in-flight requests by hooking the bridge's dispatch. The renderer
  // sends requests via window.vscode.postMessage; we count outstanding ones.
  // In real VS Code, `window.vscode` is not exposed to the page, so the hook is
  // a no-op there; the renderer's own in-flight state (__editchainInFlightCount)
  // is consulted instead (see rendererInFlight), so real-VS-Code readiness is
  // driven by actual renderer state, not DOM/font stability alone.
  let inFlight = 0;
  if (window.vscode && typeof window.vscode.postMessage === 'function') {
    const origPost = window.vscode.postMessage;
    window.vscode.postMessage = function (msg) {
      // Count every service request (the renderer tags requests with an id);
      // openJson/log/status messages carry `type` and no body, so they are not
      // counted.
      if (msg && msg.body !== undefined) {
        inFlight++;
        // The bridge responds synchronously via dispatchEvent; count it down on
        // the next tick so the renderer has processed the response.
        setTimeout(() => { inFlight--; }, 0);
      }
      return origPost.call(this, msg);
    };
  }

  // Renderer-owned in-flight request count (main.js exposes it for the harness;
  // in real VS Code this is the ONLY reliable in-flight signal, since
  // window.vscode is not exposed to the page).
  function rendererInFlight() {
    return typeof window.__editchainInFlightCount === 'function'
      ? window.__editchainInFlightCount()
      : 0;
  }

  // Deterministic reversed-search race: two rapid searches must be
  // latest-query-wins. Search A ("commit") is HELD; while it is in flight,
  // search B ("feature") is issued and its (fast) response lands FIRST. B's
  // results must be shown, and releasing A's late response must NOT replace
  // them (both searches shared a view generation — without the search epoch
  // correlation, the first response to render bumps the generation and drops
  // B, leaving query B's banner over query A's results).
  async function runReversedSearchRace() {
    const out = { steps: [] };
    const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));
    const searchInput = document.getElementById('search');
    const enter = (q) => {
      searchInput.value = q;
      searchInput.dispatchEvent(new KeyboardEvent('keydown', { key: 'Enter', bubbles: true }));
    };
    // Hold the first Search response (query A = "commit").
    window.__editchainHoldSearch = {};
    enter('commit');
    const holdDeadline = Date.now() + 3000;
    while (!window.__editchainHoldSearch.release) {
      if (Date.now() > holdDeadline) throw new Error('search-race: search response was never held');
      await sleep(5);
    }
    const releaseA = window.__editchainHoldSearch.release;
    window.__editchainHoldSearch = null;
    // Issue query B ("feature") while A is in flight — B must win.
    enter('feature');
    // B's response renders synchronously, but query A is still HELD — its
    // outstanding request keeps the in-flight counters nonzero, so wait for
    // B's result list directly instead of whenIdle (which requires zero
    // in-flight requests and would time out until A is released).
    const renderDeadline = Date.now() + 3000;
    for (;;) {
      const banner = document.querySelector('.search-banner');
      const rowCount = document.querySelectorAll('.row').length;
      if (banner && rowCount === 2 && /feature/.test(banner.textContent || '')) break;
      if (Date.now() > renderDeadline) throw new Error('search-race: query B results never rendered');
      await sleep(5);
    }
    const banner = document.querySelector('.search-banner');
    const keys = Array.from(document.querySelectorAll('.row[data-key]'))
      .map((r) => r.getAttribute('data-key'));
    const beforeRelease = {
      banner: banner ? (banner.textContent || '').trim() : '',
      keys,
    };
    // Release query A's late response: it must be dropped, not rendered.
    releaseA();
    await sleep(100);
    const keysAfter = Array.from(document.querySelectorAll('.row[data-key]'))
      .map((r) => r.getAttribute('data-key'));
    const latestWins =
      keys.length === 2 &&
      keys.indexOf('git:m3') !== -1 &&
      keys.indexOf('git:f1') !== -1 &&
      keys.indexOf('git:m0') === -1 &&
      /feature/.test(beforeRelease.banner);
    out.steps.push({
      beforeRelease,
      keysAfter,
      latestWins,
    });
    return out;
  }

  let generation = 0;
  const observer = new MutationObserver(() => { generation++; });
  observer.observe(document.body, { childList: true, subtree: true, attributes: true });

  // True while any rendered row is still a placeholder (real row data not yet
  // delivered). Readiness must distinguish real rows from placeholders.
  function hasPlaceholders() {
    return document.querySelectorAll('.row-placeholder').length > 0;
  }

  // Correlated data-ready: the webview sets this only after processing a
  // terminal event (open error, GetWindow rows, search results). Idle must not
  // be declared while the initial window is still in flight.
  function dataReady() {
    return window.__editchainDataReady === true;
  }

  function whenIdle(timeoutMs) {
    timeoutMs = timeoutMs || 5000;
    const started = Date.now();
    return new Promise((resolve, reject) => {
      const check = () => {
        const settled =
          inFlight === 0 &&
          rendererInFlight() === 0 &&
          document.fonts && document.fonts.status === 'loaded';
        if (settled && !hasPlaceholders() && dataReady()) {
          // Require two stable animation frames.
          let stable = 0;
          const frames = () => {
            stable++;
            if (stable >= 2) {
              resolve({ generation, inFlight, elapsedMs: Date.now() - started });
            } else {
              requestAnimationFrame(frames);
            }
          };
          requestAnimationFrame(frames);
        } else if (Date.now() - started > timeoutMs) {
          reject(new Error('whenIdle timed out after ' + timeoutMs + 'ms'));
        } else {
          setTimeout(check, 25);
        }
      };
      check();
    });
  }

  // --- layout dump -----------------------------------------------------------

  // Collect geometry for a single element.
  function describeElement(el, depth) {
    const cs = getComputedStyle(el);
    const d = {
      tag: el.tagName.toLowerCase(),
      id: el.id || undefined,
      cls: el.className && typeof el.className === 'string' ? el.className : undefined,
      key: el.getAttribute && el.getAttribute('data-key') || undefined,
      row: el.getAttribute && el.getAttribute('data-row') || undefined,
      text: (el.textContent || '').trim().slice(0, 60) || undefined,
      box: box(el),
      visible: visible(el),
      scroll: scrollDims(el),
      style: {
        display: cs.display,
        position: cs.position,
        overflowX: cs.overflowX,
        overflowY: cs.overflowY,
        whiteSpace: cs.whiteSpace,
        textOverflow: cs.textOverflow,
        fontSize: cs.fontSize,
        lineHeight: cs.lineHeight,
        color: cs.color,
        backgroundColor: cs.backgroundColor,
        opacity: cs.opacity,
        zIndex: cs.zIndex,
        gridTemplateColumns: cs.gridTemplateColumns,
      },
    };
    if (depth > 0 && el.children && el.children.length) {
      d.children = [];
      for (const child of el.children) {
        d.children.push(describeElement(child, depth - 1));
      }
    }
    return d;
  }

  // Parse a renderer `d` attribute (`M`/`L`/`Q` commands, space-separated
  // numbers) into [{ cmd, args: [...] }], or null when malformed.
  function parsePathD(d) {
    if (typeof d !== 'string' || !d) return null;
    const tokens = d.trim().split(/\s+/);
    const out = [];
    let i = 0;
    while (i < tokens.length) {
      const t = tokens[i];
      const n = t === 'Q' ? 4 : (t === 'M' || t === 'L') ? 2 : -1;
      if (n < 0 || i + 1 + n > tokens.length) return null;
      const args = [];
      for (let k = 1; k <= n; k++) {
        const v = Number(tokens[i + k]);
        if (!Number.isFinite(v)) return null;
        args.push(v);
      }
      out.push({ cmd: t, args });
      i += n + 1;
    }
    return out;
  }

  function pathStart(cmds) {
    return cmds && cmds.length ? cmds[0].args : null;
  }

  function pathEnd(cmds) {
    return cmds && cmds.length ? cmds[cmds.length - 1].args.slice(-2) : null;
  }

  // Extract the inline `stroke:` value from a renderer style attribute.
  function parseStroke(style) {
    const m = /(?:^|;)\s*stroke\s*:\s*([^;]+)/.exec(String(style || ''));
    return m ? m[1].trim() : null;
  }

  // Read back one row's graph geometry (node dot, vertical halves, transition
  // halves) from its rendered SVG cell. Assumes at most one transition per row
  // (the fork and highLanes fixtures each carry exactly one).
  function collectRowGeometry(cells) {
    const byRow = new Map();
    cells.forEach((cell) => {
      const rowEl = cell.closest('.row');
      const row = rowEl ? +rowEl.getAttribute('data-row') : -1;
      const entry = byRow.get(row) || {
        dot: null,
        lines: [],
        src: null,
        dst: null,
        cellW: +cell.getAttribute('width'),
      };
      const dot = cell.querySelector('circle.graphDot');
      if (dot) {
        entry.dot = {
          x: +dot.getAttribute('cx'),
          y: +dot.getAttribute('cy'),
          fill: dot.getAttribute('fill'),
        };
      }
      cell.querySelectorAll('line.graphLine').forEach((l) => {
        entry.lines.push({
          x1: +l.getAttribute('x1'),
          y1: +l.getAttribute('y1'),
          x2: +l.getAttribute('x2'),
          y2: +l.getAttribute('y2'),
        });
      });
      cell.querySelectorAll('path.graphTransition').forEach((p) => {
        const half = {
          cmds: parsePathD(p.getAttribute('d')),
          stroke: parseStroke(p.getAttribute('style')),
        };
        if (p.classList.contains('graphTransitionSrc')) entry.src = half;
        else entry.dst = half;
      });
      byRow.set(row, entry);
    });
    return byRow;
  }

  // Lane x positions where a row's rendering connects to the TOP cell boundary
  // (y=0): generic top-half lines and boundary-anchored transition sources.
  function topBoundaryXs(g, ROW_H) {
    const xs = new Set();
    g.lines.forEach((l) => { if (Math.abs(l.y1) <= 0.01) xs.add(l.x1); });
    const s = g.src && g.src.cmds && pathStart(g.src.cmds);
    if (s && Math.abs(s[1]) <= 0.01) xs.add(s[0]);
    return xs;
  }

  // Lane x positions where a row's rendering connects to the BOTTOM cell
  // boundary (y=ROW_H): generic bottom-half lines and boundary-anchored
  // transition destinations.
  function bottomBoundaryXs(g, ROW_H) {
    const xs = new Set();
    g.lines.forEach((l) => { if (Math.abs(l.y2 - ROW_H) <= 0.01) xs.add(l.x2); });
    const e = g.dst && g.dst.cmds && pathEnd(g.dst.cmds);
    if (e && Math.abs(e[1] - ROW_H) <= 0.01) xs.add(e[0]);
    return xs;
  }

  // The anchors the renderer must use for a row's first transition, derived
  // from the cached row geometry (lane / above / below / transitions). Returns
  // null when the transition lacks either a connected source or destination — the
  // renderer must not draw it at all (it would be an open stub).
  function transitionAnchors(cached) {
    const lane = cached.lane;
    const above = cached.above || [];
    const below = cached.below || [];
    const trans = (cached.transitions || []).find((tr) => Array.isArray(tr) && tr.length >= 2);
    if (!trans) return null;
    const fromLane = trans[0];
    const toLane = trans[1];
    const startAtDot = lane === fromLane;
    const startAtBoundary = !startAtDot && above.indexOf(fromLane) !== -1;
    const endAtBoundary = below.indexOf(toLane) !== -1;
    const endAtDot = !endAtBoundary && lane === toLane;
    if (!startAtDot && !startAtBoundary) return null;
    if (!endAtBoundary && !endAtDot) return null;
    return { fromLane, toLane, startAtDot, startAtBoundary, endAtDot, endAtBoundary };
  }

  // Collect per-row graph geometry (dots, vertical line segments, and rounded
  // transition paths). Each row carries its own small SVG cell, so we
  // aggregate across all rendered rows.
  function describeSvg() {
    const cells = document.querySelectorAll('.graph-cell svg.graphCell');
    if (!cells.length) return { present: false };
    const out = { present: true, cells: cells.length, dots: [], lines: [], transitions: [] };
    cells.forEach((cellSvg) => {
      const rowEl = cellSvg.closest('.row');
      const absIdx = rowEl ? rowEl.getAttribute('data-row') : null;
      for (const dot of cellSvg.querySelectorAll('circle.graphDot')) {
        out.dots.push({
          row: absIdx,
          cx: +dot.getAttribute('cx'),
          cy: +dot.getAttribute('cy'),
          r: +dot.getAttribute('r'),
          fill: dot.getAttribute('fill'),
        });
      }
      for (const line of cellSvg.querySelectorAll('line.graphLine')) {
        out.lines.push({
          row: absIdx,
          x1: +line.getAttribute('x1'),
          y1: +line.getAttribute('y1'),
          x2: +line.getAttribute('x2'),
          y2: +line.getAttribute('y2'),
        });
      }
      for (const p of cellSvg.querySelectorAll('path.graphTransition')) {
        out.transitions.push({
          row: absIdx,
          side: p.classList.contains('graphTransitionSrc') ? 'src' : 'dst',
          d: p.getAttribute('d'),
          stroke: parseStroke(p.getAttribute('style')),
        });
      }
    });
    // Each drawn transition renders exactly one source + one destination half
    // consecutively per row; group them so dumps report rendered edge counts.
    const edges = [];
    for (let i = 0; i + 1 < out.transitions.length; i += 2) {
      const a = out.transitions[i];
      const b = out.transitions[i + 1];
      if (a.row !== b.row || a.side === b.side) continue;
      edges.push({
        row: a.row,
        src: a.side === 'src' ? a.d : b.d,
        dst: a.side === 'src' ? b.d : a.d,
        srcStroke: a.side === 'src' ? a.stroke : b.stroke,
        dstStroke: a.side === 'src' ? b.stroke : a.stroke,
      });
    }
    out.edges = edges;
    return out;
  }

  function dumpLayout(scope) {
    scope = scope || '#rows';
    const rootEl = document.querySelector(scope);
    const rowsEl = document.getElementById('rows');
    const layoutEl = document.getElementById('layout');
    const graphState = typeof window.__editchainGraphState === 'function'
      ? window.__editchainGraphState()
      : null;
    return {
      viewport: { w: window.innerWidth, h: window.innerHeight, dpr: window.devicePixelRatio },
      state: {
        scenario: window.__editchainScenarioName || 'unknown',
        status: inFlight === 0 ? 'idle' : 'busy',
        generation,
        dataReady: dataReady(),
        placeholders: hasPlaceholders(),
        layoutReady: graphState ? graphState.layoutReady : undefined,
        rowsRendered: document.querySelectorAll('.row').length,
        totalRowsLoaded: window.__editchainLoadedRows || undefined,
      },
      layoutBoxes: {
        rowsEl: rowsEl ? box(rowsEl) : null,
        layoutEl: layoutEl ? box(layoutEl) : null,
        detailVisible: layoutEl ? layoutEl.classList.contains('has-detail') : false,
      },
      treeRootExists: !!rootEl,
      treeRootBox: rootEl ? box(rootEl) : null,
      treeRootScroll: rootEl ? scrollDims(rootEl) : null,
      svg: describeSvg(),
    };
  }

  // --- assertions ------------------------------------------------------------

  // A small set of textual checks. Each returns { name, pass, detail }.
  function runChecks() {
    const checks = [];
    const rowsEl = document.getElementById('rows');
    const wrapEl = rowsEl && rowsEl.querySelector('.table-wrap');
    // The single sticky header is a direct child of #rows (it must NOT live
    // inside .table-wrap, or it would scroll with content and appear mid-table).
    const headerEl = rowsEl && rowsEl.querySelector('.tbl-header');
    // A full-pane status message (loading, open error, zero search results)
    // replaces the table — table checks don't apply then.
    const viewMessage = rowsEl && rowsEl.querySelector('.view-message');

    // Check 1: header present.
    if (viewMessage) {
      checks.push({
        name: 'HEADER_PRESENT',
        pass: true,
        detail: 'skipped — full-pane message shown: "' +
          (viewMessage.textContent || '').trim().slice(0, 40) + '"',
      });
    } else {
      checks.push({
        name: 'HEADER_PRESENT',
        pass: !!headerEl,
        detail: headerEl ? 'header rendered' : 'no .tbl-header found',
      });
    }

    // Check 2: no horizontal overflow on #rows (content should not spill).
    // Resize handles are intentionally positioned at column boundaries and may
    // extend past the viewport edge; exclude them from this check.
    if (rowsEl) {
      const contentOverflow = Array.from(rowsEl.querySelectorAll('*')).some((el) => {
        if (el.classList && el.classList.contains('col-resize-handle')) return false;
        const r = el.getBoundingClientRect();
        return r.right > rowsEl.getBoundingClientRect().right + 1;
      });
      checks.push({
        name: 'NO_HORIZONTAL_OVERFLOW',
        pass: !contentOverflow,
        detail:
          'scrollW=' + rowsEl.scrollWidth + ' clientW=' + rowsEl.clientWidth +
          ' delta=' + (rowsEl.scrollWidth - rowsEl.clientWidth) +
          ' contentSpill=' + contentOverflow,
      });
    }

    // Check 3: every rendered row has a matching graph dot centered on its lane.
    // Rows carry an ABSOLUTE `data-row` index (the viewport renders a slice of
    // the full history), so we match dots by that absolute index rather than by
    // contiguous position.
    if (wrapEl) {
      const rowEls = wrapEl.querySelectorAll('.row');
      let dotsOk = true;
      let firstFail = null;
      rowEls.forEach((row) => {
        // Sub-op rows intentionally draw NO dot (they are not graph nodes), so
        // skip them — only top-level rows must have a centered dot.
        if (row.classList.contains('row-subop')) return;
        const absIdx = row.getAttribute('data-row');
        const cellSvg = row.querySelector('.graph-cell svg.graphCell');
        const dot = cellSvg && cellSvg.querySelector('circle.graphDot');
        if (!dot) { dotsOk = false; firstFail = firstFail || { rowIdx:absIdx, reason:'no dot' }; return; }
        const rowBox = row.getBoundingClientRect();
        // dot cy is relative to the cell svg, which sits at the row's top.
        const cellTop = cellSvg.getBoundingClientRect().top;
        const dotCy = cellTop + (+dot.getAttribute('cy'));
        const rowCenterY = rowBox.top + rowBox.height / 2;
        const deltaY = Math.abs(dotCy - rowCenterY);
        if (deltaY > 1.5) { dotsOk = false; firstFail = firstFail || { rowIdx:absIdx, deltaY }; }
      });
      checks.push({
        name:'DOT_ROW_ALIGNMENT',
        pass:dotsOk,
        detail:dotsOk ? 'all dots centered on their rows'
          : 'first fail=' + JSON.stringify(firstFail),
      });
    }

    // Check 4: grid columns share boundaries between header and rows. Rows also
    // contain absolutely-positioned non-grid children (e.g. .group-label
    // chips), so compare the real grid cells by class rather than by child
    // index — indexing children counts the chip and misreports alignment.
    if (headerEl && wrapEl) {
      const headerCells = headerEl.querySelectorAll('.th');
      const firstRow = wrapEl.querySelector('.row');
      let colsOk = true;
      let firstFailCol = null;
      if (firstRow) {
        const colClasses = ['graph-cell', 'text-cell', 'date-cell', 'author-cell', 'commit-cell'];
        headerCells.forEach((th, i) => {
          const rc = firstRow.querySelector('.' + colClasses[i]);
          if (!rc) return;
          const deltaL = Math.abs(th.getBoundingClientRect().left - rc.getBoundingClientRect().left);
          if (deltaL > 1.5) { colsOk=false; firstFailCol=firstFailCol||{col:i,deltaL}; }
        });
      }
      checks.push({
        name:'COLUMN_ALIGNMENT',
        pass:colsOk,
        detail:(colsOk?'columns aligned':'first fail='+JSON.stringify(firstFailCol)),
      });
    }

    // Check 5: human message turns render bold; all other rows normal weight;
    // no extra left padding on non-human (agent) rows.
    if (wrapEl && window.__editchainScenarioName === 'mixed') {
      const humanRow = wrapEl.querySelector('.row.row-human .summary');
      const agentRow = wrapEl.querySelector('.row[data-key^="node:s1:a"] .summary');
      const humanBold = humanRow ? getComputedStyle(humanRow).fontWeight === '700' : false;
      const agentNormal = agentRow ? getComputedStyle(agentRow).fontWeight === '400' : false;
      // No extra padding: agent text-cell left padding equals the base 8px.
      const agentCell = wrapEl.querySelector('.row[data-key^="node:s1:a"] .text-cell');
      const agentPad = agentCell ? parseFloat(getComputedStyle(agentCell).paddingLeft) : null;
      const noExtraPad = agentPad === null || agentPad <= 8.5;
      checks.push({
        name:'HUMAN_BOLD_NO_AGENT_PAD',
        pass:humanBold && agentNormal && noExtraPad,
        detail:'humanBold=' + humanBold + ' agentNormal=' + agentNormal +
          ' agentPad=' + agentPad,
      });
    } else if (wrapEl) {
      checks.push({
        name:'HUMAN_BOLD_NO_AGENT_PAD',
        pass:true,
        detail:'skipped — scenario lacks the human/agent rows this check needs',
      });
    }

    // Check 5b: rendered dates come from the fixture's deterministic timestamp
    // and match an explicitly-computed expectation (same Intl options as the
    // renderer, so the assertion is timezone/locale-explicit rather than
    // hardcoded to the host environment).
    if (wrapEl) {
      const firstRow = wrapEl.querySelector('.row:not(.row-placeholder)');
      const absIdx = firstRow ? parseInt(firstRow.getAttribute('data-row'), 10) : -1;
      const row = absIdx >= 0 && window.__editchainRowAt
        ? window.__editchainRowAt(absIdx)
        : null;
      const dateCell = firstRow ? firstRow.querySelector('.date-cell') : null;
      const expected = row && row.timestamp_ms
        ? fmtDateExplicit(row.timestamp_ms)
        : '';
      const rendered = dateCell ? (dateCell.textContent || '').trim() : '';
      const datesOk = !!row && !!dateCell && !!row.timestamp_ms &&
        rendered === expected && rendered !== '';
      checks.push({
        name: 'DATE_EXPLICIT_LOCALE',
        pass: datesOk || !row || !row.timestamp_ms,
        detail: datesOk || !row || !row.timestamp_ms
          ? (row && row.timestamp_ms
              ? 'date "' + rendered + '" matches explicit Intl expectation'
              : 'no dated row in this scenario (skipped)')
          : 'rendered "' + rendered + '" != expected "' + expected + '"',
      });
    }

    // Check 5c: the controls bar must fit its container (no clipping on narrow
    // panels — wrapping is allowed, overflow is not).
    const controlsEl = document.getElementById('controls');
    if (controlsEl) {
      const fits = controlsEl.scrollWidth <= controlsEl.clientWidth + 1;
      checks.push({
        name: 'CONTROLS_FIT',
        pass: fits,
        detail: 'scrollW=' + controlsEl.scrollWidth + ' clientW=' +
          controlsEl.clientWidth +
          (fits ? '' : ' — controls clipped'),
      });
    }

    // Check 5d: the graph column must stay visible (never collapsed or hidden),
    // even at narrow widths. Skipped when a full-pane message replaces the
    // table (empty chain / open error / zero search results).
    if (rowsEl && !viewMessage) {
      const graphCell = rowsEl.querySelector('.graph-cell');
      const graphW = graphCell ? graphCell.getBoundingClientRect().width : 0;
      checks.push({
        name: 'GRAPH_VISIBLE',
        pass: !!graphCell && graphW > 0,
        detail: graphCell
          ? 'graph column visible, width=' + graphW + 'px'
          : 'no .graph-cell rendered',
      });
    } else if (viewMessage) {
      checks.push({
        name: 'GRAPH_VISIBLE',
        pass: true,
        detail: 'skipped — full-pane message shown',
      });
    }

    // Check 5e (error scenario only): the open error must be visible as an
    // explicit full-pane message, not a silent blank panel.
    if (window.__editchainScenarioName === 'error') {
      const errMsg = rowsEl && rowsEl.querySelector('.view-message.error');
      checks.push({
        name: 'OPEN_ERROR_VISIBLE',
        pass: !!errMsg && /Failed to open history/.test(errMsg.textContent || ''),
        detail: errMsg
          ? 'error message shown: "' + (errMsg.textContent || '').trim().slice(0, 60) + '"'
          : 'no visible open error',
      });
    }

    // Check 5e2 (warned scenario only): Open `warnings`/`diagnostics` must be
    // surfaced as a non-blocking banner WHILE rows still render — data
    // integrity issues are never silently discarded.
    if (window.__editchainScenarioName === 'warned') {
      const banner = rowsEl && rowsEl.querySelector('.open-warning');
      const bannerText = banner ? (banner.textContent || '').trim() : '';
      const rowsRendered = document.querySelectorAll('.row:not(.row-placeholder)').length;
      const warned = banner && /missing/.test(bannerText) && rowsRendered > 0;
      checks.push({
        name: 'OPEN_WARNINGS_VISIBLE',
        pass: warned,
        detail: warned
          ? 'warning banner shown ("' + bannerText.slice(0, 80) + '") with ' + rowsRendered + ' rows rendered'
          : 'banner=' + (banner ? 'present' : 'missing') + ' rowsRendered=' + rowsRendered,
      });
    }

    // Check 5f: every data column must be visible with a nonzero width and sit
    // inside the rows container (no clipping, no collapsed tracks) — at any
    // viewport width, including the narrow ~617px webview. This is the
    // screenshot acceptance: Content/Date/Author/Commit must actually render.
    if (wrapEl && !viewMessage) {
      const firstRow = wrapEl.querySelector('.row:not(.row-placeholder)');
      if (firstRow) {
        const rowsBox = rowsEl.getBoundingClientRect();
        const cols = [
          { name: 'content', cls: 'text-cell' },
          { name: 'date', cls: 'date-cell' },
          { name: 'author', cls: 'author-cell' },
          { name: 'commit', cls: 'commit-cell' },
        ];
        const geo = {};
        let geoOk = true;
        let firstBad = null;
        for (const { name, cls } of cols) {
          const cell = firstRow.querySelector('.' + cls);
          const r = cell ? cell.getBoundingClientRect() : null;
          const w = r ? r.width : 0;
          geo[name] = Math.round(w * 100) / 100;
          if (!r || w <= 0.5 || r.right > rowsBox.right + 1 || r.left < rowsBox.left - 1) {
            geoOk = false;
            firstBad = firstBad || { col: name, w: Math.round(w * 100) / 100 };
          }
        }
        checks.push({
          name: 'CELL_GEOMETRY',
          pass: geoOk,
          detail: geoOk
            ? 'visible widths=' + JSON.stringify(geo) + 'px'
            : 'first bad=' + JSON.stringify(firstBad),
        });
      }
    }

    // Check 5g: readable contrast for Content/Date/Author/Commit text. Normal
    // rows must meet ~WCAG AA for large text (3.0); intentionally dimmed rows
    // (tool/sub-op) must stay visibly above near-black (1.8). Measured with the
    // effective (opacity-blended) text color against the real background, so a
    // "black/faint" capture is caught numerically, not by eyeballing pixels.
    if (wrapEl && !viewMessage) {
      const firstRow = wrapEl.querySelector('.row:not(.row-placeholder)');
      if (firstRow) {
        const bg = effectiveBackground(firstRow);
        const cells = [
          { sel: '.summary', name: 'content' },
          { sel: '.date-cell', name: 'date' },
          { sel: '.author-cell', name: 'author' },
          { sel: '.commit-cell', name: 'commit' },
        ];
        let contrastOk = true;
        const ratios = {};
        let firstBad = null;
        for (const { sel, name } of cells) {
          const el = firstRow.querySelector(sel);
          if (!el) continue;
          const text = parseRgb(getComputedStyle(el).color);
          if (!text) continue;
          const opacity = cumulativeOpacity(el, firstRow);
          const eff = effectiveTextColor(text, bg, opacity);
          const ratio = contrastRatio(eff, bg);
          ratios[name] = { ratio: Math.round(ratio * 100) / 100, opacity: Math.round(opacity * 100) / 100 };
          const threshold = opacity < 0.99 ? 1.8 : 3.0;
          if (ratio < threshold) {
            contrastOk = false;
            firstBad = firstBad || { name, ratio: Math.round(ratio * 100) / 100, opacity };
          }
        }
        checks.push({
          name: 'CONTRAST_READABLE',
          pass: contrastOk,
          detail: contrastOk
            ? 'ratios=' + JSON.stringify(ratios) + ' on bg=rgb(' + bg.join(',') + ')'
            : 'first bad=' + JSON.stringify(firstBad),
        });
      }
    }

    // Check 5h: the content column must be genuinely readable (not a squeezed
    // sliver). This is a screenshot-level acceptance, not just nonzero width.
    if (wrapEl && !viewMessage) {
      const firstRow = wrapEl.querySelector('.row:not(.row-placeholder)');
      const contentCell = firstRow && firstRow.querySelector('.text-cell');
      const contentW = contentCell ? contentCell.getBoundingClientRect().width : 0;
      checks.push({
        name: 'CONTENT_READABLE_WIDTH',
        pass: contentW >= 120,
        detail: contentW >= 120
          ? 'content column ' + Math.round(contentW) + 'px (>= 120px readable)'
          : 'content column only ' + Math.round(contentW) + 'px (< 120px)',
      });
    }

    // Check 5i: the graph column must not dominate the table. With many lanes
    // the lane X positions compress into the capped region instead.
    if (rowsEl && !viewMessage) {
      const graphCell = rowsEl.querySelector('.graph-cell');
      const graphW = graphCell ? graphCell.getBoundingClientRect().width : 0;
      const maxW = rowsEl.clientWidth * 0.5 + 2;
      checks.push({
        name: 'GRAPH_MAX_FRACTION',
        pass: graphW <= maxW,
        detail: graphW <= maxW
          ? 'graph ' + Math.round(graphW) + 'px <= ' + Math.round(maxW) + 'px (50% cap)'
          : 'graph ' + Math.round(graphW) + 'px exceeds ' + Math.round(maxW) + 'px cap',
      });
    }

    // Check 5j: header cells must not overlap — neither their boxes nor their
    // text. A squeezed track ellipsizes (scrollWidth > clientWidth is allowed
    // ONLY when the browser clips with text-overflow: ellipsis); visible text
    // bleeding into the next column fails.
    if (headerEl && !viewMessage) {
      const ths = Array.from(headerEl.querySelectorAll('.th'));
      let boxesOk = true;
      let textOk = true;
      let firstBad = null;
      for (let i = 0; i < ths.length; i++) {
        const r = ths[i].getBoundingClientRect();
        if (i > 0) {
          const prev = ths[i - 1].getBoundingClientRect();
          if (r.left < prev.right - 0.5) {
            boxesOk = false;
            firstBad = firstBad || { kind: 'overlap', i, left: r.left, prevRight: prev.right };
          }
        }
        const cs = getComputedStyle(ths[i]);
        if (ths[i].scrollWidth > ths[i].clientWidth + 1 && cs.textOverflow !== 'ellipsis') {
          textOk = false;
          firstBad = firstBad || { kind: 'text-spill', i, label: (ths[i].textContent || '').trim().slice(0, 12) };
        }
      }
      checks.push({
        name: 'HEADER_NO_OVERLAP',
        pass: boxesOk && textOk,
        detail: (boxesOk && textOk)
          ? 'header cells non-overlapping; labels fit or ellipsize'
          : 'first bad=' + JSON.stringify(firstBad),
      });
    }

    // Check 6: every rendered row is exactly ROW_H tall (uniform grid), even
    // when sub-op rows are present. Virtual scroll depends on this.
    if (wrapEl) {
      const rowEls = wrapEl.querySelectorAll('.row');
      let uniform = true;
      let firstBad = null;
      rowEls.forEach((row) => {
        const h = row.getBoundingClientRect().height;
        if (Math.abs(h - 34) > 0.5) { uniform = false; firstBad = firstBad || { key: row.getAttribute('data-key'), h }; }
      });
      checks.push({
        name:'UNIFORM_ROW_HEIGHT',
        pass:uniform,
        detail:uniform ? 'all rows exactly ROW_H' : 'first bad=' + JSON.stringify(firstBad),
      });
    }

    // Check 7: expanded sub-op rows render with a Codicon and are indented under
    // their parent (only meaningful in the combined scenario).
    if (wrapEl) {
      const subopRows = wrapEl.querySelectorAll('.row.row-subop');
      if (subopRows.length) {
        let iconsOk = true;
        let indentOk = true;
        subopRows.forEach((r) => {
          if (!r.querySelector('.subop-icon')) iconsOk = false;
          const pad = parseFloat(getComputedStyle(r.querySelector('.text-cell')).paddingLeft);
          if (!(pad >= 24)) indentOk = false;
        });
        checks.push({
          name:'SUBOP_ICON_INDENT',
          pass:iconsOk && indentOk,
          detail:'subopRows=' + subopRows.length + ' iconsOk=' + iconsOk + ' indentOk=' + indentOk,
        });
      } else {
        checks.push({
          name:'SUBOP_ICON_INDENT',
          pass:true,
          detail:'no sub-op rows in this scenario (skipped)',
        });
      }
    }

    // Check 8 (fork / subagent-reconnect scenario only): the graph must render
    // distinct lanes for the two fork branches AND rounded cross-lane
    // transitions whose endpoints are ALL connected — every transition anchor
    // must be either this row's node dot or a cell boundary the adjacent row's
    // geometry continues at the same lane x. The production direction is
    // (child_lane, parent_lane), so row 0's reconnect transition runs 0 -> 1
    // (completion lane -> subagent lane) and the fork jogs at rows 2/3 run
    // 1 -> 0. Assertions:
    //   - dots occupy at least two distinct x positions (two lanes);
    //   - each transition row renders exactly two exact path halves sharing the
    //     geometric midpoint seam (identical formatted coordinates);
    //   - the source half begins at the row's own dot when `lane === fromLane`
    //     (row 0's reconnect starts AT the lane-0 dot — explicitly no source
    //     stub: it never begins at y=0 and row 0 keeps no dangling lane-0
    //     half); otherwise it begins at y=0 on the from-lane ONLY when `above`
    //     lists that lane, and the row above must continue that boundary at the
    //     same x;
    //   - the destination half ends at y=ROW_H on the to-lane only when
    //     `below` lists it (and the row below continues that boundary at the
    //     same x), or at the row's own dot when `lane === toLane`;
    //   - each half is stroked in its lane's colour (sharp categorical handoff
    //     at the seam);
    //   - quadratic corner controls sit on the lane centres at the row
    //     midpoint for boundary-anchored sides (rounded onto the row midpoint);
    //     dot-anchored sides have no vertical run and stay straight along the
    //     row midpoint;
    //   - no hard horizontal `graphLine` remains (the old three-line jog is
    //     gone).
    if (wrapEl && window.__editchainScenarioName === 'fork') {
      const cells = wrapEl.querySelectorAll('.graph-cell svg.graphCell');
      const ROW_H = 34;
      const MID_Y = ROW_H / 2;
      const near = (a, b) => Math.abs(a - b) <= 0.01;
      const geometry = collectRowGeometry(cells);
      const dotAt = (rowIdx) => {
        const g = geometry.get(rowIdx);
        return g && g.dot ? g.dot : null;
      };
      // Distinct dot x-centres == distinct lanes.
      const dotXs = new Set(Array.from(geometry.values())
        .map((g) => g.dot && g.dot.x)
        .filter((x) => x !== null));
      checks.push({
        name:'FORK_DISTINCT_LANES',
        pass:dotXs.size >= 2,
        detail:'distinct lanes (x in px) = ' + JSON.stringify(Array.from(dotXs)),
      });
      // Per-row transition validation. The completion lane (lane 0) is row 0's
      // dot, the subagent lane (lane 1) is row 1's dot.
      const lane0 = dotAt(0);
      const lane1 = dotAt(1);
      const transitionRows = [0, 2, 3];
      const transitionProblems = [];
      // Lane x / fill lookups from the rendered dots (each lane's dot x is
      // uniform across its rows).
      const laneXs = new Map();
      const laneFills = new Map();
      for (const [rowIdx, g] of geometry) {
        const cached = window.__editchainRowAt ? window.__editchainRowAt(rowIdx) : null;
        if (cached && g.dot) {
          laneXs.set(cached.lane, g.dot.x);
          laneFills.set(cached.lane, g.dot.fill);
        }
      }
      if (!lane0 || !lane1) {
        transitionProblems.push('missing lane dots for transition assertions');
      } else {
        for (const row of transitionRows) {
          const g = geometry.get(row);
          const cached = window.__editchainRowAt ? window.__editchainRowAt(row) : null;
          if (!g || !cached) {
            transitionProblems.push('row ' + row + ' missing rendered or cached geometry');
            continue;
          }
          const t = { src: g.src, dst: g.dst };
          if (!t || !t.src || !t.dst || !t.src.cmds || !t.dst.cmds) {
            transitionProblems.push('row ' + row + ' missing transition halves');
            continue;
          }
          const anchors = transitionAnchors(cached);
          if (!anchors) {
            transitionProblems.push('row ' + row + ' fixture transition must have connected anchors');
            continue;
          }
          const { fromLane, toLane } = anchors;
          const fromX = laneXs.get(fromLane);
          const toX = laneXs.get(toLane);
          const srcColour = laneFills.get(fromLane);
          const dstColour = laneFills.get(toLane);
          if (fromX === undefined || toX === undefined ||
              srcColour === undefined || dstColour === undefined) {
            transitionProblems.push('row ' + row + ' missing lane x/fill references');
            continue;
          }
          const seamX = (fromX + toX) / 2;
          const srcStart = pathStart(t.src.cmds);
          const srcEnd = pathEnd(t.src.cmds);
          const dstStart = pathStart(t.dst.cmds);
          const dstEnd = pathEnd(t.dst.cmds);
          // Source anchor: the row's own dot, or the top boundary ONLY when
          // `above` lists the from-lane (the path owns that top half).
          const srcAnchor = anchors.startAtDot
            ? [fromX, MID_Y]
            : anchors.startAtBoundary ? [fromX, 0] : null;
          if (!srcAnchor) {
            transitionProblems.push('row ' + row + ' transition has no connected source anchor');
          } else if (!srcStart || !near(srcStart[0], srcAnchor[0]) || !near(srcStart[1], srcAnchor[1])) {
            transitionProblems.push('row ' + row + ' src must begin at ' + JSON.stringify(srcAnchor));
          } else if (anchors.startAtBoundary) {
            const prev = geometry.get(row - 1);
            if (!prev || !bottomBoundaryXs(prev, ROW_H).has(fromX)) {
              transitionProblems.push('row ' + row + ' src top boundary at x=' + fromX +
                ' is not continued by row ' + (row - 1) + '\'s bottom geometry');
            }
          }
          // Destination anchor: the bottom boundary ONLY when `below` lists the
          // to-lane (the path owns that bottom half), else the row's own dot.
          const dstAnchor = anchors.endAtBoundary
            ? [toX, ROW_H]
            : anchors.endAtDot ? [toX, MID_Y] : null;
          if (!dstAnchor) {
            transitionProblems.push('row ' + row + ' transition has no connected destination anchor');
          } else if (!dstEnd || !near(dstEnd[0], dstAnchor[0]) || !near(dstEnd[1], dstAnchor[1])) {
            transitionProblems.push('row ' + row + ' dst must end at ' + JSON.stringify(dstAnchor));
          } else if (anchors.endAtBoundary) {
            const next = geometry.get(row + 1);
            if (!next || !topBoundaryXs(next, ROW_H).has(toX)) {
              transitionProblems.push('row ' + row + ' dst bottom boundary at x=' + toX +
                ' is not continued by row ' + (row + 1) + '\'s top geometry');
            }
          }
          const sharedSeam = srcEnd && dstStart &&
            srcEnd[0] === dstStart[0] && srcEnd[1] === dstStart[1] &&
            near(srcEnd[0], seamX) && near(srcEnd[1], MID_Y);
          if (!sharedSeam) {
            transitionProblems.push('row ' + row + ' halves must share the exact midpoint seam');
          }
          const srcQ = t.src.cmds.find((c) => c.cmd === 'Q');
          const dstQ = t.dst.cmds.find((c) => c.cmd === 'Q');
          // Boundary-anchored sides round onto the row midpoint (quadratic
          // control on the lane centre); dot-anchored sides have no vertical
          // run and must stay straight along the row midpoint.
          const srcElbowOk = anchors.startAtBoundary
            ? (!!srcQ && near(srcQ.args[0], fromX) && near(srcQ.args[1], MID_Y))
            : !srcQ;
          const dstElbowOk = anchors.endAtBoundary
            ? (!!dstQ && near(dstQ.args[0], toX) && near(dstQ.args[1], MID_Y))
            : !dstQ;
          if (!srcElbowOk || !dstElbowOk) {
            transitionProblems.push('row ' + row + ' elbows must match their anchors');
          }
          if (t.src.stroke !== srcColour || t.dst.stroke !== dstColour) {
            transitionProblems.push('row ' + row + ' colour handoff mismatch');
          }
        }
      }
      // Explicit stub check for row 0: the reconnect begins at the lane-0 dot,
      // never at y=0, and row 0 keeps no source-lane (lane 0) vertical half —
      // no top line into the dot and no bottom line below it.
      const row0 = geometry.get(0);
      if (row0 && row0.src && row0.src.cmds && row0.src.cmds.length) {
        const s = pathStart(row0.src.cmds);
        if (!s || !near(s[0], lane0.x) || !near(s[1], MID_Y)) {
          transitionProblems.push('row 0 reconnect must start at the lane-0 dot (no source top stub)');
        }
      } else {
        transitionProblems.push('row 0 must render its reconnect transition');
      }
      if (row0 && lane0) {
        const lane0Halves = row0.lines.filter((l) => near(l.x1, lane0.x) && near(l.x2, lane0.x));
        if (lane0Halves.length) {
          transitionProblems.push('row 0 must keep no source-lane half (open stub)');
        }
      }
      // The old hard three-line jog rendered a horizontal connector; the
      // rounded transition replaces it, so every remaining graphLine must be
      // a vertical segment.
      Array.from(geometry.values()).forEach((g) => {
        g.lines.forEach((l) => {
          if (Math.abs(l.x1 - l.x2) > 0.5) {
            transitionProblems.push('hard horizontal connector still present');
          }
        });
      });
      checks.push({
        name:'FORK_ROUNDED_TRANSITIONS',
        pass:transitionProblems.length === 0,
        detail: transitionProblems.length === 0
          ? '3 connected transitions: dot/boundary anchors, exact shared seams, per-lane colours'
          : transitionProblems.join('; '),
      });

      // Check 8c: the service's structural relationship kinds must render as
      // compact badges on the rows that start/return/fork branches — the
      // completion result row carries "return" (ReconnectsTo), the subagent's
      // first op row carries "subagent" (SubagentOf) and "fork" (ForkOf) —
      // and every badge must correspond to an ACTUAL drawn parent edge:
      //   - the badge's parent key is one of that row's final parents as
      //     delivered to the renderer (read back through __editchainRowAt);
      //   - the fixture's drawn edge list contains (child -> parent) for it;
      //   - no unrelated rows are badged (the badged row set is EXACTLY the
      //     expected set, with the expected badge count);
      //   - badges never inherit the tool/dim row's opacity dimming (the
      //     completion-result row is a tool-kind system row, so this verifies
      //     the .row-has-badges opacity override end-to-end).
      // Badges are provider-neutral (no raw provider JSON in the webview).
      const expectedBadges = [
        { key: 'node:f:0', cls: 'rel-reconnect', kind: 'reconnect', parent: 'node:f:1' },
        { key: 'node:f:2', cls: 'rel-subagent', kind: 'subagent', parent: 'node:f:3' },
        { key: 'node:f:2', cls: 'rel-fork', kind: 'fork', parent: 'node:f:4' },
      ];
      const problems = [];
      const badgeEls = Array.from(wrapEl.querySelectorAll('.rel-badge'));
      const badgeRows = new Set(Array.from(wrapEl.querySelectorAll('.row')).filter((r) =>
        r.querySelector('.rel-badge')).map((r) => r.getAttribute('data-key')));
      const expectedBadgeRows = new Set(expectedBadges.map((b) => b.key));
      // Ground truth for the drawn parent edges: use the fixture's explicit
      // GetLayout edge list, not the row parent metadata under test.
      const drawnEdges = new Set();
      const fixtureEdges = (window.__editchainFixture && window.__editchainFixture.edges) || [];
      for (const edge of fixtureEdges) {
        if (edge && edge.child && edge.parent) {
          drawnEdges.add(edge.child + '->' + edge.parent);
        }
      }
      for (const b of expectedBadges) {
        const row = wrapEl.querySelector('.row[data-key="' + b.key + '"]');
        if (!row) { problems.push('missing row ' + b.key); continue; }
        if (!row.querySelector('.' + b.cls)) {
          problems.push('missing badge ' + b.cls + ' on ' + b.key);
        }
        const cached = window.__editchainRowAt
          ? window.__editchainRowAt(parseInt(row.getAttribute('data-row'), 10))
          : null;
        if (!cached || cached.node_key !== b.key) {
          problems.push('no cached row for ' + b.key);
          continue;
        }
        const rels = cached.parent_relations || [];
        if (!rels.some((rel) => rel && rel.parent === b.parent && rel.kind === b.kind)) {
          problems.push(b.key + ' cache lacks relation ' +
            JSON.stringify({ parent: b.parent, kind: b.kind }) + ' in ' + JSON.stringify(rels));
        }
        if (!Array.isArray(cached.parents) || cached.parents.indexOf(b.parent) === -1) {
          problems.push(b.key + ' parents ' + JSON.stringify(cached.parents) +
            ' lack relation parent ' + b.parent);
        }
        if (!drawnEdges.has(b.key + '->' + b.parent)) {
          problems.push(b.key + ' has no drawn parent edge to ' + b.parent);
        }
        if (!badgeRows.has(b.key)) {
          problems.push('badge row set lacks ' + b.key);
        }
      }
      for (const key of badgeRows) {
        if (!expectedBadgeRows.has(key)) problems.push('unexpected badge row ' + key);
      }
      if (badgeEls.length !== expectedBadges.length) {
        problems.push('badge count ' + badgeEls.length + ' != expected ' + expectedBadges.length);
      }
      const dimmedBadges = badgeEls.filter((el) => cumulativeOpacity(el, document.body) < 0.99);
      if (dimmedBadges.length) {
        problems.push(dimmedBadges.length + ' badge(s) rendered below full opacity');
      }
      const badgesOk = problems.length === 0;
      checks.push({
        name:'FORK_RELATION_BADGES',
        pass:badgesOk,
        detail: badgesOk
          ? 'badges=' + badgeEls.length + ' on rows=' + JSON.stringify(Array.from(badgeRows)) +
            ' parents/edges match, all full-opacity'
          : problems.join('; '),
      });
    }

    // Check 8b (highLanes scenario only): every service lane must be drawn
    // INSIDE the graph column — no 128-lane clipping. The fixture has 200
    // lanes (> the former cap): all dot centres must land within their SVG
    // cell's width and more than 128 distinct lane x positions must render.
    //
    // Rows 0..4 form a connected production-like zigzag (lanes 0,1,0,1,0 with
    // adjacent transitions 0->1, 1->0, ...), so the rounded paths are exercised
    // under heavy compression: the corner radius clamps to the lane distance,
    // and at extreme spacing the renderer falls back to a straight orthogonal
    // jog. Every transition begins at its row's own dot (no synthetic top
    // half), ends at the next lane's bottom boundary — continued by the
    // following row's `above` at the same x — and must keep two exact halves
    // sharing the midpoint seam, per-lane colours, and all coordinates inside
    // the cell.
    if (wrapEl && window.__editchainScenarioName === 'highLanes') {
      const cells = wrapEl.querySelectorAll('.graph-cell svg.graphCell');
      const ROW_H = 34;
      const MID_Y = ROW_H / 2;
      const distinctXs = new Set();
      let allInside = true;
      let firstBad = null;
      cells.forEach((cell) => {
        const w = +cell.getAttribute('width');
        cell.querySelectorAll('circle.graphDot').forEach((dot) => {
          const x = +dot.getAttribute('cx');
          distinctXs.add(x);
          if (x < 0 || x > w + 0.5) {
            allInside = false;
            firstBad = firstBad || { x, w };
          }
        });
      });
      checks.push({
        name: 'HIGH_LANES_ALL_VISIBLE',
        pass: allInside && distinctXs.size > 128,
        detail: 'distinct lane x positions=' + distinctXs.size +
          ' allInside=' + allInside +
          (firstBad ? ' firstBad=' + JSON.stringify(firstBad) : ''),
      });
      const transitionProblems = [];
      const byRow = collectRowGeometry(cells);
      const near = (a, b) => Math.abs(a - b) <= 0.01;
      // Lane x / fill lookups from the rendered dots.
      const laneXs = new Map();
      const laneFills = new Map();
      for (const [rowIdx, g] of byRow) {
        const cached = window.__editchainRowAt ? window.__editchainRowAt(rowIdx) : null;
        if (cached && g.dot) {
          laneXs.set(cached.lane, g.dot.x);
          laneFills.set(cached.lane, g.dot.fill);
        }
      }
      let transitionRows = 0;
      for (const [row, t] of byRow) {
        if (!t.src && !t.dst) continue; // rows without transitions
        transitionRows++;
        if (!t.src || !t.dst || !t.src.cmds || !t.dst.cmds) {
          transitionProblems.push('row ' + row + ' missing transition halves');
          continue;
        }
        const cached = window.__editchainRowAt ? window.__editchainRowAt(row) : null;
        if (!cached) {
          transitionProblems.push('row ' + row + ' missing cached row');
          continue;
        }
        const anchors = transitionAnchors(cached);
        if (!anchors) {
          transitionProblems.push('row ' + row + ' fixture transition must have connected anchors');
          continue;
        }
        const { fromLane, toLane, startAtDot, startAtBoundary, endAtDot, endAtBoundary } = anchors;
        const fromX = laneXs.get(fromLane);
        const toX = laneXs.get(toLane);
        const rowDot = t.dot;
        if (fromX === undefined || toX === undefined || !rowDot) {
          transitionProblems.push('row ' + row + ' missing lane x references');
          continue;
        }
        const dx = toX - fromX;
        const seamX = (fromX + toX) / 2;
        const srcStart = pathStart(t.src.cmds);
        const srcEnd = pathEnd(t.src.cmds);
        const dstStart = pathStart(t.dst.cmds);
        const dstEnd = pathEnd(t.dst.cmds);
        // Source anchor: the row's own dot, or the top boundary ONLY when
        // `above` lists the from-lane.
        const srcAnchor = startAtDot
          ? [fromX, MID_Y]
          : startAtBoundary ? [fromX, 0] : null;
        if (!srcAnchor) {
          transitionProblems.push('row ' + row + ' transition has no connected source anchor');
        } else if (!srcStart || !near(srcStart[0], srcAnchor[0]) || !near(srcStart[1], srcAnchor[1])) {
          transitionProblems.push('row ' + row + ' src must begin at ' + JSON.stringify(srcAnchor));
        } else if (startAtBoundary) {
          const prev = byRow.get(row - 1);
          if (!prev || !bottomBoundaryXs(prev, ROW_H).has(fromX)) {
            transitionProblems.push('row ' + row + ' src top boundary at x=' + fromX +
              ' is not continued by row ' + (row - 1) + '\'s bottom geometry');
          }
        }
        // Destination anchor: the bottom boundary ONLY when `below` lists the
        // to-lane (and the row below continues it), else the row's own dot.
        const dstAnchor = endAtBoundary
          ? [toX, ROW_H]
          : endAtDot ? [toX, MID_Y] : null;
        if (!dstAnchor) {
          transitionProblems.push('row ' + row + ' transition has no connected destination anchor');
        } else if (!dstEnd || !near(dstEnd[0], dstAnchor[0]) || !near(dstEnd[1], dstAnchor[1])) {
          transitionProblems.push('row ' + row + ' dst must end at ' + JSON.stringify(dstAnchor));
        } else if (endAtBoundary) {
          const next = byRow.get(row + 1);
          if (!next || !topBoundaryXs(next, ROW_H).has(toX)) {
            transitionProblems.push('row ' + row + ' dst bottom boundary at x=' + toX +
              ' is not continued by row ' + (row + 1) + '\'s top geometry');
          }
        }
        if (!srcEnd || !dstStart ||
            srcEnd[0] !== dstStart[0] || srcEnd[1] !== dstStart[1] ||
            !near(srcEnd[0], seamX) || !near(srcEnd[1], MID_Y)) {
          transitionProblems.push('row ' + row + ' halves must share the exact midpoint seam');
        }
        if (t.src.stroke !== laneFills.get(fromLane) || t.dst.stroke !== laneFills.get(toLane)) {
          transitionProblems.push('row ' + row + ' colour handoff mismatch');
        }
        // Dot-anchored sides have no vertical run and must stay straight (no
        // Q). Boundary-anchored sides round onto the row midpoint with the
        // corner radius (midY - cornerStartY) clamped by the lane distance;
        // straight mode (no Q) is the sanctioned fallback under compression.
        const srcQ = t.src.cmds.find((c) => c.cmd === 'Q');
        const dstQ = t.dst.cmds.find((c) => c.cmd === 'Q');
        if (startAtDot && srcQ) {
          transitionProblems.push('row ' + row + ' dot-anchored src must stay straight');
        }
        if (endAtDot && dstQ) {
          transitionProblems.push('row ' + row + ' dot-anchored dst must stay straight');
        }
        const clampOk = (q, cmds, laneX, isSrc) => {
          if (!q) return true; // straight fallback is sanctioned
          const idx = cmds.indexOf(q);
          const cornerStart = cmds[idx - 1];
          if (!cornerStart || cornerStart.cmd !== 'L') return false;
          // Source elbows start their corner at midY - r (vertical run x is the
          // from-lane); destination elbows end their corner at midY + r (the Q
          // end x is the to-lane).
          const radius = isSrc ? MID_Y - cornerStart.args[1] : q.args[3] - MID_Y;
          const verticalX = isSrc ? cornerStart.args[0] : q.args[2];
          return near(verticalX, laneX) &&
            radius <= Math.abs(dx) / 2 + 0.01 && radius >= -0.01;
        };
        if (startAtBoundary && !clampOk(srcQ, t.src.cmds, fromX, true)) {
          transitionProblems.push('row ' + row + ' src corner radius not clamped by lane distance');
        }
        if (endAtBoundary && !clampOk(dstQ, t.dst.cmds, toX, false)) {
          transitionProblems.push('row ' + row + ' dst corner radius not clamped by lane distance');
        }
        // All path coordinates must stay inside the cell (compression never
        // clips a transition).
        const coords = t.src.cmds.concat(t.dst.cmds).reduce((acc, c) => acc.concat(c.args), []);
        if (coords.some((v, i) => i % 2 === 0 && (v < -0.01 || v > t.cellW + 0.01))) {
          transitionProblems.push('row ' + row + ' transition escapes the graph cell');
        }
      }
      checks.push({
        name: 'HIGH_LANES_TRANSITIONS',
        pass: transitionProblems.length === 0,
        detail: transitionProblems.length === 0
          ? transitionRows + ' compressed zigzag transitions valid (clamped radius / fallback, seams, colours, connected endpoints)'
          : transitionProblems.join('; '),
      });
    }

    return checks;
  }

  function assertLayout() {
    const checks = runChecks();
    const failed = checks.filter((c) => !c.pass);
    return { passCount: checks.length - failed.length, failCount: failed.length, checks };
  }

  // --- metrics ---------------------------------------------------------------

  function getMetrics() {
    return {
      generation,
      inFlight,
      domNodes: document.querySelectorAll('*').length,
      listenersApprox:
        document.querySelectorAll('[onclick], [onmousedown], [onkeydown]').length +
        2 /* search keydown/input */ +
        2 /* rows scroll + window resize */ +
        1 /* progressive timer */ +
        1 /* uncaught error */ ,
      progressiveTimerActive:
        typeof window.__editchainProgressiveTimerActive === 'boolean'
          ? window.__editchainProgressiveTimerActive : undefined,
    };
  }

  // Capture the geometry the resize assertion compares across viewport sizes:
  // graph SVG cell width, the header's graph track width, lane dot positions,
  // and the render generation (so a rebuild is detectable). The harness host
  // changes the viewport between captures (ui-dump calls page.setViewport).
  function captureResizeMetrics() {
    const header = document.querySelector('.tbl-header');
    const firstRow = document.querySelector('.row:not(.row-placeholder)');
    const graphCell = firstRow && firstRow.querySelector('.graph-cell svg.graphCell');
    const dots = firstRow
      ? Array.from(firstRow.querySelectorAll('circle.graphDot')).map((d) => +d.getAttribute('cx'))
      : [];
    return {
      innerW: window.innerWidth,
      graphSvgW: graphCell ? +graphCell.getAttribute('width') : 0,
      graphColW: graphCell
        ? Math.round(graphCell.getBoundingClientRect().width * 100) / 100
        : 0,
      headerGraphW: header
        ? Math.round(header.querySelector('.th.graph').getBoundingClientRect().width * 100) / 100
        : 0,
      firstDotX: dots.length ? dots[0] : null,
      lastDotX: dots.length ? dots[dots.length - 1] : null,
      dotCount: dots.length,
      rowsRendered: document.querySelectorAll('.row').length,
      generation,
    };
  }

  /** Resize assertion used by ui-dump: compare two captures.
   *
   * `before` is captured at the initial viewport; the host then resizes and
   * captures `after`. The renderer must RECOMPUTE graph geometry (not just
   * restretch the DOM): the graph SVG/column/header widths must track the new
   * viewport, the header must stay aligned with the rows, every dot must stay
   * inside its cell, and the DOM must actually rebuild.
   */
  function evaluateResizeAssert(before, after) {
    const widthChanged = before.innerW !== after.innerW;
    const rebuilt = after.generation > before.generation;
    // The graph column legitimately keeps its NATURAL width when the lane
    // count fits at both viewport sizes (e.g. 2 lanes → 54px at 1440 AND 864).
    // What must always happen is a full rebuild against the current geometry;
    // actual lane-compression / budget-bound SVG-width recompute is exercised
    // by the highLanes resize (where the width is viewport-bound). graphAdjusted
    // is therefore diagnostic, not blocking.
    const graphAdjusted =
      widthChanged &&
      (after.graphSvgW !== before.graphSvgW || after.graphColW !== before.graphColW);
    const headerAligned = Math.abs(after.graphSvgW - after.headerGraphW) <= 1.5;
    const dotsInside =
      after.lastDotX === null ||
      (after.lastDotX !== null && after.lastDotX <= after.graphSvgW + 0.5 && after.lastDotX >= 0);
    const pass = rebuilt && headerAligned && dotsInside && after.rowsRendered > 0;
    return {
      pass,
      detail: { widthChanged, rebuilt, graphAdjusted, headerAligned, dotsInside },
    };
  }

  // --- expose ----------------------------------------------------------------

  window.__editchainDebug = {
    whenIdle,
    dumpLayout,
    assertLayout,
    getMetrics,
    runSearch,
    runReversedSearchRace,
    captureResizeMetrics,
    evaluateResizeAssert,
  };
})();
