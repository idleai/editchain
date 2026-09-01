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
  // Enter, wait for the result list to settle, then single-click the first
  // result to verify selection stays inline. Double-click explicitly requests
  // its JSON editor with the right identity: a Git hit must navigate by
  // (git_oid, repository) — never by its synthetic index-only op_id.
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
    const secondaryPane = !!document.getElementById('detail') ||
      !!(layoutEl && layoutEl.classList.contains('has-detail'));
    const selected = !!document.querySelector('.row.row-selected');
    if (firstRow) firstRow.dispatchEvent(new MouseEvent('dblclick', { bubbles: true }));
    await whenIdle(timeoutMs || 5000);
    window.vscode.postMessage = origPost;
    const clicked = captured.length ? captured[0] : null;
    return {
      resultRows,
      bannerText,
      secondaryPane,
      selected,
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
    const endAtDot = lane === toLane;
    const endAtBoundary = !endAtDot && below.indexOf(toLane) !== -1;
    if (!startAtDot && !startAtBoundary) return null;
    if (!endAtBoundary && !endAtDot) return null;
    return { fromLane, toLane, startAtDot, startAtBoundary, endAtDot, endAtBoundary };
  }

  // Return the single quadratic segment emitted for one transition colour
  // half, or null if the renderer regressed to a multi-segment elbow/jog.
  function quadraticSegment(cmds) {
    if (!cmds || cmds.length !== 2 || cmds[0].cmd !== 'M' || cmds[1].cmd !== 'Q') return null;
    return {
      start: cmds[0].args,
      control: cmds[1].args.slice(0, 2),
      end: cmds[1].args.slice(2, 4),
    };
  }

  // Validate the shape contract independently of lane direction: every half
  // is one convex quadratic, the colour seam is exact and tangent-continuous,
  // external tangents match the connected dot/boundary, and a transition with
  // a node endpoint is one globally non-inflecting convex bow. The bounding
  // box guard rejects outward overshoot/inward hooks.
  function smoothTransitionProblems(srcCmds, dstCmds, anchors) {
    const problems = [];
    const near = (a, b) => Math.abs(a - b) <= 0.02;
    const src = quadraticSegment(srcCmds);
    const dst = quadraticSegment(dstCmds);
    if (!src || !dst) return ['halves must each be one quadratic Bézier'];
    if (src.end[0] !== dst.start[0] || src.end[1] !== dst.start[1]) {
      problems.push('halves must share one exact seam');
      return problems;
    }
    const incoming = [src.end[0] - src.control[0], src.end[1] - src.control[1]];
    const outgoing = [dst.control[0] - dst.start[0], dst.control[1] - dst.start[1]];
    const cross = incoming[0] * outgoing[1] - incoming[1] * outgoing[0];
    const dot = incoming[0] * outgoing[0] + incoming[1] * outgoing[1];
    const scale = Math.max(1,
      Math.hypot(incoming[0], incoming[1]) * Math.hypot(outgoing[0], outgoing[1]));
    if (Math.abs(cross) > scale * 0.01 || dot <= 0) {
      problems.push('colour seam must be tangent-continuous');
    }
    if (anchors.startAtDot && !near(src.start[1], src.control[1])) {
      problems.push('node source must leave on a smooth horizontal tangent');
    }
    if (anchors.startAtBoundary && !near(src.start[0], src.control[0])) {
      problems.push('boundary source must enter on a smooth vertical tangent');
    }
    if (anchors.endAtDot && !near(dst.control[1], dst.end[1])) {
      problems.push('node destination must enter on a smooth horizontal tangent');
    }
    if (anchors.endAtBoundary && !near(dst.control[0], dst.end[0])) {
      problems.push('boundary destination must leave on a smooth vertical tangent');
    }
    const all = [src.start, src.control, src.end, dst.control, dst.end];
    const minX = Math.min(src.start[0], dst.end[0]) - 0.02;
    const maxX = Math.max(src.start[0], dst.end[0]) + 0.02;
    const minY = Math.min(src.start[1], dst.end[1]) - 0.02;
    const maxY = Math.max(src.start[1], dst.end[1]) + 0.02;
    if (all.some((p) => p[0] < minX || p[0] > maxX || p[1] < minY || p[1] > maxY)) {
      problems.push('curve must stay inside its convex endpoint bounds');
    }
    // A quadratic has constant-sign curvature. When one endpoint is a node,
    // both emitted halves are subdivisions of ONE quadratic and therefore must
    // keep the same turn sign (no inflection / concave hook).
    if (anchors.startAtDot !== anchors.endAtDot) {
      const turn = (q) => {
        const a = [q.control[0] - q.start[0], q.control[1] - q.start[1]];
        const b = [q.end[0] - q.control[0], q.end[1] - q.control[1]];
        return a[0] * b[1] - a[1] * b[0];
      };
      const srcTurn = turn(src);
      const dstTurn = turn(dst);
      if (srcTurn * dstTurn < -0.01) problems.push('node transition must be one convex bow');
    }
    return problems;
  }

  // Collect per-row graph geometry (dots, vertical line segments, and smooth
  // transition paths). Each row carries its own small SVG cell, so we
  // aggregate across all rendered rows.
  function describeSvg() {
    const cells = document.querySelectorAll('.graph-cell svg.graphCell');
    if (!cells.length) return { present: false };
    const out = { present: true, cells: cells.length, dots: [], capsules: [], lines: [], transitions: [] };
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
      // Activity execute-run bundles render a capsule + two terminals instead
      // of a node dot; report them so dumps/artifacts count the row's graph
      // node even when it has no graphDot.
      for (const cap of cellSvg.querySelectorAll('rect.graphBundleCapsule')) {
        const terminal = (cls) => {
          const t = Array.from(cellSvg.querySelectorAll('circle.graphBundleTerminal'))
            .find((el) => el.classList.contains(cls));
          return t ? {
            cx: +t.getAttribute('cx'),
            cy: +t.getAttribute('cy'),
            r: +t.getAttribute('r'),
          } : null;
        };
        out.capsules.push({
          row: absIdx,
          x: +cap.getAttribute('x'),
          y: +cap.getAttribute('y'),
          w: +cap.getAttribute('width'),
          h: +cap.getAttribute('height'),
          entry: terminal('graphBundleEntry'),
          exit: terminal('graphBundleExit'),
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
        secondaryPanePresent: !!document.getElementById('detail') ||
          !!(layoutEl && layoutEl.classList.contains('has-detail')),
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
    const layoutEl = document.getElementById('layout');
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

    // Check 1b: the history is one uninterrupted reading surface. Treatments
    // may change hierarchy, never introduce a secondary sibling surface.
    const secondaryPane = document.getElementById('detail');
    const hasSplitState = !!(layoutEl && layoutEl.classList.contains('has-detail'));
    checks.push({
      name: 'SINGLE_PANE_HISTORY',
      pass: !secondaryPane && !hasSplitState,
      detail: !secondaryPane && !hasSplitState
        ? 'no secondary detail pane or split-layout state'
        : 'secondaryElement=' + !!secondaryPane + ' splitState=' + hasSplitState,
    });

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

    // Check 3: every rendered row has a graph node centered on its row. The
    // node is normally the lane `graphDot`; a typed Activity execute-run
    // bundle row replaces it with a `graphBundleCapsule` (the enclosing
    // capsule's centre sits on the same row midpoint). Rows carry an ABSOLUTE
    // `data-row` index (the viewport renders a slice of the full history), so
    // we match by that absolute index rather than by contiguous position.
    if (wrapEl) {
      const rowEls = wrapEl.querySelectorAll('.row');
      let dotsOk = true;
      let firstFail = null;
      rowEls.forEach((row) => {
        // Sub-op rows intentionally draw NO dot (they are not graph nodes), so
        // skip them — only top-level rows must have a centered node.
        if (row.classList.contains('row-subop')) return;
        const absIdx = row.getAttribute('data-row');
        const cellSvg = row.querySelector('.graph-cell svg.graphCell');
        const dot = cellSvg && cellSvg.querySelector('circle.graphDot');
        const capsule = cellSvg && cellSvg.querySelector('rect.graphBundleCapsule');
        // A bundle capsule counts as this row's graph node; a row with
        // neither would break the graph topology.
        const node = dot
          ? { kind: 'dot', cy: +dot.getAttribute('cy') }
          : capsule
            ? {
                kind: 'capsule',
                cy: +capsule.getAttribute('y') + (+capsule.getAttribute('height')) / 2,
              }
            : null;
        if (!node) {
          dotsOk = false;
          firstFail = firstFail || { rowIdx: absIdx, reason: 'no graph node (dot nor bundle capsule)' };
          return;
        }
        const rowBox = row.getBoundingClientRect();
        // node cy is relative to the cell svg, which sits at the row's top.
        const cellTop = cellSvg.getBoundingClientRect().top;
        const nodeCy = cellTop + node.cy;
        const rowCenterY = rowBox.top + rowBox.height / 2;
        const deltaY = Math.abs(nodeCy - rowCenterY);
        if (deltaY > 1.5) { dotsOk = false; firstFail = firstFail || { rowIdx: absIdx, kind: node.kind, deltaY }; }
      });
      checks.push({
        name:'DOT_ROW_ALIGNMENT',
        pass:dotsOk,
        detail:dotsOk ? 'all graph nodes centered on their rows'
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

    // Check 5f: Pulse keeps narrative + date and leaves Author/Commit behind
    // explicit raw activation. Date drops at <=400px so Content keeps readable
    // width. Every visible column must have nonzero width and stay inside #rows;
    // hidden columns must be display:none, never squeezed to a sliver.
    if (wrapEl && !viewMessage) {
      const firstRow = wrapEl.querySelector('.row:not(.row-placeholder)');
      if (firstRow) {
        const rowsBox = rowsEl.getBoundingClientRect();
        const innerW = window.innerWidth || rowsEl.clientWidth || 0;
        const hidden = new Set(['author', 'commit']);
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

      // The section label is background chrome. Its opaque/translucent chip
      // must never mask a lane segment or dot: the graph cell owns the higher
      // stacking layer while the label remains visible beneath it.
      const labelRow = labelEl ? labelEl.closest('.row') : null;
      const graphCell = labelRow ? labelRow.querySelector('.graph-cell') : null;
      const labelZ = computed ? parseInt(computed.zIndex, 10) : 0;
      const graphStyle = graphCell ? getComputedStyle(graphCell) : null;
      const graphZ = graphStyle ? parseInt(graphStyle.zIndex, 10) : 0;
      const graphPositioned = !!graphStyle && graphStyle.position !== 'static';
      checks.push({
        name: 'GROUP_LABEL_BEHIND_GRAPH',
        pass: !labelEl || (!!graphCell && graphPositioned && graphZ > labelZ),
        detail: labelEl
          ? 'label z=' + labelZ + '; graph z=' + graphZ +
            '; graph position=' + (graphStyle ? graphStyle.position : 'missing')
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
    // rows; Raw sends hide_trace=false and renders all 5 rows. Routine
    // successful conversation stays pure prose, while execute activity keeps
    // its quiet semantic "run" kicker.
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
      const badgeOk = !t0Outcome && t0ActCount === 0 && !!t2Act && t2ActText === 'run';
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
          ? 'routine conversation is pure prose; execute "run" kicker rendered'
          : 'badges: t0 outcome=' + (t0Outcome ? 'UNEXPECTED' : 'none') +
            ' t0 act-badges=' + t0ActCount + ' t2 execute=' +
            (t2Act ? '"' + t2ActText + '"' : 'MISSING'),
      });
    }

    // Check 5p (badges scenario only): the EXACT Rust wire taxonomy renders
    // through the renderer whitelists. Common source_control ("git") and
    // success ("ok") badges are retained as code options but default OFF;
    // every other meaningful activity/outcome carries its exact class/text.
    // Legacy tool_call/command/edit/commit/review + error/interrupted vocabulary
    // must never leak back in. Visible outcome colors must track VS Code theme
    // tokens, never fixed colors alone.
    if (window.__editchainScenarioName === 'badges') {
      const expected = {
        'node:b:exec': { act: ['act-execute', 'run'], out: ['outcome-failure', '✕'] },
        'git:b:sc': { act: null, out: null },
        'node:b:chg': { act: ['act-change', 'change'], out: ['outcome-warning', 'warn'] },
        'node:b:plan': { act: ['act-plan', 'plan'], out: ['outcome-neutral', 'cancelled'] },
        'node:b:exp': { act: ['act-explore', 'explore'], out: null },
        'node:b:ver': { act: ['act-verify', 'verify'], out: null },
        'node:b:diag': { act: ['act-diagnose', 'diagnose'], out: ['outcome-failure', '✕'] },
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
        if (exp.act) {
          const actText = actBadge ? (actBadge.textContent || '').trim() : '';
          if (!actBadge || !actBadge.classList.contains(exp.act[0]) || actText !== exp.act[1]) {
            problems.push(key + ' activity badge != ' + exp.act[0] + ' "' + exp.act[1] + '" got ' +
              (actBadge ? actBadge.className + ' "' + actText + '"' : 'none'));
          }
        } else if (actBadge) {
          problems.push(key + ' has unexpected activity badge ' + actBadge.className);
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
        probeTokenDriven('--vscode-editorWarning-foreground', '.out-badge.outcome-warning', '#00ff00') &&
        probeTokenDriven('--vscode-editorError-foreground', '.out-badge.outcome-failure', '#00ff00');
      if (!themeTokensOk) {
        problems.push('outcome badge colors are not driven by VS Code theme tokens');
      }
      checks.push({
        name: 'BADGE_VOCABULARY_COVERAGE',
        pass: problems.length === 0,
        detail: problems.length === 0
          ? '9 uncommon activity kinds + 3 exceptional outcomes exact; git/ok default off; theme-token colors'
          : problems.join('; '),
      });
    }

    // Check 5q: common clean-state chrome is globally quiet by default. This
    // intentionally excludes aggregate bundle completion checks, which occur
    // once per collapsed run rather than repeating on every row.
    if (wrapEl && !viewMessage) {
      const commonBadges = Array.from(wrapEl.querySelectorAll(
        '.act-badge.act-source-control, .out-badge.outcome-success'));
      checks.push({
        name: 'COMMON_ROW_BADGES_DEFAULT_OFF',
        pass: commonBadges.length === 0,
        detail: commonBadges.length === 0
          ? 'no repeated git/ok row badges'
          : commonBadges.length + ' repeated git/ok row badge(s)',
      });
    }

    // Check 5r: Git text before the first colon becomes one exact chip and the
    // delimiter disappears. A Git summary without a colon remains chip-free.
    if (wrapEl && !viewMessage) {
      const problems = [];
      let prefixed = 0;
      let plain = 0;
      for (const el of wrapEl.querySelectorAll('.row')) {
        const abs = Number(el.getAttribute('data-row'));
        const row = window.__editchainRowAt ? window.__editchainRowAt(abs) : null;
        if (!row || !row.git_oid) continue;
        const source = String(row.summary || '');
        const colon = source.indexOf(':');
        const expectedPrefix = colon > 0 ? source.slice(0, colon).trim() : '';
        const chip = el.querySelector('.git-prefix-chip');
        if (expectedPrefix) {
          prefixed++;
          if (!chip || chip.textContent.trim() !== expectedPrefix) {
            problems.push(row.node_key + ': Git prefix chip mismatch');
          } else if ((chip.parentElement.textContent || '').includes(expectedPrefix + ':')) {
            problems.push(row.node_key + ': Git prefix delimiter is still visible');
          }
        } else {
          plain++;
          if (chip) problems.push(row.node_key + ': colon-free Git summary gained a chip');
        }
      }
      checks.push({
        name: 'GIT_PREFIX_CHIP_FIRST_COLON',
        pass: problems.length === 0,
        detail: problems.length === 0
          ? prefixed + ' prefixed and ' + plain + ' plain Git rows render correctly'
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
    // distinct lanes for the two fork branches AND smooth convex cross-lane
    // transitions whose endpoints are ALL connected — every transition anchor
    // must be either this row's node dot or a cell boundary the adjacent row's
    // geometry continues at the same lane x. The production direction is
    // (child_lane, parent_lane), so row 0's reconnect transition runs 0 -> 1
    // (completion lane -> subagent lane), row 2's merge-side transition runs
    // 1 -> 0, and the true fork bends 1 -> 0 in its parent row 4. Assertions:
    //   - dots occupy at least two distinct x positions (two lanes);
    //   - each transition row renders exactly two quadratic path halves sharing
    //     an exact tangent-continuous colour seam;
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
    //   - dot-to-boundary transitions form one non-inflecting convex bow;
    //     pass-through transitions use two convex halves with one smooth seam;
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
      const transitionRows = [0, 2, 4];
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
          const srcStart = pathStart(t.src.cmds);
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
          for (const problem of smoothTransitionProblems(t.src.cmds, t.dst.cmds, anchors)) {
            transitionProblems.push('row ' + row + ' ' + problem);
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
      // smooth transition replaces it, so every remaining graphLine must be
      // a vertical segment.
      Array.from(geometry.values()).forEach((g) => {
        g.lines.forEach((l) => {
          if (Math.abs(l.x1 - l.x2) > 0.5) {
            transitionProblems.push('hard horizontal connector still present');
          }
        });
      });
      checks.push({
        name:'FORK_SMOOTH_CONVEX_TRANSITIONS',
        pass:transitionProblems.length === 0,
        detail: transitionProblems.length === 0
          ? '3 connected transitions: convex Béziers, smooth seams, connected anchors, per-lane colours'
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

    // Deep session branch: the fork belongs in the anchor commit's own row,
    // entering from the lane ABOVE-RIGHT and terminating at the anchor dot.
    // This pins the requested bottom-right corner orientation; placing the
    // transition one row earlier produces the visually reversed curve even
    // though the same two lanes remain connected.
    if (wrapEl && window.__editchainScenarioName === 'sessionBranch') {
      const cells = wrapEl.querySelectorAll('.graph-cell svg.graphCell');
      const geometry = collectRowGeometry(cells);
      const anchorRow = 13;
      const prior = geometry.get(anchorRow - 1);
      const anchor = geometry.get(anchorRow);
      const cached = window.__editchainRowAt ? window.__editchainRowAt(anchorRow) : null;
      const anchors = cached ? transitionAnchors(cached) : null;
      const src = anchor && anchor.src ? quadraticSegment(anchor.src.cmds) : null;
      const dst = anchor && anchor.dst ? quadraticSegment(anchor.dst.cmds) : null;
      const noEarlyCurve = !!prior && !prior.src && !prior.dst;
      const bottomRight = !!(anchor && anchor.dot && anchors && src && dst &&
        anchors.startAtBoundary && anchors.endAtDot &&
        src.start[0] > anchor.dot.x &&
        Math.abs(src.start[1]) <= 0.01 &&
        Math.abs(src.control[0] - src.start[0]) <= 0.02 &&
        Math.abs(dst.end[0] - anchor.dot.x) <= 0.02 &&
        Math.abs(dst.end[1] - anchor.dot.y) <= 0.02 &&
        Math.abs(dst.control[1] - dst.end[1]) <= 0.02);
      const shapeProblems = anchors && anchor && anchor.src && anchor.dst
        ? smoothTransitionProblems(anchor.src.cmds, anchor.dst.cmds, anchors)
        : ['anchor transition missing'];
      checks.push({
        name: 'SESSION_BRANCH_BOTTOM_RIGHT',
        pass: noEarlyCurve && bottomRight && shapeProblems.length === 0,
        detail: noEarlyCurve && bottomRight && shapeProblems.length === 0
          ? 'lane above-right curves smoothly into the anchor dot; trunk continues below'
          : 'noEarlyCurve=' + noEarlyCurve + ' bottomRight=' + bottomRight +
            ' shape=' + shapeProblems.join(', '),
      });
    }

    // Check 8b (highLanes scenario only): every service lane must be drawn
    // INSIDE the graph column — no 128-lane clipping. The fixture has 200
    // lanes (> the former cap): all dot centres must land within their SVG
    // cell's width and more than 128 distinct lane x positions must render.
    //
    // Rows 0..4 form a connected production-like zigzag (lanes 0,1,0,1,0 with
    // adjacent transitions 0->1, 1->0, ...), so the convex Bézier paths are
    // exercised under heavy compression without a radius/fallback mode. Every
    // transition begins at its row's own dot (no synthetic top
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
        const srcStart = pathStart(t.src.cmds);
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
        for (const problem of smoothTransitionProblems(t.src.cmds, t.dst.cmds, anchors)) {
          transitionProblems.push('row ' + row + ' ' + problem);
        }
        if (t.src.stroke !== laneFills.get(fromLane) || t.dst.stroke !== laneFills.get(toLane)) {
          transitionProblems.push('row ' + row + ' colour handoff mismatch');
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
          ? transitionRows + ' compressed zigzag transitions valid (convex Béziers, smooth seams, colours, connected endpoints)'
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
    // below fails the Activity scenario unless ALL four renderer families are
    // present (work-unit boundaries, bundle styling, promotion rails, and the
    // execute-run capsule glyph). This prevents a reverted renderer from
    // turning feature checks into silent skips. Cache-side wire facts are
    // always asserted as well.
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
        any: false, workUnit: false, bundle: false, promoted: false, capsule: false,
      };
      caps.workUnit = countMarkers(
        '.row-work-unit-start, .row-work-unit-end, .work-unit-ribbon, .work-unit-count') > 0;
      caps.bundle = countMarkers(
        '.row-activity-bundle, .bundle-count, .bundle-status, [data-activity-bundle], [data-bundle-count]') > 0;
      caps.promoted = countMarkers('.row-promoted') > 0;
      caps.capsule = countMarkers(
        '.graph-cell rect.graphBundleCapsule, .graph-cell circle.graphBundleTerminal') > 0;
      caps.any = caps.workUnit || caps.bundle || caps.promoted || caps.capsule;
      const contractAbsent = (family) =>
        'renderer contract not present (no ' + family +
        ' DOM markers rendered; wire metadata verified cache-side)';

      if (activeProfile === 'activity') {
        const present = caps.workUnit && caps.bundle && caps.promoted && caps.capsule;
        checks.push({
          name: 'ROUND_TWO_RENDERER_CONTRACT_PRESENT',
          pass: present,
          detail: 'workUnit=' + caps.workUnit + ' bundle=' + caps.bundle +
            ' promoted=' + caps.promoted + ' capsule=' + caps.capsule,
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
      // row). Multi-entry non-Git activities carry the exact view-wide count;
      // Git and single-entry units follow VS Code's native count-free header.
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
              const expectsCount = row.activity_kind !== 'source_control' &&
                row.work_unit.count > 1;
              if (!!countEl !== expectsCount) {
                problems.push(row.node_key + ': count DOM=' + !!countEl +
                  ' expected=' + expectsCount);
              } else if (countEl) {
                const text = (countEl.textContent || '').trim();
                if (parseCount(text) !== row.work_unit.count) {
                  problems.push(row.node_key + ': count text "' + text + '" != wire ' + row.work_unit.count);
                }
                if (!/entr(?:y|ies)$/.test(text)) {
                  problems.push(row.node_key + ': count is not labelled as entries: "' + text + '"');
                }
              }
              // The ribbon leads with the unit title: the DTO title when
              // present (t1/t2), else the renderer's short fallback label
              // (ops -> "Git") — never the raw opaque unit id.
              const ribbonText = (ribbonEls[0].textContent || '').trim();
              const expectedTitle = row.work_unit.title
                ? row.work_unit.title
                  .replace(/`+([^`]+)`+/g, '$1')
                  .replace(/\*\*([^*]+)\*\*/g, '$1')
                  .replace(/__([^_]+)__/g, '$1')
                  .replace(/~~([^~]+)~~/g, '$1')
                  .trim()
                : (row.activity_kind === 'source_control' ? 'Git' : null);
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

      // Check C2: model and agent provenance render once per visible session
      // run, with exact labels from the typed `session_meta` DTO. Rows inside
      // the run keep an empty slot so virtualization can promote a new window
      // boundary without reconstructing the row compositor.
      {
        const problems = [];
        let previousGroup = null;
        for (const el of rowEls) {
          const row = cachedRow(el);
          if (!row) continue;
          const meta = row.session_meta || null;
          const atBoundary = previousGroup === null || row.group !== previousGroup;
          const expects = !!meta && !row.is_subop && atBoundary;
          const model = el.querySelector('.session-chip-model');
          const agent = el.querySelector('.session-chip-agent');
          if (!!model !== (expects && !!meta.model_provider)) {
            problems.push(row.node_key + ': model chip boundary mismatch');
          }
          if (!!agent !== (expects && !!meta.agent_nickname)) {
            problems.push(row.node_key + ': agent chip boundary mismatch');
          }
          if (model && model.textContent.trim() !== meta.model_provider) {
            problems.push(row.node_key + ': model chip text mismatch');
          }
          if (agent && agent.textContent.trim() !== meta.agent_nickname) {
            problems.push(row.node_key + ': agent chip text mismatch');
          }
          previousGroup = row.group;
        }
        checks.push({
          name: 'SESSION_META_CHIPS_AT_BOUNDARY',
          pass: problems.length === 0,
          detail: problems.length === 0
            ? 'model and agent chips are exact and boundary-sparse'
            : problems.join('; '),
        });
      }

      // Check D: ONLY typed execute-run rows render bundle styling, with the
      // exact typed member count (data attr + .bundle-count text). Structured
      // success gets one quiet check; unknown outcome gets no status wording.
      // Ordinary and unknown-kind rows stay bare.
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
              const statusText = statusEl ? (statusEl.textContent || '').trim() : '';
              if (row.outcome === 'success') {
                if (!statusEl || statusText !== '✓' ||
                    !statusEl.classList.contains('bundle-status-success')) {
                  problems.push(row.node_key + ': all-success bundle missing .bundle-status-success');
                }
              } else if (statusEl) {
                problems.push(row.node_key + ': unknown-outcome bundle must not render status wording');
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

      // Check E2: execute-run capsule gating is driven by the TYPED metadata
      // exactly. Only rows whose cached activity_bundle.kind === 'execute-run'
      // render the capsule glyph set: exactly one rect.graphBundleCapsule,
      // exactly two circle.graphBundleTerminal (one .graphBundleEntry, one
      // .graphBundleExit), and NO circle.graphDot. The unknown-kind bundle row
      // (coerced to 'unknown' on the wire), the summary look-alike execsub
      // row, and every ordinary row stay bare.
      if (activeProfile === 'activity') {
        if (!caps.capsule) {
          checks.push({ name: 'BUNDLE_CAPSULE_TYPED_EXACT', pass: true, detail: contractAbsent('capsule') });
        } else {
          const problems = [];
          let typedRows = 0;
          for (const el of rowEls) {
            const row = cachedRow(el);
            const b = row ? row.activity_bundle : null;
            const svg = el.querySelector('.graph-cell svg.graphCell');
            const rects = svg ? svg.querySelectorAll('rect.graphBundleCapsule') : [];
            const terms = svg ? svg.querySelectorAll('circle.graphBundleTerminal') : [];
            const typed = !!(b && b.kind === 'execute-run');
            if (typed) {
              typedRows++;
              if (rects.length !== 1) {
                problems.push(row.node_key + ': typed execute-run row must render exactly 1 capsule rect, got ' + rects.length);
              }
              if (terms.length !== 2) {
                problems.push(row.node_key + ': typed execute-run row must render exactly 2 terminals, got ' + terms.length);
              } else {
                const entry = Array.from(terms).filter((t) => t.classList.contains('graphBundleEntry')).length;
                const exit = Array.from(terms).filter((t) => t.classList.contains('graphBundleExit')).length;
                if (entry !== 1) problems.push(row.node_key + ': expected 1 .graphBundleEntry terminal, got ' + entry);
                if (exit !== 1) problems.push(row.node_key + ': expected 1 .graphBundleExit terminal, got ' + exit);
              }
              if (svg && svg.querySelector('circle.graphDot')) {
                problems.push(row.node_key + ': typed execute-run row must not render a graphDot');
              }
            } else if (rects.length || terms.length) {
              problems.push(row.node_key + ': untyped row leaks capsule markup (rects=' + rects.length +
                ' terminals=' + terms.length + ' typed metadata: ' + JSON.stringify(b) + ')');
            }
          }
          if (isSmall && typedRows !== 2) {
            problems.push('expected exactly 2 typed execute-run capsule rows, got ' + typedRows);
          }
          checks.push({
            name: 'BUNDLE_CAPSULE_TYPED_EXACT',
            pass: problems.length === 0,
            detail: problems.length === 0
              ? (isSmall ? '2/2 ' : '') +
                'typed execute-run rows carry one capsule + entry/exit terminals and no dot; unknown-kind + ordinary rows bare'
              : problems.join('; '),
          });
        }
      }

      // Check E3: capsule geometry. Both terminals sit on the row's lane x
      // (the same x the neighbouring ordinary rows' dots use for that lane);
      // the entry terminal is above the row midpoint and the exit terminal
      // below it (entry y < HALF_H < exit y); the capsule rect encloses both
      // terminal circles.
      if (activeProfile === 'activity') {
        if (!caps.capsule) {
          checks.push({ name: 'BUNDLE_CAPSULE_GEOMETRY', pass: true, detail: contractAbsent('capsule') });
        } else {
          const ROW_H = 34;
          const HALF_H = ROW_H / 2;
          const near = (a, b) => Math.abs(a - b) <= 0.01;
          // Lane x anchors come from ordinary rows' node dots (a bundle row
          // carries terminals instead, so its lane x must equal the dots of
          // other rows in the same lane).
          const laneDotX = new Map();
          for (const el of rowEls) {
            const row = cachedRow(el);
            const dot = el.querySelector('.graph-cell svg.graphCell circle.graphDot');
            if (row && dot) laneDotX.set(row.lane, +dot.getAttribute('cx'));
          }
          const problems = [];
          for (const el of rowEls) {
            const row = cachedRow(el);
            const b = row ? row.activity_bundle : null;
            if (!(b && b.kind === 'execute-run')) continue;
            const svg = el.querySelector('.graph-cell svg.graphCell');
            const rect = svg && svg.querySelector('rect.graphBundleCapsule');
            const entry = svg && svg.querySelector('circle.graphBundleTerminal.graphBundleEntry');
            const exit = svg && svg.querySelector('circle.graphBundleTerminal.graphBundleExit');
            if (!rect || !entry || !exit) {
              problems.push(row.node_key + ': capsule geometry missing (rect/entry/exit)');
              continue;
            }
            const x = +rect.getAttribute('x');
            const y = +rect.getAttribute('y');
            const w = +rect.getAttribute('width');
            const h = +rect.getAttribute('height');
            const eCx = +entry.getAttribute('cx');
            const eCy = +entry.getAttribute('cy');
            const eR = +entry.getAttribute('r');
            const xCx = +exit.getAttribute('cx');
            const xCy = +exit.getAttribute('cy');
            const xR = +exit.getAttribute('r');
            if (!near(eCx, xCx)) {
              problems.push(row.node_key + ': terminals do not share the row lane x (' + eCx + ' vs ' + xCx + ')');
            }
            const laneX = laneDotX.get(row.lane);
            if (laneX !== undefined && (!near(eCx, laneX) || !near(xCx, laneX))) {
              problems.push(row.node_key + ': terminal x=' + eCx + ' != lane x=' + laneX + ' (lane ' + row.lane + ')');
            }
            if (!(eCy < HALF_H && xCy > HALF_H)) {
              problems.push(row.node_key + ': entry y=' + eCy + ' must be < HALF_H and exit y=' + xCy + ' must be > HALF_H');
            }
            const encloses = (cx, cy, r) =>
              x - 0.01 <= cx - r && cx + r <= x + w + 0.01 &&
              y - 0.01 <= cy - r && cy + r <= y + h + 0.01;
            if (!encloses(eCx, eCy, eR) || !encloses(xCx, xCy, xR)) {
              problems.push(row.node_key + ': capsule (' + x + ',' + y + ',' + w + 'x' + h +
                ') does not enclose terminals (' + eCx + ',' + eCy + ') and (' + xCx + ',' + xCy + ')');
            }
          }
          checks.push({
            name: 'BUNDLE_CAPSULE_GEOMETRY',
            pass: problems.length === 0,
            detail: problems.length === 0
              ? 'terminals share the lane x, entry < HALF_H < exit, capsule encloses both'
              : problems.join('; '),
          });
        }
      }

      // Check E4: ordinary-row exclusion — every top-level row that is NOT a
      // typed execute-run bundle keeps its graphDot and renders zero capsule
      // markup (sub-op rows draw no node at all, as always).
      if (activeProfile === 'activity') {
        const problems = [];
        for (const el of rowEls) {
          if (el.classList.contains('row-subop')) continue;
          const row = cachedRow(el);
          const b = row ? row.activity_bundle : null;
          if (b && b.kind === 'execute-run') continue;
          const svg = el.querySelector('.graph-cell svg.graphCell');
          const dot = svg && svg.querySelector('circle.graphDot');
          if (!dot) problems.push(row.node_key + ': ordinary row lost its graphDot');
          if (svg && (svg.querySelector('rect.graphBundleCapsule') ||
                      svg.querySelector('circle.graphBundleTerminal'))) {
            problems.push(row.node_key + ': ordinary row leaks capsule markup');
          }
        }
        checks.push({
          name: 'BUNDLE_CAPSULE_ORDINARY_EXCLUDED',
          pass: problems.length === 0,
          detail: problems.length === 0
            ? 'ordinary + unknown-kind rows keep their graphDot and carry zero capsule glyphs'
            : problems.join('; '),
        });
      }

      // Check E4b: bounded 34px row geometry — the capsule rect and both
      // terminals stay inside their 34px SVG cell (no glyph escapes the row's
      // vertical band or the graph column), and the bundle row itself stays
      // exactly ROW_H tall like every other row.
      if (activeProfile === 'activity') {
        const ROW_H = 34;
        const problems = [];
        for (const el of rowEls) {
          const row = cachedRow(el);
          const b = row ? row.activity_bundle : null;
          if (!(b && b.kind === 'execute-run')) continue;
          const rowH = el.getBoundingClientRect().height;
          if (Math.abs(rowH - ROW_H) > 0.5) {
            problems.push(row.node_key + ': bundle row height ' + rowH + ' != ' + ROW_H);
          }
          const svg = el.querySelector('.graph-cell svg.graphCell');
          if (!svg) { problems.push(row.node_key + ': bundle row has no graph cell'); continue; }
          const cellW = +svg.getAttribute('width');
          const cellH = +svg.getAttribute('height');
          if (Math.abs(cellH - ROW_H) > 0.01) {
            problems.push(row.node_key + ': graph cell height ' + cellH + ' != ' + ROW_H);
          }
          const rect = svg.querySelector('rect.graphBundleCapsule');
          if (rect) {
            const x = +rect.getAttribute('x');
            const y = +rect.getAttribute('y');
            const w = +rect.getAttribute('width');
            const h = +rect.getAttribute('height');
            if (y < -0.01 || y + h > ROW_H + 0.01) {
              problems.push(row.node_key + ': capsule escapes the 34px cell vertically (y=' + y + ' h=' + h + ')');
            }
            if (x < -0.01 || x + w > cellW + 0.01) {
              problems.push(row.node_key + ': capsule escapes the graph column (x=' + x + ' w=' + w + ' cellW=' + cellW + ')');
            }
          }
          svg.querySelectorAll('circle.graphBundleTerminal').forEach((t) => {
            const cy = +t.getAttribute('cy');
            const cx = +t.getAttribute('cx');
            if (cy < -0.01 || cy > ROW_H + 0.01) {
              problems.push(row.node_key + ': terminal escapes the 34px cell vertically (cy=' + cy + ')');
            }
            if (cx < -0.01 || cx > cellW + 0.01) {
              problems.push(row.node_key + ': terminal escapes the graph column (cx=' + cx + ' cellW=' + cellW + ')');
            }
          });
        }
        checks.push({
          name: 'BUNDLE_ROW_GEOMETRY_BOUNDED',
          pass: problems.length === 0,
          detail: problems.length === 0
            ? 'capsule + terminals bounded inside the 34px cell; bundle rows stay 34px tall'
            : problems.join('; '),
        });
      }

      // Check E5: Raw zero leakage — the Raw profile renders NONE of the
      // capsule glyphs anywhere even though its cached rows carry the wire
      // metadata; raw rows keep their graph dots.
      if (activeProfile === 'raw') {
        const capsules = countMarkers('.graph-cell rect.graphBundleCapsule');
        const terminals = countMarkers('.graph-cell circle.graphBundleTerminal');
        const dots = countMarkers('.graph-cell circle.graphDot');
        const leakFree = capsules === 0 && terminals === 0 && dots >= 1;
        checks.push({
          name: 'BUNDLE_CAPSULE_RAW_ZERO_LEAKAGE',
          pass: leakFree,
          detail: leakFree
            ? 'raw renders zero capsule/terminal glyphs (' + dots + ' dots kept)'
            : 'raw leaked capsule DOM: capsules=' + capsules + ' terminals=' + terminals + ' dots=' + dots,
        });
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
          el.querySelector('.graph-cell rect.graphBundleCapsule, .graph-cell circle.graphBundleTerminal') !== null ||
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

  // --- single-pane activation vs disclosure routing ------------------------

  // A row click must SELECT inline without fetching details or opening another
  // surface. Double-click is the explicit raw-JSON editor activation; both the
  // explicit chevron and its full parent row toggle bundled sub-ops.
  async function runSinglePaneRouting(timeoutMs) {
    const out = { steps: [] };
    const captured = [];
    const origPost = window.vscode.postMessage.bind(window.vscode);
    window.vscode.postMessage = function (msg) {
      if (msg && msg.type === 'openJson') captured.push(msg);
      return origPost(msg);
    };
    await whenIdle(timeoutMs || 5000);
    const rowEls = Array.from(document.querySelectorAll('.row:not(.row-placeholder)'));
    const rawCapable = rowEls.find((el) => {
      const abs = Number(el.getAttribute('data-row'));
      const row = window.__editchainRowAt ? window.__editchainRowAt(abs) : null;
      return row && (row.op_id || row.git_oid) && !(row.sub_ops || []).length;
    });
    if (!rawCapable) throw new Error('no raw-JSON-capable rendered row');
    const key = rawCapable.getAttribute('data-key');
    const absIdx = Number(rawCapable.getAttribute('data-row'));
    const row = window.__editchainRowAt(absIdx);
    const objectReqBefore = (window.__editchainRequestLog || [])
      .filter((b) => b && (b.GetNodeDetails !== undefined || b.ResolveObject !== undefined)).length;
    rawCapable.click();
    await whenIdle(timeoutMs || 5000);
    const layoutEl = document.getElementById('layout');
    const objectReqAfterClick = (window.__editchainRequestLog || [])
      .filter((b) => b && (b.GetNodeDetails !== undefined || b.ResolveObject !== undefined));
    const selected = document.querySelector('.row.row-selected');
    out.steps.push({
      name: 'row-click',
      key,
      secondaryPane: !!document.getElementById('detail') ||
        !!(layoutEl && layoutEl.classList.contains('has-detail')),
      editorOpened: captured.length > 0,
      objectRequestIssued: objectReqAfterClick.length > objectReqBefore,
      selectedKey: selected ? selected.getAttribute('data-key') : null,
    });

    rawCapable.dispatchEvent(new MouseEvent('dblclick', { bubbles: true }));
    await whenIdle(timeoutMs || 5000);
    const activation = captured[captured.length - 1] || null;
    const isGit = !!(row && row.git_oid);
    out.steps.push({
      name: 'double-click-raw',
      editorOpened: captured.length === 1,
      identityMatches: !!activation && (isGit
        ? activation.git_oid === row.git_oid && activation.repository === row.repository && !activation.op_id
        : activation.op_id === row.op_id && !activation.git_oid),
      secondaryPane: !!document.getElementById('detail') ||
        !!(layoutEl && layoutEl.classList.contains('has-detail')),
    });

    // Disclosure routing: on a combined row, chevron and full-row clicks must
    // toggle without opening raw JSON or another pane. Re-query around each
    // interaction because expansion rebuilds the rows.
    const chevronBtn = document.querySelector('.row .subop-chevron');
    if (chevronBtn) {
      const chevronRow = chevronBtn.closest('.row');
      const abs = Number(chevronRow.getAttribute('data-row'));
      const cRow = window.__editchainRowAt ? window.__editchainRowAt(abs) : null;
      const hadSubops = !!cRow && (cRow.sub_ops || []).length > 0;
      const expandedBefore = chevronRow.getAttribute('aria-expanded');
      const chevronKey = chevronRow.getAttribute('data-key');
      const activationsBeforeChevron = captured.length;
      chevronBtn.click();
      await whenIdle(timeoutMs || 5000);
      const freshRow = Array.from(document.querySelectorAll('.row')).find(
        (r) => r.getAttribute('data-key') === chevronKey
      );
      const expandedAfter = freshRow ? freshRow.getAttribute('aria-expanded') : null;
      const subopRows = document.querySelectorAll('.row.row-subop').length;
      const activationsBeforeDoubleClick = captured.length;
      const freshChevron = freshRow && freshRow.querySelector('.subop-chevron');
      if (freshChevron) freshChevron.dispatchEvent(new MouseEvent('dblclick', { bubbles: true }));
      if (freshRow) freshRow.click();
      await whenIdle(timeoutMs || 5000);
      const afterRowClick = Array.from(document.querySelectorAll('.row')).find(
        (r) => r.getAttribute('data-key') === chevronKey
      );
      const expandedAfterRowClick = afterRowClick
        ? afterRowClick.getAttribute('aria-expanded')
        : null;
      out.steps.push({
        name: 'disclosure-routing',
        hadSubops,
        expandedBefore,
        expandedAfter,
        subopRows,
        rawActivationChanged: captured.length !== activationsBeforeChevron,
        chevronDoubleClickOpenedRaw: captured.length !== activationsBeforeDoubleClick,
        secondaryPane: !!document.getElementById('detail') ||
          !!(layoutEl && layoutEl.classList.contains('has-detail')),
        toggled: hadSubops && expandedBefore !== expandedAfter,
        rowToggled: expandedAfter !== expandedAfterRowClick,
      });
    } else {
      out.steps.push({
        name: 'disclosure-routing',
        skipped: true,
        detail: 'no combined row rendered in this scenario (skipped)',
      });
    }

    window.vscode.postMessage = origPost;

    const s1 = out.steps[0];
    const s2 = out.steps[1];
    const s3 = out.steps[2];
    out.pass =
      s1.secondaryPane === false &&
      s1.editorOpened === false &&
      s1.objectRequestIssued === false &&
      s1.selectedKey === s1.key &&
      s2.editorOpened === true &&
      s2.identityMatches === true &&
      s2.secondaryPane === false &&
      (s3.skipped || (s3.toggled === true &&
        s3.rowToggled === true &&
        s3.rawActivationChanged === false &&
        s3.chevronDoubleClickOpenedRaw === false && s3.secondaryPane === false));
    return out;
  }

  // --- keyboard activation --------------------------------------------------

  // Enter on a focused ordinary row opens raw JSON; Space on a combined row
  // toggles its bundled sub-ops; the chevron button stays natively operable.
  async function runKeyboardProbe(timeoutMs) {
    const out = { steps: [] };
    const captured = [];
    const origPost = window.vscode.postMessage.bind(window.vscode);
    window.vscode.postMessage = function (msg) {
      if (msg && msg.type === 'openJson') captured.push(msg);
      return origPost(msg);
    };
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
    const rawCapable = (el) => {
      const abs = Number(el.getAttribute('data-row'));
      const row = window.__editchainRowAt ? window.__editchainRowAt(abs) : null;
      return row && (row.op_id || row.git_oid) && !(row.sub_ops || []).length;
    };
    const target = rawCapable(rovingRow) ? rovingRow : rowEls.find(rawCapable);
    if (!target) throw new Error('no keyboard-capable rendered row');

    target.focus();
    target.dispatchEvent(new KeyboardEvent('keydown', { key: 'Enter', bubbles: true }));
    await whenIdle(timeoutMs || 5000);
    const enterActivated = captured.length === 1;
    const enterSelected = !!document.querySelector('.row.row-selected');
    const secondaryPane = !!document.getElementById('detail') ||
      !!(layoutEl && layoutEl.classList.contains('has-detail'));
    out.steps.push({ name: 'enter-activates', enterActivated, enterSelected, secondaryPane });

    // Roving navigation: ArrowDown must move focus to the NEXT rendered row and
    // re-pin the single tab stop to it (the previous row drops to -1).
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
    window.vscode.postMessage = origPost;
    out.pass =
      s0.gridOk === true &&
      s0.rovingRows === 1 &&
      s1.enterActivated === true &&
      s1.enterSelected === true &&
      s1.secondaryPane === false &&
      (s2.skipped || s2.moved === true) &&
      (s3.skipped || s3.toggled === true) &&
      (s4.skipped || s4.pinned === true);
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
    const capsule = sel('.graph-cell rect.graphBundleCapsule, .graph-cell circle.graphBundleTerminal');
    const rowEls = Array.from(document.querySelectorAll('.row:not(.row-placeholder)'));
    const cacheHasMetadata = rowEls.some((el) => {
      const abs = Number(el.getAttribute('data-row'));
      const row = window.__editchainRowAt ? window.__editchainRowAt(abs) : null;
      return row && (!!row.work_unit || row.promoted || row.activity_bundle);
    });
    return {
      profile,
      markers: { workUnit, bundle, promoted, capsule },
      any: workUnit + bundle + promoted + capsule > 0,
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
  //     pattern instead of opening another surface;
  //   - ArrowUp/Down keep roving focus moving row-to-row without touching
  //     expansion state or opening another surface.
  // When the renderer has not landed the contract, returns pass:true with
  // skipped evidence (no markers to drive deterministically yet).
  async function runWorkUnitProbe(timeoutMs) {
    const out = { steps: [], pass: false, skipped: false, detail: null };
    const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));
    await whenIdle(timeoutMs || 5000);
    const layoutEl = document.getElementById('layout');
    const secondaryPanePresent = () => !!document.getElementById('detail') ||
      !!(layoutEl && layoutEl.classList.contains('has-detail'));
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
      secondaryPane: secondaryPanePresent(),
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

    // Space expands (ARIA toggle) without opening another surface.
    bundleRow.focus();
    press(bundleRow, ' ');
    await whenIdle(timeoutMs || 5000);
    bundleRow = rowByKey(BUNDLE_KEY);
    out.steps.push({
      name: 'space-expands',
      bundleAria: bundleRow ? bundleRow.getAttribute('aria-expanded') : null,
      subopRows: subopCount(),
      secondaryPane: secondaryPanePresent(),
    });

    // Enter on the collapsed bundle row also expands it inline.
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
      secondaryPane: secondaryPanePresent(),
    });

    // ArrowUp/Down roving stays stable: focus moves row-to-row, expansion
    // state is untouched, and no secondary surface opens. Collapse the bundle first so
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
      secondaryPane: secondaryPanePresent(),
    });

    const s = out.steps;
    const s1 = s[1], s2 = s[2], s3 = s[3], s4 = s[4], s5 = s[5];
    const expandedOk = s1.bundleAria === 'true' && s1.subopRows > 0 &&
      s1.rovingTabs === 1 && s1.secondaryPane === false;
    const collapsedOk = s2.bundleAria === 'false' && s2.subopRows === 0 &&
      s2.rovingTabs === 1;
    const spaceOk = s3.bundleAria === 'true' && s3.subopRows > 0 &&
      s3.secondaryPane === false;
    const enterOk = s4.bundleAria === 'true' && s4.subopRows > 0 &&
      s4.secondaryPane === false;
    const roveOk = s5.movedToBundle === true && s5.movedPast === true &&
      s5.rovingTabs === 1 && s5.secondaryPane === false &&
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
    runSinglePaneRouting,
    runKeyboardProbe,
    runRestoreStateProbe,
    runWorkUnitProbe,
    workUnitContractState,
    captureResizeMetrics,
    evaluateResizeAssert,
  };
})();
