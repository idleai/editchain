// Node tests for the webview renderer race fixes in media/main.js, driven
// against the REAL harness page (test/harness/index.html) in headless Chromium
// via puppeteer-core. The harness fixture bridge answers requests
// synchronously and exposes controlled-release hooks, so every regression is
// deterministic — no wall-clock sleeps decide the outcome.
//
//  1. Profile-reset race: Raw -> Activity (via setProfile/resetHistory) with a
//     HELD GetWindow must clear the stale DOM grid, clear data-readiness, and
//     leave nothing interactive until the new generation's rows arrive; once
//     released, keyboard Enter on a focused CACHE-BACKED row must request its
//     raw JSON editor (the stale-row race used to swallow Enter because the abs
//     index was absent from the cleared cache).
//
//  2. Group-chip prepend semantics: upward incremental scrolling that
//     prepends rows across a group boundary must place the chip on the FIRST
//     (newest) row of each run — identical before and after a full reanchor
//     rebuild — while rows stay contiguous at ROW_H height.
//
//  3. Stale-chip cleanup: a mid-group reanchor marks the first rendered row
//     as a group start solely because it is the window's top edge; prepending
//     same-group rows above it must strip that chip instead of leaving a
//     duplicate stale boundary (one-row prepends must not accumulate chips).
//
// Run: node --test test/harness/domRace.test.js
'use strict';

const { test, before, after } = require('node:test');
const assert = require('node:assert/strict');
const path = require('node:path');
const puppeteer = require('puppeteer-core');

const EXT_ROOT = path.join(__dirname, '..', '..');
const HARNESS = 'file://' + path.join(EXT_ROOT, 'test', 'harness', 'index.html');

const CHROME = process.env.CHROME_PATH ||
  '/mnt/hot/ambientlight/.cache/puppeteer/chrome/linux-151.0.7922.71/chrome-linux64/chrome';

let browser;

before(async () => {
  browser = await puppeteer.launch({
    executablePath: CHROME,
    headless: 'new',
    args: ['--no-sandbox', '--disable-setuid-sandbox'],
  });
});

after(async () => {
  if (browser) await browser.close();
});

async function newPage() {
  const page = await browser.newPage();
  await page.setViewport({ width: 1440, height: 900 });
  const errors = [];
  page.on('pageerror', (e) => errors.push(e.message));
  await page.goto(HARNESS, { waitUntil: 'networkidle0' });
  return { page, errors };
}

/** Load a fixture scenario through the real harness startup handshake. */
async function bootScenario(page, scenario) {
  await page.evaluate((name) => {
    window.__editchainSetScenario(name);
    window.__editchainStart();
  }, scenario);
  await page.evaluate(() => window.__editchainDebug.whenIdle(20000));
}

test('Pulse is the sole narrative-first, single-pane presentation', async () => {
  const { page, errors } = await newPage();
  try {
    await bootScenario(page, 'workUnits');

    const state = await page.evaluate(() => {
      const graph = document.querySelector('.row .graph-cell');
      const summary = document.querySelector('.row .summary');
      const date = document.querySelector('.row .date-cell');
      return {
        treatment: document.body.dataset.treatment,
        treatmentControl: !!document.getElementById('treatment-control'),
        treatmentButtons: document.querySelectorAll('.treatment-btn').length,
        productMark: !!document.getElementById('product-mark'),
        graphWidth: graph ? graph.getBoundingClientRect().width : 0,
        summaryFont: summary ? getComputedStyle(summary).fontSize : '',
        dateFont: date ? getComputedStyle(date).fontSize : '',
        visibleColumns: Array.from(document.querySelectorAll('.tbl-header .th'))
          .filter((cell) => getComputedStyle(cell).display !== 'none' &&
            cell.getBoundingClientRect().width > 0.5)
          .map((cell) => (cell.textContent || '').trim()),
        secondaryPane: !!document.getElementById('detail') ||
          document.getElementById('layout').classList.contains('has-detail'),
      };
    });

    assert.equal(state.treatment, 'pulse');
    assert.equal(state.treatmentControl, false, 'the prototype treatment switch must be removed');
    assert.equal(state.treatmentButtons, 0);
    assert.equal(state.productMark, false, 'the toolbar must not repeat the EditChain product sign');
    assert.ok(state.graphWidth > 0, 'Pulse retains a visible topology rail');
    assert.equal(state.summaryFont, '13px', 'row prose uses the normalized readable type size');
    assert.equal(state.dateFont, '11px', 'metadata uses the normalized secondary type size');
    assert.deepEqual(state.visibleColumns, ['Graph', 'Content', 'Date']);
    assert.equal(state.secondaryPane, false, 'Pulse remains a single-pane history surface');
    assert.deepEqual(errors, [], 'no uncaught page errors: ' + errors.join(' | '));
  } finally {
    await page.close();
  }
});

test('Markdown summaries render semantic inline content without executing imported HTML', async () => {
  const { page, errors } = await newPage();
  try {
    await bootScenario(page, 'longsummary');

    const rendered = await page.evaluate(() => {
      const row = document.querySelector('.row[data-key="node:l:1"]');
      const summary = row && row.querySelector('.summary-text');
      const taskSummary = document.querySelector('.row[data-key="node:l:2"] .summary-text');
      const quoteSummary = document.querySelector('.row[data-key="node:l:3"] .summary-text');
      const fallbackSummary = document.querySelector('.row[data-key="node:l:4"] .summary-text');
      const completedRow = document.querySelector('.row[data-key="node:l:5"]');
      const completedSummary = completedRow?.querySelector('.summary-text');
      const failedSummary = document.querySelector('.row[data-key="node:l:6"] .summary-text');
      const workerRow = document.querySelector('.row[data-key="node:l:7"]');
      const workerSummary = workerRow?.querySelector('.summary-text');
      const truncatedWorkerSummary = document.querySelector(
        '.row[data-key="node:l:8"] .summary-text'
      );
      const summaries = Array.from(document.querySelectorAll('.summary-text'));
      return {
        rowHeight: row ? row.getBoundingClientRect().height : 0,
        text: summaries.map((el) => el.textContent || '').join(' | '),
        heading: !!summary?.querySelector('.md-heading'),
        task: !!taskSummary?.querySelector('.md-task.md-task-done'),
        strong: !!summary?.querySelector('.md-strong'),
        code: !!summary?.querySelector('.md-code'),
        link: !!summary?.querySelector('.md-link'),
        quote: !!quoteSummary?.querySelector('.md-quote'),
        callout: quoteSummary?.querySelector('.md-callout')?.textContent || '',
        strike: !!quoteSummary?.querySelector('.md-strike'),
        more: summary?.querySelector('.md-more')?.textContent || '',
        breaks: summary ? summary.querySelectorAll('.md-break').length : 0,
        anchors: document.querySelectorAll('.summary-text a').length,
        images: document.querySelectorAll('.summary-text img').length,
        injected: window.__markdownInjected === true,
        html: summaries.map((el) => el.innerHTML).join(' | '),
        title: row ? row.getAttribute('title') || '' : '',
        fallback: fallbackSummary ? fallbackSummary.textContent || '' : '',
        toolResults: [completedSummary?.textContent || '', failedSummary?.textContent || ''],
        toolHtml: [completedSummary?.innerHTML || '', failedSummary?.innerHTML || ''].join(' | '),
        toolDetail: completedRow?.getAttribute('title') || '',
        workerResult: workerSummary?.textContent || '',
        workerStrong: !!workerSummary?.querySelector('.md-strong'),
        workerDetail: workerRow?.getAttribute('title') || '',
        truncatedWorker: truncatedWorkerSummary?.textContent || '',
        completedOk: !!completedRow?.querySelector('.outcome-success'),
      };
    });

    assert.equal(rendered.rowHeight, 34, 'Markdown must not disturb virtual-row geometry');
    assert.equal(rendered.heading, true);
    assert.equal(rendered.task, true);
    assert.equal(rendered.strong, true);
    assert.equal(rendered.code, true);
    assert.equal(rendered.link, true);
    assert.equal(rendered.quote, true);
    assert.equal(rendered.callout, 'NOTE');
    assert.equal(rendered.strike, true);
    assert.equal(rendered.more, '+3 lines');
    assert.equal(rendered.breaks, 0, 'the preview renders one calm semantic line, not horizontal paragraphs');
    assert.equal(rendered.anchors, 0, 'imported links stay inert inside the webview');
    assert.equal(rendered.images, 0, 'raw HTML must be omitted rather than injected');
    assert.equal(rendered.injected, false);
    assert.doesNotMatch(rendered.html, /<img|&lt;img/i, 'raw HTML is omitted from the compact preview');
    assert.doesNotMatch(rendered.text, /\*\*|`|^# |\[!NOTE\]|~~/,
      'Markdown punctuation must not leak into displayed prose');
    assert.doesNotMatch(rendered.title, /\*\*|`|^# |\[!NOTE\]|~~/,
      'tooltips use readable plain Markdown text too');
    assert.equal(rendered.fallback, 'Readable fallback',
      'markup-only leading lines are skipped instead of producing an empty +N row');
    assert.deepEqual(rendered.toolResults, ['Script completed', 'Script failed'],
      'structured tool envelopes collapse to one readable lifecycle line');
    assert.doesNotMatch(rendered.toolHtml, /[{}\[\]]|input_text|\\n/,
      'serialized payload syntax must not leak into visible tool content');
    assert.match(rendered.toolDetail, /Wall time 0\.2 seconds/,
      'the complete plain payload remains available as non-primary detail');
    assert.equal(rendered.workerResult, 'Verification complete.',
      'recognized worker status envelopes show their narrative instead of raw JSON');
    assert.equal(rendered.workerStrong, true,
      'Markdown inside a decoded status message receives the same restrained renderer');
    assert.match(rendered.workerDetail, /agent_path/,
      'the serialized worker envelope remains available as non-primary detail');
    assert.equal(rendered.truncatedWorker, 'Recovered preview.',
      'truncated structured envelopes still recover a clean first-line preview');
    assert.equal(rendered.completedOk, false,
      'successful tool prose does not repeat the same state in an ok chip');
    assert.deepEqual(errors, [], 'no uncaught page errors: ' + errors.join(' | '));
  } finally {
    await page.close();
  }
});

test('profile switch clears stale rows; Enter activates raw JSON on a cache-backed row without a side pane', async () => {
  const { page, errors } = await newPage();
  try {
    await bootScenario(page, 'large');

    const baseline = await page.evaluate(() => ({
      rows: document.querySelectorAll('.row').length,
      dataReady: window.__editchainDataReady,
      profile: window.__editchainGetProfile(),
      total: window.__editchainGetTotal(),
      gen: window.__editchainViewGen(),
    }));
    assert.ok(baseline.rows > 0, 'baseline must render rows');
    assert.equal(baseline.dataReady, true);
    assert.equal(baseline.profile, 'activity');
    assert.equal(baseline.total, 600);

    // Pause the progressive loader so the ONLY GetWindow issued after this
    // point is the profile reset's own fetch, then hold that fetch and click
    // Raw through the REAL control path. All synchronous: the reset's GetWindow
    // is captured by the hook before the evaluate returns.
    await page.evaluate(() => {
      window.__editchainPauseLoader = true;
      window.__editchainHoldWindow = {};
      document.getElementById('profile-raw').click();
    });

    const held = await page.evaluate(() => {
      const hold = window.__editchainHoldWindow;
      const active = document.activeElement;
      return {
        held: !!hold && typeof hold.release === 'function',
        rows: document.querySelectorAll('.row').length,
        dataReady: window.__editchainDataReady,
        profile: window.__editchainGetProfile(),
        total: window.__editchainGetTotal(),
        gen: window.__editchainViewGen(),
        message: (document.querySelector('.view-message') || {}).textContent || '',
        activeIsRow: !!(active && active.closest && active.closest('.row')),
      };
    });
    assert.equal(held.held, true, 'the reset GetWindow must be captured by the hold hook');
    assert.equal(held.rows, 0, 'stale DOM rows must disappear while the new window is in flight');
    assert.equal(held.dataReady, false, 'readiness must be cleared during the reset');
    assert.equal(held.profile, 'raw', 'the new profile must be active immediately');
    assert.equal(held.total, -1, 'total must be unknown until the new window arrives');
    assert.ok(held.gen > baseline.gen, 'the view generation must bump on reset');
    assert.match(held.message, /Loading/);
    assert.equal(held.activeIsRow, false, 'no stale row may hold focus during the reset');

    // Release the held window: the new generation's rows must render and every
    // rendered row must be backed by the current profile's cache.
    await page.evaluate(() => {
      const release = window.__editchainHoldWindow.release;
      window.__editchainHoldWindow = null;
      release();
    });
    await page.evaluate(() => window.__editchainDebug.whenIdle(20000));

    const after = await page.evaluate(() => {
      const rows = Array.from(document.querySelectorAll('.row'));
      return {
        rowCount: rows.length,
        cacheBacked: rows.every((r) => {
          const abs = Number(r.getAttribute('data-row'));
          return Number.isFinite(abs) && window.__editchainRowAt(abs) != null;
        }),
        dataReady: window.__editchainDataReady,
        profile: window.__editchainGetProfile(),
      };
    });
    assert.ok(after.rowCount > 0, 'rows must render after the release');
    assert.equal(after.cacheBacked, true, 'every rendered row must be cache-backed under the new profile');
    assert.equal(after.dataReady, true);
    assert.equal(after.profile, 'raw');

    // Keyboard Enter on a focused, cache-backed row must request raw JSON —
    // the exact interaction the stale-row race broke (Enter was delivered to a
    // stale .row whose abs index was absent from the cleared cache).
    const key = await page.evaluate(() => {
      const row = document.querySelector('.row:not(.row-placeholder)');
      if (!row) return { error: 'no rendered row for keyboard probe' };
      const abs = Number(row.getAttribute('data-row'));
      const captured = [];
      const originalPost = window.vscode.postMessage.bind(window.vscode);
      window.vscode.postMessage = (message) => {
        if (message && message.type === 'openJson') captured.push(message);
        return originalPost(message);
      };
      row.focus();
      row.dispatchEvent(new KeyboardEvent('keydown', { key: 'Enter', bubbles: true }));
      window.vscode.postMessage = originalPost;
      return {
        abs,
        cacheBacked: window.__editchainRowAt(abs) != null,
        captured,
        selectedAbs: Number(document.querySelector('.row.row-selected')?.getAttribute('data-row')),
        hasDetailClass: document.getElementById('layout').classList.contains('has-detail'),
        secondaryElement: !!document.getElementById('detail'),
      };
    });
    assert.equal(key.error, undefined);
    assert.equal(key.cacheBacked, true, 'Enter must target a cache-backed row');
    assert.equal(key.selectedAbs, key.abs, 'Enter must select the focused row inline');
    assert.equal(key.captured.length, 1, 'Enter must issue one explicit raw JSON activation');
    assert.equal(key.hasDetailClass, false, 'activation must never split the history surface');
    assert.equal(key.secondaryElement, false, 'the webview must not render a secondary detail pane');

    assert.deepEqual(errors, [], 'no uncaught page errors: ' + errors.join(' | '));
  } finally {
    await page.close();
  }
});

test('prepend across a group boundary keeps chips on the first row, before and after a rebuild', async () => {
  const { page, errors } = await newPage();
  try {
    await bootScenario(page, 'multigroup');

    // Known boundaries: rows 0..99 repo:a, 100..199 repo:b, 200+ session:s1.
    const baselineChips = await page.evaluate(() =>
      Array.from(document.querySelectorAll('.row.row-group-start'))
        .map((r) => Number(r.getAttribute('data-row'))));
    assert.deepEqual(baselineChips, [0, 100, 200]);

    const setScroll = (vis) => page.evaluate((v) => {
      document.getElementById('rows').scrollTop = v * 34;
    }, vis);

    // Scroll DOWN incrementally (small steps) so the window trims above the
    // 100 boundary without a mid-group reanchor: rows 0..119 are trimmed,
    // leaving a window whose first rendered row (120) has NO chip.
    for (const vis of [100, 200, 300, 400, 520]) {
      await setScroll(vis);
      await page.evaluate(() => window.__editchainDebug.whenIdle(20000));
    }
    const trimmed = await page.evaluate(() => {
      const g = window.__editchainGraphState();
      return {
        renderTop: g.renderTop,
        renderBottom: g.renderBottom,
        chips: Array.from(document.querySelectorAll('.row.row-group-start'))
          .map((r) => Number(r.getAttribute('data-row'))),
      };
    });
    assert.equal(trimmed.renderTop, 120, 'down-scroll must trim to renderTop=120');
    assert.equal(trimmed.renderBottom, 599);
    assert.deepEqual(trimmed.chips, [200], 'mid-group trim must not invent a chip at 120');

    // Scroll UP so prependRowsAbove rebuilds rows [0..119] across the boundary
    // at 100. The chips must land on the FIRST row of each run (0 and 100).
    await setScroll(300);
    await page.waitForFunction(() => {
      const g = window.__editchainGraphState();
      return g && g.renderTop === 0;
    }, { timeout: 20000, polling: 50 });
    await page.evaluate(() => window.__editchainDebug.whenIdle(20000));

    const before = await page.evaluate(() => {
      const rows = Array.from(document.querySelectorAll('.row'));
      return {
        chips: Array.from(document.querySelectorAll('.row.row-group-start'))
          .map((r) => Number(r.getAttribute('data-row'))),
        dataRows: rows.map((r) => Number(r.getAttribute('data-row'))),
        heights: Array.from(new Set(rows.map((r) => r.offsetHeight))),
      };
    });
    assert.deepEqual(before.chips, [0, 100, 200], 'prepend chips must mark the newest/first row of each group');
    assert.deepEqual(
      before.dataRows,
      Array.from({ length: 600 }, (_, i) => i),
      'rows must stay contiguous after prepend (virtualization invariant)'
    );
    assert.deepEqual(before.heights, [34], 'every row must keep ROW_H height after prepend');

    // Force a full reanchor rebuild of the SAME range (viewport-resize path
    // rebuilds from cache) and require identical chip positions and row set.
    await page.evaluate(() => {
      window.__chipProbeRowRef = document.querySelector('.row');
    });
    await page.evaluate(() => window.dispatchEvent(new Event('resize')));
    await page.waitForFunction(() => {
      const ref = window.__chipProbeRowRef;
      return !ref || !document.contains(ref);
    }, { timeout: 10000, polling: 50 });
    await page.evaluate(() => window.__editchainDebug.whenIdle(20000));

    const after = await page.evaluate(() => ({
      chips: Array.from(document.querySelectorAll('.row.row-group-start'))
        .map((r) => Number(r.getAttribute('data-row'))),
      dataRows: Array.from(document.querySelectorAll('.row'))
        .map((r) => Number(r.getAttribute('data-row'))),
    }));
    assert.deepEqual(after.chips, before.chips, 'chip positions must be identical before/after a rebuild');
    assert.deepEqual(after.dataRows, before.dataRows, 'the rendered row set must survive the rebuild');

    assert.deepEqual(errors, [], 'no uncaught page errors: ' + errors.join(' | '));
  } finally {
    await page.close();
  }
});

test('prepending same-group rows removes a stale chip left by a mid-group reanchor', async () => {
  const { page, errors } = await newPage();
  try {
    await bootScenario(page, 'multigroup');

    // Known boundaries: rows 0..99 repo:a, 100..199 repo:b, 200+ session:s1.
    const setScroll = (vis) => page.evaluate((v) => {
      document.getElementById('rows').scrollTop = v * 34;
    }, vis);
    const chips = () => page.evaluate(() =>
      Array.from(document.querySelectorAll('.row.row-group-start'))
        .map((r) => Number(r.getAttribute('data-row'))));
    const renderTop = () => page.evaluate(() => window.__editchainGraphState().renderTop);

    // Trim into the middle of repo:b (rows 100..199) by scrolling down
    // incrementally: the first rendered row (120) carries NO chip.
    for (const vis of [100, 200, 300, 400, 520]) {
      await setScroll(vis);
      await page.evaluate(() => window.__editchainDebug.whenIdle(20000));
    }
    assert.equal(await renderTop(), 120);
    assert.deepEqual(await chips(), [200]);

    // Force a MID-GROUP reanchor (viewport-resize path rebuilds the window
    // from cache). reanchorTo marks the first rendered row (120) as a group
    // start solely because it is the window's top edge — even though the real
    // repo:b start is 100. Prepending same-group rows above it must strip that
    // chip instead of leaving a duplicate stale boundary.
    await page.evaluate(() => {
      window.__chipProbeRowRef = document.querySelector('.row');
    });
    await page.evaluate(() => window.dispatchEvent(new Event('resize')));
    await page.waitForFunction(() => {
      const ref = window.__chipProbeRowRef;
      return !ref || !document.contains(ref);
    }, { timeout: 10000, polling: 50 });
    await page.evaluate(() => window.__editchainDebug.whenIdle(20000));
    assert.equal(await renderTop(), 120, 'the mid-group reanchor must keep the same window');
    assert.deepEqual(await chips(), [200], 'no stale chip may survive the mid-group reanchor');

    // Scroll UP across the 100 boundary: prependRowsAbove rebuilds rows
    // 0..119. The chip at the old window edge (120) must not reappear, so the
    // final chips match a full reanchor of the same window.
    await setScroll(300);
    await page.waitForFunction(() => {
      const g = window.__editchainGraphState();
      return g && g.renderTop === 0;
    }, { timeout: 20000, polling: 50 });
    await page.evaluate(() => window.__editchainDebug.whenIdle(20000));
    assert.deepEqual(await chips(), [0, 100, 200], 'prepend must not leave a duplicate stale chip at the old window edge');

    // Incremental upward scrolling inside one group must not accumulate a chip
    // per prepended row: every one-row prepend re-evaluates the boundary, so
    // only the (window-rule) top chip remains for repo:b.
    // Pause the progressive loader so its 300ms syncWindow tick cannot run an
    // extra prepend+trim cycle between assertions (the scroll-handler sync is
    // the only renderer mutation while paused — fully deterministic).
    await page.evaluate(() => { window.__editchainPauseLoader = true; });
    await setScroll(520);
    await page.evaluate(() => window.__editchainDebug.whenIdle(20000));
    for (const vis of [519, 518, 517, 516, 515]) {
      await setScroll(vis);
      await page.evaluate(() => window.__editchainDebug.whenIdle(20000));
      assert.equal(await renderTop(), vis - 400, 'one-row prepends must extend the window top');
      assert.deepEqual(await chips(), [vis - 400, 200], 'one-row prepends must not accumulate stale chips at ' + vis);
    }

    assert.deepEqual(errors, [], 'no uncaught page errors: ' + errors.join(' | '));
  } finally {
    await page.close();
  }
});

// --- Round-two parallel contract: work-unit/bundle grouping races ----------
//
// The Activity profile's work-unit/bundle/promotion/capsule DOM layer must
// follow the same deterministic races as the plain grid:
//   - a profile switch with a HELD window clears grouping/promotion DOM
//     synchronously (no stale ribbons/bundles/rails/capsules left
//     interactive), and the released raw view renders ZERO grouping DOM even
//     though its cached rows carry the wire metadata (Activity vs Raw
//     gating);
//   - deep scroll + one-row prepends/trims across the virtual window never
//     invent a duplicate work-unit start (per-id uniqueness in every slice);
//   - bundle expand/collapse (ArrowRight/ArrowLeft/Space/Enter) and roving
//     focus stay stable across the DOM rebuilds they cause.
//
// These DOM-level assertions are mandatory now that the renderer contract is
// part of this change: missing marker classes fail the race tests. The
// cache/wire-level behaviour is independently covered by
// workUnitBridge.test.js.
'use strict';

test('profile switch with a held window clears work-unit/bundle grouping; raw stays gated; keyboard stays stable', async () => {
  const { page, errors } = await newPage();
  try {
    await bootScenario(page, 'workUnits');

    const baseline = await page.evaluate(() => ({
      state: window.__editchainDebug.workUnitContractState(),
      rows: document.querySelectorAll('.row').length,
      dataReady: window.__editchainDataReady,
      total: window.__editchainGetTotal(),
      workUnitTitle: (document.querySelector('.row[data-key="wu:req1"] .work-unit-ribbon') || {}).textContent || '',
      bundleText: (document.querySelector('.row[data-key="wu:run1"] .summary') || {}).textContent || '',
      unknownBundleStatus: !!document.querySelector('.row[data-key="wu:run1"] .bundle-status'),
      successfulBundleStatus: (document.querySelector('.row[data-key="wu:run2"] .bundle-status') || {}).textContent || '',
    }));
    assert.equal(baseline.rows, 12, 'activity view renders the 12 authored rows');
    assert.equal(baseline.total, 23, 'window total includes expandable bundle member sub-ops');
    assert.equal(baseline.dataReady, true);
    assert.equal(baseline.state.any, true,
      'Activity must render the work-unit/bundle/promotion contract');
    assert.ok(baseline.state.markers.workUnit >= 11,
      'work-unit markers render with the Git section count intentionally suppressed');
    assert.ok(baseline.state.markers.bundle >= 4, 'bundle rows and concise labels rendered in Activity');
    assert.ok(baseline.state.markers.promoted === 5, 'promotion rails rendered in Activity');
    assert.ok(baseline.state.markers.capsule >= 6,
      'execute-run capsule glyphs (1 rect + 2 terminals per typed row) rendered in Activity');
    assert.equal(baseline.state.cacheHasMetadata, true);
    assert.equal(baseline.workUnitTitle, 'User asks to fix the build',
      'work-unit titles must strip Markdown punctuation');
    assert.doesNotMatch(baseline.workUnitTitle, /\*\*|`/);
    assert.equal(baseline.bundleText.trim(), '▸3 commands',
      'typed bundles render one concise structured label without summary/activity/outcome repetition');
    assert.equal(baseline.unknownBundleStatus, false,
      'unknown bundle outcome adds no noisy status label');
    assert.equal(baseline.successfulBundleStatus.trim(), '✓',
      'structured success is a quiet check instead of a second wording chip');

    // Hold the Raw profile's first GetWindow; click Raw through the REAL
    // control path. The reset must clear the grouping DOM synchronously.
    await page.evaluate(() => {
      window.__editchainPauseLoader = true;
      window.__editchainHoldWindow = {};
      document.getElementById('profile-raw').click();
    });
    const held = await page.evaluate(() => {
      const hold = window.__editchainHoldWindow;
      const state = window.__editchainDebug.workUnitContractState();
      return {
        held: !!hold && typeof hold.release === 'function',
        rows: document.querySelectorAll('.row').length,
        dataReady: window.__editchainDataReady,
        profile: state.profile,
        total: window.__editchainGetTotal(),
        markers: state.markers,
        message: (document.querySelector('.view-message') || {}).textContent || '',
      };
    });
    assert.equal(held.held, true, 'the reset GetWindow must be captured by the hold hook');
    assert.equal(held.rows, 0, 'stale rows must disappear while the new window is in flight');
    assert.equal(held.dataReady, false, 'readiness must be cleared during the reset');
    assert.equal(held.profile, 'raw', 'the new profile must be active immediately');
    assert.equal(held.total, -1, 'total must be unknown until the new window arrives');
    assert.deepEqual(held.markers, { workUnit: 0, bundle: 0, promoted: 0, capsule: 0 },
      'no grouping/promotion/capsule DOM may survive into the reset');
    assert.match(held.message, /Loading/);

    // Release: the raw view must render fully, be cache-backed, and keep
    // zero grouping DOM while its cached rows still carry wire metadata.
    await page.evaluate(() => {
      const release = window.__editchainHoldWindow.release;
      window.__editchainHoldWindow = null;
      release();
    });
    await page.evaluate(() => window.__editchainDebug.whenIdle(20000));
    const raw = await page.evaluate(() => {
      const rows = Array.from(document.querySelectorAll('.row'));
      return {
        rows: rows.length,
        cacheBacked: rows.every((r) => {
          const abs = Number(r.getAttribute('data-row'));
          return Number.isFinite(abs) && window.__editchainRowAt(abs) != null;
        }),
        state: window.__editchainDebug.workUnitContractState(),
        checks: window.__editchainDebug.assertLayout(),
      };
    });
    assert.equal(raw.rows, 18, 'raw profile serves the unbundled 18-row stream');
    assert.equal(raw.cacheBacked, true, 'every raw row must be cache-backed');
    assert.deepEqual(raw.state.markers, { workUnit: 0, bundle: 0, promoted: 0, capsule: 0 },
      'raw renders none of the grouping/promotion/capsule DOM');
    assert.equal(raw.state.cacheHasMetadata, true,
      'raw rows must still carry work_unit/promoted wire metadata (gating, not dropping)');
    const rawGated = raw.checks.checks.find((c) => c.name === 'RAW_PROFILE_GATED');
    assert.ok(rawGated && rawGated.pass, 'RAW_PROFILE_GATED layout check must pass');

    // Back to Activity: grouping returns and the ARIA/keyboard behaviour is
    // stable across the bundle expand/collapse DOM rebuilds.
    await page.evaluate(() => {
      window.__editchainHoldWindow = null;
      document.getElementById('profile-activity').click();
    });
    await page.evaluate(() => window.__editchainDebug.whenIdle(20000));
    const activity = await page.evaluate(() => window.__editchainDebug.workUnitContractState());
    assert.equal(activity.profile, 'activity');
    assert.ok(activity.any, 'grouping markers must return in Activity');
    assert.ok(activity.markers.capsule > 0,
      'execute-run capsule glyphs must return in Activity after the reset');

    const probe = await page.evaluate(() => window.__editchainDebug.runWorkUnitProbe(20000));
    console.log('[domRace] work-unit probe:', JSON.stringify(probe.detail));
    assert.equal(probe.pass, true, 'bundle expand/collapse + roving focus must stay stable');

    assert.deepEqual(errors, [], 'no uncaught page errors: ' + errors.join(' | '));
  } finally {
    await page.close();
  }
});

test('deep scroll/prepend/trim across the virtual window never invents duplicate work-unit starts', async () => {
  const { page, errors } = await newPage();
  try {
    await bootScenario(page, 'workUnitsDeep');

    const state = await page.evaluate(() => window.__editchainDebug.workUnitContractState());
    assert.equal(state.any, true,
      'Activity must render the work-unit/bundle/promotion contract');

    const setScroll = (vis) => page.evaluate((v) => {
      document.getElementById('rows').scrollTop = v * 34;
    }, vis);
    const snapshot = () => page.evaluate(() => {
      const rows = Array.from(document.querySelectorAll('.row'));
      const byId = new Map();
      for (const el of rows) {
        if (!el.classList.contains('row-work-unit-start')) continue;
        const abs = Number(el.getAttribute('data-row'));
        const row = window.__editchainRowAt(abs);
        if (!row || !row.work_unit) continue;
        const agg = byId.get(row.work_unit.id) || { starts: 0, keys: [] };
        agg.starts++;
        agg.keys.push(row.node_key);
        byId.set(row.work_unit.id, agg);
      }
      const g = window.__editchainGraphState();
      return {
        renderTop: g.renderTop,
        renderBottom: g.renderBottom,
        rows: rows.length,
        starts: byId.size,
        dupStarts: Array.from(byId.values()).filter((a) => a.starts > 1).length,
      };
    });

    // Scroll down through the whole dataset: the DOM stays windowed
    // (viewport + buffer) and every slice keeps per-id start uniqueness.
    const downSnaps = [];
    for (const vis of [200, 400, 600, 800, 900, 1000, 1100]) {
      await setScroll(vis);
      await page.evaluate(() => window.__editchainDebug.whenIdle(20000));
      downSnaps.push(await snapshot());
    }
    for (const s of downSnaps) {
      assert.equal(s.dupStarts, 0, 'no duplicate work-unit starts in a deep-scroll slice');
      assert.ok(s.rows > 0 && s.rows <= 850, 'DOM must stay windowed (viewport + 2*BUFFER): ' + s.rows);
    }
    assert.ok(downSnaps[downSnaps.length - 1].renderTop >= 700,
      'virtualization must actually advance the rendered window');

    // One-row prepends while scrolling back up: renderTop tracks the scroll
    // and no stale/duplicate start appears at the re-anchored top edge.
    await setScroll(1050);
    await page.evaluate(() => window.__editchainDebug.whenIdle(20000));
    for (const vis of [1049, 1048, 1047, 1046, 1045]) {
      await setScroll(vis);
      await page.evaluate(() => window.__editchainDebug.whenIdle(20000));
      const s = await snapshot();
      assert.equal(s.dupStarts, 0, 'one-row prepends must not accumulate duplicate starts at ' + vis);
      assert.equal(s.renderTop, vis - 400, 'one-row prepends must extend the window top');
    }

    const layout = await page.evaluate(() => window.__editchainDebug.assertLayout());
    const failedLayout = layout.checks.filter((check) => !check.pass);
    assert.equal(layout.failCount, 0,
      'all layout checks pass after deep scroll/prepend/trim: ' + JSON.stringify(failedLayout));

    assert.deepEqual(errors, [], 'no uncaught page errors: ' + errors.join(' | '));
  } finally {
    await page.close();
  }
});
