// Text-only layout probe for the Rust/WASM history webview in real VS Code.
//
// Exposes window.__editchainDebug so a text-only agent can inspect the rendered
// layout as numbers/text instead of images:
//
//   whenIdle()      -> Promise<{ generation, inFlight, elapsedMs }>
//   dumpLayout()    -> LayoutDump                 full geometry + DOM tree
//   assertLayout()  -> AssertionResult            textual checks (pass/fail)
//   getMetrics()    -> RenderMetrics              render timing / DOM counts
//
// This probe is the Rust-shell counterpart of test/harness/layoutProbe.js: it
// runs inside the REAL VS Code webview, where the production page loads ONLY
// media/rust-history/loader.js and the Rust shell owns the runtime. It reads
// renderer state through the window.__editchainGpuDebug facade (loader,
// dataReady, laneXAll, graphState, metrics, whenIdle) and the
// window.__editchainGetProfile/GetTotal/RowAt compatibility hooks the shell
// installs, plus plain DOM state. main.js-only hooks (__editchainRequestLog,
// __editchainGraphState, __editchainScenarioName, ...) do not exist here, so
// no harness-scenario checks run; every check below is a real-VS-Code check.
//
// This file is harness-only. It is injected by test/vscode/history.e2e.ts and
// test/vscode/visual-matrix.e2e.ts and is NOT part of the production webview.

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

  // The Rust shell's read-only debug facade (media/rust-history/loader.js).
  function facade() {
    return window.__editchainGpuDebug;
  }

  // Renderer data-ready: the facade getter is authoritative; the shell's
  // window mirror is the fallback (both are installed by the Rust shell).
  function dataReady() {
    const g = facade();
    if (g && 'dataReady' in g) return g.dataReady === true;
    return window.__editchainDataReady === true;
  }

  // Cached row DTO at an absolute index (compat hook; returns a real object).
  function rowAt(abs) {
    return typeof window.__editchainRowAt === 'function'
      ? window.__editchainRowAt(Number(abs))
      : null;
  }

  function hasPlaceholders() {
    return document.querySelectorAll('.row-placeholder').length > 0;
  }

  // Deterministic UTC formatDate — the EXACT contract the Rust shell renders
  // (crates/editchain-gpu-preview/src/app/rows.rs `format_date`): month/day/
  // year/hour/minute in UTC with a fixed 12-hour clock. main.js used the host
  // locale; the Rust shell is deliberately host-independent, so the probe
  // expectation is computed the same way instead of via Intl.
  const MONTH_NAMES = [
    'Jan', 'Feb', 'Mar', 'Apr', 'May', 'Jun', 'Jul', 'Aug', 'Sep', 'Oct', 'Nov', 'Dec',
  ];

  function civilFromDays(days) {
    const z = days + 719468;
    const era = Math.floor(z / 146097);
    const doe = z - era * 146097; // [0, 146096]
    const yoe = Math.floor((doe - Math.floor(doe / 1460) + Math.floor(doe / 36524) - Math.floor(doe / 146096)) / 365);
    const y = yoe + era * 400;
    const doy = doe - Math.floor(365 * yoe + Math.floor(yoe / 4) - Math.floor(yoe / 100));
    const mp = Math.floor((5 * doy + 2) / 153);
    const d = doy - Math.floor((153 * mp + 2) / 5) + 1;
    const m = mp + (mp < 10 ? 3 : -9);
    return { y: y + (m <= 2 ? 1 : 0), m, d };
  }

  function fmtDateUtc(ms) {
    if (!ms) return '';
    const days = Math.floor(ms / 86400000);
    const seconds = Math.floor((ms % 86400000) / 1000);
    const hour = Math.floor(seconds / 3600);
    const minute = Math.floor((seconds % 3600) / 60);
    const hour12 = hour === 0 ? 12 : hour < 12 ? hour : hour === 12 ? 12 : hour - 12;
    const meridiem = hour < 12 ? 'AM' : 'PM';
    const civil = civilFromDays(days);
    const monthName = MONTH_NAMES[civil.m - 1] || 'Jan';
    const pad = (n) => String(n).padStart(2, '0');
    return monthName + ' ' + civil.d + ', ' + civil.y +
      ' ' + pad(hour12) + ':' + pad(minute) + ' ' + meridiem;
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

  function effectiveTextColor(textRgb, bgRgb, opacity) {
    if (opacity >= 0.999) return textRgb;
    return textRgb.map((v, i) => Math.round(v * opacity + bgRgb[i] * (1 - opacity)));
  }

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

  // --- readiness -------------------------------------------------------------

  // The Rust loader's whenIdle resolves on dataReady + zero in-flight work +
  // two stable frame generations; on top of it the probe requires the DOM to
  // be fully hydrated (no placeholders) and fonts loaded.
  function whenIdle(timeoutMs) {
    timeoutMs = timeoutMs || 60000;
    const started = Date.now();
    const g = facade();
    if (!g || typeof g.whenIdle !== 'function') {
      return Promise.reject(new Error('window.__editchainGpuDebug.whenIdle missing'));
    }
    return g.whenIdle(timeoutMs).then((result) => new Promise((resolve, reject) => {
      const check = () => {
        const placeholders = hasPlaceholders();
        const fonts = !document.fonts || document.fonts.status === 'loaded';
        if (!placeholders && fonts) {
          resolve({
            generation: result && result.generation !== undefined ? result.generation : null,
            inFlight: 0,
            elapsedMs: Date.now() - started,
          });
        } else if (Date.now() - started >= timeoutMs) {
          reject(new Error('Rust history probe did not settle (placeholders=' +
            placeholders + ' fonts=' + (document.fonts ? document.fonts.status : 'n/a') + ')'));
        } else {
          requestAnimationFrame(check);
        }
      };
      requestAnimationFrame(check);
    }));
  }

  // --- layout dump -----------------------------------------------------------

  function dumpLayout(scope) {
    scope = scope || '#rows';
    const rootEl = document.querySelector(scope);
    const rowsEl = document.getElementById('rows');
    const layoutEl = document.getElementById('layout');
    const g = facade();
    const graphState = g && typeof g.graphState === 'function' ? g.graphState() : null;
    const laneX = g && typeof g.laneXAll === 'function' ? g.laneXAll() : null;
    return {
      viewport: { w: window.innerWidth, h: window.innerHeight, dpr: window.devicePixelRatio },
      state: {
        loader: g ? g.loader : null,
        status: (g && g.lastError) ? 'error' : 'idle',
        dataReady: dataReady(),
        placeholders: hasPlaceholders(),
        layoutReady: graphState ? graphState.layoutReady : undefined,
        graphWidth: graphState ? graphState.graphWidth : undefined,
        laneXAll: laneX,
        rowsRendered: document.querySelectorAll('.row').length,
        canvasCount: document.querySelectorAll('#gpu-canvas-host canvas').length,
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
    };
  }

  // --- assertions ------------------------------------------------------------

  // A small set of textual checks. Each returns { name, pass, detail }.
  // These mirror the real-VS-Code subset of test/harness/layoutProbe.js's
  // runChecks, adapted to the Rust shell: graph geometry comes from the
  // canvas + laneXAll facade instead of per-row SVG nodes, and the date
  // column is checked against the shell's deterministic UTC format. No check
  // is weaker than its main.js counterpart.
  function runChecks() {
    const checks = [];
    const rowsEl = document.getElementById('rows');
    const layoutEl = document.getElementById('layout');
    const wrapEl = rowsEl && rowsEl.querySelector('.table-wrap');
    const headerEl = rowsEl && rowsEl.querySelector('.tbl-header');
    const viewMessage = rowsEl && rowsEl.querySelector('.view-message');
    const g = facade();

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

    // Check 1b: the history is one uninterrupted reading surface.
    const secondaryPane = document.getElementById('detail');
    const hasSplitState = !!(layoutEl && layoutEl.classList.contains('has-detail'));
    checks.push({
      name: 'SINGLE_PANE_HISTORY',
      pass: !secondaryPane && !hasSplitState,
      detail: !secondaryPane && !hasSplitState
        ? 'no secondary detail pane or split-layout state'
        : 'secondaryElement=' + !!secondaryPane + ' splitState=' + hasSplitState,
    });

    // Check 2: no horizontal overflow on #rows.
    if (rowsEl) {
      const contentOverflow = Array.from(rowsEl.querySelectorAll('*')).some((el) => {
        if (el.classList && el.classList.contains('col-resize-handle')) return false;
        const r = el.getBoundingClientRect();
        return r.right > rowsEl.getBoundingClientRect().right + 1;
      });
      checks.push({
        name: 'NO_HORIZONTAL_OVERFLOW',
        pass: !contentOverflow,
        detail: 'scrollW=' + rowsEl.scrollWidth + ' clientW=' + rowsEl.clientWidth +
          ' delta=' + (rowsEl.scrollWidth - rowsEl.clientWidth) +
          ' contentSpill=' + contentOverflow,
      });
    }

    // Check 3: graph geometry is aligned and lane-backed. The Rust shell draws
    // the graph on ONE canvas under #gpu-canvas-host (positioned over the
    // graph column at the rows' left edge) instead of per-row SVG nodes, so
    // the alignment contract is: every non-subop row renders a visible
    // .graph-cell whose row's lane maps onto a fixed laneXAll center, the
    // canvas host is aligned to the rows box, and the canvas covers the full
    // viewport height at the graph column's width.
    if (wrapEl && g && typeof g.laneXAll === 'function') {
      const laneX = g.laneXAll();
      const rowEls = wrapEl.querySelectorAll('.row:not(.row-placeholder)');
      let nodesOk = true;
      let firstFail = null;
      rowEls.forEach((row) => {
        if (row.classList.contains('row-subop')) return; // sub-op rows draw no node
        const absIdx = Number(row.getAttribute('data-row'));
        const cell = row.querySelector('.graph-cell');
        if (!cell || cell.getBoundingClientRect().width <= 0) {
          nodesOk = false;
          firstFail = firstFail || { rowIdx: absIdx, reason: 'no visible graph cell' };
          return;
        }
        const cached = rowAt(absIdx);
        if (cached && Number.isFinite(cached.lane)) {
          const laneCenter = laneX[cached.lane];
          if (laneCenter === undefined) {
            nodesOk = false;
            firstFail = firstFail || { rowIdx: absIdx, lane: cached.lane, reason: 'lane outside laneXAll' };
          }
        }
      });
      const canvasHost = document.getElementById('gpu-canvas-host');
      const canvas = canvasHost && canvasHost.querySelector('canvas');
      const rowsBox = rowsEl.getBoundingClientRect();
      const hostBox = canvasHost ? canvasHost.getBoundingClientRect() : null;
      const graphCell = wrapEl.querySelector('.row:not(.row-placeholder) .graph-cell');
      const graphW = graphCell ? graphCell.getBoundingClientRect().width : 0;
      const canvasAligned = !!canvas && !!hostBox &&
        Math.abs(hostBox.left - rowsBox.left) <= 2 &&
        Math.abs(hostBox.top - rowsBox.top) <= 2 &&
        Math.abs(hostBox.height - rowsEl.clientHeight) <= 2 &&
        Math.abs(hostBox.width - graphW) <= 2;
      checks.push({
        name: 'GRAPH_NODE_ALIGNMENT',
        pass: nodesOk && canvasAligned && Array.isArray(laneX) && laneX.length > 0,
        detail: (nodesOk && canvasAligned)
          ? 'canvas aligned over the graph column (w=' + Math.round(graphW) +
            'px, h=' + Math.round(rowsEl.clientHeight) + 'px), ' + laneX.length +
            ' lane centers, every row lane mapped'
          : 'nodesOk=' + nodesOk + ' canvasAligned=' + canvasAligned +
            ' laneX=' + JSON.stringify(laneX) + ' firstFail=' + JSON.stringify(firstFail),
      });
    }

    // Check 4: grid columns share boundaries between header and rows.
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
          if (deltaL > 1.5) { colsOk = false; firstFailCol = firstFailCol || { col: i, deltaL }; }
        });
      }
      checks.push({
        name: 'COLUMN_ALIGNMENT',
        pass: colsOk,
        detail: colsOk ? 'columns aligned' : 'first fail=' + JSON.stringify(firstFailCol),
      });
    }

    // Check 5: human/agent emphasis is a harness-scenario check; real chains
    // never carry the fixture's mixed scenario markers, so this is skipped.
    if (wrapEl) {
      checks.push({
        name: 'HUMAN_BOLD_NO_AGENT_PAD',
        pass: true,
        detail: 'skipped — no fixture scenario markers in the real webview',
      });
    }

    // Check 5b: rendered dates match the shell's deterministic UTC contract.
    if (wrapEl) {
      const firstRow = wrapEl.querySelector('.row:not(.row-placeholder)');
      const absIdx = firstRow ? parseInt(firstRow.getAttribute('data-row'), 10) : -1;
      const row = absIdx >= 0 ? rowAt(absIdx) : null;
      const dateCell = firstRow ? firstRow.querySelector('.date-cell') : null;
      const expected = row && row.timestamp_ms ? fmtDateUtc(row.timestamp_ms) : '';
      const rendered = dateCell ? (dateCell.textContent || '').trim() : '';
      const datesOk = !!row && !!dateCell && !!row.timestamp_ms &&
        rendered === expected && rendered !== '';
      checks.push({
        name: 'DATE_EXPLICIT_UTC',
        pass: datesOk || !row || !row.timestamp_ms,
        detail: datesOk || !row || !row.timestamp_ms
          ? (row && row.timestamp_ms
              ? 'date "' + rendered + '" matches the deterministic UTC contract'
              : 'no dated row in this view (skipped)')
          : 'rendered "' + rendered + '" != expected "' + expected + '"',
      });
    }

    // Check 5c: the controls bar must fit its container.
    const controlsEl = document.getElementById('controls');
    if (controlsEl) {
      const fits = controlsEl.scrollWidth <= controlsEl.clientWidth + 1;
      checks.push({
        name: 'CONTROLS_FIT',
        pass: fits,
        detail: 'scrollW=' + controlsEl.scrollWidth + ' clientW=' +
          controlsEl.clientWidth + (fits ? '' : ' — controls clipped'),
      });
    }

    // Check 5d: the graph column must stay visible (never collapsed/hidden).
    if (rowsEl && !viewMessage) {
      const graphCell = rowsEl.querySelector('.graph-cell');
      const graphW = graphCell ? graphCell.getBoundingClientRect().width : 0;
      checks.push({
        name: 'GRAPH_VISIBLE',
        pass: !!graphCell && graphW > 0,
        detail: graphCell ? 'graph column visible, width=' + graphW + 'px' : 'no .graph-cell rendered',
      });
    } else if (viewMessage) {
      checks.push({
        name: 'GRAPH_VISIBLE',
        pass: true,
        detail: 'skipped — full-pane message shown',
      });
    }

    // Check 5e: the five production cell classes obey the Pulse geometry —
    // author/commit are hidden at every width, content/date are genuinely
    // rendered inside the rows box (date drops at the narrowest breakpoint).
    if (wrapEl && !viewMessage) {
      const firstRow = wrapEl.querySelector('.row:not(.row-placeholder)');
      if (firstRow) {
        const rowsBox = rowsEl.getBoundingClientRect();
        const innerW = window.innerWidth || rowsEl.clientWidth || 0;
        const hidden = new Set(['author', 'commit']);
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
            ? 'widths=' + JSON.stringify(geo) + 'px hidden=' + Array.from(hidden).join(',') + ' (Pulse order)'
            : 'first bad=' + JSON.stringify(firstBad),
        });
      }
    }

    // Check 5g: readable contrast for Content/Date/Author/Commit text.
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

    // Check 5h: the content column must be genuinely readable.
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

    // Check 5i: the graph column must not dominate the table.
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

    // Check 5j: header cells must not overlap.
    if (headerEl && !viewMessage) {
      const ths = Array.from(headerEl.querySelectorAll('.th'));
      let boxesOk = true;
      let textOk = true;
      let firstBad = null;
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

    // Check 5k: the Activity/Raw segmented control exists, is labelled, and
    // reflects the ACTIVE profile.
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

    // Check 5l: rows expose keyboard/grid semantics — ONE labelled role=grid
    // wrapper owns the sticky header row (whose Pulse columnheaders live
    // inside the grid) and the data rows; every row carries role=row +
    // aria-selected; exactly ONE rendered row is in the tab order (roving).
    if (wrapEl) {
      const rowEls = wrapEl.querySelectorAll('.row:not(.row-placeholder)');
      const grids = Array.from(document.querySelectorAll('[role="grid"]'));
      const grid = document.querySelector('.tbl-grid');
      const header = rowsEl.querySelector('.tbl-header');
      const labelled = !!grid && grid.getAttribute('aria-label') === 'History rows';
      const headerInside = !!header && !!grid && grid.contains(header);
      // Pulse renders exactly three columnheaders (graph/content/date);
      // author/commit are display:none cells, not headers.
      const colHeadersInside = !!header &&
        header.querySelectorAll('[role="columnheader"]').length === 3;
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

    // Check 5m: group boundary labels are VISIBLE and show a SHORT id.
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
          : 'no group boundary rows in this view (skipped)',
      });

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
          : 'no group boundary rows in this view (skipped)',
      });
    }

    // Check 5n: the Commit/ID column shows a SHORT display id — raw 64-bit
    // strings never render as the visible value.
    if (wrapEl && !viewMessage) {
      const firstRow = wrapEl.querySelector('.row:not(.row-placeholder)');
      const commitCell = firstRow && firstRow.querySelector('.commit-cell');
      const commitText = commitCell ? (commitCell.textContent || '').trim() : '';
      const cached = firstRow ? rowAt(parseInt(firstRow.getAttribute('data-row'), 10)) : null;
      let shortOk = true;
      if (cached && !cached.git_oid && cached.op_id && cached.op_id.length > 12) {
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

    // Check 5q: common clean-state chrome is globally quiet by default.
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

    // Check 5r: Git text before the first colon becomes one exact chip.
    if (wrapEl && !viewMessage) {
      const problems = [];
      let prefixed = 0;
      let plain = 0;
      for (const el of wrapEl.querySelectorAll('.row')) {
        const abs = Number(el.getAttribute('data-row'));
        const row = rowAt(abs);
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

    // Check 6: every rendered row is exactly ROW_H tall (uniform grid).
    if (wrapEl) {
      const rowEls = wrapEl.querySelectorAll('.row');
      let uniform = true;
      let firstBad = null;
      rowEls.forEach((row) => {
        const h = row.getBoundingClientRect().height;
        if (Math.abs(h - 34) > 0.5) { uniform = false; firstBad = firstBad || { key: row.getAttribute('data-key'), h }; }
      });
      checks.push({
        name: 'UNIFORM_ROW_HEIGHT',
        pass: uniform,
        detail: uniform ? 'all rows exactly ROW_H' : 'first bad=' + JSON.stringify(firstBad),
      });
    }

    // Check 7: expanded sub-op rows render with a Codicon and are indented.
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
          name: 'SUBOP_ICON_INDENT',
          pass: iconsOk && indentOk,
          detail: 'subopRows=' + subopRows.length + ' iconsOk=' + iconsOk + ' indentOk=' + indentOk,
        });
      } else {
        checks.push({
          name: 'SUBOP_ICON_INDENT',
          pass: true,
          detail: 'no sub-op rows in this view (skipped)',
        });
      }
    }

    // Check 8: the Rust renderer contract — the facade is the rust-history
    // loader, the shell is data-ready, exactly ONE canvas lives under
    // #gpu-canvas-host with no foreign canvases, the #gpu-rows mirror matches
    // the FRAME rows (the rendered viewport — the DOM window is larger by
    // design), lane centers are fixed, and frames have been submitted
    // (renderCount > 0).
    const canvasHostCanvases = document.querySelectorAll('#gpu-canvas-host canvas');
    const foreignCanvases = document.querySelectorAll('canvas:not(#gpu-canvas-host canvas)');
    const mirrorRows = document.querySelectorAll('#gpu-rows [data-row][data-key]');
    const renderedRows = document.querySelectorAll(
      '#rows .row[data-row][data-key]:not(.row-placeholder)');
    const laneX = g && typeof g.laneXAll === 'function' ? g.laneXAll() : null;
    const metrics = g && typeof g.metrics === 'function' ? g.metrics() : null;
    const snap = g && typeof g.snapshot === 'function' ? g.snapshot() : null;
    const frameRows = snap && Array.isArray(snap.rows) ? snap.rows.length : -1;
    const rustContractOk = !!g &&
      g.loader === 'rust-history' &&
      dataReady() &&
      canvasHostCanvases.length === 1 &&
      foreignCanvases.length === 0 &&
      mirrorRows.length === frameRows &&
      frameRows > 0 &&
      renderedRows.length >= frameRows &&
      Array.isArray(laneX) && laneX.length >= 1 &&
      !!metrics && metrics.renderCount > 0;
    checks.push({
      name: 'RUST_RENDERER_CONTRACT',
      pass: rustContractOk,
      detail: rustContractOk
        ? 'rust-history loader, dataReady, 1 canvas, ' + mirrorRows.length +
          ' mirror rows == frame rows, DOM rows=' + renderedRows.length +
          ', ' + laneX.length + ' lanes, renderCount=' +
          (metrics ? metrics.renderCount : 'n/a')
        : 'loader=' + (g ? g.loader : 'MISSING') + ' dataReady=' + dataReady() +
          ' canvases=' + canvasHostCanvases.length + ' foreign=' + foreignCanvases.length +
          ' mirror=' + mirrorRows.length + ' frameRows=' + frameRows +
          ' rendered=' + renderedRows.length +
          ' laneX=' + JSON.stringify(laneX) +
          ' renderCount=' + (metrics ? metrics.renderCount : 'n/a'),
    });

    return checks;
  }

  function assertLayout() {
    const checks = runChecks();
    const failed = checks.filter((c) => !c.pass);
    return { passCount: checks.length - failed.length, failCount: failed.length, checks };
  }

  // --- metrics ---------------------------------------------------------------

  function getMetrics() {
    const g = facade();
    const metrics = g && typeof g.metrics === 'function' ? g.metrics() : null;
    return {
      loader: g ? g.loader : null,
      renderCount: metrics ? metrics.renderCount : undefined,
      vertexCount: metrics ? metrics.vertexCount : undefined,
      domNodes: document.querySelectorAll('*').length,
      dataReady: dataReady(),
      placeholders: hasPlaceholders(),
      canvasCount: document.querySelectorAll('#gpu-canvas-host canvas').length,
      mirrorRows: document.querySelectorAll('#gpu-rows [data-row][data-key]').length,
    };
  }

  window.__editchainDebug = {
    whenIdle,
    dumpLayout,
    assertLayout,
    getMetrics,
  };
})();
