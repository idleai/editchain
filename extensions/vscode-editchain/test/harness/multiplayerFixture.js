'use strict';

const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const Module = require('node:module');
const { randomUUID } = require('node:crypto');
const { execFileSync } = require('node:child_process');

// Reuse the production local-service client. Only its unused VS Code resolver
// dependency is stubbed; service framing, process lifetime and capture are real.
function serviceClass() {
  const load = Module._load;
  try {
    Module._load = function (name, ...rest) { return name === 'vscode' ? {} : load.call(this, name, ...rest); };
    return require(path.join(process.env.EDITCHAIN_MULTIPLAYER_TEST_EXTENSION || path.resolve(__dirname, '../..'), 'out/stdioClient')).StdioClient;
  } finally { Module._load = load; }
}

const repository = path.resolve(__dirname, '../../../..');
const binaries = {
  peer: process.env.EDITCHAIN_PEER_TEST_BINARY || path.join(repository, 'target/debug/editchain-peer'),
  service: process.env.EDITCHAIN_SERVICE_TEST_BINARY || path.join(repository, 'target/debug/editchain-vscode-service'),
};

function fixture() {
  const directory = fs.mkdtempSync(path.join(os.tmpdir(), 'editchain-multiplayer-'));
  const clients = [];
  const workspace = (name, userName) => {
    const root = path.join(directory, name);
    fs.mkdirSync(root);
    execFileSync('git', ['-c', 'init.defaultBranch=main', 'init', '--quiet', root], { stdio: 'pipe' });
    fs.writeFileSync(path.join(root, 'shared.ts'), 'Working tree stays local.\n');
    const Client = serviceClass();
    const client = new Client(); client.start(binaries.service); clients.push(client);
    const session = randomUUID();
    let sequence = 0;
    const call = async body => {
      const response = await client.request(body, { timeoutMs: 30_000 });
      if (!response?.Ok) throw new Error(`Native history fixture: ${response?.Error?.code || 'invalid_response'}`);
      return response.Ok;
    };
    const send = events => call({ RecordEditorEvents: { workspace_path: root, chain_dir: '.editchain', events:
      events.map(event => ({ schema: 1, session, ...(userName ? { user_name: userName } : {}), sequence: ++sequence, time_ms: Date.now(), event })) } });
    const start = () => send([{ type: 'tracking_started', dwell_ms: 2000, vscode_version: '1.137.0', activity_schema: 2 },
      { type: 'workspace_context', workspace_path: root, observed_ms: Date.now(), repositories: [] }]);
    const edit = async (before, after) => {
      const uri = `file://${path.join(root, 'shared.ts')}`;
      const id = randomUUID();
      const document = { id, uri, path: 'shared.ts', version: 1 };
      await send([{ type: 'document_snapshot', document, text: before }]);
      const change = sequence + 1;
      return send([{ type: 'document_changed', document: { ...document, version: 2 }, before_version: 1, before, after,
        changes: [{ offset: 0, length: before.length, text: after }], reason: 'undo' },
      { type: 'human_edit', change, signal: 'undo' }]);
    };
    return { root, chain: path.join(root, '.editchain'), device: path.join(directory, `${name}-device`), call, start, edit, client, session };
  };
  return { directory, workspace, stop() { for (const client of clients) client.stop(); fs.rmSync(directory, { recursive: true, force: true }); } };
}

async function until(check, message, timeout = 30_000) {
  const deadline = Date.now() + timeout;
  while (Date.now() < deadline) { if (await check()) return; await new Promise(resolve => setTimeout(resolve, 50)); }
  throw new Error(message);
}

function blobs(chain) {
  const root = path.join(chain, 'blobs');
  if (!fs.existsSync(root)) return [];
  return fs.readdirSync(root).filter(name => /^[a-f0-9]{64}$/.test(name)).sort();
}

async function rows(workspace) {
  const opened = await workspace.call({ Open: { workspace_path: workspace.root, chain_dir: '.editchain' } });
  const window = await workspace.call({ GetWindow: { snapshot_id: opened.snapshot_id, offset: 0, limit: 200, include_layout: false } });
  return window.rows;
}

async function diffs(workspace) {
  const opened = await workspace.call({ Open: { workspace_path: workspace.root, chain_dir: '.editchain' } });
  const window = await workspace.call({ GetWindow: { snapshot_id: opened.snapshot_id, offset: 0, limit: 200, include_layout: false } });
  const values = [];
  for (const row of window.rows) {
    if (row.file_change) values.push(await workspace.call({ GetFileDiff: { snapshot_id: opened.snapshot_id, change: row.file_change } }));
  }
  return values;
}

module.exports = { fixture, binaries, until, blobs, rows, diffs };
