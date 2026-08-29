#!/usr/bin/env node
// Phase-by-phase startup probe for the real EditChain stdio service.
//
// Measures the exact production sequence without browser/extension-host noise:
// Open -> row-only first window -> layout hydration -> warm deep window. The
// probe also verifies that provisional and laid-out pages contain the same row
// identities, making the first-paint split an executable protocol contract.

import { spawn } from 'child_process';
import fs from 'fs';
import path from 'path';

function parseArgs(argv) {
  const args = {
    workspace: '/mnt/hot/ambientlight/repos/editchain',
    chainDir: '.editchain',
    limit: 500,
    warmOffset: 75_000,
    timeoutMs: 120_000,
    out: null,
  };
  for (let i = 0; i < argv.length; i++) {
    const arg = argv[i];
    if (arg === '--workspace') args.workspace = argv[++i];
    else if (arg === '--chain-dir') args.chainDir = argv[++i];
    else if (arg === '--limit') args.limit = Number(argv[++i]) || args.limit;
    else if (arg === '--warm-offset') args.warmOffset = Number(argv[++i]) || 0;
    else if (arg === '--timeout') args.timeoutMs = Number(argv[++i]) || args.timeoutMs;
    else if (arg === '--out') args.out = argv[++i];
  }
  return args;
}

function servicePath(workspace) {
  if (process.env.SERVICE_PATH) return process.env.SERVICE_PATH;
  const release = path.join(workspace, 'target', 'release', 'editchain-vscode-service');
  if (fs.existsSync(release)) return release;
  return path.join(workspace, 'target', 'debug', 'editchain-vscode-service');
}

function processMemory(pid) {
  try {
    const status = fs.readFileSync('/proc/' + pid + '/status', 'utf8');
    const value = (name) => {
      const match = new RegExp('^' + name + ':\\s+(\\d+) kB$', 'm').exec(status);
      return match ? Number(match[1]) : null;
    };
    return { rss_kib: value('VmRSS'), hwm_kib: value('VmHWM') };
  } catch {
    return { rss_kib: null, hwm_kib: null };
  }
}

function client(binary, timeoutMs) {
  const proc = spawn(binary, [], { stdio: ['pipe', 'pipe', 'pipe'] });
  let buffer = Buffer.alloc(0);
  let nextId = 1;
  const pending = new Map();
  let stderr = '';

  const fail = (reason) => {
    for (const [id, request] of pending) {
      pending.delete(id);
      clearTimeout(request.timer);
      request.reject(new Error(reason));
    }
  };
  proc.on('error', (error) => fail(error.message));
  proc.on('exit', (code) => fail('service exited with code ' + code));
  proc.stderr.on('data', (chunk) => { stderr += chunk.toString(); });
  proc.stdout.on('data', (chunk) => {
    buffer = Buffer.concat([buffer, chunk]);
    while (buffer.length >= 4) {
      const length = buffer.readUInt32LE(0);
      if (buffer.length < length + 4) break;
      const frame = buffer.subarray(4, length + 4);
      buffer = buffer.subarray(length + 4);
      const message = JSON.parse(frame.toString('utf8'));
      const request = pending.get(message.id);
      if (!request) continue;
      pending.delete(message.id);
      clearTimeout(request.timer);
      request.resolve({ message, responseBytes: length });
    }
  });

  return {
    pid: proc.pid,
    request(body) {
      const id = nextId++;
      const payload = Buffer.from(JSON.stringify({ id, body }), 'utf8');
      const header = Buffer.alloc(4);
      header.writeUInt32LE(payload.length, 0);
      const started = performance.now();
      return new Promise((resolve, reject) => {
        const timer = setTimeout(() => {
          pending.delete(id);
          reject(new Error('request timed out: ' + JSON.stringify(body).slice(0, 100)));
        }, timeoutMs);
        pending.set(id, {
          timer,
          reject,
          resolve: ({ message, responseBytes }) => resolve({
            message,
            responseBytes,
            elapsedMs: performance.now() - started,
          }),
        });
        proc.stdin.write(Buffer.concat([header, payload]));
      });
    },
    stop() { proc.kill(); },
    stderr() { return stderr; },
  };
}

function okValue(result, phase) {
  const body = result.message && result.message.body;
  if (!body || body.Ok === undefined) {
    throw new Error(phase + ' failed: ' + JSON.stringify(body));
  }
  return body.Ok;
}

async function main() {
  const args = parseArgs(process.argv.slice(2));
  const binary = servicePath(args.workspace);
  if (!fs.existsSync(binary)) throw new Error('service binary not found: ' + binary);
  const svc = client(binary, args.timeoutMs);
  const phases = {};
  const filter = {
    summary_pattern: '',
    kind_pattern: '',
    include_kind_pattern: '',
    hide_undated: false,
    splice: true,
  };

  const measure = async (name, body) => {
    const result = await svc.request(body);
    const value = okValue(result, name);
    phases[name] = {
      elapsed_ms: Math.round(result.elapsedMs * 10) / 10,
      response_bytes: result.responseBytes,
      ...processMemory(svc.pid),
      rows: Array.isArray(value.rows) ? value.rows.length : undefined,
      total: value.total,
      layout_ready: value.layout_ready,
      max_lane: value.max_lane,
    };
    return value;
  };

  try {
    const opened = await measure('open', {
      Open: { workspace_path: args.workspace, chain_dir: args.chainDir },
    });
    const windowBase = {
      offset: 0,
      limit: args.limit,
      hide_submodules: true,
      filter,
    };
    const provisional = await measure('first_rows', {
      GetWindow: { ...windowBase, include_layout: false },
    });
    const laidOut = await measure('layout', {
      GetWindow: { ...windowBase, include_layout: true },
    });
    await measure('warm_scroll', {
      GetWindow: { ...windowBase, offset: args.warmOffset, include_layout: true },
    });

    const provisionalKeys = provisional.rows.map((row) => row.node_key);
    const laidOutKeys = laidOut.rows.map((row) => row.node_key);
    const release = path.join(args.workspace, 'target', 'release', 'editchain-vscode-service');
    const checks = {
      release_preferred_when_available: !fs.existsSync(release) || binary === release,
      rows_before_layout: provisional.rows.length > 0 && provisional.layout_ready === false,
      layout_completes: laidOut.rows.length > 0 && laidOut.layout_ready === true,
      row_identity_stable: JSON.stringify(provisionalKeys) === JSON.stringify(laidOutKeys),
    };
    const report = {
      binary,
      workspace: args.workspace,
      chain_dir: args.chainDir,
      chain_generation: opened.chain_generation,
      diagnostics: opened.diagnostics,
      phases,
      totals: {
        first_rows_ms: Math.round((phases.open.elapsed_ms + phases.first_rows.elapsed_ms) * 10) / 10,
        layout_complete_ms: Math.round((phases.open.elapsed_ms + phases.first_rows.elapsed_ms + phases.layout.elapsed_ms) * 10) / 10,
      },
      checks,
      pass: Object.values(checks).every(Boolean),
    };
    const json = JSON.stringify(report, null, 2);
    if (args.out) fs.writeFileSync(args.out, json + '\n');
    console.log(json);
    if (!report.pass) process.exitCode = 1;
  } finally {
    svc.stop();
    const serviceStderr = svc.stderr();
    if (serviceStderr && process.exitCode) console.error(serviceStderr);
  }
}

main().catch((error) => {
  console.error(error.stack || error.message || String(error));
  process.exit(1);
});
