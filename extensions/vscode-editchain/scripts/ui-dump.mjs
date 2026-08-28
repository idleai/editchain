#!/usr/bin/env node
// Text-only layout inspection for the EditChain history webview.
//
// Loads the exact production renderer (media/main.js + media/main.css) into
// Chromium via the harness page, reproduces a scenario with fixtures, and dumps
// the rendered layout as text/number artifacts — no screenshots.
//
// Usage:
//   node scripts/ui-dump.mjs dump    --scenario merge --viewport 1440x900 [--out DIR]
//   node scripts/ui-dump.mjs inspect --scenario merge --selector ".row" [--out DIR]
//   node scripts/ui-dump.mjs check   --scenario merge [--out DIR]
//
// Artifacts written to --out (default ./.ui-out/<scenario>):
//   summary.md      scenario, state, counts, console errors, failed checks
//   layout.txt      human-readable DOM/layout hierarchy
//   layout.json     full normalized machine representation
//   svg.json        dots, lanes, paths, endpoints, boxes
//   console.txt     browser console output
//   metrics.json    render timing / DOM counts
//   aria.yml        textual accessibility tree (best-effort)

import puppeteer from 'puppeteer-core';
import fs from 'fs';
import path from 'path';
import { fileURLToPath } from 'url';

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const EXT_ROOT = path.join(__dirname, '..');
const HARNESS = 'file://' + path.join(EXT_ROOT, 'test', 'harness', 'index.html');

const CHROME = process.env.CHROME_PATH ||
  '/mnt/hot/ambientlight/.cache/puppeteer/chrome/linux-151.0.7922.71/chrome-linux64/chrome';

const SCENARIOS = ['empty', 'linear', 'merge', 'mixed', 'filtered', 'undated', 'error', 'warned', 'large', 'longsummary', 'combined', 'fork', 'highLanes'];

function parseArgs(argv) {
  const args = { cmd: argv[0], scenario: 'merge', viewport: '1440x900', out: null, selector: null, search: null, staleRace: false, searchRace: false, resize: false, shot: null };
  for (let i = 1; i < argv.length; i++) {
    const a = argv[i];
    if (a === '--scenario') args.scenario = argv[++i];
    else if (a === '--viewport') args.viewport = argv[++i];
    else if (a === '--out') args.out = argv[++i];
    else if (a === '--selector') args.selector = argv[++i];
    else if (a === '--search') args.search = argv[++i];
    else if (a === '--stale-race') args.staleRace = true;
    else if (a === '--search-race') args.searchRace = true;
    else if (a === '--resize') args.resize = true;
    else if (a === '--shot') args.shot = argv[++i];
  }
  return args;
}

function parseViewport(vp) {
  const m = /^(\d+)x(\d+)$/.exec(vp);
  if (!m) throw new Error('bad viewport: ' + vp + ' (expected WxH)');
  return { width: +m[1], height: +m[2] };
}

async function main() {
  const args = parseArgs(process.argv.slice(2));
  if (!['dump', 'inspect', 'check'].includes(args.cmd)) {
    console.error('usage: ui-dump.mjs <dump|inspect|check> [--scenario S] [--viewport WxH] [--selector Q] [--out DIR]');
    process.exit(1);
  }
  if (!SCENARIOS.includes(args.scenario)) {
    console.error('unknown scenario "' + args.scenario + '" — choose from: ' + SCENARIOS.join(', '));
    process.exit(1);
  }

  const vp = parseViewport(args.viewport);
  const outDir = args.out || path.join(EXT_ROOT, '.ui-out', args.scenario);
  fs.mkdirSync(outDir, { recursive: true });

  const browser = await puppeteer.launch({
    executablePath: CHROME,
    headless: 'new',
    args: ['--no-sandbox', '--disable-setuid-sandbox'],
  });
  const page = await browser.newPage();
  await page.setViewport({ width: vp.width, height: vp.height });

  const consoleLines = [];
  const pageErrors = [];
  page.on('console', (m) => consoleLines.push(m.text()));
  page.on('pageerror', (e) => pageErrors.push(e.message));

  await page.goto(HARNESS, { waitUntil: 'networkidle0' });
  await page.evaluate((scenario) => {
    window.__editchainSetScenario(scenario);
    window.__editchainStart();
  }, args.scenario);

  // Wait for the UI to settle deterministically (no arbitrary sleep).
  await page.evaluate(() => window.__editchainDebug.whenIdle(5000));

  // Collect artifacts.
  const layout = await page.evaluate(() => window.__editchainDebug.dumpLayout());
  const metrics = await page.evaluate(() => window.__editchainDebug.getMetrics());
  const assertion = await page.evaluate(() => window.__editchainDebug.assertLayout());

  // Combined scenario: click the combined op and verify ALL bundled sub-ops
  // reveal inline (the "only the first sub-op appears" regression).
  let expansion = null;
  if (args.scenario === 'combined') {
    await page.evaluate(() => {
      const row = document.querySelector('.row[data-key="node:c:1"]');
      if (row) row.click();
    });
    await page.evaluate(() => window.__editchainDebug.whenIdle(5000));
    expansion = await page.evaluate(() => ({
      subopRows: document.querySelectorAll('.row.row-subop').length,
      rowsRendered: document.querySelectorAll('.row').length,
      total: window.__editchainGetTotal ? window.__editchainGetTotal() : -1,
    }));
  }

  // Optional search run: exercise the real renderer's search path (input ->
  // Enter -> result list -> click navigation) and record the outcome.
  let searchResult = null;
  if (args.search) {
    searchResult = await page.evaluate((q) => window.__editchainDebug.runSearch(q, 5000), args.search);
  }

  // Stale-response race (scenario must be `undated`): a GetWindow issued
  // before a filter reset lands after it and must be rejected via the view
  // generation, leaving the table on the current filter's rows.
  let staleRace = null;
  if (args.staleRace) {
    staleRace = await page.evaluate(() => window.__editchainDebug.runStaleResponseRace());
  }

  // Reversed-search race (scenario must be `merge`): two rapid searches share
  // a view generation; the LATEST issued query must win regardless of response
  // order (latest-query-wins via search-epoch correlation).
  let searchRace = null;
  if (args.searchRace) {
    searchRace = await page.evaluate(() => window.__editchainDebug.runReversedSearchRace());
  }

  // Resize assertion: capture geometry at the initial viewport, resize, and
  // verify the renderer RECOMPUTED the graph/header/rows (not just stretched
  // the DOM). The 150ms debounce plus a settle window makes the re-render
  // deterministic; whenIdle then confirms the DOM is stable.
  let resizeResult = null;
  if (args.resize) {
    const before = await page.evaluate(() => window.__editchainDebug.captureResizeMetrics());
    await page.setViewport({ width: Math.max(400, Math.round(vp.width * 0.6)), height: vp.height });
    await new Promise((resolve) => setTimeout(resolve, 400));
    await page.evaluate(() => window.__editchainDebug.whenIdle(5000));
    const after = await page.evaluate(() => window.__editchainDebug.captureResizeMetrics());
    resizeResult = {
      before,
      after,
      ...(await page.evaluate((b, a) => window.__editchainDebug.evaluateResizeAssert(b, a), before, after)),
    };
  }

  // Deterministic screenshot: taken only after whenIdle (and scenario
  // interactions) so the capture shows a settled, rendered view — never the
  // loading state or a placeholder-filled DOM.
  if (args.shot) {
    await page.screenshot({ path: args.shot });
  }

  // Write artifacts.
  fs.writeFileSync(path.join(outDir, 'layout.json'), JSON.stringify(layout, null, 2));
  fs.writeFileSync(path.join(outDir, 'svg.json'), JSON.stringify(layout.svg, null, 2));
  fs.writeFileSync(path.join(outDir, 'metrics.json'), JSON.stringify(metrics, null, 2));
  fs.writeFileSync(path.join(outDir, 'console.txt'), consoleLines.join('\n'));
  fs.writeFileSync(path.join(outDir, 'layout.txt'), formatLayoutText(layout));
  fs.writeFileSync(path.join(outDir, 'aria.yml'), formatAria(page));
  if (expansion) fs.writeFileSync(path.join(outDir, 'expansion.json'), JSON.stringify(expansion, null, 2));
  if (searchResult) fs.writeFileSync(path.join(outDir, 'search.json'), JSON.stringify(searchResult, null, 2));
  if (staleRace) fs.writeFileSync(path.join(outDir, 'stale.json'), JSON.stringify(staleRace, null, 2));
  if (searchRace) fs.writeFileSync(path.join(outDir, 'search-race.json'), JSON.stringify(searchRace, null, 2));
  if (resizeResult) fs.writeFileSync(path.join(outDir, 'resize.json'), JSON.stringify(resizeResult, null, 2));

  // Summary.
  const failedChecks = assertion.checks.filter((c) => !c.pass);
  const summary = [
    '# EditChain UI dump',
    '',
    '- scenario: ' + args.scenario,
    '- viewport: ' + args.viewport,
    '- state: ' + JSON.stringify(layout.state),
    '- rows rendered: ' + layout.state.rowsRendered,
    '- svg dots: ' + (layout.svg && layout.svg.dots ? layout.svg.dots.length : 0),
    '- svg edges: ' + (layout.svg && layout.svg.edges ? layout.svg.edges.length : 0),
    '- checks: pass=' + assertion.passCount + ' fail=' + assertion.failCount,
    '- console lines: ' + consoleLines.length,
    '- page errors: ' + pageErrors.length,
    '',
  ];
  if (expansion) {
    const expOk = expansion.subopRows === 7 && expansion.rowsRendered >= 9;
    summary.push('- combined expansion: subopRows=' + expansion.subopRows +
      ' rowsRendered=' + expansion.rowsRendered + ' total=' + expansion.total +
      ' (' + (expOk ? 'OK' : 'FAIL') + ')');
  }
  if (searchResult) {
    const searchOk = searchResult.resultRows > 0 &&
      (searchResult.navigated || searchResult.firstRowChevron);
    summary.push('- search "' + args.search + '": results=' + searchResult.resultRows +
      ' banner="' + searchResult.bannerText + '" navigated=' + searchResult.navigated +
      ' (' + (searchOk ? 'OK' : 'FAIL') + ')');
  }
  if (staleRace) {
    const step = staleRace.steps[0];
    const raceOk = step && step.staleRejected === true;
    summary.push('- stale-response race: total=' + step.total +
      ' keys=' + JSON.stringify(step.keys) +
      ' (' + (raceOk ? 'OK — stale response rejected' : 'FAIL — stale response applied') + ')');
  }
  if (searchRace) {
    const step = searchRace.steps[0];
    const raceOk = step && step.latestWins === true;
    summary.push('- reversed-search race: ' +
      ' (' + (raceOk ? 'OK — latest query wins' : 'FAIL — stale search applied') + ')');
  }
  if (resizeResult) {
    summary.push('- resize: rebuilt=' + resizeResult.detail.rebuilt +
      ' graphAdjusted=' + resizeResult.detail.graphAdjusted +
      ' headerAligned=' + resizeResult.detail.headerAligned +
      ' dotsInside=' + resizeResult.detail.dotsInside +
      ' (' + (resizeResult.pass ? 'OK — geometry recomputed' : 'FAIL') + ')');
  }
  summary.push(
    '## Failed checks',
    '',
  );
  if (failedChecks.length) {
    failedChecks.forEach((c) => summary.push('- **' + c.name + '** FAIL — ' + c.detail));
  } else {
    summary.push('_none_');
  }
  if (pageErrors.length) {
    summary.push('', '## Page errors', '', ...pageErrors.map((e) => '- ' + e));
  }
  fs.writeFileSync(path.join(outDir, 'summary.md'), summary.join('\n'));

  // Blocking evaluation: `check` mode fails the run (non-zero exit) when any
  // layout check fails, a page error occurred, or a scenario interaction
  // (expansion / search) misbehaves. `dump`/`inspect` stay diagnostic.
  const interactionFailed =
    (expansion !== null && !(expansion.subopRows === 7 && expansion.rowsRendered >= 9)) ||
    (searchResult !== null && !(searchResult.resultRows > 0 &&
      (searchResult.navigated || searchResult.firstRowChevron))) ||
    (staleRace !== null && !(staleRace.steps[0] && staleRace.steps[0].staleRejected === true)) ||
    (searchRace !== null && !(searchRace.steps[0] && searchRace.steps[0].latestWins === true)) ||
    (resizeResult !== null && resizeResult.pass !== true);
  if (args.cmd === 'check' && (failedChecks.length > 0 || pageErrors.length > 0 || interactionFailed)) {
    console.error('CHECK FAILED: ' + failedChecks.length + ' layout check(s), ' +
      pageErrors.length + ' page error(s), interactionFailed=' + interactionFailed);
    await browser.close();
    process.exit(1);
  }

  // Console output.
  console.log('scenario=' + args.scenario + ' viewport=' + args.viewport);
  console.log('state=' + JSON.stringify(layout.state));
  console.log('svg dots=' + (layout.svg && layout.svg.dots ? layout.svg.dots.length : 0) +
    ' edges=' + (layout.svg && layout.svg.edges ? layout.svg.edges.length : 0));
  console.log('checks pass=' + assertion.passCount + ' fail=' + assertion.failCount);
  failedChecks.forEach((c) => console.log('FAIL ' + c.name + ': ' + c.detail));
  if (pageErrors.length) console.log('PAGE ERRORS: ' + pageErrors.length);
  if (expansion) console.log('combined expansion subopRows=' + expansion.subopRows +
    ' rowsRendered=' + expansion.rowsRendered);
  if (searchResult) console.log('search results=' + searchResult.resultRows +
    ' navigated=' + searchResult.navigated);
  if (staleRace) console.log('stale-race total=' + staleRace.steps[0].total +
    ' staleRejected=' + staleRace.steps[0].staleRejected);
  if (searchRace) console.log('search-race latestWins=' + searchRace.steps[0].latestWins);
  if (resizeResult) console.log('resize pass=' + resizeResult.pass +
    ' detail=' + JSON.stringify(resizeResult.detail));
  if (args.shot) console.log('shot -> ' + args.shot);
  console.log('artifacts -> ' + outDir);

  // inspect mode: also print the matched element's geometry.
  if (args.cmd === 'inspect' && args.selector) {
    const sel = await page.evaluate((s) => {
      const el = document.querySelector(s);
      if (!el) return null;
      const r = el.getBoundingClientRect();
      return { tag: el.tagName.toLowerCase(), cls: el.className || '', key: el.getAttribute('data-key'),
        box: { x: Math.round(r.x), y: Math.round(r.y), w: Math.round(r.width), h: Math.round(r.height) },
        text: (el.textContent || '').trim().slice(0, 80) };
    }, args.selector);
    console.log('INSPECT "' + args.selector + '" ->', JSON.stringify(sel));
    fs.writeFileSync(path.join(outDir, 'inspect.json'), JSON.stringify(sel, null, 2));
  }

  await browser.close();
}

function formatLayoutText(layout) {
  const lines = [];
  lines.push('VIEWPORT ' + layout.viewport.w + 'x' + layout.viewport.h +
    ' dpr=' + layout.viewport.dpr);
  lines.push('STATE scenario=' + layout.state.scenario +
    ' status=' + layout.state.status +
    ' generation=' + layout.state.generation +
    ' rowsRendered=' + layout.state.rowsRendered);
  lines.push('');
  lines.push('#layout box=' + fmtBox(layout.layoutBoxes.layoutEl));
  lines.push('#rows box=' + fmtBox(layout.layoutBoxes.rowsEl));
  lines.push('');
  const svg = layout.svg || {};
  lines.push('#svg present=' + svg.present +
    (svg.present ? ' box=' + fmtBox(svg.box) : ''));
  for (const d of (svg.dots || [])) {
    lines.push('dot row=' + d.row + ' center=(' + d.cx + ',' + d.cy + ') r=' + d.r);
  }
  for (const e of (svg.edges || [])) {
    lines.push('edge len=' + e.len +
      (e.start ? (' start=(' + e.start.x + ',' + e.start.y + ')') : '') +
      (e.end ? (' end=(' + e.end.x + ',' + e.end.y + ')') : ''));
  }
  return lines.join('\n');
}

function fmtBox(b) {
  if (!b) return '(none)';
  return '(' + b.x + ',' + b.y + ',' + b.w + ',' + b.h + ')';
}

function formatAria(page) {
  // Best-effort accessibility tree from roles/names of key controls.
  return [
    '# Accessibility tree',
    '',
    '- search input: #search',
    '- toggle "Show git submodules": #hideSubmodules',
    '- toggle "Show messages only": #hideSystem',
    '- rows container: #rows',
    '- detail pane: #detail',
    '',
    '_Full ARIA snapshot requires Playwright; this is a structural summary._',
    '',
  ].join('\n');
}

main().catch((e) => { console.error(e); process.exit(1); });
