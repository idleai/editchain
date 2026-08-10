#!/usr/bin/env node
// Render the REAL EditChain data from a chain dir through the harness, using
// the actual Rust service over framed stdio. Dumps the full DOM tree + styles
// as text artifacts — no screenshots.
//
// Usage:
//   node scripts/ui-real.mjs [--workspace DIR] [--chain-dir .editchain]
//                            [--viewport WxH] [--out DIR] [--selector Q]
//
// The service binary path comes from SERVICE_PATH or defaults to
// <workspace>/target/debug/editchain-vscode-service.

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
  const args = { workspace: null, chainDir: '.editchain', viewport: '1440x900', out: null, selector: null };
  for (let i = 0; i < argv.length; i++) {
    const a = argv[i];
    if (a === '--workspace') args.workspace = argv[++i];
    else if (a === '--chain-dir') args.chainDir = argv[++i];
    else if (a === '--viewport') args.viewport = argv[++i];
    else if (a === '--out') args.out = argv[++i];
    else if (a === '--selector') args.selector = argv[++i];
  }
  if (!args.workspace) args.workspace = '/mnt/hot/ambientlight/repos/editchain';
  return args;
}

function parseViewport(vp) {
  const m = /^(\d+)x(\d+)$/.exec(vp);
  if (!m) throw new Error('bad viewport: ' + vp);
  return { width: +m[1], height: +m[2] };
}

// --- framed stdio client for the Rust service --------------------------------

function makeServiceClient(binaryPath) {
  const proc = spawn(binaryPath, [], { stdio: ['pipe', 'pipe', 'pipe'] });
  let buf = Buffer.alloc(0);
  let nextId = 1;
  const pending = new Map();
  const stderrLines = [];

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
        const resolve = pending.get(msg.id);
        pending.delete(msg.id);
        resolve(msg.body);
      }
    }
  });

  return {
    send(body, timeoutMs) {
      const id = nextId++;
      return new Promise((resolve, reject) => {
        const t = setTimeout(() => {
          pending.delete(id);
          reject(new Error('service request timed out: ' + JSON.stringify(body).slice(0, 80)));
        }, timeoutMs || 15000);
        pending.set(id, (body) => { clearTimeout(t); resolve(body); });
        const payload = Buffer.from(JSON.stringify({ id, body }), 'utf8');
        const header = Buffer.alloc(4);
        header.writeUInt32LE(payload.length, 0);
        proc.stdin.write(Buffer.concat([header, payload]));
      });
    },
    stderr() { return stderrLines.join(''); },
    kill() { proc.kill(); },
  };
}

async function main() {
  console.log('STEP start');
  const args = parseArgs(process.argv.slice(2));
  const vp = parseViewport(args.viewport);

  const servicePath = process.env.SERVICE_PATH ||
    path.join(args.workspace, 'target', 'debug', 'editchain-vscode-service');
  if (!fs.existsSync(servicePath)) {
    console.error('service binary not found at ' + servicePath);
    process.exit(1);
  }

  const outDir = args.out || path.join(EXT_ROOT, '.ui-out', 'real');
  fs.mkdirSync(outDir, { recursive: true });

  const svc = makeServiceClient(servicePath);

  // Open the workspace first to confirm it loads.
  console.log('STEP open...');
  const openResp = await svc.send({ Open: { workspace_path: args.workspace, chain_dir: args.chainDir } });
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
      send(body) {
        return window.__editchainServiceSend(body);
      },
    };
  }, args.workspace, args.chainDir);

  // Bridge Node-side send into the page (must be exposed before goto so the
  // page's __editchainService.send can call it).
  console.log('STEP exposeFunction...');
  await page.exposeFunction('__editchainServiceSend', (body) => svc.send(body));

  await page.goto(HARNESS, { waitUntil: 'networkidle0' });

  console.log('STEP wait for serviceBridge...');
  // Wait for serviceBridge to load and define vscode.
  await page.waitForFunction(() => typeof window.vscode !== 'undefined' &&
    typeof window.__editchainStart === 'function', { timeout: 10000 });

  // Start the handshake.
  console.log('STEP start handshake...');
  await page.evaluate(() => window.__editchainStart());

  // Wait for rows to actually render (the real service round-trips async).
  console.log('STEP wait for rows...');
  await page.waitForFunction(() => document.querySelectorAll('.row').length > 0,
    { timeout: 20000 });
  console.log('STEP rows present');

  // Wait for the UI to settle deterministically.
  console.log('STEP whenIdle...');
  await page.evaluate(() => window.__editchainDebug.whenIdle(8000));
  console.log('STEP idle done');

  // Collect artifacts.
  const layout = await page.evaluate(() => window.__editchainDebug.dumpLayout());
  const metrics = await page.evaluate(() => window.__editchainDebug.getMetrics());
  const assertion = await page.evaluate(() => window.__editchainDebug.assertLayout());

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
  fs.writeFileSync(path.join(outDir, 'console.txt'), consoleLines.join('\n'));
  fs.writeFileSync(path.join(outDir, 'dom.json'), JSON.stringify(domTree, null, 2));
  fs.writeFileSync(path.join(outDir, 'dom.txt'), formatDomText(domTree));
  fs.writeFileSync(path.join(outDir, 'service-stderr.txt'), svc.stderr());

  // Summary.
  const failedChecks = assertion.checks.filter((c) => !c.pass);
  const summary = [
    '# EditChain real UI dump',
    '',
    '- workspace: ' + args.workspace,
    '- chain dir: ' + args.chainDir,
    '- open response: ' + JSON.stringify(openResp),
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
  if (failedChecks.length) failedChecks.forEach((c) => summary.push('- **' + c.name + '** FAIL — ' + c.detail));
  else summary.push('_none_');
  if (pageErrors.length) summary.push('', '## Page errors', '', ...pageErrors.map((e) => '- ' + e));
  fs.writeFileSync(path.join(outDir, 'summary.md'), summary.join('\n'));

  // Console output.
  console.log('state=' + JSON.stringify(layout.state));
  console.log('svg dots=' + (layout.svg && layout.svg.dots ? layout.svg.dots.length : 0) +
    ' edges=' + (layout.svg && layout.svg.edges ? layout.svg.edges.length : 0));
  console.log('checks pass=' + assertion.passCount + ' fail=' + assertion.failCount);
  failedChecks.forEach((c) => console.log('FAIL ' + c.name + ': ' + c.detail));
  if (pageErrors.length) console.log('PAGE ERRORS:', pageErrors.length);
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