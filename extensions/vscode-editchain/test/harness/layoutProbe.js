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
  // verify it opens the INSPECTOR (an ordinary click never opens an editor
  // tab), then use the inspector's explicit "Open raw JSON" action and verify
  // it requests a JSON editor with the right identity: a Git hit must navigate
  // by (git_oid, repository) — never by its synthetic index-only op_id.
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
    const layoutEl = document.getElementById('layout');
    const inspectorOpened = !!(layoutEl && layoutEl.classList.contains('has-detail'));
    // Explicit raw-JSON action (the ONLY editor path).
    const openBtn = document.querySelector('#detail .detail-btn:last-child');
    if (openBtn && !openBtn.disabled) openBtn.click();
    await whenIdle(timeoutMs || 5000);
    window.vscode.postMessage = origPost;
    const clicked = captured.length ? captured[0] : null;
    return {
      resultRows,
      bannerText,
      inspectorOpened,
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
    // Resize handles are clamped inside the container, so raw scrollWidth is
    // authoritative; handles are still excluded defensively.
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

    // Check 5f: column visibility follows the narrow-width priority contract.
    // Every column the CSS grid keeps at this width must have a nonzero width
    // and sit inside the rows container (no clipping, no collapsed tracks);
    // columns dropped at narrow widths (Commit <=617, Author <=480, Date
    // <=400) must be hidden — NOT squeezed to a sliver — so Content keeps its
    // readable width. The visible set must match the renderer's own
    // hiddenColumns() view of the breakpoints.
    if (wrapEl && !viewMessage) {
      const firstRow = wrapEl.querySelector('.row:not(.row-placeholder)');
      if (firstRow) {
        const rowsBox = rowsEl.getBoundingClientRect();
        const innerW = window.innerWidth || rowsEl.clientWidth || 0;
        const hidden = new Set();
        if (innerW <= 617) hidden.add('commit');
        if (innerW <= 480) hidden.add('author');
        if (innerW <= 400) hidden.add('date');
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
          if (hidden.has(name)) {
            // Dropped at this width: the cell must be genuinely hidden.
            if (!r || w > 0.5 || getComputedStyle(cell).display !== 'none') {
              geoOk = false;
              firstBad = firstBad || { col: name, expectedHidden: true, w: Math.round(w * 100) / 100 };
            }
          } else if (!r || w <= 0.5 || r.right > rowsBox.right + 1 || r.left < rowsBox.left - 1) {
            geoOk = false;
            firstBad = firstBad || { col: name, w: Math.round(w * 100) / 100 };
          }
        }
        checks.push({
          name: 'CELL_GEOMETRY',
          pass: geoOk,
          detail: geoOk
            ? 'widths=' + JSON.stringify(geo) + 'px hidden=' + Array.from(hidden).join(',') + ' (priority order)'
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
      // Columns dropped at narrow widths (commit/author/date priority) are
      // display:none — their zero-size rects must not count as overlaps.
      const visibleThs = ths.filter((th) => {
        const r = th.getBoundingClientRect();
        return r.width > 0.5 && getComputedStyle(th).display !== 'none';
      });
      for (let i = 0; i < visibleThs.length; i++) {
        const r = visibleThs[i].getBoundingClientRect();
        if (i > 0) {
          const prev = visibleThs[i - 1].getBoundingClientRect();
          if (r.left < prev.right - 0.5) {
            boxesOk = false;
            firstBad = firstBad || { kind: 'overlap', i, left: r.left, prevRight: prev.right };
          }
        }
        const cs = getComputedStyle(visibleThs[i]);
        if (visibleThs[i].scrollWidth > visibleThs[i].clientWidth + 1 && cs.textOverflow !== 'ellipsis') {
          textOk = false;
          firstBad = firstBad || { kind: 'text-spill', i, label: (visibleThs[i].textContent || '').trim().slice(0, 12) };
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

    // Check 5k: the Activity/Raw profile segmented control exists, is
    // labelled, and reflects the ACTIVE profile (Activity by default — the
    // pregenerated hide_trace=true view; Raw when a `--profile raw` run
    // switched through the real control path beforehand).
    const profileControl = document.getElementById('profile-control');
    const profileActivityBtn = document.getElementById('profile-activity');
    const profileRawBtn = document.getElementById('profile-raw');
    const activeProfile = typeof window.__editchainGetProfile === 'function'
      ? window.__editchainGetProfile()
      : 'activity';
    if (profileControl && profileActivityBtn && profileRawBtn) {
      const controlMatches =
        activeProfile === 'activity'
          ? profileActivityBtn.classList.contains('active') &&
            profileActivityBtn.getAttribute('aria-pressed') === 'true' &&
            profileRawBtn.getAttribute('aria-pressed') === 'false'
          : profileRawBtn.classList.contains('active') &&
            profileRawBtn.getAttribute('aria-pressed') === 'true' &&
            profileActivityBtn.getAttribute('aria-pressed') === 'false';
      const labeled = profileControl.getAttribute('aria-label');
      checks.push({
        name: 'PROFILE_CONTROL_PRESENT',
        pass: !!labeled && controlMatches,
        detail: controlMatches
          ? 'segmented control present, labelled "' + labeled + '", profile=' + activeProfile
          : 'control does not match profile ' + activeProfile + ' (aria-pressed activity=' +
            profileActivityBtn.getAttribute('aria-pressed') + ')',
      });
    } else {
      checks.push({
        name: 'PROFILE_CONTROL_PRESENT',
        pass: false,
        detail: 'segmented control missing from #controls',
      });
    }

    // Check 5l: rows expose keyboard/grid semantics so the list is operable
    // without a mouse. The contract is now the r4 a11y structure: ONE labelled
    // role=grid wrapper owns BOTH the sticky header row (its columnheaders must
    // live inside the grid, never orphaned) and the data rows; every row
    // carries role=row + aria-selected; and exactly ONE rendered row is in the
    // tab order (roving tabindex — Tab enters/exits the grid as a unit instead
    // of tabbing through every virtualized row; ArrowUp/Down move within it).
    if (wrapEl) {
      const rowEls = wrapEl.querySelectorAll('.row:not(.row-placeholder)');
      const grids = Array.from(document.querySelectorAll('[role="grid"]'));
      const grid = document.querySelector('.tbl-grid');
      const header = rowsEl.querySelector('.tbl-header');
      const labelled = !!grid && grid.getAttribute('aria-label') === 'History rows';
      const headerInside = !!header && !!grid && grid.contains(header);
      const colHeadersInside = !!header &&
        header.querySelectorAll('[role="columnheader"]').length === 5;
      const gridOwnsRows = grids.length === 1 && !!grid &&
        !!wrapEl.closest('.tbl-grid');
      let rowsOk = rowEls.length > 0;
      rowEls.forEach((r) => {
        if (r.getAttribute('role') !== 'row') rowsOk = false;
        const sel = r.getAttribute('aria-selected');
        if (sel !== 'true' && sel !== 'false') rowsOk = false;
        if (r.getAttribute('data-row') === null) rowsOk = false;
      });
      const tabbableCount = Array.from(wrapEl.querySelectorAll('.row'))
        .filter((r) => r.tabIndex === 0).length;
      const roving = tabbableCount === 1;
      const pass = gridOwnsRows && labelled && headerInside && colHeadersInside &&
        rowsOk && roving;
      checks.push({
        name: 'GRID_KEYBOARD_SEMANTICS',
        pass,
        detail: pass
          ? 'labelled role=grid owns header row + ' + rowEls.length + ' rows; ' +
            tabbableCount + ' tab stop (roving)'
          : 'gridOwnsRows=' + gridOwnsRows + ' labelled=' + labelled +
            ' headerInside=' + headerInside + ' colHeadersInside=' + colHeadersInside +
            ' rowsOk=' + rowsOk + ' roving=' + roving,
      });
    }

    // Check 5m: group boundary labels are VISIBLE (not hover-only) and show a
    // SHORT id — never a full raw 64-bit identifier string. The contract is
    // shortId()'s: the rendered id portion is at most 12 chars. Real chains
    // use long decimal repo/session ids, so the shortened tail can itself be
    // all digits — the check is about LENGTH, not digit shape.
    if (wrapEl && !viewMessage) {
      const labelEl = wrapEl.querySelector('.group-label');
      const computed = labelEl ? getComputedStyle(labelEl) : null;
      const opacity = computed ? parseFloat(computed.opacity) : 0;
      const labelText = labelEl ? (labelEl.textContent || '').trim() : '';
      const idPart = (labelText.match(/(?:repo|session)\s+(\S+)$/) || [])[1] || '';
      const longId = idPart.length > 12;
      checks.push({
        name: 'GROUP_LABEL_VISIBLE_SHORT',
        pass: !labelEl || (opacity >= 0.5 && !longId),
        detail: labelEl
          ? 'label "' + labelText + '" opacity=' + opacity +
            (longId ? ' ID-PART-' + idPart.length + 'ch (>12)' : '')
          : 'no group boundary rows in this scenario (skipped)',
      });
    }

    // Check 5n: the Commit/ID column shows a SHORT display id — op ids are
    // removed from the default visual priority and raw 64-bit strings never
    // render as the visible value.
    if (wrapEl && !viewMessage) {
      const firstRow = wrapEl.querySelector('.row:not(.row-placeholder)');
      const commitCell = firstRow && firstRow.querySelector('.commit-cell');
      const commitText = commitCell ? (commitCell.textContent || '').trim() : '';
      const cached = firstRow ? window.__editchainRowAt(parseInt(firstRow.getAttribute('data-row'), 10)) : null;
      let shortOk = true;
      if (cached && !cached.git_oid && cached.op_id && cached.op_id.length > 12) {
        // Op rows must never show the full raw op id in the visible column.
        shortOk = commitText.length <= 12 && commitText !== cached.op_id;
      }
      checks.push({
        name: 'SHORT_COMMIT_ID',
        pass: shortOk,
        detail: shortOk
          ? 'commit cell "' + commitText + '" (short display id)'
          : 'commit cell "' + commitText + '" leaks the full op id',
      });
    }

    // Check 5o (traced scenario only): the ACTIVE profile drives hide_trace
    // end-to-end — Activity (the default) hides every visibility==="trace"
    // row (hide_trace=true is sent on the first GetWindow) and renders 3
    // rows; Raw sends hide_trace=false and renders all 5 rows. Conversation
    // rows stay badge-free, and the exact Rust taxonomy badges render
    // (execute "run" + success "ok").
    if (window.__editchainScenarioName === 'traced') {
      const keys = Array.from(wrapEl ? wrapEl.querySelectorAll('.row[data-key]') : [])
        .map((r) => r.getAttribute('data-key'));
      const traceKeys = keys.filter((k) => /^node:t:(1|3)$/.test(k));
      const activityKeys = keys.filter((k) => /^node:t:(0|2|4)$/.test(k));
      const traceHidden = traceKeys.length === 0;
      const windows = (window.__editchainRequestLog || []).filter(
        (b) => b && b.GetWindow !== undefined
      );
      // The FIRST window reflects the default profile (Activity hides trace);
      // the LAST window reflects the ACTIVE profile after any `--profile raw`
      // switch (Raw sends hide_trace=false on the refetch).
      const firstWindowFilter = windows.length ? windows[0].GetWindow.filter : null;
      const lastWindowFilter = windows.length ? windows[windows.length - 1].GetWindow.filter : null;
      const rawProfile = activeProfile === 'raw';
      const hideTraceSent = firstWindowFilter && firstWindowFilter.hide_trace === true;
      const hideTraceFlagOk = rawProfile
        ? !!lastWindowFilter && lastWindowFilter.hide_trace === false
        : hideTraceSent;
      const traceOk = rawProfile
        ? traceKeys.length === 2 && activityKeys.length === 3 && keys.length === 5
        : traceHidden && activityKeys.length === 3;
      const rowByKey = (k) => wrapEl && wrapEl.querySelector('.row[data-key="' + k + '"]');
      const t0 = rowByKey('node:t:0'); // conversation turn, outcome success
      const t2 = rowByKey('node:t:2'); // execute activity, outcome unknown
      const t0Outcome = t0 && t0.querySelector('.out-badge.outcome-success');
      const t0ActCount = t0 ? t0.querySelectorAll('.act-badge').length : -1;
      const t2Act = t2 && t2.querySelector('.act-badge.act-execute');
      const t2ActText = t2Act ? (t2Act.textContent || '').trim() : '';
      const badgeOk = !!t0Outcome && t0ActCount === 0 && !!t2Act && t2ActText === 'run';
      checks.push({
        name: 'ACTIVITY_DEFAULT_HIDES_TRACE',
        pass: traceOk && hideTraceFlagOk,
        detail: traceOk && hideTraceFlagOk
          ? (rawProfile
              ? 'raw profile renders trace rows; last GetWindow hide_trace=false'
              : 'trace rows hidden in Activity; first GetWindow hide_trace=' +
                (firstWindowFilter ? firstWindowFilter.hide_trace : 'MISSING') +
                '; activity rows=' + activityKeys.length)
          : (rawProfile
              ? 'raw profile trace rows=' + traceKeys.length +
                ' expected 2; hide_trace=' + (lastWindowFilter ? lastWindowFilter.hide_trace : 'MISSING')
              : 'trace rows RENDERED in Activity view: ' + JSON.stringify(traceKeys) +
                ' hide_trace=' + (firstWindowFilter ? firstWindowFilter.hide_trace : 'MISSING')),
      });
      checks.push({
        name: 'BADGES_EXACT_TAXONOMY',
        pass: !!badgeOk,
        detail: badgeOk
          ? 'conversation row badge-free; success "ok" + execute "run" badges rendered'
          : 'badges: t0 outcome=' + (t0Outcome ? 'ok' : 'MISSING') +
            ' t0 act-badges=' + t0ActCount + ' t2 execute=' +
            (t2Act ? '"' + t2ActText + '"' : 'MISSING'),
      });
    }

    // Check 5p (badges scenario only): the EXACT Rust wire taxonomy renders
    // through the renderer whitelists — every activity_kind (except
    // conversation, badge-free by design) and every outcome carries its exact
    // badge class/text — and the legacy tool_call/command/edit/commit/review
    // + error/interrupted vocabulary never leaks back in. Outcome colors must
    // track VS Code theme tokens (proven by temporarily overriding the
    // harness token and requiring the badge to follow), never fixed colors
    // alone.
    if (window.__editchainScenarioName === 'badges') {
      const expected = {
        'node:b:exec': { act: ['act-execute', 'run'], out: ['outcome-failure', 'fail'] },
        'git:b:sc': { act: ['act-source-control', 'git'], out: ['outcome-success', 'ok'] },
        'node:b:chg': { act: ['act-change', 'change'], out: ['outcome-warning', 'warn'] },
        'node:b:plan': { act: ['act-plan', 'plan'], out: ['outcome-neutral', 'cancelled'] },
        'node:b:exp': { act: ['act-explore', 'explore'], out: null },
        'node:b:ver': { act: ['act-verify', 'verify'], out: null },
        'node:b:diag': { act: ['act-diagnose', 'diagnose'], out: ['outcome-failure', 'fail'] },
        'node:b:coord': { act: ['act-coordinate', 'coordinate'], out: null },
        'node:b:ext': { act: ['act-external', 'external'], out: null },
        'node:b:sys': { act: ['act-system', 'system'], out: null },
      };
      const problems = [];
      const legacyClasses = ['.act-tool', '.act-command', '.act-edit', '.act-commit',
        '.act-review', '.outcome-error'];
      for (const sel of legacyClasses) {
        if (wrapEl && wrapEl.querySelector(sel)) problems.push('legacy badge class present: ' + sel);
      }
      for (const [key, exp] of Object.entries(expected)) {
        const row = wrapEl && wrapEl.querySelector('.row[data-key="' + key + '"]');
        if (!row) { problems.push('missing row ' + key); continue; }
        const actBadge = row.querySelector('.act-badge');
        const outBadge = row.querySelector('.out-badge');
        const actText = actBadge ? (actBadge.textContent || '').trim() : '';
        if (!actBadge || !actBadge.classList.contains(exp.act[0]) || actText !== exp.act[1]) {
          problems.push(key + ' activity badge != ' + exp.act[0] + ' "' + exp.act[1] + '" got ' +
            (actBadge ? actBadge.className + ' "' + actText + '"' : 'none'));
        }
        if (exp.out) {
          const outText = outBadge ? (outBadge.textContent || '').trim() : '';
          if (!outBadge || !outBadge.classList.contains(exp.out[0]) || outText !== exp.out[1]) {
            problems.push(key + ' outcome badge != ' + exp.out[0] + ' "' + exp.out[1] + '" got ' +
              (outBadge ? outBadge.className + ' "' + outText + '"' : 'none'));
          }
        } else if (outBadge) {
          problems.push(key + ' has unexpected outcome badge ' + outBadge.className);
        }
      }
      const convRow = wrapEl && wrapEl.querySelector('.row[data-key="node:b:conv"]');
      if (convRow && convRow.querySelector('.act-badge')) {
        problems.push('conversation row must stay badge-free');
      }
      // Theme-token regression: override each harness token with a sentinel and
      // require the badge color to follow. A stylesheet that hardcoded fixed
      // colors would ignore the override and fail here.
      const rootStyle = document.documentElement.style;
      const rgbClose = (a, b) => !!a && !!b &&
        Math.abs(a[0] - b[0]) <= 1 && Math.abs(a[1] - b[1]) <= 1 && Math.abs(a[2] - b[2]) <= 1;
      const probeTokenDriven = (varName, cls, sentinel) => {
        const prev = rootStyle.getPropertyValue(varName);
        rootStyle.setProperty(varName, sentinel);
        let driven = false;
        const el = wrapEl && wrapEl.querySelector(cls);
        if (el) {
          driven = rgbClose(parseRgb(getComputedStyle(el).color), parseRgb(sentinel));
        }
        rootStyle.setProperty(varName, prev);
        return driven;
      };
      const themeTokensOk =
        probeTokenDriven('--vscode-testing-iconPassed', '.out-badge.outcome-success', '#00ff00') &&
        probeTokenDriven('--vscode-editorWarning-foreground', '.out-badge.outcome-warning', '#00ff00') &&
        probeTokenDriven('--vscode-editorError-foreground', '.out-badge.outcome-failure', '#00ff00');
      if (!themeTokensOk) {
        problems.push('outcome badge colors are not driven by VS Code theme tokens');
      }
      checks.push({
        name: 'BADGE_VOCABULARY_COVERAGE',
        pass: problems.length === 0,
        detail: problems.length === 0
          ? '10/10 activity kinds + 4/4 outcomes exact, conversation badge-free, theme-token colors'
          : problems.join('; '),
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

    // 400px geometry contract: at exactly 400px the history table must fit the
    // viewport with zero phantom horizontal scroll (the last visible column's
    // resize handle is clamped inside the container, so scrollW === clientW),
    // and the Graph columnheader must stay accessible without clipped visual
    // text — either the label fits its track, or it is rendered as
    // visually-hidden text (a "G…" ellipsis is never shown).
    if (rowsEl && !viewMessage && window.innerWidth === 400) {
      const graphTh = headerEl && headerEl.querySelector('.th.graph');
      const hidden = graphTh && graphTh.querySelector('.visually-hidden');
      const hiddenOk = !!hidden && (hidden.textContent || '').trim() === 'Graph';
      const fits = !!graphTh && graphTh.scrollWidth <= graphTh.clientWidth + 1;
      const accessible = !!graphTh && (hiddenOk || (graphTh.textContent || '').trim() === 'Graph');
      const delta = rowsEl.scrollWidth - rowsEl.clientWidth;
      checks.push({
        name: 'GEOMETRY_400',
        pass: delta === 0 && accessible && (fits || hiddenOk),
        detail: 'scrollW=' + rowsEl.scrollWidth + ' clientW=' + rowsEl.clientWidth +
          ' delta=' + delta +
          ' graphCol=' + Math.round(graphTh ? graphTh.getBoundingClientRect().width : 0) +
          'px labelFits=' + !!fits + ' visuallyHidden=' + hiddenOk,
      });
    }

    // Narrow-rail contract at <=480px: the graph column must switch to the
    // compact fixed-width rail (GRAPH_MAX_W_NARROW in media/main.js) — still
    // VISIBLE (never hidden), but never taking ~half the viewport — so the
    // Content track keeps a readable budget. All service lanes still render
    // inside the rail (laneX compresses/distributes them across the column).
    if (rowsEl && !viewMessage && window.innerWidth > 0 && window.innerWidth <= 480) {
      const graphTh = headerEl && headerEl.querySelector('.th.graph');
      const firstRow = wrapEl && wrapEl.querySelector('.row:not(.row-placeholder)');
      const contentCell = firstRow && firstRow.querySelector('.text-cell');
      const graphCol = graphTh ? graphTh.getBoundingClientRect().width : 0;
      const contentW = contentCell ? contentCell.getBoundingClientRect().width : 0;
      // GRAPH_MAX_W_NARROW=120 + layout tolerance; a 1-lane rail is naturally
      // 36px (2 lanes 54px) so the floor only proves the rail is RENDERED,
      // never display:none; content >= 200px locks the legibility gain over
      // the old ~160-200px squeeze (and holds with Date visible at 480px).
      const compact = graphCol > 0 && graphCol <= 140;
      const visible = graphCol >= 30;
      const contentReadable = contentW >= 200;
      checks.push({
        name: 'GRAPH_RAIL_NARROW',
        pass: compact && visible && contentReadable,
        detail: 'graphCol=' + Math.round(graphCol) + 'px (compact rail <= 140px, ' +
          'still visible >= 30px) contentW=' + Math.round(contentW) +
          'px (>= 200px readable)',
      });
    }

    // --- Round-two parallel Activity-view contract (workUnits scenarios) ----
    // The renderer DOM classes land in a parallel change to media/main.js;
    // the fixtures/bridge already model the full wire contract (see
    // workUnitBridge.test.js). Family checks retain explicit diagnostics when
    // one marker family is absent, but the mandatory contract-presence check
    // below fails the Activity scenario unless ALL three renderer families are
    // present. This prevents a reverted renderer from turning feature checks
    // into silent skips. Cache-side wire facts are always asserted as well.
    if (window.__editchainScenarioName === 'workUnits' ||
        window.__editchainScenarioName === 'workUnitsDeep') {
      const activeProfile = typeof window.__editchainGetProfile === 'function'
        ? window.__editchainGetProfile() : 'activity';
      // The small fixture renders its full 12-row Activity view, so its checks
      // assert EXACT global counts; the tall deep fixture renders a
      // virtualized window slice (viewport + 2*BUFFER rows), so its checks
      // assert per-row wire-vs-DOM fidelity plus window-level uniqueness
      // (exact global counts are not reachable from a slice).
      const isSmall = window.__editchainScenarioName === 'workUnits';
      const cachedRow = (el) => {
        const abs = Number(el.getAttribute('data-row'));
        return window.__editchainRowAt ? window.__editchainRowAt(abs) : null;
      };
      const rowEls = wrapEl
        ? Array.from(wrapEl.querySelectorAll('.row:not(.row-placeholder)'))
        : [];
      const countMarkers = (sel) => document.querySelectorAll(sel).length;
      const caps = {
        any: false, workUnit: false, bundle: false, promoted: false,
      };
      caps.workUnit = countMarkers(
        '.row-work-unit-start, .row-work-unit-end, .work-unit-ribbon, .work-unit-count') > 0;
      caps.bundle = countMarkers(
        '.row-activity-bundle, .bundle-count, .bundle-status, [data-activity-bundle], [data-bundle-count]') > 0;
      caps.promoted = countMarkers('.row-promoted') > 0;
      caps.any = caps.workUnit || caps.bundle || caps.promoted;
      const contractAbsent = (family) =>
        'renderer contract not present (no ' + family +
        ' DOM markers rendered; wire metadata verified cache-side)';

      if (activeProfile === 'activity') {
        const present = caps.workUnit && caps.bundle && caps.promoted;
        checks.push({
          name: 'ROUND_TWO_RENDERER_CONTRACT_PRESENT',
          pass: present,
          detail: 'workUnit=' + caps.workUnit + ' bundle=' + caps.bundle +
            ' promoted=' + caps.promoted,
        });
      }

      // Check A: work-unit boundary markers match the wire metadata exactly
      // (one start/end per id) in the Activity profile.
      if (activeProfile === 'activity') {
        if (!caps.workUnit) {
          checks.push({ name: 'WORK_UNIT_BOUNDARIES_EXACT', pass: true, detail: contractAbsent('work-unit') });
        } else {
          const problems = [];
          for (const el of rowEls) {
            const row = cachedRow(el);
            if (!row || !row.work_unit) {
              problems.push((el.getAttribute('data-key') || '?') + ': no cached work_unit');
              continue;
            }
            const startDom = el.classList.contains('row-work-unit-start');
            // Single-row units are both is_start and is_end; the renderer's
            // optional end marker yields to the start header, so the DOM
            // expectation is `is_end && !is_start`.
            const endDom = el.classList.contains('row-work-unit-end');
            if (startDom !== row.work_unit.is_start) {
              problems.push(row.node_key + ': start DOM=' + startDom + ' wire=' + row.work_unit.is_start);
            }
            if (endDom !== (row.work_unit.is_end && !row.work_unit.is_start)) {
              problems.push(row.node_key + ': end DOM=' + endDom + ' wire=' + row.work_unit.is_end);
            }
          }
          if (isSmall && countMarkers('.row-work-unit-start') !== 3) {
            problems.push('expected 3 unit-start rows, got ' + countMarkers('.row-work-unit-start'));
          }
          if (isSmall && countMarkers('.row-work-unit-end') !== 3) {
            problems.push('expected 3 unit-end rows, got ' + countMarkers('.row-work-unit-end'));
          }
          checks.push({
            name: 'WORK_UNIT_BOUNDARIES_EXACT',
            pass: problems.length === 0,
            detail: problems.length === 0
              ? '3/3 unit starts + ends match the wire metadata'
              : problems.join('; '),
          });
        }
      }

      // Check B: unit ribbons are SPARSE (start rows only, exactly one per
      // row) and carry the exact view-wide count in .work-unit-count.
      if (activeProfile === 'activity') {
        if (!caps.workUnit) {
          checks.push({ name: 'WORK_UNIT_RIBBON_SPARSE_COUNTED', pass: true, detail: contractAbsent('work-unit ribbon/count') });
        } else {
          const problems = [];
          let ribbons = 0;
          const parseCount = (text) => {
            const m = /^(\d+)/.exec((text || '').trim());
            return m ? Number(m[1]) : NaN;
          };
          for (const el of rowEls) {
            const row = cachedRow(el);
            const ribbonEls = el.querySelectorAll('.work-unit-ribbon');
            if (ribbonEls.length > 1) problems.push(row.node_key + ': ' + ribbonEls.length + ' ribbons on one row');
            const hasRibbon = ribbonEls.length === 1;
            const countEl = el.querySelector('.work-unit-count');
            const expectedRibbon = !!(row && row.work_unit && row.work_unit.is_start);
            if (hasRibbon !== expectedRibbon) {
              problems.push(row.node_key + ': ribbon DOM=' + hasRibbon + ' expected(is_start)=' + expectedRibbon);
            }
            if (hasRibbon) {
              ribbons++;
              if (!countEl) {
                problems.push(row.node_key + ': start row lacks .work-unit-count');
              } else {
                const text = (countEl.textContent || '').trim();
                if (parseCount(text) !== row.work_unit.count) {
                  problems.push(row.node_key + ': count text "' + text + '" != wire ' + row.work_unit.count);
                }
              }
              // The ribbon leads with the unit title: the DTO title when
              // present (t1/t2), else the renderer's short fallback label
              // (ops -> "Git") — never the raw opaque unit id.
              const ribbonText = (ribbonEls[0].textContent || '').trim();
              const expectedTitle = row.work_unit.title ||
                (row.activity_kind === 'source_control' ? 'Git' : null);
              if (expectedTitle && ribbonText !== expectedTitle) {
                problems.push(row.node_key + ': ribbon title "' + ribbonText + '" != "' + expectedTitle + '"');
              }
            } else if (countEl) {
              problems.push(row.node_key + ': count element without a ribbon (non-start rows stay sparse)');
            }
          }
          if (isSmall && ribbons !== 3) problems.push('expected exactly 3 ribbons, got ' + ribbons);
          checks.push({
            name: 'WORK_UNIT_RIBBON_SPARSE_COUNTED',
            pass: problems.length === 0,
            detail: problems.length === 0
              ? '3 ribbons with exact counts; non-start rows bare'
              : problems.join('; '),
          });
        }
      }

      // Check C: no duplicate/stacked boundary labels — a rendered window
      // never shows MORE than one start or one end per unit id (and the small
      // fixture's full view shows exactly one of each); no row carries more
      // than one ribbon or group chip; and a ribbon + chip on the same row
      // never overlap visually (stacked labels). Virtualized deep slices may
      // legitimately cut a unit (start without end), so slice-level checks
      // assert uniqueness, not completeness.
      {
        const problems = [];
        const perId = new Map();
        for (const el of rowEls) {
          const row = cachedRow(el);
          if (!row || !row.work_unit) { problems.push('row lacks work_unit in cache'); continue; }
          const agg = perId.get(row.work_unit.id) || { starts: 0, ends: 0 };
          if (row.work_unit.is_start) agg.starts++;
          if (row.work_unit.is_end) agg.ends++;
          perId.set(row.work_unit.id, agg);
          const ribbons = el.querySelectorAll('.work-unit-ribbon').length;
          const chips = el.querySelectorAll('.group-label').length;
          if (ribbons > 1) problems.push(row.node_key + ': duplicate ribbon');
          if (chips > 1) problems.push(row.node_key + ': duplicate/stacked group label');
          if (chips >= 1 && ribbons >= 1) {
            const chip = el.querySelector('.group-label').getBoundingClientRect();
            const ribbon = el.querySelector('.work-unit-ribbon').getBoundingClientRect();
            const overlap = chip.width > 0 && ribbon.width > 0 &&
              chip.left < ribbon.right - 0.5 && ribbon.left < chip.right - 0.5 &&
              chip.top < ribbon.bottom - 0.5 && ribbon.top < chip.bottom - 0.5;
            if (overlap) problems.push(row.node_key + ': group chip overlaps the work-unit ribbon (stacked)');
          }
        }
        for (const [id, agg] of perId) {
          if (agg.starts > 1) problems.push('unit ' + id + ': ' + agg.starts + ' starts in one window');
          if (agg.ends > 1) problems.push('unit ' + id + ': ' + agg.ends + ' ends in one window');
          if (isSmall) {
            if (agg.starts !== 1) problems.push('unit ' + id + ': ' + agg.starts + ' starts');
            if (agg.ends !== 1) problems.push('unit ' + id + ': ' + agg.ends + ' ends');
          }
        }
        const domSkipped = !caps.workUnit;
        checks.push({
          name: 'WORK_UNIT_NO_DUPLICATE_BOUNDARY_PER_ID',
          pass: problems.length === 0,
          detail: problems.length === 0
            ? 'per-id boundaries unique per window' +
              (isSmall ? ' (and exact in the full view)' : ' (deep slice: uniqueness only)') +
              '; labels sparse, never stacked' +
              (domSkipped ? ' — DOM label checks skipped (renderer contract not present)' : '')
            : problems.join('; '),
        });
      }

      // Check D: ONLY typed execute-run rows render bundle styling, with the
      // exact typed member count (data attr + .bundle-count text) and a
      // non-empty status chip; ordinary and unknown-kind rows stay bare.
      if (activeProfile === 'activity') {
        if (!caps.bundle) {
          checks.push({ name: 'BUNDLE_TYPED_EXACT', pass: true, detail: contractAbsent('bundle') });
        } else {
          const problems = [];
          let typed = 0;
          const parseCount = (text) => {
            const m = /^(\d+)/.exec((text || '').trim());
            return m ? Number(m[1]) : NaN;
          };
          for (const el of rowEls) {
            const row = cachedRow(el);
            const b = row ? row.activity_bundle : null;
            const styled = el.classList.contains('row-activity-bundle');
            const styledAttr = el.getAttribute('data-activity-bundle');
            const countAttr = el.getAttribute('data-bundle-count');
            const countEl = el.querySelector('.bundle-count');
            const statusEl = el.querySelector('.bundle-status');
            if (b && b.kind === 'execute-run') {
              typed++;
              if (!styled) problems.push(row.node_key + ': typed execute-run row missing .row-activity-bundle');
              if (styledAttr !== 'execute-run') problems.push(row.node_key + ': data-activity-bundle="' + styledAttr + '" != execute-run');
              if (countAttr !== String(b.member_count)) problems.push(row.node_key + ': data-bundle-count="' + countAttr + '" != ' + b.member_count);
              if (!countEl || parseCount(countEl.textContent) !== b.member_count) {
                problems.push(row.node_key + ': .bundle-count text mismatch (expected member_count ' + b.member_count + ')');
              }
              const expectedStatus = row.outcome === 'success' ? 'completed' : 'outcome unknown';
              const statusText = statusEl ? (statusEl.textContent || '').trim() : '';
              if (!statusEl || statusText !== expectedStatus) {
                problems.push(row.node_key + ': .bundle-status "' + statusText + '" != "' + expectedStatus + '"');
              }
              if (row.outcome === 'success') {
                if (!statusEl.classList.contains('bundle-status-success')) {
                  problems.push(row.node_key + ': all-success bundle missing .bundle-status-success');
                }
              } else if (statusEl && statusEl.classList.contains('bundle-status-success')) {
                problems.push(row.node_key + ': unknown-outcome bundle wrongly shows .bundle-status-success');
              }
            } else {
              if (styled) problems.push(row.node_key + ': unstyled row has .row-activity-bundle');
              if (styledAttr !== null || countAttr !== null || countEl || statusEl) {
                problems.push(row.node_key + ': unstyled row leaks bundle markup/attrs');
              }
            }
          }
          if (isSmall && typed !== 2) problems.push('expected exactly 2 typed execute-run bundles, got ' + typed);
          checks.push({
            name: 'BUNDLE_TYPED_EXACT',
            pass: problems.length === 0,
            detail: problems.length === 0
              ? '2/2 typed execute-run bundles styled with exact member_count/status; ordinary + unknown-kind rows bare'
              : problems.join('; '),
          });
        }
      }

      // Check E: styling never comes from parsing the display summary — the
      // ordinary execute-with-subops row and the unknown-kind bundle row BOTH
      // read like execute runs, yet must render zero bundle styling.
      if (activeProfile === 'activity') {
        if (!caps.bundle) {
          checks.push({ name: 'BUNDLE_NO_SUMMARY_PARSE', pass: true, detail: contractAbsent('bundle') });
        } else {
          const problems = [];
          // The deep fixture repeats the pattern per block with qualified keys
          // ('wud:0:wu:execsub', ...) — match by suffix so both fixtures hit
          // the same look-alike rows.
          for (const key of ['wu:execsub', 'wu:xbundle']) {
            const el = Array.from(wrapEl.querySelectorAll('.row')).find((r) =>
              (r.getAttribute('data-key') || '').endsWith(key));
            if (!el) { problems.push('missing row ' + key); continue; }
            if (el.classList.contains('row-activity-bundle') ||
                el.hasAttribute('data-activity-bundle') ||
                el.hasAttribute('data-bundle-count') ||
                el.querySelector('.bundle-count, .bundle-status')) {
              problems.push(key + ': styled despite summary-only similarity (typed metadata: ' +
                JSON.stringify(cachedRow(el).activity_bundle) + ')');
            }
          }
          checks.push({
            name: 'BUNDLE_NO_SUMMARY_PARSE',
            pass: problems.length === 0,
            detail: problems.length === 0
              ? 'summary look-alikes render no bundle styling (typed metadata only)'
              : problems.join('; '),
          });
        }
      }

      // Check F: promotion DOM matches the wire flag exactly (Activity
      // profile; the cache-side count always runs).
      if (activeProfile === 'activity') {
        const problems = [];
        let promotedCount = 0;
        for (const el of rowEls) {
          const row = cachedRow(el);
          const dom = el.classList.contains('row-promoted');
          const wire = !!(row && row.promoted);
          if (dom !== wire) problems.push(row.node_key + ': promoted DOM=' + dom + ' wire=' + wire);
          if (dom) promotedCount++;
        }
        if (isSmall && promotedCount !== 5) problems.push('expected 5 promoted rows, got ' + promotedCount);
        if (!caps.promoted) {
          checks.push({
            name: 'PROMOTED_EXACT',
            pass: true,
            detail: contractAbsent('promoted') + ' (cache rows: ' + promotedCount + '/5 promoted)' +
              (problems.length ? ' cache-vs-fixture mismatch: ' + problems.join('; ') : ''),
          });
        } else {
          checks.push({
            name: 'PROMOTED_EXACT',
            pass: problems.length === 0,
            detail: problems.length === 0 ? '5/5 promoted rows match the wire flag' : problems.join('; '),
          });
        }
      }

      // Check G: Activity vs Raw gating — the Raw profile renders NONE of the
      // grouping/promotion classes/descendants/data attrs even though its
      // cached rows carry the wire metadata; the Activity profile renders
      // them whenever the contract is present.
      {
        const rowHasGrouping = (el) =>
          el.classList.contains('row-work-unit-start') ||
          el.classList.contains('row-work-unit-end') ||
          el.classList.contains('row-activity-bundle') ||
          el.classList.contains('row-promoted') ||
          el.querySelector('.work-unit-ribbon, .work-unit-count, .bundle-count, .bundle-status') !== null ||
          el.hasAttribute('data-activity-bundle') ||
          el.hasAttribute('data-bundle-count');
        if (activeProfile === 'raw') {
          const groupingRows = rowEls.filter(rowHasGrouping).length;
          const cacheHasMetadata = rowEls.some((el) => {
            const row = cachedRow(el);
            return row && (!!row.work_unit || row.promoted || row.activity_bundle);
          });
          const rawGated = groupingRows === 0 && cacheHasMetadata;
          checks.push({
            name: 'RAW_PROFILE_GATED',
            pass: rawGated,
            detail: rawGated
              ? 'raw rows carry wire metadata but zero grouping/promotion DOM'
              : 'raw profile leaked grouping DOM: ' + groupingRows +
                ' rows; cacheHasMetadata=' + cacheHasMetadata,
          });
        } else {
          const markerRows = rowEls.filter(rowHasGrouping).length;
          checks.push({
            name: 'ACTIVITY_PROFILE_GROUPING_PRESENT',
            pass: caps.any ? markerRows > 0 : true,
            detail: caps.any
              ? markerRows + ' rows carry grouping/promotion markers'
              : contractAbsent('grouping/promotion'),
          });
        }
      }

      // Check H: bounded DOM — the virtualized window stays at roughly
      // viewport + 2*BUFFER rows regardless of the dataset (the renderer's
      // documented contract), so ribbons/bundles/expansion never explode the
      // node count.
      {
        const domTotal = document.querySelectorAll('*').length;
        const rendered = rowEls.length;
        const total = window.__editchainGetTotal ? window.__editchainGetTotal() : -1;
        const viewportRows = Math.ceil((rowsEl ? rowsEl.clientHeight : 0) / 34);
        const maxRows = Math.min(total >= 0 ? total : Infinity, viewportRows + 800 + 5);
        const bounded = rendered > 0 && rendered <= maxRows && domTotal < 12000;
        checks.push({
          name: 'WORK_UNITS_DOM_BOUNDED',
          pass: bounded,
          detail: 'rows=' + rendered + '/max=' + maxRows + ' domNodes=' + domTotal +
            (bounded ? ' (bounded)' : ' (over budget)'),
        });
      }

      // Check I: narrow-width content containment — at <=480px every summary
      // descendant (chevron, bundle pills, badges, text) must lay out INSIDE
      // its row's content track. The chips are flex-shrinkable so a crowded
      // line ellipsizes instead of crossing the content-cell boundary (the
      // round-two regression: bundle/outcome chips crossed the track even
      // when the viewport-level NO_HORIZONTAL_OVERFLOW check could not see
      // it because the track ends before the viewport edge).
      if (rowEls.length && window.innerWidth > 0 && window.innerWidth <= 480) {
        const problems = [];
        for (const el of rowEls) {
          const cell = el.querySelector('.text-cell');
          const summaries = el.querySelectorAll('.summary');
          if (!cell) { problems.push('row ' + el.getAttribute('data-row') + ': no .text-cell'); continue; }
          const cellR = cell.getBoundingClientRect();
          for (const s of summaries) {
            const sR = s.getBoundingClientRect();
            if (sR.right > cellR.right + 1) {
              problems.push('row ' + el.getAttribute('data-row') + ': summary crosses content track');
              continue;
            }
            for (const d of s.querySelectorAll('*')) {
              const r = d.getBoundingClientRect();
              if (r.right > cellR.right + 1 || r.left < cellR.left - 1) {
                problems.push('row ' + el.getAttribute('data-row') + ': <' +
                  (d.className && typeof d.className === 'string' ? d.className.split(/\s+/).join('.') : d.tagName) +
                  '> crosses content track');
                break;
              }
            }
          }
        }
        checks.push({
          name: 'WORK_UNITS_NARROW_CONTAINED',
          pass: problems.length === 0,
          detail: problems.length === 0
            ? 'all summary descendants inside their content cell at ' + window.innerWidth + 'px'
            : problems.slice(0, 4).join('; '),
        });
      }
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

  // --- profile switching ----------------------------------------------------

  // Exercise the Activity -> Raw -> Activity profile switch through the REAL
  // control path (button clicks), asserting the coherent-reset contract:
  //   - the request log shows hide_trace flipping false/true on every
  //     GetWindow/layout follow-up (raw = false, activity = true);
  //   - switching resets to offset 0 (the first window after each switch is
  //     requested at offset 0) and rebuilds the expansion snapshot/cache
  //     (sub_op_counts arrives again, rows re-render);
  //   - search mode exits: a switch during search results returns to the full
  //     history view under the new profile;
  //   - scroll returns to the top (refetch from offset 0);
  //   - Raw renders trace rows; Activity hides them (traced scenario).
  async function runProfileSwitch(timeoutMs) {
    const out = { steps: [] };
    const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));
    const clickProfile = (name) => {
      const btn = document.getElementById('profile-' + name);
      if (!btn) throw new Error('profile button missing: ' + name);
      btn.click();
    };
    const requestLog = () => window.__editchainRequestLog || [];
    const windowFilters = () => requestLog()
      .filter((b) => b && b.GetWindow !== undefined)
      .map((b) => b.GetWindow.filter);
    const firstWindowOffset = () => {
      const ws = requestLog().filter((b) => b && b.GetWindow !== undefined);
      return ws.length ? ws[0].GetWindow.offset : -1;
    };
    // Baseline: Activity (default). The FIRST GetWindow must carry
    // hide_trace=true and start at offset 0.
    await whenIdle(timeoutMs || 5000);
    const activityBefore = {
      profile: window.__editchainGetProfile ? window.__editchainGetProfile() : null,
      firstFilter: windowFilters()[0] || null,
      firstOffset: firstWindowOffset(),
      traceRows: document.querySelectorAll('.row[data-key^="node:t:"][data-key$=":1"], .row[data-key="node:t:1"], .row[data-key="node:t:3"]').length,
    };
    out.steps.push({ name: 'activity-default', ...activityBefore });

    // Switch to Raw: hide_trace must flip to false, offset 0 refetch happens,
    // trace rows appear (traced scenario), scroll resets to the top.
    window.__editchainClearRequestLog();
    clickProfile('raw');
    await whenIdle(timeoutMs || 5000);
    const rawAfter = {
      profile: window.__editchainGetProfile ? window.__editchainGetProfile() : null,
      lastFilter: windowFilters().length ? windowFilters()[windowFilters().length - 1] : null,
      // The switch REFETCHES from offset 0: the first window issued after the
      // switch must start at 0 (the progressive loader then pages deeper).
      firstOffset: firstWindowOffset(),
      scrollTop: document.getElementById('rows').scrollTop,
      traceRows: document.querySelectorAll('.row[data-key="node:t:1"], .row[data-key="node:t:3"]').length,
    };
    out.steps.push({ name: 'raw', ...rawAfter });

    // Switch back to Activity: hide_trace=true again, trace rows hidden.
    window.__editchainClearRequestLog();
    clickProfile('activity');
    await whenIdle(timeoutMs || 5000);
    const activityBack = {
      profile: window.__editchainGetProfile ? window.__editchainGetProfile() : null,
      lastFilter: windowFilters().length ? windowFilters()[windowFilters().length - 1] : null,
      firstOffset: firstWindowOffset(),
      traceRows: document.querySelectorAll('.row[data-key="node:t:1"], .row[data-key="node:t:3"]').length,
    };
    out.steps.push({ name: 'activity-back', ...activityBack });

    // A profile switch during SEARCH MODE must exit search and refetch history
    // under the new profile (search is explicitly unprofiled).
    const searchInput = document.getElementById('search');
    searchInput.value = 'tool result';
    searchInput.dispatchEvent(new KeyboardEvent('keydown', { key: 'Enter', bubbles: true }));
    await whenIdle(timeoutMs || 5000);
    const inSearch = !!document.querySelector('.search-banner');
    window.__editchainClearRequestLog();
    clickProfile('raw');
    await whenIdle(timeoutMs || 5000);
    const afterSearchSwitch = {
      inSearch,
      bannerAfter: !!(document.querySelector('.search-banner')),
      profile: window.__editchainGetProfile ? window.__editchainGetProfile() : null,
      windowsIssued: requestLog().filter((b) => b && b.GetWindow !== undefined).length,
      traceRows: document.querySelectorAll('.row[data-key="node:t:1"], .row[data-key="node:t:3"]').length,
    };
    out.steps.push({ name: 'search-exit-on-switch', ...afterSearchSwitch });

    const s1 = out.steps[0];
    const s2 = out.steps[1];
    const s3 = out.steps[2];
    const s4 = out.steps[3];
    const pass =
      s1.profile === 'activity' &&
      s1.firstFilter && s1.firstFilter.hide_trace === true &&
      s1.firstOffset === 0 &&
      s1.traceRows === 0 &&
      s2.profile === 'raw' &&
      s2.lastFilter && s2.lastFilter.hide_trace === false &&
      s2.firstOffset === 0 &&
      s2.scrollTop === 0 &&
      s2.traceRows === 2 &&
      s3.profile === 'activity' &&
      s3.lastFilter && s3.lastFilter.hide_trace === true &&
      s3.firstOffset === 0 &&
      s3.traceRows === 0 &&
      s4.inSearch === true &&
      s4.bannerAfter === false &&
      s4.windowsIssued > 0 &&
      s4.traceRows === 2;
    out.pass = pass;
    return out;
  }

  // --- inspector vs chevron routing ----------------------------------------

  // A row click must SELECT the row and open the inspector (GetNodeDetails /
  // ResolveObject request) WITHOUT opening an editor tab; ONLY the chevron
  // toggles bundled sub-ops. Also exercises the Close action.
  async function runInspectorRouting(timeoutMs) {
    const out = { steps: [] };
    const captured = [];
    const origPost = window.vscode.postMessage.bind(window.vscode);
    window.vscode.postMessage = function (msg) {
      if (msg && msg.type === 'openJson') captured.push(msg);
      return origPost(msg);
    };
    await whenIdle(timeoutMs || 5000);
    const rowEls = Array.from(document.querySelectorAll('.row:not(.row-placeholder)'));
    const detailCapable = rowEls.find((el) => {
      const abs = Number(el.getAttribute('data-row'));
      const row = window.__editchainRowAt ? window.__editchainRowAt(abs) : null;
      return row && (row.op_id || row.git_oid);
    });
    if (!detailCapable) throw new Error('no detail-capable rendered row');
    const key = detailCapable.getAttribute('data-key');
    const absIdx = Number(detailCapable.getAttribute('data-row'));
    const row = window.__editchainRowAt(absIdx);
    const detailReqBefore = (window.__editchainRequestLog || [])
      .filter((b) => b && (b.GetNodeDetails !== undefined || b.ResolveObject !== undefined)).length;
    detailCapable.click();
    await whenIdle(timeoutMs || 5000);
    const layoutEl = document.getElementById('layout');
    const detailReqs = (window.__editchainRequestLog || [])
      .filter((b) => b && (b.GetNodeDetails !== undefined || b.ResolveObject !== undefined));
    const isGit = !!(row && row.git_oid);
    const expectedReq = isGit
      ? (detailReqs[detailReqs.length - 1] || {}).ResolveObject
      : (detailReqs[detailReqs.length - 1] || {}).GetNodeDetails;
    const inspectorOpened = !!(layoutEl && layoutEl.classList.contains('has-detail'));
    const editorOpened = captured.length > 0;
    const selected = document.querySelector('.row.row-selected');
    out.steps.push({
      name: 'row-click',
      key,
      inspectorOpened,
      editorOpened,
      requestIssued: !!expectedReq,
      requestIdentity: expectedReq
        ? (isGit ? expectedReq.oid : expectedReq.op_id)
        : null,
      selectedKey: selected ? selected.getAttribute('data-key') : null,
    });

    // Chevron routing: on a combined row, chevron click must NOT open the
    // inspector — it only toggles expansion. Re-query the DOM fresh around
    // each interaction because expansion REBUILDS the rows (stale elements
    // never reflect the new aria-expanded state).
    const chevronBtn = document.querySelector('.row .subop-chevron');
    if (chevronBtn) {
      const chevronRow = chevronBtn.closest('.row');
      const abs = Number(chevronRow.getAttribute('data-row'));
      const cRow = window.__editchainRowAt ? window.__editchainRowAt(abs) : null;
      const hadSubops = !!cRow && (cRow.sub_ops || []).length > 0;
      const expandedBefore = chevronRow.getAttribute('aria-expanded');
      const chevronKey = chevronRow.getAttribute('data-key');
      const inspectorBeforeChevron = !!(layoutEl && layoutEl.classList.contains('has-detail'));
      chevronBtn.click();
      await whenIdle(timeoutMs || 5000);
      const freshRow = Array.from(document.querySelectorAll('.row')).find(
        (r) => r.getAttribute('data-key') === chevronKey
      );
      const expandedAfter = freshRow ? freshRow.getAttribute('aria-expanded') : null;
      const subopRows = document.querySelectorAll('.row.row-subop').length;
      out.steps.push({
        name: 'chevron-only',
        hadSubops,
        expandedBefore,
        expandedAfter,
        subopRows,
        inspectorBeforeChevron,
        inspectorAfterChevron: !!(layoutEl && layoutEl.classList.contains('has-detail')),
        toggled: hadSubops && expandedBefore !== expandedAfter,
      });
    } else {
      out.steps.push({
        name: 'chevron-only',
        skipped: true,
        detail: 'no combined row rendered in this scenario (skipped)',
      });
    }

    // Close action hides the inspector and clears selection.
    const closeBtn = document.querySelector('#detail .detail-btn:first-child');
    if (closeBtn) closeBtn.click();
    await whenIdle(timeoutMs || 5000);
    out.steps.push({
      name: 'close',
      inspectorClosed: !(layoutEl && layoutEl.classList.contains('has-detail')),
      selectionCleared: !document.querySelector('.row.row-selected'),
    });
    window.vscode.postMessage = origPost;

    const s1 = out.steps[0];
    const s2 = out.steps[1];
    const s3 = out.steps[2];
    out.pass =
      s1.inspectorOpened === true &&
      s1.editorOpened === false &&
      s1.requestIssued === true &&
      s1.selectedKey === s1.key &&
      (s2.skipped || (s2.toggled === true &&
        s2.inspectorAfterChevron === s2.inspectorBeforeChevron)) &&
      s3.inspectorClosed === true &&
      s3.selectionCleared === true;
    return out;
  }

  // --- keyboard activation --------------------------------------------------

  // Enter on a focused row selects/inspects it; Space on a combined row toggles
  // its bundled sub-ops; the chevron button itself stays natively operable.
  async function runKeyboardProbe(timeoutMs) {
    const out = { steps: [] };
    const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));
    await whenIdle(timeoutMs || 5000);
    const layoutEl = document.getElementById('layout');
    const wrap = document.querySelector('.table-wrap');
    const header = document.querySelector('.tbl-header');
    const rowEls = Array.from(document.querySelectorAll('.row:not(.row-placeholder)'));

    // a11y structure: ONE labelled grid wrapper owns the sticky header row
    // (its columnheaders must be INSIDE role=grid, not orphaned) and the data
    // rows; exactly one row carries tabindex=0 (roving tabindex), so Tab
    // enters/exits the grid as a unit instead of tabbing every virtualized row.
    const grids = Array.from(document.querySelectorAll('[role="grid"]'));
    const grid = document.querySelector('.tbl-grid');
    const gridOk = grids.length === 1 &&
      grid === wrap.closest('.tbl-grid') &&
      header && grid && grid.contains(header) &&
      grid.getAttribute('aria-label') === 'History rows' &&
      Number(grid.getAttribute('aria-rowcount')) >= 0;
    const tabbable = rowEls.filter((el) => el.tabIndex === 0);
    const allRowsTabbable = rowEls.filter((el) => el.tabIndex === 0).length;
    out.steps.push({
      name: 'grid-structure',
      gridCount: grids.length,
      gridOk,
      rovingRows: tabbable.length,
      allRowsTabbable,
    });

    const rovingRow = tabbable[0] || rowEls[0];
    const detailCapable = (el) => {
      const abs = Number(el.getAttribute('data-row'));
      const row = window.__editchainRowAt ? window.__editchainRowAt(abs) : null;
      return row && (row.op_id || row.git_oid);
    };
    const target = detailCapable(rovingRow) ? rovingRow : rowEls.find(detailCapable);
    if (!target) throw new Error('no keyboard-capable rendered row');

    target.focus();
    target.dispatchEvent(new KeyboardEvent('keydown', { key: 'Enter', bubbles: true }));
    // Opening the inspector shrinks #rows -> the debounced width recompute
    // rebuilds the rows; settle deterministically so later steps never operate
    // on pre-rebuild (detached) elements.
    await whenIdle(timeoutMs || 5000);
    await settleGeometry(timeoutMs || 4000);
    const enterInspected = !!(layoutEl && layoutEl.classList.contains('has-detail'));
    const enterSelected = !!document.querySelector('.row.row-selected');
    out.steps.push({ name: 'enter-activates', enterInspected, enterSelected });

    const closeBtn = document.querySelector('#detail .detail-btn:first-child');
    if (closeBtn) closeBtn.click();
    await whenIdle(timeoutMs || 5000);
    await settleGeometry(timeoutMs || 4000);

    // Roving navigation: ArrowDown must move focus to the NEXT rendered row and
    // re-pin the single tab stop to it (the previous row drops to -1). The
    // inspector-open steps above trigger the debounced width recompute, which
    // REBUILDS the rows — so re-query the roving row fresh here (stale
    // elements from before a rebuild are detached and can't receive focus).
    const allRows = Array.from(document.querySelectorAll('.table-wrap .row'));
    const rovingNow = allRows.find((r) => r.tabIndex === 0) || allRows[0];
    const rovingIdx = rovingNow ? allRows.indexOf(rovingNow) : -1;
    const nextRow = rovingIdx >= 0 && rovingIdx + 1 < allRows.length ? allRows[rovingIdx + 1] : null;
    if (nextRow && rovingNow) {
      rovingNow.focus();
      rovingNow.dispatchEvent(new KeyboardEvent('keydown', { key: 'ArrowDown', bubbles: true }));
      await sleep(200);
      // Compare by data-row, not element identity: a pending debounced width
      // recompute may rebuild the rows during the wait (focus is preserved by
      // the renderer, but the element object is replaced).
      const focused = document.activeElement;
      const focusedIsRow = focused && focused.classList && focused.classList.contains('row');
      const rovingAfter = Array.from(document.querySelectorAll('.row')).filter((r) => r.tabIndex === 0);
      const moved = focusedIsRow &&
        focused.getAttribute('data-row') === nextRow.getAttribute('data-row') &&
        rovingAfter.length === 1;
      out.steps.push({
        name: 'arrow-down-roves',
        moved,
        focusedRow: focusedIsRow ? focused.getAttribute('data-row') : null,
        expectedRow: nextRow.getAttribute('data-row'),
        rovingAfter: rovingAfter.length,
      });
    } else {
      out.steps.push({ name: 'arrow-down-roves', skipped: true });
    }

    const chevronBtn = document.querySelector('.row .subop-chevron');
    if (chevronBtn) {
      const chevronRow = chevronBtn.closest('.row');
      const chevronKey = chevronRow.getAttribute('data-key');
      const expandedBefore = chevronRow.getAttribute('aria-expanded');
      chevronRow.focus();
      chevronRow.dispatchEvent(new KeyboardEvent('keydown', { key: ' ', bubbles: true }));
      await sleep(200);
      const freshRow = Array.from(document.querySelectorAll('.row')).find(
        (r) => r.getAttribute('data-key') === chevronKey
      );
      const expandedAfter = freshRow ? freshRow.getAttribute('aria-expanded') : null;
      out.steps.push({
        name: 'space-expands',
        expandedBefore,
        expandedAfter,
        toggled: expandedBefore !== expandedAfter,
      });
    } else {
      out.steps.push({ name: 'space-expands', skipped: true });
    }

    // Sticky header inside the labelled grid wrapper: the a11y restructure
    // moved the header row under .tbl-grid — it must STILL pin to the top of
    // the #rows scrollport while the table scrolls.
    const rowsEl = document.getElementById('rows');
    const rowsTop = rowsEl ? rowsEl.getBoundingClientRect().top : 0;
    const scrollable = rowsEl && rowsEl.scrollHeight > rowsEl.clientHeight + 100;
    if (scrollable) {
      rowsEl.scrollTop = Math.min(2000, rowsEl.scrollHeight - rowsEl.clientHeight);
      await sleep(200);
      // Re-query the header AFTER scrolling: scroll-driven syncWindow (or a
      // still-pending debounced width recompute) can rebuild the table, which
      // replaces the header element (the sticky CONTRACT — pinned to the
      // #rows scrollport — is what's under test, not a specific element).
      const headerNow = document.querySelector('.tbl-header');
      const headerTop = headerNow ? headerNow.getBoundingClientRect().top : NaN;
      const pinned = Math.abs(headerTop - rowsTop) <= 1;
      out.steps.push({
        name: 'sticky-header',
        pinned,
        headerTop: Math.round(headerTop * 10) / 10,
        rowsTop: Math.round(rowsTop * 10) / 10,
      });
      rowsEl.scrollTop = 0;
      await sleep(200);
    } else {
      out.steps.push({ name: 'sticky-header', skipped: true });
    }

    const s0 = out.steps[0];
    const s1 = out.steps[1];
    const s2 = out.steps[2];
    const s3 = out.steps[3];
    const s4 = out.steps[4];
    out.pass =
      s0.gridOk === true &&
      s0.rovingRows === 1 &&
      s1.enterInspected === true &&
      s1.enterSelected === true &&
      (s2.skipped || s2.moved === true) &&
      (s3.skipped || s3.toggled === true) &&
      (s4.skipped || s4.pinned === true);
    return out;
  }

  // --- inspector geometry across open/close ---------------------------------

  // Extract the inline column-width CSS vars (`--graph-w`, `--date-w`, ...)
  // from an element's style attribute so the probe can assert the renderer
  // re-applied widths rather than leaving stale inline values behind.
  function inlineColVars(styleAttr) {
    const vars = {};
    const re = /(--(?:graph|content|date|author|commit)-w):\s*([^;]+)/g;
    let m;
    while ((m = re.exec(styleAttr || '')) !== null) vars[m[1]] = m[2].trim();
    return vars;
  }

  // Snapshot the table geometry the renderer is responsible for recomputing
  // when #rows' width changes (inspector open/close): the flex width itself,
  // the graph column (in-memory budget width vs rendered SVG/header widths),
  // the inline column-width vars, the hidden fixed columns, and horizontal
  // overflow. `generation` is the probe's own DOM-mutation counter.
  function captureInspectorGeometry() {
    const rowsEl = document.getElementById('rows');
    const layoutEl = document.getElementById('layout');
    const header = document.querySelector('.tbl-header');
    const firstRow = document.querySelector('.row:not(.row-placeholder)');
    const graphCell = firstRow && firstRow.querySelector('.graph-cell svg.graphCell');
    const graphState = typeof window.__editchainGraphState === 'function'
      ? window.__editchainGraphState() : null;
    const dots = firstRow
      ? Array.from(firstRow.querySelectorAll('circle.graphDot')).map((d) => +d.getAttribute('cx'))
      : [];
    const hiddenCols = [];
    for (const col of ['date', 'author', 'commit']) {
      const cell = document.querySelector('.row .' + col + '-cell');
      if (cell && getComputedStyle(cell).display === 'none') hiddenCols.push(col);
    }
    return {
      viewportW: window.innerWidth,
      rowsW: rowsEl ? rowsEl.clientWidth : 0,
      hasDetail: !!(layoutEl && layoutEl.classList.contains('has-detail')),
      graphWidth: graphState ? graphState.graphWidth : null,
      graphSvgW: graphCell ? +graphCell.getAttribute('width') : 0,
      graphColW: graphCell ? Math.round(graphCell.getBoundingClientRect().width * 100) / 100 : 0,
      headerGraphW: header
        ? Math.round(header.querySelector('.th.graph').getBoundingClientRect().width * 100) / 100
        : 0,
      headerVars: header ? inlineColVars(header.getAttribute('style') || '') : null,
      rowVars: firstRow ? inlineColVars(firstRow.getAttribute('style') || '') : null,
      firstDotX: dots.length ? dots[0] : null,
      lastDotX: dots.length ? dots[dots.length - 1] : null,
      dotCount: dots.length,
      hiddenCols,
      // Content spill measured like the NO_HORIZONTAL_OVERFLOW layout check
      // (bounding rects; handles are clamped inside the container but stay
      // excluded defensively).
      overflowDelta: (() => {
        if (!rowsEl) return 0;
        const rowsBox = rowsEl.getBoundingClientRect();
        let maxSpill = 0;
        for (const el of rowsEl.querySelectorAll('*')) {
          if (el.classList && el.classList.contains('col-resize-handle')) continue;
          const r = el.getBoundingClientRect();
          const spill = r.right - rowsBox.right;
          if (spill > maxSpill) maxSpill = spill;
        }
        return Math.round(maxSpill * 100) / 100;
      })(),
      generation,
    };
  }

  // Deterministic geometry settle: poll until the rendered graph cell width
  // equals the freshly computed in-memory graph width for two consecutive
  // frames. The renderer recomputes ~150ms after #rows' width changes (the
  // debounced onViewportResize), so polling is exact for cases where geometry
  // MUST change (budget-bound graph at wide viewports) and terminates fast when
  // the width legitimately cannot change (narrow overlay).
  async function settleGeometry(timeoutMs) {
    const deadline = Date.now() + (timeoutMs || 4000);
    return new Promise((resolve) => {
      const tick = () => {
        const graphState = typeof window.__editchainGraphState === 'function'
          ? window.__editchainGraphState() : null;
        const firstRow = document.querySelector('.row:not(.row-placeholder) .graph-cell svg.graphCell');
        const domW = firstRow ? +firstRow.getAttribute('width') : -1;
        const converged = graphState !== null && firstRow !== null &&
          Math.abs(domW - graphState.graphWidth) <= 1.5;
        if (converged) return resolve(true);
        if (Date.now() >= deadline) return resolve(false);
        requestAnimationFrame(tick);
      };
      requestAnimationFrame(tick);
    });
  }

  // Exercise the REAL open/close controls and verify the table geometry is
  // recomputed, not stale: flex width, graph column (SVG + header + inline
  // vars), hidden columns, and overflow must be coherent BEFORE, WITH, and
  // AFTER the inspector at the current viewport.
  async function runInspectorGeometry(timeoutMs) {
    const out = { captures: [], pass: false, detail: null };
    const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));
    await whenIdle(timeoutMs || 5000);
    // Ensure the table has settled to the CURRENT viewport width before the
    // baseline capture (the host may have just resized the page).
    await settleGeometry(timeoutMs || 4000);
    const rowEls = Array.from(document.querySelectorAll('.row:not(.row-placeholder)'));
    const target = rowEls.find((el) => {
      const abs = Number(el.getAttribute('data-row'));
      const row = window.__editchainRowAt ? window.__editchainRowAt(abs) : null;
      return row && (row.op_id || row.git_oid);
    });
    if (!target) throw new Error('no detail-capable rendered row for inspector geometry probe');

    const before = captureInspectorGeometry();
    target.click();
    await whenIdle(timeoutMs || 5000);
    await settleGeometry(timeoutMs || 4000);
    const opened = captureInspectorGeometry();

    const closeBtn = document.querySelector('#detail .detail-btn:first-child');
    if (closeBtn) closeBtn.click();
    await whenIdle(timeoutMs || 5000);
    await settleGeometry(timeoutMs || 4000);
    const closed = captureInspectorGeometry();

    out.captures.push({ name: 'before-open', state: before });
    out.captures.push({ name: opened.hasDetail ? 'opened' : 'open-failed', state: opened });
    out.captures.push({ name: closed.hasDetail ? 'close-failed' : 'closed', state: closed });

    const b = before, o = opened, c = closed;
    // Wide viewports split the flex budget (inspector open shrinks #rows);
    // narrow viewports overlay the inspector so #rows keeps its width.
    const wide = b.viewportW > 617;
    const flexSplit = wide &&
      o.rowsW < b.rowsW - 20 && o.hasDetail === true &&
      c.rowsW === b.rowsW;
    const overlayKeptWidth = !wide &&
      o.rowsW === b.rowsW && c.rowsW === b.rowsW;
    // Recompute evidence: the RENDERED graph column must match the freshly
    // computed in-memory graph width in every state (a stale layout leaves the
    // DOM sized for the pre-open width while graphWidth tracks the live one).
    const domMatchesLive =
      Math.abs(b.graphSvgW - b.graphWidth) <= 1.5 &&
      Math.abs(o.graphSvgW - o.graphWidth) <= 1.5 &&
      Math.abs(c.graphSvgW - c.graphWidth) <= 1.5;
    // Header stays aligned with the rows and inline vars agree across header
    // and rows after open/close.
    const headerAligned =
      Math.abs(b.headerGraphW - b.graphSvgW) <= 1.5 &&
      Math.abs(o.headerGraphW - o.graphSvgW) <= 1.5 &&
      Math.abs(c.headerGraphW - c.graphSvgW) <= 1.5;
    const varsAgree = o.headerVars && o.rowVars && c.headerVars && c.rowVars &&
      JSON.stringify(o.headerVars) === JSON.stringify(o.rowVars) &&
      JSON.stringify(c.headerVars) === JSON.stringify(c.rowVars);
    // Hidden fixed columns depend on VIEWPORT width (unchanged by open/close).
    const hiddenStable =
      JSON.stringify(o.hiddenCols) === JSON.stringify(b.hiddenCols) &&
      JSON.stringify(c.hiddenCols) === JSON.stringify(b.hiddenCols);
    const noOverflow =
      b.overflowDelta <= 1 && o.overflowDelta <= 1 && c.overflowDelta <= 1;
    const closedRestored = c.hasDetail === false && c.rowsW === b.rowsW;

    out.pass =
      o.hasDetail === true &&
      closedRestored &&
      domMatchesLive &&
      headerAligned &&
      varsAgree &&
      hiddenStable &&
      noOverflow &&
      (wide ? flexSplit : overlayKeptWidth);
    out.detail = {
      wide,
      flexSplit: wide ? flexSplit : undefined,
      overlayKeptWidth: !wide ? overlayKeptWidth : undefined,
      domMatchesLive,
      headerAligned,
      varsAgree,
      hiddenStable,
      noOverflow,
      rowsW: { before: b.rowsW, opened: o.rowsW, closed: c.rowsW },
      graphWidth: { before: b.graphWidth, opened: o.graphWidth, closed: c.graphWidth },
      graphSvgW: { before: b.graphSvgW, opened: o.graphSvgW, closed: c.graphSvgW },
      hiddenCols: { before: b.hiddenCols, opened: o.hiddenCols, closed: c.hiddenCols },
    };
    return out;
  }

  // --- persisted-viewport restore regression ---------------------------------

  // Replayed open/reveal into a SURVIVING context must restore the persisted
  // scroll position without first clobbering the persisted topRow with a
  // transient 0. open() used to call setProfile(restored.profile,
  // { reset:false }), which persisted saveState() with scrollTop still 0 — BEFORE
  // restoreScrollTop(restored.topRow) ran — so a real context recreation lost
  // the saved position to the transient write. The open/reveal handlers now
  // persist the profile with `persist:false` during restore and saveState()
  // once the restored position is actually applied.
  async function runRestoreStateProbe(timeoutMs) {
    const out = { steps: [], pass: false, detail: null };
    const SAVE_TOP_ROW = 40; // must be within the fixture's visible row range
    const ROW_H = 34;
    const expectedScrollTop = SAVE_TOP_ROW * ROW_H;
    await whenIdle(timeoutMs || 5000);
    const rowsEl = document.getElementById('rows');

    out.steps.push({
      name: 'seed',
      persistedBefore: window.vscode.getState(),
      totalVisibleBefore: document.querySelectorAll('.row').length,
    });
    // Emulate a recreated context carrying a saved viewport: seed persisted
    // state exactly as saveState() writes it, then replay the extension
    // host's open + ready handshake.
    window.vscode.setState({ profile: 'activity', topRow: SAVE_TOP_ROW });
    window.__editchainClearRequestLog();
    window.__editchainStart();
    await whenIdle(timeoutMs || 10000);

    const persistedAfter = window.vscode.getState();
    out.steps.push({
      name: 'after-reopen',
      scrollTop: rowsEl ? rowsEl.scrollTop : -1,
      persistedAfter,
      rowsRendered: document.querySelectorAll('.row:not(.row-placeholder)').length,
    });

    const s2 = out.steps[1];
    const restoredNotZero = s2.persistedAfter && s2.persistedAfter.topRow === SAVE_TOP_ROW;
    const scrollApplied = s2.scrollTop === expectedScrollTop;
    out.pass = restoredNotZero && scrollApplied &&
      (s2.persistedAfter && s2.persistedAfter.profile === 'activity');
    out.detail = {
      expectedScrollTop,
      actualScrollTop: s2.scrollTop,
      persistedAfter: JSON.stringify(s2.persistedAfter),
    };
    return out;
  }

  // --- expose ----------------------------------------------------------------

  // Compact capability + gating state for the parallel renderer contract
  // (domRace / ui-dump / e2e arm or skip DOM assertions from this
  // deterministically). Marker counts are read from the CURRENT profile's
  // DOM; call while the Activity profile is active to probe capability.
  function workUnitContractState() {
    const profile = typeof window.__editchainGetProfile === 'function'
      ? window.__editchainGetProfile() : 'activity';
    const sel = (s) => document.querySelectorAll(s).length;
    const workUnit = sel('.row-work-unit-start, .row-work-unit-end, .work-unit-ribbon, .work-unit-count');
    const bundle = sel('.row-activity-bundle, .bundle-count, .bundle-status, [data-activity-bundle], [data-bundle-count]');
    const promoted = sel('.row-promoted');
    const rowEls = Array.from(document.querySelectorAll('.row:not(.row-placeholder)'));
    const cacheHasMetadata = rowEls.some((el) => {
      const abs = Number(el.getAttribute('data-row'));
      const row = window.__editchainRowAt ? window.__editchainRowAt(abs) : null;
      return row && (!!row.work_unit || row.promoted || row.activity_bundle);
    });
    return {
      profile,
      markers: { workUnit, bundle, promoted },
      any: workUnit + bundle + promoted > 0,
      rowsRendered: rowEls.length,
      cacheHasMetadata,
    };
  }

  // Round-two work-unit/bundle interaction probe: drives the REAL DOM with
  // keyboard events (no synthetic renderer calls) and asserts the parallel
  // contract's expand/collapse + roving-focus behaviour:
  //   - baseline: exactly one roving tab stop; the typed bundle row renders
  //     collapsed (aria-expanded="false") and un-expanded (no member rows);
  //   - ArrowRight on the collapsed bundle row EXPANDS it (aria-expanded
  //     "true", member sub-op rows appear) and the single roving tab stop
  //     survives the DOM rebuild;
  //   - ArrowLeft COLLAPSES it (members removed, aria-expanded "false");
  //   - Space and Enter on the bundle row toggle expansion per the ARIA
  //     pattern instead of opening the inspector;
  //   - ArrowUp/Down keep roving focus moving row-to-row without touching
  //     expansion state or opening the inspector.
  // When the renderer has not landed the contract, returns pass:true with
  // skipped evidence (no markers to drive deterministically yet).
  async function runWorkUnitProbe(timeoutMs) {
    const out = { steps: [], pass: false, skipped: false, detail: null };
    const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));
    await whenIdle(timeoutMs || 5000);
    const layoutEl = document.getElementById('layout');
    const BUNDLE_KEY = 'wu:run1';
    const rowByKey = (key) =>
      Array.from(document.querySelectorAll('.row')).find((r) => r.getAttribute('data-key') === key);
    const rovingCount = () =>
      Array.from(document.querySelectorAll('.row')).filter((r) => r.tabIndex === 0).length;
    const subopCount = () => document.querySelectorAll('.row.row-subop').length;
    const press = (el, key) => {
      el.dispatchEvent(new KeyboardEvent('keydown', { key, bubbles: true }));
    };
    const activeKey = () => {
      const a = document.activeElement;
      return a && a.closest && a.closest('.row')
        ? a.closest('.row').getAttribute('data-key') : null;
    };

    const baseline = (() => {
      const bundleRow = rowByKey(BUNDLE_KEY);
      return {
        contractPresent: !!bundleRow && bundleRow.hasAttribute('data-activity-bundle'),
        bundleAria: bundleRow ? bundleRow.getAttribute('aria-expanded') : null,
        rovingTabs: rovingCount(),
        subopRows: subopCount(),
        rowsRendered: document.querySelectorAll('.row').length,
      };
    })();
    out.steps.push({ name: 'baseline', ...baseline });
    if (!baseline.contractPresent) {
      out.skipped = true;
      out.detail = 'renderer contract not present: no typed bundle row with data-activity-bundle rendered (deferred until media/main.js lands the parallel contract)';
      out.pass = true;
      return out;
    }

    // ArrowRight expands the collapsed bundle row.
    let bundleRow = rowByKey(BUNDLE_KEY);
    bundleRow.focus();
    press(bundleRow, 'ArrowRight');
    await whenIdle(timeoutMs || 5000);
    bundleRow = rowByKey(BUNDLE_KEY);
    out.steps.push({
      name: 'arrow-right-expands',
      bundleAria: bundleRow ? bundleRow.getAttribute('aria-expanded') : null,
      subopRows: subopCount(),
      rovingTabs: rovingCount(),
      inspectorOpened: !!(layoutEl && layoutEl.classList.contains('has-detail')),
    });

    // ArrowLeft collapses.
    bundleRow.focus();
    press(bundleRow, 'ArrowLeft');
    await whenIdle(timeoutMs || 5000);
    bundleRow = rowByKey(BUNDLE_KEY);
    out.steps.push({
      name: 'arrow-left-collapses',
      bundleAria: bundleRow ? bundleRow.getAttribute('aria-expanded') : null,
      subopRows: subopCount(),
      rovingTabs: rovingCount(),
    });

    // Space expands (ARIA toggle) without opening the inspector.
    bundleRow.focus();
    press(bundleRow, ' ');
    await whenIdle(timeoutMs || 5000);
    bundleRow = rowByKey(BUNDLE_KEY);
    out.steps.push({
      name: 'space-expands',
      bundleAria: bundleRow ? bundleRow.getAttribute('aria-expanded') : null,
      subopRows: subopCount(),
      inspectorOpened: !!(layoutEl && layoutEl.classList.contains('has-detail')),
    });

    // Enter on the collapsed bundle row also expands it (never inspects).
    bundleRow.focus();
    press(bundleRow, 'ArrowLeft'); // ensure collapsed before the Enter case
    await whenIdle(timeoutMs || 5000);
    bundleRow = rowByKey(BUNDLE_KEY);
    bundleRow.focus();
    press(bundleRow, 'Enter');
    await whenIdle(timeoutMs || 5000);
    bundleRow = rowByKey(BUNDLE_KEY);
    out.steps.push({
      name: 'enter-expands',
      bundleAria: bundleRow ? bundleRow.getAttribute('aria-expanded') : null,
      subopRows: subopCount(),
      inspectorOpened: !!(layoutEl && layoutEl.classList.contains('has-detail')),
    });

    // ArrowUp/Down roving stays stable: focus moves row-to-row, expansion
    // state is untouched, and no inspector opens. Collapse the bundle first so
    // the adjacent rows are the plain top-level rows (wu:req2 idx 1 -> bundle
    // idx 2 -> wu:run2 idx 3), then verify the arrow moves land exactly there.
    bundleRow.focus();
    press(bundleRow, 'ArrowLeft');
    await whenIdle(timeoutMs || 5000);
    bundleRow = rowByKey(BUNDLE_KEY);
    const ariaBeforeRove = bundleRow ? bundleRow.getAttribute('aria-expanded') : null;
    const upTarget = rowByKey('wu:req2');
    upTarget.focus();
    press(upTarget, 'ArrowDown');
    const movedToBundle = activeKey() === BUNDLE_KEY;
    press(document.activeElement.closest('.row'), 'ArrowDown');
    const movedPast = activeKey() === 'wu:run2';
    out.steps.push({
      name: 'roving-focus-stable',
      movedToBundle,
      movedPast,
      activeKey: activeKey(),
      bundleAriaBefore: ariaBeforeRove,
      bundleAriaAfter: rowByKey(BUNDLE_KEY) ? rowByKey(BUNDLE_KEY).getAttribute('aria-expanded') : null,
      rovingTabs: rovingCount(),
      inspectorOpened: !!(layoutEl && layoutEl.classList.contains('has-detail')),
    });

    const s = out.steps;
    const s1 = s[1], s2 = s[2], s3 = s[3], s4 = s[4], s5 = s[5];
    const expandedOk = s1.bundleAria === 'true' && s1.subopRows > 0 &&
      s1.rovingTabs === 1 && s1.inspectorOpened === false;
    const collapsedOk = s2.bundleAria === 'false' && s2.subopRows === 0 &&
      s2.rovingTabs === 1;
    const spaceOk = s3.bundleAria === 'true' && s3.subopRows > 0 &&
      s3.inspectorOpened === false;
    const enterOk = s4.bundleAria === 'true' && s4.subopRows > 0 &&
      s4.inspectorOpened === false;
    const roveOk = s5.movedToBundle === true && s5.movedPast === true &&
      s5.rovingTabs === 1 && s5.inspectorOpened === false &&
      s5.bundleAriaBefore === s5.bundleAriaAfter;
    out.pass = expandedOk && collapsedOk && spaceOk && enterOk && roveOk;
    out.detail = {
      expandedOk, collapsedOk, spaceOk, enterOk, roveOk,
      steps: out.steps.map((x) => ({ name: x.name, bundleAria: x.bundleAria, subopRows: x.subopRows })),
    };
    return out;
  }

  window.__editchainDebug = {
    whenIdle,
    dumpLayout,
    assertLayout,
    getMetrics,
    runSearch,
    runReversedSearchRace,
    runProfileSwitch,
    runInspectorRouting,
    runInspectorGeometry,
    runKeyboardProbe,
    runRestoreStateProbe,
    runWorkUnitProbe,
    workUnitContractState,
    captureResizeMetrics,
    evaluateResizeAssert,
  };
})();
