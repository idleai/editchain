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

  // --- readiness -------------------------------------------------------------

  // Track in-flight requests by hooking the bridge's dispatch. The renderer
  // sends requests via window.vscode.postMessage; we count outstanding ones.
  // In real VS Code, `window.vscode` is not exposed to the page, so the hook is
  // a no-op there and readiness falls back to DOM/font stability.
  let inFlight = 0;
  if (window.vscode && typeof window.vscode.postMessage === 'function') {
    const origPost = window.vscode.postMessage;
    window.vscode.postMessage = function (msg) {
      if (msg && msg.body !== undefined && msg.id === undefined) {
        inFlight++;
        // The bridge responds synchronously via dispatchEvent; count it down on
        // the next tick so the renderer has processed the response.
        setTimeout(() => { inFlight--; }, 0);
      }
      return origPost.call(this, msg);
    };
  }

  let generation = 0;
  const observer = new MutationObserver(() => { generation++; });
  observer.observe(document.body, { childList: true, subtree: true, attributes: true });

  function whenIdle(timeoutMs) {
    timeoutMs = timeoutMs || 5000;
    const started = Date.now();
    return new Promise((resolve, reject) => {
      const check = () => {
        const settled =
          inFlight === 0 &&
          document.fonts && document.fonts.status === 'loaded';
        if (settled) {
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

    // Check 1: header present.
    checks.push({
      name: 'HEADER_PRESENT',
      pass: !!headerEl,
      detail: headerEl ? 'header rendered' : 'no .tbl-header found',
    });

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

    // Check 4: grid columns share boundaries between header and rows.
    if (headerEl && wrapEl) {
      const headerCells = headerEl.querySelectorAll('.th');
      const firstRow = wrapEl.querySelector('.row');
      let colsOk = true;
      let firstFailCol = null;
      if (firstRow) {
        const rowCells = firstRow.children;
        headerCells.forEach((th, i) => {
          const rc = rowCells[i];
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
    if (wrapEl) {
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

  // --- expose ----------------------------------------------------------------

  window.__editchainDebug = {
    whenIdle,
    dumpLayout,
    assertLayout,
    getMetrics,
  };
})();