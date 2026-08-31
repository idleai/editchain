#!/usr/bin/env node
// Render the REAL EditChain data from a chain dir through the harness, using
// the actual Rust service over framed stdio. Dumps the full DOM tree + styles
// as text artifacts — no screenshots.
//
// Usage:
//   node scripts/ui-real.mjs [--workspace DIR] [--chain-dir .editchain]
//                            [--viewport WxH] [--out DIR] [--selector Q]
//                            [--top-row N] [--scroll-row N] [--expand-visible]
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

const CHROME = process.env.CHROME_PATH ||
  '/mnt/hot/ambientlight/.cache/puppeteer/chrome/linux-151.0.7922.71/chrome-linux64/chrome';

function parseArgs(argv) {
  const args = {
    workspace: null, chainDir: '.editchain', viewport: '1440x900', out: null,
    selector: null, shot: null, topRow: null, scrollRow: null,
    expandVisible: false,
    // Row-ready deadline. The real service can take >20s to Open + deliver the
    // first window on a large chain, so this must be long and configurable —
    // the outer runner (CI/timeout wrapper) bounds the whole run instead.
    rowTimeoutMs: 120000,
  };
  for (let i = 0; i < argv.length; i++) {
    const a = argv[i];
    if (a === '--workspace') args.workspace = argv[++i];
    else if (a === '--chain-dir') args.chainDir = argv[++i];
    else if (a === '--viewport') args.viewport = argv[++i];
    else if (a === '--out') args.out = argv[++i];
    else if (a === '--selector') args.selector = argv[++i];
    else if (a === '--shot') args.shot = argv[++i];
    else if (a === '--top-row') args.topRow = parseInt(argv[++i], 10);
    else if (a === '--scroll-row') args.scrollRow = parseInt(argv[++i], 10);
    else if (a === '--expand-visible') args.expandVisible = true;
    else if (a === '--row-timeout') args.rowTimeoutMs = parseInt(argv[++i], 10) || args.rowTimeoutMs;
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

// --- framed stdio client for the Rust service --------------------------------

function makeServiceClient(binaryPath, defaultTimeoutMs) {
  const proc = spawn(binaryPath, [], { stdio: ['pipe', 'pipe', 'pipe'] });
  let buf = Buffer.alloc(0);
  let nextId = 1;
  const pending = new Map();
  const stderrLines = [];
  // Bounded default for requests that don't opt out explicitly. Open passes 0
  // (no deadline — it can legitimately take minutes), everything else gets a
  // finite cap so a hung service can never stall the harness forever.
  const DEFAULT_TIMEOUT_MS = Number.isFinite(defaultTimeoutMs) && defaultTimeoutMs > 0
    ? defaultTimeoutMs
    : 30_000;

  // Reject every outstanding request with `reason`. Used when the process
  // errors, exits, is killed, or its stdin dies — without this a pending
  // request (especially an unbounded Open) would never settle.
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
  // A write to a dead process surfaces as an 'error' on the stdin stream; with
  // no listener it would be an uncaught 'error' event that crashes the runner.
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
        // An explicit 0 means "no deadline" (used for Open, which can take
        // minutes on a large workspace); the request still settles when the
        // service errors/exits or the process is killed (see failPending).
        // Undefined/null falls back to a bounded default so nothing hangs.
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
  console.log('STEP start');
  const args = parseArgs(process.argv.slice(2));
  const vp = parseViewport(args.viewport);

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

  const outDir = args.out || path.join(EXT_ROOT, '.ui-out', 'real');
  fs.mkdirSync(outDir, { recursive: true });

  // Apply the configured large-chain row deadline to renderer-originated
  // GetWindow calls too. Open still opts out explicitly with timeout 0 below.
  const svc = makeServiceClient(servicePath, args.rowTimeoutMs);

  // Open the workspace first to confirm it loads.
  console.log('STEP open...');
  // No fixed deadline: opening a workspace can take minutes on large chains.
  const openResp = await svc.send({ Open: { workspace_path: args.workspace, chain_dir: args.chainDir } }, 0);
  console.log('OPEN:', JSON.stringify(openResp));

  console.log('STEP launch browser...');
  const browser = await puppeteer.launch({
    executablePath: CHROME,
    headless: 'new',
    args: ['--no-sandbox', '--disable-setuid-sandbox'],
  });
  const page = await browser.newPage();
  await page.setViewport({ width: vp.width, height: vp.height });
  console.log('STEP goto harness...');

  const consoleLines = [];
  const pageErrors = [];
  page.on('console', (m) => consoleLines.push(m.text()));
  page.on('pageerror', (e) => pageErrors.push(e.message));

  // Load the harness in service mode. The page loads serviceBridge.js (which
  // defines window.vscode) BEFORE main.js, so the renderer captures the right
  // bridge. We set workspace/chainDir/service via evaluateOnNewDocument so they
  // exist before any page script runs, and expose a Node-side send() shim.
  await page.evaluateOnNewDocument((workspace, chainDir) => {
    window.__editchainWorkspace = workspace;
    window.__editchainChainDir = chainDir;
    window.__editchainService = {
      // Forward the caller's optional timeout (Open passes 0 = no deadline);
      // undefined falls through to the bounded default in makeServiceClient.
      send(body, timeoutMs) {
        return window.__editchainServiceSend(body, timeoutMs);
      },
    };
  }, args.workspace, args.chainDir);

  // Bridge Node-side send into the page (must be exposed before goto so the
  // page's __editchainService.send can call it).
  console.log('STEP exposeFunction...');
  // Pass the caller's timeout preference through (Open uses 0 = no deadline).
  await page.exposeFunction('__editchainServiceSend', (body, timeoutMs) => svc.send(body, timeoutMs));

  await page.goto(HARNESS, { waitUntil: 'networkidle0' });

  console.log('STEP wait for serviceBridge...');
  // Wait for serviceBridge to load and define vscode.
  await page.waitForFunction(() => typeof window.vscode !== 'undefined' &&
    typeof window.__editchainStart === 'function', { timeout: 10000 });

  // Start the handshake.
  console.log('STEP start handshake...');
  // Preload persisted VS Code webview state before the handshake, so the
  // renderer's restoreState() opens around the requested visible top row.
  const preloadState = {};
  if (args.topRow !== null && Number.isFinite(args.topRow)) preloadState.topRow = args.topRow;
  if (Object.keys(preloadState).length) {
    console.log('STEP preload state: ' + JSON.stringify(preloadState));
    await page.evaluate((state) => {
      if (window.vscode && typeof window.vscode.setState === 'function') {
        window.vscode.setState(state);
      }
    }, preloadState);
  }
  await page.evaluate(() => window.__editchainStart());

  // Wait for rows to actually render (the real service round-trips async; the
  // deadline is long/configurable — never a fixed service deadline).
  console.log('STEP wait for rows...');
  let waitError = null;
  try {
    await page.waitForFunction(() => document.querySelectorAll('.row').length > 0,
      { timeout: args.rowTimeoutMs });
  } catch (e) {
    waitError = e;
  }
  if (waitError) {
    // Failure diagnostics: page status + console + service stderr, never a
    // bare Puppeteer timeout.
    const diag = await page.evaluate(() => {
      const msg = document.querySelector('.view-message');
      return {
        bodyText: (document.body.textContent || '').trim().slice(0, 400),
        viewMessage: msg ? (msg.textContent || '').trim().slice(0, 200) : null,
        dataReady: window.__editchainDataReady === true,
        rows: document.querySelectorAll('.row').length,
        scenario: window.__editchainScenarioName || null,
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

  // Wait for the UI to settle deterministically.
  console.log('STEP whenIdle...');
  await page.evaluate((timeoutMs) => window.__editchainDebug.whenIdle(timeoutMs), args.rowTimeoutMs);
  console.log('STEP idle done');

  // `--top-row` exercises persisted-state restoration during startup. For a
  // smoke test that must inspect a specific part of an already-loaded virtual
  // history, jump after the first window has settled and require the renderer
  // to cover that absolute visible-row index before collecting artifacts.
  // This avoids mistaking a preload-state value for an actual scroll sample.
  let scrollSample = null;
  if (args.scrollRow !== null && Number.isFinite(args.scrollRow)) {
    const requestedRow = Math.max(0, Math.floor(args.scrollRow));
    console.log('STEP scroll to visible row ' + requestedRow + '...');
    const preScroll = await page.evaluate(() => {
      const rows = document.getElementById('rows');
      const spacer = rows && rows.querySelector('.scroll-spacer');
      const state = typeof window.__editchainGraphState === 'function'
        ? window.__editchainGraphState()
        : null;
      return {
        total: typeof window.__editchainGetTotal === 'function'
          ? window.__editchainGetTotal()
          : null,
        scrollHeight: rows ? rows.scrollHeight : null,
        clientHeight: rows ? rows.clientHeight : null,
        spacerHeight: spacer ? spacer.style.height : null,
        graphState: state,
      };
    });
    console.log('STEP scroll precondition: ' + JSON.stringify(preScroll));
    // A GetWindow response can make real rows available just before the
    // renderer installs its virtual-scroll spacer. Wait for that scaffold;
    // otherwise assigning scrollTop is silently clamped to zero and a
    // purported deep-position smoke still captures the first viewport. A
    // short, non-scrollable chain still has a one-pixel spacer and remains a
    // valid target when the requested row is already in the first viewport.
    await page.waitForFunction(() => {
      const rows = document.getElementById('rows');
      const spacer = rows && rows.querySelector('.scroll-spacer');
      return rows && spacer && parseFloat(spacer.style.height) >= 1;
    }, { timeout: args.rowTimeoutMs });
    const applied = await page.evaluate((row) => {
      const rows = document.getElementById('rows');
      if (!rows) throw new Error('#rows is unavailable');
      const firstRow = rows.querySelector('.row');
      const rowHeight = firstRow ? firstRow.getBoundingClientRect().height : 34;
      const maxScroll = Math.max(0, rows.scrollHeight - rows.clientHeight);
      rows.scrollTop = Math.min(row * rowHeight, maxScroll);
      rows.dispatchEvent(new Event('scroll'));
      return { rowHeight, scrollTop: rows.scrollTop, maxScroll };
    }, requestedRow);
    console.log('STEP scroll applied: ' + JSON.stringify(applied));
    await page.evaluate((timeoutMs) => window.__editchainDebug.whenIdle(timeoutMs), args.rowTimeoutMs);
    const postIdle = await page.evaluate((rowHeight) => {
      const rows = document.getElementById('rows');
      const state = window.__editchainGraphState();
      const rendered = Array.from(document.querySelectorAll('#rows .row'));
      return {
        visibleTop: Math.floor(rows.scrollTop / rowHeight),
        scrollTop: rows.scrollTop,
        scrollHeight: rows.scrollHeight,
        clientHeight: rows.clientHeight,
        renderTop: state.renderTop,
        renderBottom: state.renderBottom,
        firstDataRow: rendered[0] ? rendered[0].getAttribute('data-row') : null,
        lastDataRow: rendered.length ? rendered[rendered.length - 1].getAttribute('data-row') : null,
        placeholders: document.querySelectorAll('#rows .row-placeholder').length,
      };
    }, applied.rowHeight);
    console.log('STEP scroll post-idle: ' + JSON.stringify(postIdle));
    await page.waitForFunction((row) => {
      const state = typeof window.__editchainGraphState === 'function'
        ? window.__editchainGraphState()
        : null;
      return state && state.renderTop <= row && state.renderBottom >= row &&
        !document.querySelector('#rows .row-placeholder');
    }, { timeout: args.rowTimeoutMs }, requestedRow);
    await page.evaluate((timeoutMs) => window.__editchainDebug.whenIdle(timeoutMs), args.rowTimeoutMs);
    scrollSample = await page.evaluate((row, initial) => {
      const rows = document.getElementById('rows');
      const state = window.__editchainGraphState();
      return {
        requestedRow: row,
        visibleTop: Math.floor(rows.scrollTop / initial.rowHeight),
        scrollTop: rows.scrollTop,
        rowHeight: initial.rowHeight,
        maxScroll: initial.maxScroll,
        renderTop: state.renderTop,
        renderBottom: state.renderBottom,
      };
    }, requestedRow, applied);
    console.log('STEP scroll done: ' + JSON.stringify(scrollSample));
  }

  // Optionally expand every currently visible top-level subop chevron: click
  // each one (the renderer rebuilds the DOM per toggle), wait for the UI to
  // settle between clicks, then settle once more before artifacts are captured.
  let expandCount = 0;
  if (args.expandVisible) {
    console.log('STEP expand visible chevrons...');
    const MAX_EXPAND_PASSES = 2000;
    let lastRow = null;
    for (let pass = 0; pass < MAX_EXPAND_PASSES; pass++) {
      const res = await page.evaluate(() => {
        const vh = window.innerHeight;
        const visibleCollapsed = Array.from(document.querySelectorAll('.subop-chevron'))
          .filter((c) => (c.textContent || '').trim() === '▸')
          .filter((c) => {
            const r = c.getBoundingClientRect();
            return r.top < vh && r.bottom > 0;
          });
        if (!visibleCollapsed.length) return { clicked: false, row: null };
        const rowEl = visibleCollapsed[0].closest('.row');
        const row = rowEl ? rowEl.getAttribute('data-row') : null;
        visibleCollapsed[0].click();
        return { clicked: true, row };
      });
      if (!res.clicked) break;
      if (res.row !== null && res.row === lastRow) break; // no progress — stop
      lastRow = res.row;
      expandCount++;
      await page.evaluate((t) => window.__editchainDebug.whenIdle(t), args.rowTimeoutMs);
    }
    await page.evaluate((t) => window.__editchainDebug.whenIdle(t), args.rowTimeoutMs);
    console.log('STEP expand done: ' + expandCount + ' chevrons expanded');
  }

  // Collect artifacts.
  const layout = await page.evaluate(() => window.__editchainDebug.dumpLayout());
  const metrics = await page.evaluate(() => window.__editchainDebug.getMetrics());
  const graphState = await page.evaluate(() =>
    typeof window.__editchainGraphState === 'function'
      ? window.__editchainGraphState()
      : null);
  const assertion = await page.evaluate(() => window.__editchainDebug.assertLayout());

  // Deterministic screenshot: taken only after whenIdle + assertions, so the
  // capture shows a settled real-data table (never the loading state or
  // placeholder-filled DOM).
  if (args.shot) {
    await page.screenshot({ path: args.shot });
  }

  // Full DOM tree with computed styles — the user's request.
  const domTree = await page.evaluate((selector) => {
    const rootEl = document.querySelector(selector || '#rows');
    function describe(el, depth) {
      if (!el || depth > 12) return null;
      const cs = getComputedStyle(el);
      const r = el.getBoundingClientRect();
      const node = {
        tag: el.tagName.toLowerCase(),
        id: el.id || undefined,
        cls: el.className && typeof el.className === 'string' ? el.className : undefined,
        key: el.getAttribute && el.getAttribute('data-key') || undefined,
        text: (el.childElementCount === 0 ? (el.textContent || '').trim() : '').slice(0, 80) || undefined,
        box: { x: Math.round(r.x), y: Math.round(r.y), w: Math.round(r.width), h: Math.round(r.height) },
        style: {
          display: cs.display,
          position: cs.position,
          gridTemplateColumns: cs.gridTemplateColumns,
          width: cs.width,
          height: cs.height,
          minWidth: cs.minWidth,
          padding: cs.padding,
          margin: cs.margin,
          fontSize: cs.fontSize,
          lineHeight: cs.lineHeight,
          whiteSpace: cs.whiteSpace,
          textOverflow: cs.textOverflow,
          overflowX: cs.overflowX,
          overflowY: cs.overflowY,
          color: cs.color,
          backgroundColor: cs.backgroundColor,
          opacity: cs.opacity,
          zIndex: cs.zIndex,
        },
      };
      if (el.children && el.children.length && depth > 0) {
        node.children = Array.from(el.children).map((c) => describe(c, depth - 1)).filter(Boolean);
      }
      return node;
    }
    return describe(rootEl, 12);
  }, args.selector);

  // Write artifacts.
  fs.writeFileSync(path.join(outDir, 'layout.json'), JSON.stringify(layout, null, 2));
  fs.writeFileSync(path.join(outDir, 'metrics.json'), JSON.stringify(metrics, null, 2));
  fs.writeFileSync(path.join(outDir, 'graph-state.json'), JSON.stringify(graphState, null, 2));
  fs.writeFileSync(path.join(outDir, 'console.txt'), consoleLines.join('\n'));
  fs.writeFileSync(path.join(outDir, 'dom.json'), JSON.stringify(domTree, null, 2));
  fs.writeFileSync(path.join(outDir, 'dom.txt'), formatDomText(domTree));
  fs.writeFileSync(path.join(outDir, 'service-stderr.txt'), svc.stderr());

  // Summary.
  const failedChecks = assertion.checks.filter((c) => !c.pass);
  const settings = [];
  if (args.topRow !== null) settings.push('topRow=' + args.topRow);
  if (args.scrollRow !== null) settings.push('scrollRow=' + args.scrollRow);
  if (args.expandVisible) settings.push('expandVisible=true' + (expandCount ? ' (expanded ' + expandCount + ' chevrons)' : ''));
  const summary = [
    '# EditChain real UI dump',
    '',
    '- workspace: ' + args.workspace,
    '- chain dir: ' + args.chainDir,
    '- settings: ' + (settings.length ? settings.join(', ') : '_none_'),
    '- open response: ' + JSON.stringify(openResp),
    '- viewport: ' + args.viewport,
    '- state: ' + JSON.stringify(layout.state),
    '- graph state: ' + JSON.stringify(graphState),
    '- scroll sample: ' + JSON.stringify(scrollSample),
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
  if (failedChecks.length) failedChecks.forEach((c) => summary.push('- **' + c.name + '** FAIL — ' + c.detail));
  else summary.push('_none_');
  if (pageErrors.length) summary.push('', '## Page errors', '', ...pageErrors.map((e) => '- ' + e));
  fs.writeFileSync(path.join(outDir, 'summary.md'), summary.join('\n'));

  // Console output.
  console.log('state=' + JSON.stringify(layout.state));
  console.log('graph state=' + JSON.stringify(graphState));
  console.log('settings: ' + (settings.length ? settings.join(', ') : 'none'));
  if (args.expandVisible) console.log('expanded chevrons=' + expandCount);
  console.log('svg dots=' + (layout.svg && layout.svg.dots ? layout.svg.dots.length : 0) +
    ' capsules=' + (layout.svg && layout.svg.capsules ? layout.svg.capsules.length : 0) +
    ' edges=' + (layout.svg && layout.svg.edges ? layout.svg.edges.length : 0));
  console.log('checks pass=' + assertion.passCount + ' fail=' + assertion.failCount);
  failedChecks.forEach((c) => console.log('FAIL ' + c.name + ': ' + c.detail));
  if (pageErrors.length) console.log('PAGE ERRORS:', pageErrors.length);
  if (args.shot) console.log('shot -> ' + args.shot);
  console.log('artifacts -> ' + outDir);

  await browser.close();
  svc.kill();
  process.exit(0);
}

function formatDomText(node, depth) {
  if (!node) return '';
  const pad = ' '.repeat(depth * 2);
  let line = pad + '<' + node.tag +
    (node.id ? (' #' + node.id) : '') +
    (node.cls ? (' .' + node.cls.split(/\s+/).join('.')) : '') +
    (node.key ? (' [data-key=' + node.key.slice(0,12) + ']') : '') +
    (node.box ? (' box=(' + node.box.x + ',' + node.box.y + ',' + node.box.w + ',' + node.box.h + ')') : '') +
    (node.text ? (' "' + node.text.replace(/\n/g,' ') + '"') : '') +
    '';
  let out = line;
  if (node.style && node.style.display !== undefined) {
    out += '\n' + pad + '   style display=' + node.style.display +
      (node.style.gridTemplateColumns !== undefined && node.style.gridTemplateColumns !== '' ?
        (' grid=' + node.style.gridTemplateColumns.replace(/px/g,'')) : '') +
      (node.style.fontSize ? (' font=' + node.style.fontSize) : '') +
      (node.style.color ? (' color=' + node.style.color) : '') +
      (node.style.backgroundColor ? (' bg=' + node.style.backgroundColor) : '') +
      (node.style.opacity !== undefined && node.style.opacity !== '' && node.style.opacity !== '1' ?
        (' opacity=' + node.style.opacity) : '');
  }
  if (node.children) for (const c of node.children) out += '\n' + formatDomText(c, depth + 1);
  return out;
}

main().catch((e) => { console.error(e); process.exit(1); });
