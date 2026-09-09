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
// This probe runs inside the REAL VS Code webview (the browser-based harness
// page is test/harness/rust.html): the production page loads ONLY
// media/rust-history/loader.js and the Rust shell owns the runtime. It reads
// renderer state through the window.__editchainRendererDebug facade (loader,
// dataReady, laneXAll, graphState, metrics, whenIdle) and the
// window.__editchainGetTotal/RowAt inspection hooks the shell
// installs, plus plain DOM state. Legacy JS-only hooks (__editchainRequestLog,
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

  // --- graph fragment + scroll parity (regression coverage for the
  // scrolling/graph parity fix) ------------------------------------------------

  const ROW_H = 34; // fixed production row height (also used by the probes)

  function round2(v) {
    return Math.round(v * 100) / 100;
  }

  function clamp(v, lo, hi) {
    return Math.min(hi, Math.max(lo, v));
  }

  function shortKey(key) {
    if (!key) return null;
    return key.length > 28 ? key.slice(0, 12) + '\u2026' + key.slice(-12) : key;
  }

  // The parity-fix row-local graph contract: every hydrated row's .graph-cell
  // owns exactly one svg.graph-row-fragment[aria-hidden="true"].
  function rowFragmentInfo(rowEl) {
    const cell = rowEl.querySelector('.graph-cell');
    if (!cell) return { cell: null, svg: null, ariaHidden: false, note: 'no .graph-cell' };
    const svg = cell.querySelector('svg.graph-row-fragment');
    if (!svg) {
      return {
        cell, svg: null, ariaHidden: false, note: 'no row svg fragment',
      };
    }
    const ariaHidden = svg.getAttribute('aria-hidden') === 'true';
    return { cell, svg, ariaHidden, note: 'ok' };
  }

  // Vertical centre (viewport CSS px) of the row's node marker. Lane halves
  // legitimately occupy only one side on tip/root rows, so using all drawn
  // geometry would falsely call those correct endpoint fragments off-centre.
  // Sub-op rows have no marker and fall back to the row-aligned SVG box.
  function fragmentMarkerCenterY(svg) {
    const shapes = Array.from(svg.querySelectorAll('.graphDot, .graphBundleCapsule'));
    let minY = Infinity;
    let maxY = -Infinity;
    let any = false;
    for (const shape of shapes) {
      const b = shape.getBoundingClientRect();
      if (b.width <= 0 && b.height <= 0) continue;
      any = true;
      if (b.top < minY) minY = b.top;
      if (b.bottom > maxY) maxY = b.bottom;
    }
    return any ? (minY + maxY) / 2 : null;
  }

  function readGraphWindow() {
    const debug = window.__editchainRendererDebug;
    const state = debug && typeof debug.graphState === 'function'
      ? debug.graphState()
      : (typeof window.__editchainGraphState === 'function'
          ? window.__editchainGraphState() : null);
    return state && Number.isFinite(Number(state.renderTop)) ? state : null;
  }

  // Snapshot the rendered window's structural invariants at the CURRENT scroll
  // position (synchronous, no motion, no waiting). Live snapshots taken while
  // the renderer is still catching up may contain placeholders — every
  // invariant below is stated per-row so a lagging window never false-fails:
  //   - keys unique, data-row strictly monotonic in DOM order;
  //   - rows contiguous at exactly ROW_H (gap-contiguity; no internal snaps),
  //     and retained rows' viewport tops move by exactly the scroll delta
  //     between consecutive snapshots (same-key stability; no wrapper drift
  //     or rebuild snaps — works even when collapsed sub-op slots make
  //     data-row values non-dense);
  //   - every HYDRATED row owns a row-local fragment, and its geometry centre
  //     sits on the row's vertical centre (<= 1px).
  function sampleScrollState() {
    const rowsEl = document.getElementById('rows');
    const wrapEl = rowsEl && rowsEl.querySelector('.table-wrap');
    const graphWindow = readGraphWindow();
    const wrapTopPx = wrapEl ? round2(parseFloat(getComputedStyle(wrapEl).top) || 0) : null;
    const renderTop = graphWindow ? Number(graphWindow.renderTop) : null;
    const rows = wrapEl ? Array.from(wrapEl.querySelectorAll('.row')) : [];
    const keyCounts = new Map();
    let dupKeys = 0;
    const dupExamples = [];
    let orderOk = true;
    let firstOrderBad = null;
    let prevRow = null;
    let minGap = Infinity;
    let maxGap = -Infinity;
    let firstBadGap = null;
    const hydrated = [];
    const keyTops = {};
    let fragmentCount = 0;
    const fragmentIssues = [];
    let maxAlignDelta = 0;
    const alignExamples = [];
    for (let i = 0; i < rows.length; i++) {
      const el = rows[i];
      const abs = Number(el.getAttribute('data-row'));
      const key = el.getAttribute('data-key') || null;
      if (!Number.isFinite(abs)) continue;
      if (key) {
        const c = (keyCounts.get(key) || 0) + 1;
        keyCounts.set(key, c);
        if (c === 2) {
          dupKeys++;
          if (dupExamples.length < 5) dupExamples.push({ row: abs, key: shortKey(key) });
        }
      }
      if (prevRow !== null && abs <= prevRow) {
        orderOk = false;
        if (!firstOrderBad) firstOrderBad = { prev: prevRow, cur: abs };
      }
      prevRow = abs;
      const b = el.getBoundingClientRect();
      if (i > 0) {
        const gap = b.top - rows[i - 1].getBoundingClientRect().bottom;
        if (gap < minGap) minGap = gap;
        if (gap > maxGap) maxGap = gap;
        if (Math.abs(gap) > 0.5 && !firstBadGap) firstBadGap = { at: abs, gap: round2(gap) };
      }
      const info = rowFragmentInfo(el);
      if (el.classList.contains('row-placeholder')) continue;
      hydrated.push({ row: abs, key: shortKey(key), top: round2(b.top) });
      if (key && !(key in keyTops)) keyTops[key] = round2(b.top);
      if (!info.cell || !info.svg || !info.ariaHidden) {
        if (fragmentIssues.length < 5) fragmentIssues.push({ row: abs, reason: info.note });
      } else {
        fragmentCount++;
        const svgBox = info.svg.getBoundingClientRect();
        const rowBox = b;
        const geomCenter = fragmentMarkerCenterY(info.svg);
        const center = geomCenter === null ? svgBox.top + svgBox.height / 2 : geomCenter;
        const delta = Math.abs(center - (rowBox.top + rowBox.height / 2));
        if (delta > maxAlignDelta) maxAlignDelta = delta;
        if (delta > 1 && alignExamples.length < 5) {
          alignExamples.push({ row: abs, delta: round2(delta) });
        }
      }
    }
    // Keep only the head + tail hydrated keys so the cross-sample stability
    // check has plenty of overlap while the returned sample stays lean.
    const keyEntries = Object.entries(keyTops);
    const trimmedKeyTops = {};
    for (const entry of keyEntries.slice(0, 40).concat(keyEntries.slice(-40))) {
      trimmedKeyTops[entry[0]] = entry[1];
    }
    return {
      scrollTop: rowsEl ? rowsEl.scrollTop : -1,
      scrollHeight: rowsEl ? rowsEl.scrollHeight : -1,
      clientHeight: rowsEl ? rowsEl.clientHeight : -1,
      maxScroll: rowsEl ? Math.max(0, rowsEl.scrollHeight - rowsEl.clientHeight) : -1,
      rowCount: rows.length,
      hydratedCount: hydrated.length,
      placeholders: rows.length - hydrated.length,
      firstRow: hydrated.length ? hydrated[0] : null,
      lastRow: hydrated.length ? hydrated[hydrated.length - 1] : null,
      head: hydrated.slice(0, 3),
      tail: hydrated.slice(-2),
      wrapRectTop: wrapEl ? round2(wrapEl.getBoundingClientRect().top) : null,
      wrapTopPx,
      renderTop,
      wrapAnchorDelta: wrapTopPx === null || renderTop === null
        ? null : round2(Math.abs(wrapTopPx - renderTop * ROW_H)),
      spacerH: rowsEl && rowsEl.querySelector('.scroll-spacer')
        ? Math.round(rowsEl.querySelector('.scroll-spacer').offsetHeight) : null,
      keyTops: trimmedKeyTops,
      minGap: minGap === Infinity ? null : round2(minGap),
      maxGap: maxGap === -Infinity ? null : round2(maxGap),
      firstBadGap,
      dupKeys,
      dupExamples,
      orderOk,
      firstOrderBad,
      fragmentCount,
      fragmentIssues,
      maxAlignDelta: round2(maxAlignDelta),
      alignExamples,
    };
  }

  // Drive #rows.scrollTop continuously (scrollbar-like per-frame increments,
  // no arbitrary sleeps) and record a LIVE snapshot each time
  // `sampleEveryPx` of travel is crossed. Also records renderer scrollTop
  // corrections observed between frames (an assignment the renderer later
  // overrides mid-range is the "unbounded scrollTop correction" pathology).
  //
  // Real VS Code WebDriver sessions enforce a ~30s script timeout on every
  // execute/sync command (ChromeDriver's default). A full 60k px bidirectional
  // sweep with idle settle waits can exceed that inside ONE command, which
  // aborts the command, gets retried by the driver, and leaves the webview in
  // a state that poisons later tests. So the probe also runs as a chunked
  // session: pass a finite `chunkPx` (max travel per command) and optionally
  // `chunkBudgetMs` (hard wall-clock budget per command) and the probe keeps
  // its sweep state page-side across bounded execute/sync calls, returning
  // `{ done: false, sessionId, progress }` until a final `{ done: true }`
  // result with the same checks and samples as the one-shot path.
  let scrollParitySession = null;
  let scrollParitySessionSeq = 0;

  function probeScrollParityPartial(session, commandStartedAt, awaitingIdle, reason) {
    const rowsEl = session.rowsEl;
    return {
      done: false,
      sessionId: session.id,
      awaitingIdle: !!awaitingIdle,
      reason,
      phase: session.phase,
      legIndex: session.legIndex,
      legPhase: session.legPhase,
      progress: {
        totalTraveledPx: Math.round(session.totalTraveled),
        sampleCount: session.samples.length,
        commandedScrollTop: rowsEl ? Math.round(rowsEl.scrollTop) : null,
        targetScrollTop: session.target === null ? null : Math.round(session.target),
        maxScroll: Math.round(session.maxScroll),
        sessionElapsedMs: Math.round(performance.now() - session.startedPerf),
        commandElapsedMs: Math.round(Date.now() - commandStartedAt),
      },
    };
  }

  // Advance the active leg for one bounded command: scrollTop moves by up to
  // `pxPerFrame` per animation frame until the leg target, the command's
  // travel quota, or the command deadline is reached. Sampling and
  // renderer-correction detection are identical to the one-shot sweep.
  function driveLegChunk(session, commandDeadline, travelQuota) {
    const rowsEl = session.rowsEl;
    const dir = session.dir;
    const target = session.target;
    const phase = session.legPhase;
    const sampleEveryPx = session.sampleEveryPx;
    const pxPerFrame = session.pxPerFrame;
    return new Promise((resolve) => {
      let commanded = session.commanded;
      let nextSampleAt = session.nextSampleAt;
      let commandTraveled = 0;
      const step = () => {
        if (Date.now() >= commandDeadline || commandTraveled >= travelQuota) {
          session.commanded = commanded;
          session.nextSampleAt = nextSampleAt;
          resolve({ budgetExhausted: true });
          return;
        }
        if (!rowsEl.isConnected) {
          resolve({ detached: true });
          return;
        }
        const actual = rowsEl.scrollTop; // post-scroll-event value from last frame
        if (Math.abs(actual - commanded) > 2) {
          const nearEnd = actual <= rowsEl.clientHeight ||
            actual >= Math.max(0, rowsEl.scrollHeight - rowsEl.clientHeight) - rowsEl.clientHeight;
          if (!nearEnd && session.corrections.length < 8) {
            session.corrections.push({ at: actual, commanded: round2(commanded), actual: round2(actual) });
          }
        }
        const remaining = target - commanded;
        const move = Math.min(Math.abs(remaining), pxPerFrame);
        commanded += move * dir;
        commandTraveled += move;
        rowsEl.scrollTop = commanded;
        const crossed = dir > 0 ? commanded >= nextSampleAt : commanded <= nextSampleAt;
        if (crossed) {
          const snap = sampleScrollState();
          snap.kind = 'live';
          snap.phase = phase;
          snap.commanded = round2(commanded);
          snap.traveled = round2(Math.abs(commanded - session.legStart));
          session.samples.push(snap);
          nextSampleAt = commanded + dir * sampleEveryPx;
        }
        if (Math.abs(commanded - target) > 0.5) {
          requestAnimationFrame(step);
        } else {
          rowsEl.scrollTop = target;
          session.commanded = target;
          session.nextSampleAt = nextSampleAt;
          session.legTraveled = Math.abs(target - session.legStart);
          resolve({ budgetExhausted: false, legDone: true });
        }
      };
      requestAnimationFrame(step);
    });
  }

  // Shared verdict computation for the one-shot and chunked paths: aggregates
  // the per-sample invariants into the same named checks with the same
  // diagnostics, then returns the final result (samples minus the bulky
  // keyTops, exactly like the original probe).
  function finalizeScrollParity(session) {
    const problems = {
      fragment: [],
      alignment: [],
      duplicates: [],
      order: [],
      wrapper: [],
      scrollTop: [],
      keyStability: [],
    };
    let maxAlignmentDelta = 0;
    let prevSample = null;
    for (const s of session.samples) {
      if (s.hydratedCount > 0) {
        if (s.fragmentCount !== s.hydratedCount) {
          problems.fragment.push({
            at: s.scrollTop, phase: s.phase, kind: s.kind,
            missing: s.hydratedCount - s.fragmentCount,
            issues: s.fragmentIssues,
          });
        }
        if (s.maxAlignDelta > 1) {
          problems.alignment.push({
            at: s.scrollTop, phase: s.phase, kind: s.kind,
            maxAlignDelta: s.maxAlignDelta, examples: s.alignExamples,
          });
        }
        if (s.maxAlignDelta > maxAlignmentDelta) maxAlignmentDelta = s.maxAlignDelta;
      }
      if (s.dupKeys > 0) {
        problems.duplicates.push({
          at: s.scrollTop, phase: s.phase, count: s.dupKeys, examples: s.dupExamples,
        });
      }
      if (!s.orderOk) {
        problems.order.push({ at: s.scrollTop, phase: s.phase, firstBad: s.firstOrderBad });
      }
      if (s.firstBadGap) {
        problems.wrapper.push({
          at: s.scrollTop, phase: s.phase, firstBadGap: s.firstBadGap,
          minGap: s.minGap, maxGap: s.maxGap,
        });
      }
      if (s.wrapAnchorDelta !== null && s.wrapAnchorDelta > 0.5) {
        problems.wrapper.push({
          at: s.scrollTop,
          phase: s.phase,
          renderTop: s.renderTop,
          wrapTopPx: s.wrapTopPx,
          expectedWrapTopPx: s.renderTop * ROW_H,
          delta: s.wrapAnchorDelta,
        });
      }
      if (s.kind === 'live' && s.commanded !== undefined) {
        const nearEnd = s.scrollTop <= s.clientHeight ||
          s.scrollTop >= s.maxScroll - s.clientHeight;
        const drift = Math.abs(s.scrollTop - s.commanded);
        if (drift > 2 && !nearEnd) {
          problems.scrollTop.push({
            at: s.scrollTop, commanded: s.commanded, drift: round2(drift),
          });
        }
      }
      // Same-key stability: any row key present in both consecutive samples
      // must move by EXACTLY the scroll delta (viewport physics; prepends,
      // trims, and re-anchors never shift a retained row's document position).
      // A wrapper drift or rebuild snap moves the retained row, so this is the
      // "no drift/snaps" proof that works even when collapsed sub-op slots
      // make data-row values non-dense.
      if (prevSample && s.keyTops && prevSample.keyTops) {
        let worst = 0;
        const examples = [];
        const dScroll = s.scrollTop - prevSample.scrollTop;
        for (const key of Object.keys(s.keyTops)) {
          const prevTop = prevSample.keyTops[key];
          if (prevTop === undefined) continue;
          const dTop = s.keyTops[key] - prevTop;
          const dev = Math.abs(dTop + dScroll);
          if (dev > worst) worst = dev;
          if (dev > 1 && examples.length < 5) {
            examples.push({
              key: shortKey(key), dTop: round2(dTop), dScroll: round2(dScroll),
              dev: round2(dev),
            });
          }
        }
        if (worst > 1) {
          problems.keyStability.push({
            at: s.scrollTop, phase: s.phase, worst: round2(worst), examples,
          });
        }
      }
      prevSample = s;
    }
    for (const c of session.corrections) {
      problems.scrollTop.push({ at: c.at, commanded: c.commanded, actual: c.actual, kind: 'renderer-correction' });
    }

    const addCheck = (name, pass, detail) => session.checks.push({ name, pass, detail });
    const observedScrollTops = session.samples.map((s) => Number(s.scrollTop))
      .filter((value) => Number.isFinite(value));
    const observedScrollSpan = observedScrollTops.length > 0
      ? Math.max(...observedScrollTops) - Math.min(...observedScrollTops) : 0;
    const initial = session.samples[0];
    const expectedScrollSpan = Math.abs(session.legs[0].target - initial.scrollTop);
    addCheck('SCROLL_PARITY_SWEEP_ADVANCED',
      session.samples.length >= 2 && session.totalTraveled >= session.expectedTravel * 0.8 &&
        expectedScrollSpan > 0 && observedScrollSpan >= expectedScrollSpan * 0.8,
      'commanded ' + Math.round(session.totalTraveled) + 'px and observed ' +
        Math.round(observedScrollSpan) + 'px of scroll range across ' +
        session.samples.length + ' samples (expected observed >= ' +
        Math.round(expectedScrollSpan * 0.8) + 'px)');
    addCheck('SCROLL_PARITY_FRAGMENT_PRESENT',
      problems.fragment.length === 0 &&
        session.samples.some((s) => s.hydratedCount > 0 && s.kind === 'settled'),
      problems.fragment.length === 0
        ? 'every hydrated row owned svg.graph-row-fragment[aria-hidden=true] in every sample'
        : 'fragment gaps=' + JSON.stringify(problems.fragment.slice(0, 3)));
    const anyFragment = session.samples.some((s) => s.fragmentCount > 0 && s.kind === 'settled');
    addCheck('SCROLL_PARITY_FRAGMENT_ALIGNED',
      problems.alignment.length === 0 && anyFragment,
      problems.alignment.length === 0
        ? (anyFragment
            ? 'fragment geometry centered on its row through the sweep (max delta ' +
              round2(maxAlignmentDelta) + 'px)'
            : 'no graph-row-fragment rendered in any settled sample (presence check fails)')
        : 'alignment failures=' + JSON.stringify(problems.alignment.slice(0, 3)));
    addCheck('SCROLL_PARITY_KEYS_UNIQUE',
      problems.duplicates.length === 0,
      problems.duplicates.length === 0
        ? 'no duplicate rendered row keys in any sample'
        : 'duplicate keys=' + JSON.stringify(problems.duplicates.slice(0, 3)));
    addCheck('SCROLL_PARITY_ORDER_MONOTONIC',
      problems.order.length === 0,
      problems.order.length === 0
        ? 'data-row strictly increasing in DOM order in every sample'
        : 'non-monotonic DOM order=' + JSON.stringify(problems.order.slice(0, 3)));
    addCheck('SCROLL_PARITY_WRAPPER_STABLE',
      problems.wrapper.length === 0 && problems.keyStability.length === 0,
      (problems.wrapper.length === 0 && problems.keyStability.length === 0)
        ? 'contiguous ROW_H spacing and scroll-locked retained rows across ' +
          session.samples.length + ' samples'
        : 'wrapper=' + JSON.stringify(problems.wrapper.slice(0, 3)) +
          ' keyStability=' + JSON.stringify(problems.keyStability.slice(0, 3)));
    addCheck('SCROLL_PARITY_SCROLLTOP_BOUNDED',
      problems.scrollTop.length === 0,
      problems.scrollTop.length === 0
        ? 'no unbounded scrollTop corrections mid-sweep'
        : 'scrollTop corrections=' + JSON.stringify(problems.scrollTop.slice(0, 3)));

    for (const s of session.samples) delete s.keyTops;
    const failCount = session.checks.filter((c) => !c.pass).length;
    return {
      done: true,
      ok: failCount === 0,
      failCount,
      checks: session.checks,
      samples: session.samples,
      summary: {
        maxScroll: Math.round(session.maxScroll),
        sweepPx: Math.round(session.sweepPx),
        sampleEveryPx: Math.round(session.sampleEveryPx),
        pxPerFrame: Math.round(session.pxPerFrame),
        sampleCount: session.samples.length,
        traveledPx: Math.round(session.totalTraveled),
        observedScrollSpanPx: Math.round(observedScrollSpan),
        elapsedMs: Math.round(performance.now() - session.startedPerf),
      },
    };
  }

  // Continuous scrollbar-like deep sweep with live + settled sampling. The
  // probe verifies the fixed Activity presentation alongside scroll geometry.
  async function probeScrollParity(options) {
    options = options || {};
    const rowsEl = document.getElementById('rows');
    if (!rowsEl || !rowsEl.querySelector('.table-wrap')) {
      return {
        done: true,
        ok: false,
        failCount: 1,
        checks: [{
          name: 'SCROLL_PARITY_RUNNABLE',
          pass: false,
          detail: '#rows virtualized container missing',
        }],
        samples: [],
        summary: { traveledPx: 0, sampleCount: 0, elapsedMs: 0 },
      };
    }
    const sweepPx = Math.max(Number(options.sweepPx) || 60000, 1);
    const sampleEveryPx = Math.max(Number(options.sampleEveryPx) || 680, 200);
    const pxPerFrame = Math.max(Number(options.pxPerFrame) || 136, 1);
    const idleTimeoutMs = Number(options.idleTimeoutMs) || 120000;
    const rawChunkPx = Number(options.chunkPx);
    const chunkPx = Number.isFinite(rawChunkPx) && rawChunkPx > 0 ? rawChunkPx : Infinity;
    const rawChunkBudgetMs = Number(options.chunkBudgetMs);
    const chunkBudgetMs = Number.isFinite(rawChunkBudgetMs) && rawChunkBudgetMs > 0
      ? rawChunkBudgetMs : Infinity;
    const requestedId = options.sessionId !== undefined && options.sessionId !== null
      ? String(options.sessionId) : null;
    const commandStartedAt = Date.now();

    let session = scrollParitySession;
    const resume = session && requestedId !== null && session.id === requestedId;
    if (resume && session.finalResult) {
      // The sweep already finished; re-issuing the same session id returns
      // the same verdict instead of re-running the aggregation.
      return session.finalResult;
    }
    if (!resume) {
      scrollParitySession = session = {
        id: requestedId !== null ? requestedId : String(++scrollParitySessionSeq),
        rowsEl,
        checks: [],
        samples: [],
        corrections: [],
        sweepPx,
        sampleEveryPx,
        pxPerFrame,
        idleTimeoutMs,
        maxScroll: Math.max(0, rowsEl.scrollHeight - rowsEl.clientHeight),
        startedPerf: performance.now(),
        idleElapsedMs: 0,
        phase: 'idle-start',
        legs: [],
        legIndex: 0,
        legPhase: null,
        initialScrollTop: rowsEl.scrollTop,
        legStart: rowsEl.scrollTop,
        commanded: null,
        nextSampleAt: null,
        dir: 1,
        target: null,
        legTraveled: 0,
        totalTraveled: 0,
      };
      const leg1Target = clamp(session.initialScrollTop + sweepPx, 0, session.maxScroll);
      const leg2Target = clamp(leg1Target - sweepPx, 0, session.maxScroll);
      session.legs = [
        { target: leg1Target, phase: 'descend' },
        { target: leg2Target, phase: 'ascend' },
      ];
      session.expectedTravel = Math.abs(leg1Target - session.initialScrollTop) +
        Math.abs(leg2Target - leg1Target);
    }
    const addCheck = (name, pass, detail) => session.checks.push({ name, pass, detail });
    const budgetExhausted = () => Number.isFinite(chunkBudgetMs) &&
      Date.now() - commandStartedAt >= chunkBudgetMs;
    const beginLeg = (legIndex) => {
      const leg = session.legs[legIndex];
      session.legIndex = legIndex;
      session.legPhase = leg.phase;
      session.legStart = session.rowsEl.scrollTop;
      session.target = leg.target;
      session.dir = leg.target > session.legStart ? 1 : -1;
      session.commanded = session.legStart;
      session.nextSampleAt = session.legStart + session.dir * sampleEveryPx;
      session.legTraveled = 0;
    };
    const nextIdleSlice = () => Math.max(250, Math.min(
      idleTimeoutMs - session.idleElapsedMs,
      Number.isFinite(chunkBudgetMs) ? chunkBudgetMs - (Date.now() - commandStartedAt) : Infinity,
      8000));
    const failIdle = (where, error) => {
      const detail = 'renderer did not settle ' + where + ': ' +
        (error && error.message ? error.message : error);
      if (session.phase === 'idle-start') {
        // Mirrors the one-shot probe's early return (minimal summary).
        return {
          done: true,
          ok: false,
          failCount: session.checks.length + 1,
          checks: session.checks.concat([{ name: 'SCROLL_PARITY_IDLE', pass: false, detail }]),
          samples: session.samples,
          summary: {
            traveledPx: 0,
            sampleCount: session.samples.length,
            elapsedMs: Math.round(performance.now() - session.startedPerf),
          },
        };
      }
      session.checks.push({ name: 'SCROLL_PARITY_IDLE', pass: false, detail });
      session.phase = 'finalize';
      return null;
    };

    while (true) {
      if (session.phase === 'idle-start') {
        if (budgetExhausted()) {
          return probeScrollParityPartial(session, commandStartedAt, true, 'idle-start');
        }
        const sliceMs = nextIdleSlice();
        const sliceStartedAt = Date.now();
        let sliceError = null;
        try {
          await whenIdle(sliceMs);
        } catch (error) {
          sliceError = error;
        }
        if (sliceError) {
          const elapsedMs = Math.min(sliceMs, Math.max(0, Date.now() - sliceStartedAt));
          if (elapsedMs < sliceMs) {
            // whenIdle rejected before its slice expired (renderer lastError
            // path): fail fast exactly like the one-shot probe, which treats
            // any whenIdle rejection as an immediate settle failure.
            const failed = failIdle('before sweeping', sliceError);
            if (failed) return failed;
            continue;
          }
          session.idleElapsedMs += elapsedMs;
          if (session.idleElapsedMs >= idleTimeoutMs) {
            const failed = failIdle('before sweeping', sliceError);
            if (failed) return failed;
            continue;
          }
          if (budgetExhausted()) {
            return probeScrollParityPartial(session, commandStartedAt, true, 'idle-start');
          }
          continue;
        }
        const initial = sampleScrollState();
        initial.kind = 'settled';
        initial.phase = 'start';
        session.samples.push(initial);
        beginLeg(0);
        session.phase = 'drive';
        continue;
      }
      if (session.phase === 'drive') {
        if (Math.abs(session.target - session.legStart) < 1) {
          // Mirrors the one-shot loop: legs with no travel are skipped without
          // an idle wait or a settled sample.
          session.legIndex++;
          if (session.legIndex >= session.legs.length) {
            session.phase = 'finalize';
          } else {
            beginLeg(session.legIndex);
          }
          continue;
        }
        if (budgetExhausted()) {
          return probeScrollParityPartial(session, commandStartedAt, false, 'drive');
        }
        const remainingBudget = Number.isFinite(chunkBudgetMs)
          ? chunkBudgetMs - (Date.now() - commandStartedAt) : Infinity;
        const result = await driveLegChunk(
          session,
          Date.now() + Math.max(1, remainingBudget),
          chunkPx);
        if (result.detached) {
          session.checks.push({
            name: 'SCROLL_PARITY_RUNNABLE',
            pass: false,
            detail: '#rows virtualized container detached mid-sweep',
          });
          session.phase = 'finalize';
          continue;
        }
        if (result.budgetExhausted) {
          return probeScrollParityPartial(session, commandStartedAt, false, 'drive');
        }
        if (result.legDone) {
          session.totalTraveled += session.legTraveled;
          session.phase = 'idle-settle';
          session.idleElapsedMs = 0;
        }
        continue;
      }
      if (session.phase === 'idle-settle') {
        if (budgetExhausted()) {
          return probeScrollParityPartial(session, commandStartedAt, true, 'idle-settle');
        }
        const sliceMs = nextIdleSlice();
        const sliceStartedAt = Date.now();
        let sliceError = null;
        try {
          await whenIdle(sliceMs);
        } catch (error) {
          sliceError = error;
        }
        if (sliceError) {
          const elapsedMs = Math.min(sliceMs, Math.max(0, Date.now() - sliceStartedAt));
          if (elapsedMs < sliceMs) {
            failIdle('after ' + session.legPhase, sliceError);
            continue;
          }
          session.idleElapsedMs += elapsedMs;
          if (session.idleElapsedMs >= idleTimeoutMs) {
            failIdle('after ' + session.legPhase, sliceError);
            continue;
          }
          if (budgetExhausted()) {
            return probeScrollParityPartial(session, commandStartedAt, true, 'idle-settle');
          }
          continue;
        }
        const settled = sampleScrollState();
        settled.kind = 'settled';
        settled.phase = session.legPhase + '-end';
        settled.commanded = round2(session.target);
        settled.traveled = round2(session.legTraveled);
        session.samples.push(settled);
        session.legIndex++;
        if (session.legIndex >= session.legs.length) {
          session.phase = 'finalize';
        } else {
          beginLeg(session.legIndex);
          session.phase = 'drive';
        }
        continue;
      }
      const finalResult = finalizeScrollParity(session);
      session.finalResult = finalResult;
      return finalResult;
    }
  }

  // The Rust shell's read-only debug facade (media/rust-history/loader.js).
  function facade() {
    return window.__editchainRendererDebug;
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
  // (crates/editchain-history-renderer/src/app/rows.rs `format_date`): month/day/
  // year/hour/minute in UTC with a fixed 12-hour clock. The legacy JS
  // bootstrap used the host locale; the Rust shell is deliberately
  // host-independent, so the probe expectation is computed the same way
  // instead of via Intl.
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
      return Promise.reject(new Error('window.__editchainRendererDebug.whenIdle missing'));
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
        canvasCount: document.querySelectorAll('canvas').length,
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
  // Checks target the Rust shell's contract: row-local SVG fragments are
  // checked per row, and the date column is checked against the shell's
  // deterministic UTC format.
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

    // Check 3: graph geometry is aligned and lane-backed. The parity fix moved
    // the graph INTO each row as a row-local svg.graph-row-fragment (proven by
    // checks 3b/3c), so this check
    // asserts the lane contract and the absence of any canvas surface: every
    // row renders a visible .graph-cell and a node marker whose lane maps onto
    // a fixed laneXAll center, and no canvas exists.
    if (wrapEl && g && typeof g.laneXAll === 'function') {
      const laneX = g.laneXAll();
      const rowEls = wrapEl.querySelectorAll('.row:not(.row-placeholder)');
      let nodesOk = true;
      let firstFail = null;
      rowEls.forEach((row) => {
        const absIdx = Number(row.getAttribute('data-row'));
        const cell = row.querySelector('.graph-cell');
        if (!cell || cell.getBoundingClientRect().width <= 0) {
          nodesOk = false;
          firstFail = firstFail || { rowIdx: absIdx, reason: 'no visible graph cell' };
          return;
        }
        const marker = row.classList.contains('row-subop')
          ? cell.querySelector('.graphDot')
          : cell.querySelector('.graphDot, .graphBundleCapsule');
        if (!marker) {
          nodesOk = false;
          firstFail = firstFail || { rowIdx: absIdx, reason: 'no node marker' };
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
      const canvases = document.querySelectorAll('canvas');
      const graphCell = wrapEl.querySelector('.row:not(.row-placeholder) .graph-cell');
      const graphW = graphCell ? graphCell.getBoundingClientRect().width : 0;
      checks.push({
        name: 'GRAPH_NODE_ALIGNMENT',
        pass: nodesOk && canvases.length === 0 && Array.isArray(laneX) && laneX.length > 0,
        detail: (nodesOk && canvases.length === 0)
          ? 'per-row SVG fragments replace the overlay canvas (graph column w=' +
            Math.round(graphW) + 'px), ' + laneX.length + ' lane centers, every row lane mapped'
          : 'nodesOk=' + nodesOk + ' canvases=' + canvases.length +
            ' laneX=' + JSON.stringify(laneX) + ' firstFail=' + JSON.stringify(firstFail),
      });
    }

    // Check 3b: every hydrated rendered row owns a ROW-LOCAL graph fragment
    // inside its own .graph-cell — svg.graph-row-fragment, aria-hidden=true
    // (the scrolling/graph parity contract).
    if (wrapEl && !viewMessage) {
      const rows = Array.from(wrapEl.querySelectorAll('.row:not(.row-placeholder)'));
      const problems = [];
      let fragments = 0;
      for (const row of rows) {
        const abs = Number(row.getAttribute('data-row'));
        const info = rowFragmentInfo(row);
        if (!info.cell || !info.svg) {
          problems.push({ row: abs, reason: info.note });
          continue;
        }
        if (!info.ariaHidden) {
          problems.push({ row: abs, reason: 'aria-hidden != true' });
          continue;
        }
        const count = info.cell.querySelectorAll('svg.graph-row-fragment').length;
        if (count !== 1) {
          problems.push({ row: abs, reason: 'expected 1 fragment, found ' + count });
          continue;
        }
        fragments++;
      }
      checks.push({
        name: 'ROW_GRAPH_FRAGMENT',
        pass: rows.length > 0 && problems.length === 0 && fragments === rows.length,
        detail: problems.length === 0 && rows.length > 0
          ? rows.length + '/' + rows.length + ' hydrated rows own a row-local ' +
            'svg.graph-row-fragment[aria-hidden=true]'
          : rows.length + ' hydrated rows, ' + fragments + ' fragments; first ' +
            'problems=' + JSON.stringify(problems.slice(0, 3)),
      });
    }

    // Check 3c: the fragment's geometry is vertically aligned to the SAME row
    // (fragment fills the row and its marker centre sits on the row centre,
    // within 1 CSS px). Sub-op rows with no drawn marker still must be covered
    // by a row-aligned fragment box.
    if (wrapEl && !viewMessage) {
      const rows = Array.from(wrapEl.querySelectorAll('.row:not(.row-placeholder)'));
      const misaligned = [];
      let maxDelta = 0;
      let aligned = 0;
      for (const row of rows) {
        const info = rowFragmentInfo(row);
        if (!info.svg) continue;
        const rowBox = row.getBoundingClientRect();
        const svgBox = info.svg.getBoundingClientRect();
        const topDelta = Math.abs(svgBox.top - rowBox.top);
        const hDelta = Math.abs(svgBox.height - rowBox.height);
        const geomCenter = fragmentMarkerCenterY(info.svg);
        const centerDelta = geomCenter === null
          ? Math.abs((svgBox.top + svgBox.height / 2) - (rowBox.top + rowBox.height / 2))
          : Math.abs(geomCenter - (rowBox.top + rowBox.height / 2));
        const delta = Math.max(topDelta, hDelta, centerDelta);
        if (delta > maxDelta) maxDelta = delta;
        if (delta <= 1) {
          aligned++;
        } else if (misaligned.length < 5) {
          misaligned.push({
            row: Number(row.getAttribute('data-row')),
            topDelta: round2(topDelta),
            hDelta: round2(hDelta),
            centerDelta: round2(centerDelta),
          });
        }
      }
      checks.push({
        name: 'GRAPH_FRAGMENT_ROW_ALIGNMENT',
        pass: rows.length > 0 && misaligned.length === 0,
        detail: misaligned.length === 0
          ? (rows.length > 0
              ? 'fragment geometry centered on ' + aligned + '/' + rows.length +
                ' rows (max delta ' + round2(maxDelta) + 'px)'
              : 'no hydrated rows')
          : 'max delta ' + round2(maxDelta) + 'px; first=' + JSON.stringify(misaligned),
      });
    }

    // Check 4: grid columns share boundaries between header and rows.
    if (headerEl && wrapEl) {
      const headerCells = headerEl.querySelectorAll('.th');
      const firstRow = wrapEl.querySelector('.row');
      let colsOk = true;
      let firstFailCol = null;
      if (firstRow) {
        const colClasses = [
          'graph-cell', 'activity-cell', 'tags-cell', 'text-cell', 'date-cell',
          'author-cell', 'commit-cell',
        ];
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

    // Check 5c: the default visible Date track must fit its complete label.
    // A custom user drag may intentionally narrow it later; this probe runs
    // against the freshly opened default layout.
    if (wrapEl) {
      const dateCells = Array.from(wrapEl.querySelectorAll('.date-cell'))
        .filter((cell) => (cell.textContent || '').trim() !== '' &&
          getComputedStyle(cell).display !== 'none');
      const clipped = dateCells.filter((cell) =>
        cell.scrollWidth > cell.clientWidth + 1);
      checks.push({
        name: 'DATE_COLUMN_FITS',
        pass: clipped.length === 0,
        detail: clipped.length === 0
          ? dateCells.length + ' visible date labels fit without ellipsis'
          : clipped.length + '/' + dateCells.length + ' date labels overflow; first=' +
            JSON.stringify({
              text: (clipped[0].textContent || '').trim(),
              clientWidth: clipped[0].clientWidth,
              scrollWidth: clipped[0].scrollWidth,
            }),
      });
    }

    // Check 5d: the renderer exposes no visual backend/status strip above the
    // actual history controls.
    const rendererStatusBars = document.querySelectorAll(
      '#gpu-toolbar, #gpu-backend, #gpu-status');
    checks.push({
      name: 'RENDERER_STATUS_BAR_ABSENT',
      pass: rendererStatusBars.length === 0,
      detail: rendererStatusBars.length === 0
        ? 'no SVG backend/status strip'
        : rendererStatusBars.length + ' renderer status elements remain',
    });

    // Check 5e: the controls bar must fit its container.
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

    // Check 5f: the graph column must stay visible (never collapsed/hidden).
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

    // Check 5g: the production cell classes obey the Pulse geometry — Activity
    // and Tags are always visible before Content, author/commit are hidden,
    // and date drops only at the narrowest breakpoint.
    if (wrapEl && !viewMessage) {
      const firstRow = wrapEl.querySelector('.row:not(.row-placeholder)');
      if (firstRow) {
        const rowsBox = rowsEl.getBoundingClientRect();
        const innerW = window.innerWidth || rowsEl.clientWidth || 0;
        const hidden = new Set(['author', 'commit']);
        if (innerW <= 400) hidden.add('date');
        const cols = [
          { name: 'activity', cls: 'activity-cell' },
          { name: 'tags', cls: 'tags-cell' },
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

    // Check 5h: readable contrast for Content/Date/Author/Commit text.
    if (wrapEl && !viewMessage) {
      const firstRow = wrapEl.querySelector('.row:not(.row-placeholder)');
      if (firstRow) {
        const bg = effectiveBackground(firstRow);
        const cells = [
          { sel: '.activity-cell', name: 'activity' },
          { sel: '.tags-cell', name: 'tags' },
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

    // Check 5ja: each visible header boundary has an ordered, visibly marked
    // drag target. Activity and Tags participate in the same resize contract
    // as Graph, Content, and Date.
    if (headerEl && !viewMessage) {
      const handles = Array.from(headerEl.querySelectorAll('.col-resize-handle'));
      const expected = ['graph', 'activity', 'tags', 'content'];
      if ((window.innerWidth || 0) > 400) expected.push('date');
      const actual = handles.map((handle) => handle.getAttribute('data-col') || '');
      const positions = handles.map((handle) => handle.getBoundingClientRect().left);
      const ordered = positions.every((left, index) =>
        index === 0 || left > positions[index - 1]);
      const visibleIndicators = handles.every((handle) => {
        const style = getComputedStyle(handle, '::after');
        return style.width === '1px' && style.backgroundColor !== 'rgba(0, 0, 0, 0)';
      });
      checks.push({
        name: 'RESIZE_HANDLES_COMPLETE',
        pass: actual.join('|') === expected.join('|') && ordered && visibleIndicators,
        detail: 'expected=' + expected.join('|') + '; actual=' + actual.join('|') +
          '; ordered=' + ordered + '; indicators=' + visibleIndicators,
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
      // Pulse renders exactly five columnheaders
      // (graph/activity/tags/content/date); author/commit are hidden cells.
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

    // Check 5m: graph-endpoint subtitles are visible and any fallback id
    // remains short. Named sessions can be ordinary prose with no id prefix.
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

    // Check 5q: common clean-state outcome chrome is globally quiet by default.
    if (wrapEl && !viewMessage) {
      const commonBadges = Array.from(wrapEl.querySelectorAll(
        '.out-badge.outcome-success'));
      checks.push({
        name: 'COMMON_ROW_BADGES_DEFAULT_OFF',
        pass: commonBadges.length === 0,
        detail: commonBadges.length === 0
          ? 'no repeated ok outcome badges'
          : commonBadges.length + ' repeated ok outcome badge(s)',
      });
    }

    // Check 5qa: every hydrated row has one populated Activity cell, and no
    // legacy activity badge remains embedded in Content.
    if (wrapEl && !viewMessage) {
      const rows = Array.from(wrapEl.querySelectorAll('.row:not(.row-placeholder)'));
      const bad = [];
      for (const row of rows) {
        const cells = row.querySelectorAll('.activity-cell');
        const cell = cells[0] || null;
        const label = cell && cell.querySelector('.activity-label');
        const text = label ? (label.textContent || '').trim() : '';
        const data = row.getAttribute('data-classification') || '';
        if (cells.length !== 1 || !text || text !== data || row.querySelector('.act-badge')) {
          if (bad.length < 5) {
            bad.push({
              row: row.getAttribute('data-row'),
              cells: cells.length,
              text,
              data,
              contentBadge: !!row.querySelector('.act-badge'),
            });
          }
        }
      }
      checks.push({
        name: 'ACTIVITY_COLUMN_COMPLETE',
        pass: rows.length > 0 && bad.length === 0,
        detail: bad.length === 0
          ? rows.length + ' rows classified outside Content'
          : 'first bad=' + JSON.stringify(bad),
      });
    }

    // Check 5qb: every chip-like row annotation belongs directly to the Tags
    // column, which sits between Activity and Content. Content stays prose.
    if (wrapEl && !viewMessage) {
      const rows = Array.from(wrapEl.querySelectorAll('.row:not(.row-placeholder)'));
      const chipSelector = [
        '.git-prefix-chip', '.bundle-count', '.bundle-status',
        '.session-chip', '.rel-badge', '.out-badge', '.work-unit-count',
      ].join(',');
      const chips = Array.from(wrapEl.querySelectorAll(chipSelector));
      const header = Array.from(headerEl ? headerEl.querySelectorAll('.th') : [])
        .map((cell) => (cell.textContent || '').trim());
      const misplaced = chips.filter((chip) =>
        !chip.parentElement || !chip.parentElement.classList.contains('tags-cell'));
      const contentChips = wrapEl.querySelectorAll('.text-cell :is(' + chipSelector + ')');
      const rowsWithoutOneTagsCell = rows.filter((row) =>
        row.querySelectorAll('.tags-cell').length !== 1);
      checks.push({
        name: 'TAGS_COLUMN_OWNS_CHIPS',
        pass: rows.length > 0 &&
          header.join('|') === 'Graph|Activity|Tags|Content|Date' &&
          misplaced.length === 0 && contentChips.length === 0 &&
          rowsWithoutOneTagsCell.length === 0,
        detail: 'header=' + header.join('|') + '; chips=' + chips.length +
          '; misplaced=' + misplaced.length + '; content=' + contentChips.length +
          '; missing cells=' + rowsWithoutOneTagsCell.length,
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
          } else if (!chip.parentElement.classList.contains('tags-cell')) {
            problems.push(row.node_key + ': Git prefix chip is outside Tags');
          } else if ((el.querySelector('.text-cell')?.textContent || '')
            .includes(expectedPrefix + ':')) {
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

    // Check 7: expanded rows retain hierarchy indentation and use the shared
    // Content grammar (file grammar for edits; icon/title/subtitle otherwise).
    if (wrapEl) {
      const subopRows = wrapEl.querySelectorAll('.row.row-subop');
      if (subopRows.length) {
        let iconsOk = true;
        let indentOk = true;
        subopRows.forEach((r) => {
          const icon = r.classList.contains('row-file')
            ? r.querySelector('.file-icon')
            : r.querySelector('.content-icon[data-content-icon] svg.content-icon-svg path');
          if (!icon) iconsOk = false;
          if (r.getAttribute('data-activity-bundle') === 'work-group' &&
              (!r.querySelector('.content-icon[data-content-icon="layers"]') ||
               r.querySelector('.content-title'))) iconsOk = false;
          const pad = parseFloat(getComputedStyle(r.querySelector('.text-cell')).paddingLeft);
          if (!(pad >= 24)) indentOk = false;
        });
        checks.push({
          name: 'SUBOP_CONTENT_INDENT',
          pass: iconsOk && indentOk,
          detail: 'subopRows=' + subopRows.length + ' iconsOk=' + iconsOk + ' indentOk=' + indentOk,
        });
      } else {
        checks.push({
          name: 'SUBOP_CONTENT_INDENT',
          pass: true,
          detail: 'no sub-op rows in this view (skipped)',
        });
      }
    }

    // Check 8: the Rust renderer contract — the facade is the rust-history
    // loader, the shell is data-ready, every row owns an SVG fragment, no
    // canvas exists, lane centers are fixed, and frames have been submitted.
    const canvases = document.querySelectorAll('canvas');
    const renderedRows = document.querySelectorAll(
      '#rows .row[data-row][data-key]:not(.row-placeholder)');
    const fragmentRows = document.querySelectorAll(
      '#rows .row[data-row][data-key]:not(.row-placeholder) svg.graph-row-fragment');
    const laneX = g && typeof g.laneXAll === 'function' ? g.laneXAll() : null;
    const metrics = g && typeof g.metrics === 'function' ? g.metrics() : null;
    const snap = g && typeof g.snapshot === 'function' ? g.snapshot() : null;
    const frameRows = snap && Array.isArray(snap.rows) ? snap.rows.length : -1;
    const rustContractOk = !!g &&
      g.loader === 'rust-history' &&
      dataReady() &&
      canvases.length === 0 &&
      frameRows > 0 &&
      renderedRows.length >= frameRows &&
      fragmentRows.length === renderedRows.length &&
      Array.isArray(laneX) && laneX.length >= 1 &&
      !!metrics && metrics.renderCount > 0;
    checks.push({
      name: 'RUST_RENDERER_CONTRACT',
      pass: rustContractOk,
      detail: rustContractOk
        ? 'rust-history loader, dataReady, no canvas, frame rows=' + frameRows +
          ', DOM rows=' + renderedRows.length +
          ' with row-local fragments=' + fragmentRows.length + ', ' + laneX.length +
          ' lanes, renderCount=' + (metrics ? metrics.renderCount : 'n/a')
        : 'loader=' + (g ? g.loader : 'MISSING') + ' dataReady=' + dataReady() +
          ' canvases=' + canvases.length + ' frameRows=' + frameRows +
          ' rendered=' + renderedRows.length + ' fragments=' + fragmentRows.length +
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
      domNodes: document.querySelectorAll('*').length,
      dataReady: dataReady(),
      placeholders: hasPlaceholders(),
      canvasCount: document.querySelectorAll('canvas').length,
    };
  }

  window.__editchainDebug = {
    whenIdle,
    dumpLayout,
    assertLayout,
    getMetrics,
    probeScrollParity,
  };
})();
