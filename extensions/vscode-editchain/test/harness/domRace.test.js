// Node tests for the webview renderer race fixes in media/main.js, driven
// against the REAL harness page (test/harness/index.html) in headless Chromium
// via puppeteer-core. The harness fixture bridge answers requests
// synchronously and exposes controlled-release hooks, so every regression is
// deterministic — no wall-clock sleeps decide the outcome.
//
//  1. Profile-reset race: Raw -> Activity (via setProfile/resetHistory) with a
//     HELD GetWindow must clear the stale DOM grid, clear data-readiness, and
//     leave nothing interactive until the new generation's rows arrive; once
//     released, keyboard Enter on a focused CACHE-BACKED row must open the
//     inspector (the stale-row race used to swallow Enter because the abs
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

test('profile switch with a held GetWindow clears stale rows; Enter then opens the inspector on a cache-backed row', async () => {
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

    // Keyboard Enter on a focused, cache-backed row must open the inspector —
    // the exact interaction the stale-row race broke (Enter was delivered to a
    // stale .row whose abs index was absent from the cleared cache).
    const key = await page.evaluate(() => {
      const row = document.querySelector('.row:not(.row-placeholder)');
      if (!row) return { error: 'no rendered row for keyboard probe' };
      const abs = Number(row.getAttribute('data-row'));
      row.focus();
      row.dispatchEvent(new KeyboardEvent('keydown', { key: 'Enter', bubbles: true }));
      return { abs, cacheBacked: window.__editchainRowAt(abs) != null };
    });
    assert.equal(key.error, undefined);
    assert.equal(key.cacheBacked, true, 'Enter must target a cache-backed row');
    await page.waitForFunction(() => {
      const layoutEl = document.getElementById('layout');
      return layoutEl.classList.contains('has-detail') &&
        !!document.querySelector('.row.row-selected');
    }, { timeout: 10000, polling: 50 });

    const inspector = await page.evaluate((abs) => {
      const layoutEl = document.getElementById('layout');
      const sel = document.querySelector('.row.row-selected');
      const detail = document.getElementById('detail');
      return {
        hasDetail: layoutEl.classList.contains('has-detail'),
        selectedAbs: sel ? Number(sel.getAttribute('data-row')) : null,
        selectedCacheBacked: sel ? window.__editchainRowAt(Number(sel.getAttribute('data-row'))) != null : false,
        detailActions: detail.querySelectorAll('.detail-btn').length,
        detailTitle: (detail.querySelector('.detail-title') || {}).textContent || '',
      };
    }, key.abs);
    assert.equal(inspector.hasDetail, true, 'Enter must open the inspector');
    assert.equal(inspector.selectedAbs, key.abs, 'the inspected row must be the focused row');
    assert.equal(inspector.selectedCacheBacked, true);
    assert.ok(inspector.detailActions > 0, 'details must resolve, not hang on the loading state');

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
