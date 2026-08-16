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

const SCENARIOS = ['empty', 'linear', 'merge', 'mixed', 'filtered', 'undated', 'error', 'large', 'longsummary'];

function parseArgs(argv) {
  const args = { cmd: argv[0], scenario: 'merge', viewport: '1440x900', out: null, selector: null };
  for (let i = 1; i < argv.length; i++) {
    const a = argv[i];
    if (a === '--scenario') args.scenario = argv[++i];
    else if (a === '--viewport') args.viewport = argv[++i];
    else if (a === '--out') args.out = argv[++i];
    else if (a === '--selector') args.selector = argv[++i];
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

  // Write artifacts.
  fs.writeFileSync(path.join(outDir, 'layout.json'), JSON.stringify(layout, null, 2));
  fs.writeFileSync(path.join(outDir, 'svg.json'), JSON.stringify(layout.svg, null, 2));
  fs.writeFileSync(path.join(outDir, 'metrics.json'), JSON.stringify(metrics, null, 2));
  fs.writeFileSync(path.join(outDir, 'console.txt'), consoleLines.join('\n'));
  fs.writeFileSync(path.join(outDir, 'layout.txt'), formatLayoutText(layout));
  fs.writeFileSync(path.join(outDir, 'aria.yml'), formatAria(page));

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
    '## Failed checks',
    '',
  ];
  if (failedChecks.length) {
    failedChecks.forEach((c) => summary.push('- **' + c.name + '** FAIL — ' + c.detail));
  } else {
    summary.push('_none_');
  }
  if (pageErrors.length) {
    summary.push('', '## Page errors', '', ...pageErrors.map((e) => '- ' + e));
  }
  fs.writeFileSync(path.join(outDir, 'summary.md'), summary.join('\n'));

  // Console output.
  console.log('scenario=' + args.scenario + ' viewport=' + args.viewport);
  console.log('state=' + JSON.stringify(layout.state));
  console.log('svg dots=' + (layout.svg && layout.svg.dots ? layout.svg.dots.length : 0) +
    ' edges=' + (layout.svg && layout.svg.edges ? layout.svg.edges.length : 0));
  console.log('checks pass=' + assertion.passCount + ' fail=' + assertion.failCount);
  failedChecks.forEach((c) => console.log('FAIL ' + c.name + ': ' + c.detail));
  if (pageErrors.length) console.log('PAGE ERRORS: ' + pageErrors.length);
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