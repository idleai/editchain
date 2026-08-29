#!/usr/bin/env node
// Graph-data harness for the EditChain webview — structural checks over the
// REAL Rust service + production renderer, built for ~150k-row chains.
//
// Extends the existing real-service harness (scripts/ui-real.mjs +
// test/harness/serviceBridge.js + index.html + media/main.js): the page loads
// the actual renderer and stylesheet, the probe pages the full filtered
// dataset through the real service bridge using the exact GetWindow DTOs the
// renderer sends, runs graph-structure + rendered-geometry checks, and writes
// machine-readable artifacts:
//
//   graph.json       aggregates: totals, relation-kind counts, duplicate rates,
//                    kind/lane histograms, paging summary
//   dataset.ndjson   one compact row per line (streamed in chunks — the full
//                    dataset is never materialized as one blob in either
//                    process)
//   checks.json      every check with pass/detail
//   render.json      rendered DOM slice + renderer state + scroll samples
//   open.json        the Open response (diagnostics included)
//   summary.md       human-readable result
//   console.txt / service-stderr.txt
//   self-test.json   (--self-test only) probe failure-detection self-tests
//
// Exits NON-ZERO on any structural failure or page error.
//
// Usage:
//   node scripts/ui-graph.mjs [--workspace DIR] [--chain-dir .editchain]
//                             [--out DIR] [--limit 2000] [--viewport WxH]
//                             [--row-timeout MS]
//   node scripts/ui-graph.mjs --self-test [--out DIR]
//
// The service binary path comes from SERVICE_PATH or prefers the workspace's
// release build, falling back to debug when release has not been built.

import puppeteer from 'puppeteer-core';
import { spawn } from 'child_process';
import fs from 'fs';
import path from 'path';
import { fileURLToPath } from 'url';

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const EXT_ROOT = path.join(__dirname, '..');
const HARNESS = 'file://' + path.join(EXT_ROOT, 'test', 'harness', 'index.html') + '?bridge=service';
const FIXTURE_HARNESS = 'file://' + path.join(EXT_ROOT, 'test', 'harness', 'index.html');

const CHROME = process.env.CHROME_PATH ||
  '/mnt/hot/ambientlight/.cache/puppeteer/chrome/linux-151.0.7922.71/chrome-linux64/chrome';

// How many compact rows to stream back to the host per datasetChunk call.
const DATASET_CHUNK_ROWS = 5000;

function parseArgs(argv) {
  const args = {
    workspace: null, chainDir: '.editchain', out: null, limit: 2000,
    viewport: '1440x900', rowTimeoutMs: 120000, selfTest: false,
  };
  for (let i = 0; i < argv.length; i++) {
    const a = argv[i];
    if (a === '--workspace') args.workspace = argv[++i];
    else if (a === '--chain-dir') args.chainDir = argv[++i];
    else if (a === '--out') args.out = argv[++i];
    else if (a === '--limit') args.limit = parseInt(argv[++i], 10) || 2000;
    else if (a === '--viewport') args.viewport = argv[++i];
    else if (a === '--row-timeout') args.rowTimeoutMs = parseInt(argv[++i], 10) || args.rowTimeoutMs;
    else if (a === '--self-test') args.selfTest = true;
  }
  if (!args.workspace) args.workspace = '/mnt/hot/ambientlight/repos/editchain';
  if (process.env.ROW_TIMEOUT) {
    const parsed = parseInt(process.env.ROW_TIMEOUT, 10);
    if (parsed > 0) args.rowTimeoutMs = parsed;
  }
  return args;
}

function parseViewport(vp) {
  const m = /^(\d+)x(\d+)$/.exec(vp);
  if (!m) throw new Error('bad viewport: ' + vp);
  return { width: +m[1], height: +m[2] };
}

// --- framed stdio client for the Rust service (same transport as ui-real.mjs)
// The page's serviceBridge forwards every request here; Open passes 0 (no
// deadline — large chains can take minutes), everything else gets a bounded
// default so a hung service can never stall the harness forever. The default
// IS the --row-timeout value: every page-side GetWindow (probe and renderer)
// is bounded end-to-end by the same knob, with Open explicitly unbounded.
function makeServiceClient(binaryPath, defaultTimeoutMs) {
  const proc = spawn(binaryPath, [], { stdio: ['pipe', 'pipe', 'pipe'] });
  let buf = Buffer.alloc(0);
  let nextId = 1;
  const pending = new Map();
  const stderrLines = [];
  const DEFAULT_TIMEOUT_MS = (typeof defaultTimeoutMs === 'number' && defaultTimeoutMs > 0)
    ? defaultTimeoutMs
    : 120_000;

  function failPending(reason) {
    const err = new Error(reason);
    for (const [id, entry] of pending) {
      pending.delete(id);
      if (entry.timer) clearTimeout(entry.timer);
      entry.reject(err);
    }
  }

  proc.on('error', (err) => failPending('service process error: ' + err.message));
  proc.on('exit', (code) => {
    failPending(code === null || code === 0
      ? 'service process exited unexpectedly'
      : 'service process exited with code ' + code);
  });
  proc.stdin.on('error', (err) => failPending('service stdin error: ' + err.message));
  proc.stderr.on('data', (c) => stderrLines.push(c.toString()));

  proc.stdout.on('data', (c) => {
    buf = Buffer.concat([buf, c]);
    while (buf.length >= 4) {
      const len = buf.readUInt32LE(0);
      if (buf.length < 4 + len) break;
      const payload = buf.subarray(4, 4 + len).toString('utf8');
      buf = buf.subarray(4 + len);
      let msg;
      try { msg = JSON.parse(payload); } catch { continue; }
      if (msg.id !== undefined && pending.has(msg.id)) {
        const entry = pending.get(msg.id);
        pending.delete(msg.id);
        if (entry.timer) clearTimeout(entry.timer);
        entry.resolve(msg.body);
      }
    }
  });

  return {
    send(body, timeoutMs) {
      const id = nextId++;
      return new Promise((resolve, reject) => {
        const effective = typeof timeoutMs === 'number' ? timeoutMs : DEFAULT_TIMEOUT_MS;
        const t = effective > 0
          ? setTimeout(() => {
              pending.delete(id);
              reject(new Error('service request timed out after ' + effective + 'ms: ' +
                JSON.stringify(body).slice(0, 80)));
            }, effective)
          : null;
        pending.set(id, { resolve, reject, timer: t });
        const payload = Buffer.from(JSON.stringify({ id, body }), 'utf8');
        const header = Buffer.alloc(4);
        header.writeUInt32LE(payload.length, 0);
        try {
          proc.stdin.write(Buffer.concat([header, payload]));
        } catch (e) {
          if (pending.delete(id)) {
            if (t) clearTimeout(t);
            reject(new Error('service write failed: ' + e.message));
          }
        }
      });
    },
    stderr() { return stderrLines.join(''); },
    kill() {
      failPending('service process killed');
      proc.kill();
    },
  };
}

async function main() {
  const args = parseArgs(process.argv.slice(2));
  const vp = parseViewport(args.viewport);

  // Self-test mode: exercise the probe's own failure detection against an
  // in-page SIMULATED service (no workspace, no Rust service required).
  if (args.selfTest) {
    const outDir = args.out || path.join(EXT_ROOT, '.ui-out', 'graph-self-test');
    fs.mkdirSync(outDir, { recursive: true });
    console.log('STEP self-test: launch browser (fixture harness, no service)...');
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
    await page.goto(FIXTURE_HARNESS, { waitUntil: 'networkidle0' });
    await page.waitForFunction(() => typeof window.__editchainGraph !== 'undefined' &&
      typeof window.__editchainGraph.runSelfTests === 'function', { timeout: 10000 });
    const result = await page.evaluate(() => window.__editchainGraph.runSelfTests());
    const failed = result.results.filter((r) => !r.pass);
    fs.writeFileSync(path.join(outDir, 'self-test.json'), JSON.stringify(result, null, 2));
    fs.writeFileSync(path.join(outDir, 'console.txt'), consoleLines.join('\n'));
    fs.writeFileSync(path.join(outDir, 'summary.md'), [
      '# EditChain graph probe self-tests (simulated service)',
      '',
      '- results: pass=' + (result.results.length - result.failCount) +
        ' fail=' + result.failCount,
      '- page errors: ' + pageErrors.length,
      '',
      '## Results',
      '',
      ...result.results.map((r) => '- **' + r.name + '** ' + (r.pass ? 'PASS' : 'FAIL') +
        ' — ' + r.detail),
    ].join('\n'));
    await browser.close();
    console.log('self-test results:');
    result.results.forEach((r) => console.log((r.pass ? 'PASS ' : 'FAIL ') + r.name + ': ' + r.detail));
    if (pageErrors.length) console.log('PAGE ERRORS: ' + pageErrors.length);
    console.log('artifacts -> ' + outDir);
    if (failed.length > 0 || pageErrors.length > 0) {
      console.error('GRAPH SELF-TEST FAILED: ' + failed.length + ' self-test failure(s), ' +
        pageErrors.length + ' page error(s)');
      process.exit(1);
    }
    console.log('GRAPH SELF-TEST PASS');
    process.exit(0);
  }

  const releaseServicePath = path.join(
    args.workspace,
    'target',
    'release',
    'editchain-vscode-service'
  );
  const debugServicePath = path.join(
    args.workspace,
    'target',
    'debug',
    'editchain-vscode-service'
  );
  const servicePath = process.env.SERVICE_PATH ||
    (fs.existsSync(releaseServicePath) ? releaseServicePath : debugServicePath);
  if (!fs.existsSync(servicePath)) {
    console.error('service binary not found at ' + servicePath);
    process.exit(1);
  }

  const outDir = args.out || path.join(EXT_ROOT, '.ui-out', 'graph');
  fs.mkdirSync(outDir, { recursive: true });

  const svc = makeServiceClient(servicePath, args.rowTimeoutMs);
  const consoleLines = [];
  const pageErrors = [];

  console.log('STEP launch browser...');
  const browser = await puppeteer.launch({
    executablePath: CHROME,
    headless: 'new',
    args: ['--no-sandbox', '--disable-setuid-sandbox'],
  });
  const page = await browser.newPage();
  await page.setViewport({ width: vp.width, height: vp.height });
  page.on('console', (m) => consoleLines.push(m.text()));
  page.on('pageerror', (e) => pageErrors.push(e.message));

  console.log('STEP goto harness (bridge=service)...');
  // Expose the Node-side service client into the page before main.js runs, so
  // serviceBridge.js can forward requests (same wiring as ui-real.mjs).
  await page.evaluateOnNewDocument((workspace, chainDir) => {
    window.__editchainWorkspace = workspace;
    window.__editchainChainDir = chainDir;
    window.__editchainService = {
      send(body, timeoutMs) {
        return window.__editchainServiceSend(body, timeoutMs);
      },
    };
  }, args.workspace, args.chainDir);
  await page.exposeFunction('__editchainServiceSend', (body, timeoutMs) => svc.send(body, timeoutMs));

  await page.goto(HARNESS, { waitUntil: 'networkidle0' });
  await page.waitForFunction(() => typeof window.vscode !== 'undefined' &&
    typeof window.__editchainStart === 'function', { timeout: 10000 });

  console.log('STEP start handshake...');
  await page.evaluate(() => window.__editchainStart());

  console.log('STEP wait for rows...');
  let waitError = null;
  try {
    await page.waitForFunction(() => document.querySelectorAll('.row').length > 0 ||
      !!document.querySelector('.view-message'),
      { timeout: args.rowTimeoutMs });
  } catch (e) {
    waitError = e;
  }
  if (waitError) {
    const diag = await page.evaluate(() => {
      const msg = document.querySelector('.view-message');
      return {
        bodyText: (document.body.textContent || '').trim().slice(0, 400),
        viewMessage: msg ? (msg.textContent || '').trim().slice(0, 200) : null,
        dataReady: window.__editchainDataReady === true,
        rows: document.querySelectorAll('.row').length,
      };
    }).catch(() => ({ evaluateFailed: true }));
    const detail = {
      waitError: String(waitError && waitError.message || waitError),
      page: diag,
      pageErrors,
      consoleLines: consoleLines.slice(-30),
      serviceStderr: svc.stderr().slice(-2000),
    };
    fs.writeFileSync(path.join(outDir, 'wait-failure.json'), JSON.stringify(detail, null, 2));
    console.error('WAIT FAILED: rows did not render within ' + args.rowTimeoutMs + 'ms');
    console.error(JSON.stringify(detail, null, 2));
    await browser.close();
    svc.kill();
    process.exit(1);
  }
  console.log('STEP rows present');

  // Run the graph probe: pages the full dataset through the bridge, checks it,
  // and dumps the rendered window. The compact dataset stays in the page.
  console.log('STEP run graph probe (limit=' + args.limit + ')...');
  const result = await page.evaluate((opts) => window.__editchainGraph.runGraphProbe(opts),
    { limit: args.limit, idleTimeoutMs: args.rowTimeoutMs, requestTimeoutMs: args.rowTimeoutMs });
  console.log('STEP probe done: pass=' + result.summary.passCount +
    ' fail=' + result.summary.failCount + ' datasetTotal=' +
    (result.datasetMeta ? result.datasetMeta.total : 'n/a'));

  // Stream the compact dataset out in chunks (memory-conscious on both sides).
  console.log('STEP stream dataset...');
  const datasetPath = path.join(outDir, 'dataset.ndjson');
  const datasetStream = fs.createWriteStream(datasetPath);
  const totalRows = result.datasetMeta ? result.datasetMeta.total : 0;
  let written = 0;
  for (let start = 0; start < totalRows; start += DATASET_CHUNK_ROWS) {
    const chunk = await page.evaluate(
      (from, to) => window.__editchainGraph.datasetChunk(from, to),
      start, start + DATASET_CHUNK_ROWS);
    const parsed = JSON.parse(chunk);
    for (const row of parsed) {
      datasetStream.write(JSON.stringify(row) + '\n');
      written++;
    }
  }
  await new Promise((resolve, reject) => {
    datasetStream.end((err) => (err ? reject(err) : resolve()));
  });
  console.log('STEP dataset streamed: ' + written + ' rows');

  // Write artifacts.
  const failedChecks = result.checks.filter((c) => !c.pass);
  fs.writeFileSync(path.join(outDir, 'open.json'), JSON.stringify(result.open, null, 2));
  fs.writeFileSync(path.join(outDir, 'graph.json'), JSON.stringify(result.datasetMeta, null, 2));
  fs.writeFileSync(path.join(outDir, 'render.json'), JSON.stringify({
    rendererState: result.rendererState,
    rendered: result.rendered,
    scrollSamples: result.scrollSamples || [],
  }, null, 2));
  fs.writeFileSync(path.join(outDir, 'checks.json'), JSON.stringify({
    passCount: result.summary.passCount,
    failCount: result.summary.failCount,
    checks: result.checks,
  }, null, 2));
  fs.writeFileSync(path.join(outDir, 'console.txt'), consoleLines.join('\n'));
  fs.writeFileSync(path.join(outDir, 'service-stderr.txt'), svc.stderr());

  const summary = [
    '# EditChain graph harness',
    '',
    '- workspace: ' + args.workspace,
    '- chain dir: ' + args.chainDir,
    '- viewport: ' + args.viewport,
    '- probe page size: ' + args.limit,
    '- open: ' + JSON.stringify(result.open),
    '- dataset: ' + JSON.stringify(result.datasetMeta),
    '- rendered: ' + JSON.stringify({
      domRows: result.rendererState.domRows,
      renderTop: result.rendererState.graphState && result.rendererState.graphState.renderTop,
      renderBottom: result.rendererState.graphState && result.rendererState.graphState.renderBottom,
      maxLane: result.rendererState.graphState && result.rendererState.graphState.maxLane,
    }),
    '- scroll samples: ' + JSON.stringify(result.scrollSamples || []),
    '- checks: pass=' + result.summary.passCount + ' fail=' + result.summary.failCount,
    '- console lines: ' + consoleLines.length,
    '- page errors: ' + pageErrors.length,
    '',
    '## Failed checks',
    '',
  ];
  if (failedChecks.length) failedChecks.forEach((c) => summary.push('- **' + c.name + '** FAIL — ' + c.detail));
  else summary.push('_none_');
  if (pageErrors.length) summary.push('', '## Page errors', '', ...pageErrors.map((e) => '- ' + e));
  fs.writeFileSync(path.join(outDir, 'summary.md'), summary.join('\n'));

  // Console output.
  console.log('dataset total=' + totalRows +
    ' maxLane=' + (result.datasetMeta ? result.datasetMeta.maxLane : 'n/a'));
  console.log('relation kinds=' + JSON.stringify(result.datasetMeta && result.datasetMeta.relationKinds));
  if (result.datasetMeta && result.datasetMeta.duplicateRates) {
    console.log('duplicate rates=' + JSON.stringify(result.datasetMeta.duplicateRates));
  }
  console.log('checks pass=' + result.summary.passCount + ' fail=' + result.summary.failCount);
  failedChecks.forEach((c) => console.log('FAIL ' + c.name + ': ' + c.detail));
  if (pageErrors.length) console.log('PAGE ERRORS: ' + pageErrors.length);
  console.log('artifacts -> ' + outDir);

  await browser.close();
  svc.kill();

  // Structural failures and page errors are BLOCKING: exit non-zero.
  if (failedChecks.length > 0 || pageErrors.length > 0) {
    console.error('GRAPH HARNESS FAILED: ' + failedChecks.length + ' check(s), ' +
      pageErrors.length + ' page error(s)');
    process.exit(1);
  }
  console.log('GRAPH HARNESS PASS');
  process.exit(0);
}

main().catch((e) => { console.error(e); process.exit(1); });
