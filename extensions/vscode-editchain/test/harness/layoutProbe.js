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

  // Drive the real renderer's chain-filter controls and capture the resulting
  // row keys. In the fixture harness the bridge responds synchronously inside
  // postMessage, so each step settles before the next read; a microtask tick
  // keeps the reads deterministic regardless of harness timing.
  async function runFilterProbe() {
    const out = { steps: [] };
    const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));
    const rowKeys = () => Array.from(document.querySelectorAll('.row[data-key]'))
      .map((r) => r.getAttribute('data-key'));
    const filterInput = document.getElementById('filter');
    const messagesToggle = document.getElementById('hideSystem');

    // 1. Chain-filter pattern HIDES matching rows (endpoints preserved).
    const beforeHide = rowKeys();
    filterInput.value = 'commit ';
    filterInput.dispatchEvent(new KeyboardEvent('keydown', { key: 'Enter', bubbles: true }));
    await sleep(0);
    const hideKeys = rowKeys();
    out.steps.push({ name: 'hide-pattern', beforeHide, hideKeys });

    // 2. Clearing the filter restores the full window.
    filterInput.value = '';
    filterInput.dispatchEvent(new KeyboardEvent('keydown', { key: 'Enter', bubbles: true }));
    await sleep(0);
    const restoredKeys = rowKeys();
    out.steps.push({ name: 'restore', restoredKeys });

    // 3. "Show messages only" is an INCLUSIVE kind constraint: every
    //    non-message/non-command row is excluded, including endpoints.
    const beforeMessagesOnly = rowKeys();
    if (messagesToggle) {
      messagesToggle.checked = true;
      messagesToggle.dispatchEvent(new Event('change', { bubbles: true }));
      await sleep(0);
      const messagesOnlyKeys = rowKeys();
      out.steps.push({ name: 'messages-only', beforeMessagesOnly, messagesOnlyKeys });
      messagesToggle.checked = false;
      messagesToggle.dispatchEvent(new Event('change', { bubbles: true }));
      await sleep(0);
      out.steps.push({ name: 'messages-only-restored', keys: rowKeys() });
    }
    return out;
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

  // Deterministic stale-response race: the `undated` fixture (3 rows) is loaded
  // with the loader paused and the FIRST GetWindow response HELD (controlled
  // completion — no sleep-based timing). Before it is released, "Hide undated"
  // is toggled (a new view generation). The held response belongs to the OLD
  // view; releasing it after the toggle must leave it rejected so the table
  // shows only the filtered rows (u1, u3) once the current view's window
  // arrives. Without stale-response rejection the old 3-row window would be
  // applied and displayed under the new filter state (cache/total poisoning).
  async function runStaleResponseRace() {
    const out = { steps: [] };
    const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));
    window.__editchainPauseLoader = true;
    // Hold the first GetWindow response; release it explicitly below.
    window.__editchainHoldWindow = {};
    // Start a fresh view fetch. `resetAndRefetch` clears the renderer cache
    // and issues the offset-0 window immediately, so the FIRST window of the
    // new view is the held one (a replayed `open` would leave the previous
    // load's cache in place and fetchWindow would find nothing missing).
    resetAndRefetch();
    const holdDeadline = Date.now() + 3000;
    while (!window.__editchainHoldWindow.release) {
      if (Date.now() > holdDeadline) throw new Error('stale-race: window response was never held');
      await sleep(5);
    }
    const releaseStale = window.__editchainHoldWindow.release;
    window.__editchainHoldWindow = null;
    // Toggle the filter while the window is still in flight (new view gen).
    const toggle = document.getElementById('hideUndated');
    toggle.checked = true;
    toggle.dispatchEvent(new Event('change', { bubbles: true }));
    // Release the stale response, then wait for the current view's replacement
    // window to arrive and render (controlled completion — no fixed sleeps).
    releaseStale();
    await whenIdle(3000);
    const keys = Array.from(document.querySelectorAll('.row[data-key]'))
      .map((r) => r.getAttribute('data-key'));
    const total = window.__editchainGetTotal ? window.__editchainGetTotal() : -1;
    const clean = keys.length === 2 &&
      keys.indexOf('node:u:1') !== -1 &&
      keys.indexOf('node:u:3') !== -1 &&
      total === 2;
    out.steps.push({
      keys,
      total,
      dataReady: dataReady(),
      staleRejected: clean,
    });
    window.__editchainPauseLoader = false;
    return out;
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

  // Collect per-row graph geometry (dots + line segments). Each row carries its
  // own small SVG cell, so we aggregate across all rendered rows.
  function describeSvg() {
    const cells = document.querySelectorAll('.graph-cell svg.graphCell');
    if (!cells.length) return { present: false };
    const out = { present: true, cells: cells.length, dots: [], lines: [] };
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
    });
    return out;
  }

  function dumpLayout(scope) {
    scope = scope || '#rows';
    const rootEl = document.querySelector(scope);
    const rowsEl = document.getElementById('rows');
    const layoutEl = document.getElementById('layout');
    return {
      viewport: { w: window.innerWidth, h: window.innerHeight, dpr: window.devicePixelRatio },
      state: {
        scenario: window.__editchainScenarioName || 'unknown',
        status: inFlight === 0 ? 'idle' : 'busy',
        generation,
        dataReady: dataReady(),
        placeholders: hasPlaceholders(),
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
    // distinct lanes for the two fork branches AND a cross-lane merge connector
    // where the completion result reconnects into the subagent branch.
    //   - dots occupy at least two distinct x positions (two lanes);
    //   - there is at least one horizontal `graphLine` (y1 == y2) spanning the
    //     two lanes (= the reconnect merge jog).
    if (wrapEl && window.__editchainScenarioName === 'fork') {
      const cells = wrapEl.querySelectorAll('.graph-cell svg.graphCell');
      // Distinct dot x-centres == distinct lanes.
      const dotXs = new Set(Array.from(cells).map((cell) => {
        const dot = cell.querySelector('circle.graphDot');
        return dot ? +dot.getAttribute('cx') : null;
      }).filter((x) => x !== null));
      let horizontal = false;
      cells.forEach((cell) => {
        cell.querySelectorAll('line.graphLine').forEach((l) => {
          const y1 = +l.getAttribute('y1'), y2 = +l.getAttribute('y2');
          if (Math.abs(y1 - y2) <= 0.5 && Math.abs(+l.getAttribute('x1') - +l.getAttribute('x2')) > 1) {
            horizontal = true; // a horizontal connector across lanes
          }
        });
      });
      checks.push({
        name:'FORK_DISTINCT_LANES',
        pass:dotXs.size >= 2,
        detail:'distinct lanes (x in px) = ' + JSON.stringify(Array.from(dotXs)),
      });
      checks.push({
        name:'FORK_RECONNECT_MERGE',
        pass:horizontal,
        detail: horizontal ? 'cross-lane merge connector present' : 'no horizontal connector found',
      });
    }

    // Check 8b (highLanes scenario only): every service lane must be drawn
    // INSIDE the graph column — no 128-lane clipping. The fixture has 200
    // lanes (> the former cap): all dot centres must land within their SVG
    // cell's width and more than 128 distinct lane x positions must render.
    if (wrapEl && window.__editchainScenarioName === 'highLanes') {
      const cells = wrapEl.querySelectorAll('.graph-cell svg.graphCell');
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
    }

    // Check 5e3 (linear scenario only): the chain-filter input HIDES matching
    // rows server-side (endpoints preserved). "commit " matches every row in
    // the 4-commit chain; the two intermediates are hidden while the newest
    // root and oldest leaf stay. Clearing the input restores the full window.
    if (window.__editchainScenarioName === 'linear') {
      const filterInput = document.getElementById('filter');
      const rowKeys = () => Array.from(document.querySelectorAll('.row[data-key]'))
        .map((r) => r.getAttribute('data-key'));
      const before = rowKeys();
      filterInput.value = 'commit ';
      filterInput.dispatchEvent(new KeyboardEvent('keydown', { key: 'Enter', bubbles: true }));
      const hidden = rowKeys();
      const hideDirection =
        before.length === 4 &&
        hidden.length === 2 &&
        hidden.indexOf('git:d') !== -1 &&
        hidden.indexOf('git:a') !== -1 &&
        hidden.indexOf('git:c') === -1 &&
        hidden.indexOf('git:b') === -1;
      checks.push({
        name: 'FILTER_HIDE_DIRECTION',
        pass: hideDirection,
        detail: hideDirection
          ? 'endpoints git:d/git:a kept, intermediates hidden'
          : 'before=' + JSON.stringify(before) + ' after=' + JSON.stringify(hidden),
      });
      filterInput.value = '';
      filterInput.dispatchEvent(new KeyboardEvent('keydown', { key: 'Enter', bubbles: true }));
      const restored = rowKeys();
      checks.push({
        name: 'FILTER_CLEAR_RESTORES',
        pass: restored.length === 4 && restored.every((k) => before.indexOf(k) !== -1),
        detail: restored.length === 4
          ? 'full window restored'
          : 'restored=' + JSON.stringify(restored),
      });
    }

    // Check 5e4 (mixed scenario only): "Show messages only" is an INCLUSIVE
    // kind constraint (include_kind_pattern) — the tool and git rows are
    // excluded even though they are endpoints, and only message/command rows
    // survive. The toggle is restored immediately so the rest of the checks
    // run against the full window.
    if (window.__editchainScenarioName === 'mixed') {
      const messagesToggle = document.getElementById('hideSystem');
      const rowKeys = () => Array.from(document.querySelectorAll('.row[data-key]'))
        .map((r) => r.getAttribute('data-key'));
      messagesToggle.checked = true;
      messagesToggle.dispatchEvent(new Event('change', { bubbles: true }));
      const keys = rowKeys();
      const messagesOnly =
        keys.length === 3 &&
        keys.indexOf('node:s1:a') !== -1 &&
        keys.indexOf('node:s1:h') !== -1 &&
        keys.indexOf('node:s2:c') !== -1 &&
        keys.indexOf('node:s1:b') === -1 &&
        keys.indexOf('git:x') === -1;
      checks.push({
        name: 'MESSAGES_ONLY_INCLUSIVE_KIND',
        pass: messagesOnly,
        detail: messagesOnly
          ? 'only message/command rows kept: ' + JSON.stringify(keys)
          : 'unexpected keys=' + JSON.stringify(keys),
      });
      messagesToggle.checked = false;
      messagesToggle.dispatchEvent(new Event('change', { bubbles: true }));
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
        4 /* search keydown/input + toggle change handlers */ +
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
    runFilterProbe,
    runStaleResponseRace,
    runReversedSearchRace,
    captureResizeMetrics,
    evaluateResizeAssert,
  };
})();
