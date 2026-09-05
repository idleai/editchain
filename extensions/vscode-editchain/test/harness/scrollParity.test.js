// Node tests for the scrolling/graph parity regression coverage, driven
// against the REAL harness page (test/harness/index.html) in headless
// Chromium via puppeteer-core. This is the CPU/SVG parity-reference side of
// the oracle (production media/main.js + the fixture bridge), mirroring the
// same invariants test/vscode/history.e2e.ts asserts against the Rust/WASM
// renderer in real VS Code:
//
//  1. Every hydrated rendered row owns an aria-hidden row-local SVG graph
//     fragment inside its own .graph-cell (the legacy oracle uses graphCell;
//     real VS Code strictly requires the Rust graph-row-fragment class).
//  2. The fragment's geometry is vertically aligned to that same row and
//     stays aligned through a continuous scrollbar-like deep sweep.
//  3. The sweep never produces wrapper drift/snaps, duplicate rendered row
//     keys, reversed/non-monotonic DOM order, or unbounded scrollTop
//     corrections — proven from live mid-motion snapshots AND settled
//     snapshots, in BOTH the default Activity profile and Raw.
//
// The probe (probeScrollParity in layoutProbe.js) samples rather than
// asserting once: every verdict carries concrete diagnostics and the raw
// samples are returned for failure triage. Profile switches go through the
// REAL segmented control.
//
// Run: node --test test/harness/scrollParity.test.js
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

/** Switch profiles through the REAL segmented control and wait for idle. */
async function switchProfile(page, profile) {
  await page.evaluate((name) => {
    document.getElementById('profile-' + name).click();
  }, profile);
  await page.evaluate(() => window.__editchainDebug.whenIdle(30000));
  const active = await page.evaluate(() => window.__editchainGetProfile());
  assert.equal(active, profile, 'segmented control must switch to ' + profile);
}

test('continuous deep scroll keeps row-local graph fragments aligned and the window stable (Activity + Raw)', async () => {
  const { page, errors } = await newPage();
  try {
    await bootScenario(page, 'large');

    const sweep = (profile) => page.evaluate((opts) =>
      window.__editchainDebug.probeScrollParity(opts), {
        profile,
        sweepPx: 40000,
        // Keep consecutive samples within the retained key overlap so a
        // whole-wrapper jump cannot hide behind individually contiguous rows.
        sampleEveryPx: 680,
        pxPerFrame: 136,
        idleTimeoutMs: 30000,
      });

    // Default profile: Activity (collapsed space).
    const activity = await sweep('activity');
    console.log('[scrollParity] activity summary:', JSON.stringify(activity.summary));
    activity.checks.forEach((c) =>
      console.log('[scrollParity]   ' + c.name + ' ' + (c.pass ? 'PASS' : 'FAIL') +
        ' — ' + String(c.detail).slice(0, 400)));
    if (!activity.ok) {
      console.log('[scrollParity] activity failure samples:', JSON.stringify(activity.samples));
    }
    assert.equal(activity.ok, true,
      'Activity deep-scroll parity failed: ' +
      JSON.stringify(activity.checks.filter((c) => !c.pass)));

    // Raw through the real control, then sweep again.
    await switchProfile(page, 'raw');
    const raw = await sweep('raw');
    console.log('[scrollParity] raw summary:', JSON.stringify(raw.summary));
    raw.checks.forEach((c) =>
      console.log('[scrollParity]   ' + c.name + ' ' + (c.pass ? 'PASS' : 'FAIL') +
        ' — ' + String(c.detail).slice(0, 400)));
    if (!raw.ok) {
      console.log('[scrollParity] raw failure samples:', JSON.stringify(raw.samples));
    }
    assert.equal(raw.ok, true,
      'Raw deep-scroll parity failed: ' +
      JSON.stringify(raw.checks.filter((c) => !c.pass)));

    // Leave the panel in the default Activity profile.
    await switchProfile(page, 'activity');

    assert.deepEqual(errors, [], 'no uncaught page errors: ' + errors.join(' | '));
  } finally {
    await page.close();
  }
});
