import puppeteer from 'puppeteer-core';
import { spawn } from 'child_process';
import path from 'path';
import { fileURLToPath } from 'url';
const __dirname = path.dirname(fileURLToPath(import.meta.url));
const EXT_ROOT = __dirname;
const HARNESS = 'file://' + path.join(EXT_ROOT, 'test', 'harness', 'index.html') + '?bridge=service';
const CHROME = '/mnt/hot/ambientlight/.cache/puppeteer/chrome/linux-151.0.7922.71/chrome-linux64/chrome';
const WORKSPACE = '/mnt/hot/ambientlight/repos/editchain';
const SERVICE = path.join(WORKSPACE, 'target', 'debug', 'editchain-vscode-service');
function makeServiceClient(binaryPath) {
  const proc = spawn(binaryPath, [], { stdio: ['pipe','pipe','pipe'] });
  let buf = Buffer.alloc(0); let nextId = 1; const pending = new Map(); const stderrLines = [];
  proc.stderr.on('data', c => stderrLines.push(c.toString()));
  proc.stdout.on('data', c => {
    buf = Buffer.concat([buf, c]);
    while (buf.length >= 4) {
      const len = buf.readUInt32LE(0);
      if (buf.length < 4 + len) break;
      const payload = buf.subarray(4, 4+len).toString('utf8');
      buf = buf.subarray(4+len);
      let msg; try { msg = JSON.parse(payload); } catch { continue; }
      if (msg.id !== undefined && pending.has(msg.id)) { const r = pending.get(msg.id); pending.delete(msg.id); r(msg.body); }
    }
  });
  return {
    send(body, timeoutMs) {
      const id = nextId++;
      return new Promise((resolve, reject) => {
        const t = setTimeout(() => { pending.delete(id); reject(new Error('timeout')); }, timeoutMs||20000);
        pending.set(id, b => { clearTimeout(t); resolve(b); });
        const payload = Buffer.from(JSON.stringify({id, body}), 'utf8');
        const header = Buffer.alloc(4); header.writeUInt32LE(payload.length, 0);
        proc.stdin.write(Buffer.concat([header, payload]));
      });
    },
    stderr(){ return stderrLines.join(''); },
    kill(){ proc.kill(); },
  };
}
const svc = makeServiceClient(SERVICE);
await svc.send({ Open: { workspace_path: WORKSPACE, chain_dir: '.editchain' } });
const browser = await puppeteer.launch({ executablePath: CHROME, headless: 'new', args: ['--no-sandbox','--disable-setuid-sandbox'] });
const page = await browser.newPage();
await page.setViewport({ width: 1440, height: 900 });
const logs = [];
page.on('console', m => logs.push(m.text()));
await page.evaluateOnNewDocument((ws) => {
  window.__editchainWorkspace = ws; window.__editchainChainDir = '.editchain';
  window.__editchainService = { send(body){ return window.__editchainServiceSend(body); } };
}, WORKSPACE);
await page.exposeFunction('__editchainServiceSend', body => svc.send(body));
await page.goto(HARNESS, { waitUntil: 'networkidle0' });
await page.waitForFunction(() => typeof window.vscode !== 'undefined' && typeof window.__editchainStart === 'function', { timeout: 10000 });
await page.evaluate(() => window.__editchainStart());
await page.waitForFunction(() => document.querySelectorAll('.row').length > 0, { timeout: 30000 });
await new Promise(r => setTimeout(r, 2500));
await page.evaluate(() => window.__editchainDebug.whenIdle(8000));

async function sample(label) {
  const s = await page.evaluate(() => {
    const rows = document.getElementById('rows');
    return { scrollTop: Math.round(rows.scrollTop), scrollHeight: rows.scrollHeight, clientHeight: rows.clientHeight,
      rendered: document.querySelectorAll('.row').length,
      firstKey: document.querySelector('.row')?.getAttribute('data-key'),
      lastKey: Array.from(document.querySelectorAll('.row')).slice(-1)[0]?.getAttribute('data-key') };
  });
  console.log(label, JSON.stringify(s));
}

await sample('INITIAL');

// Scroll to bottom
await page.evaluate(() => {
  const rows = document.getElementById('rows');
  rows.scrollTop = rows.scrollHeight;
  rows.dispatchEvent(new Event('scroll'));
});
await new Promise(r => setTimeout(r, 1500));
await page.evaluate(() => window.__editchainDebug.whenIdle(6000));
await sample('AFTER-SCROLL-BOTTOM');

// Check what scrollTop actually is now
const st = await page.evaluate(() => {
  const rows = document.getElementById('rows');
  return { scrollTop: rows.scrollTop, scrollHeight: rows.scrollHeight };
});
console.log('RAW scrollTop after bottom:', JSON.stringify(st));

console.log('--- console ---');
logs.slice(-15).forEach(l => console.log(l));
await browser.close(); svc.kill(); process.exit(0);
