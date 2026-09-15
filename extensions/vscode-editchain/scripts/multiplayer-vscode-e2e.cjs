#!/usr/bin/env node
'use strict';
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const { spawn, execFileSync } = require('node:child_process');

const extension = path.resolve(__dirname, '..');
const output = path.resolve(process.env.EDITCHAIN_MULTIPLAYER_UI_OUTPUT || path.join(extension, 'trace/multiplayer'));

async function cleanup(packagePath, markers) {
  if (!markers.length) return;
  const { managementClient, cleanupRelay } = require(path.join(packagePath, 'out/multiplayer/relay'));
  const management = managementClient(async () => execFileSync('gh', ['auth', 'token', '--hostname', 'github.com'], { encoding: 'utf8', stdio: ['ignore', 'pipe', 'pipe'] }).trim());
  const remaining = [];
  try {
    for (const marker of markers) {
      try { await cleanupRelay(management, marker, { forget: async () => {}, remember: async () => {} }); }
      catch { remaining.push(marker); }
    }
  } finally { await management.dispose(); }
  if (remaining.length) {
    fs.writeFileSync(path.join(output, 'pending-cleanup.json'), JSON.stringify(remaining));
    throw new Error('Temporary tunnel cleanup remains pending; recovery labels saved');
  }
}

function recoveryMarkers(root) {
  const script = `import sys,sqlite3,json,os
found=set()
for role in ['host','guest']:
 p=os.path.join(sys.argv[1],role+'-profile','settings','User','globalStorage','state.vscdb')
 if not os.path.exists(p): continue
 db=sqlite3.connect(p)
 row=db.execute("SELECT value FROM ItemTable WHERE key = ?",('ambientlight.editchain-history',)).fetchone()
 if row:
  data=json.loads(row[0])
  found.update(k.removeprefix('editchain.multiplayer.pending.') for k in data if k.startswith('editchain.multiplayer.pending.'))
 db.close()
print(json.dumps(sorted(found)))`;
  return JSON.parse(execFileSync('python3', ['-c', script, root], { encoding: 'utf8', stdio: ['ignore', 'pipe', 'pipe'] }));
}

function privateArtifacts(root) {
  const values = [];
  for (const name of ['github-token', 'invitation']) {
    const file = path.join(root, name);
    if (fs.existsSync(file)) values.push(fs.readFileSync(file, 'utf8').trim());
  }
  const invitation = values.find(value => value.startsWith('editchain:'));
  let clean = true;
  if (invitation) {
    try { values.push(JSON.parse(Buffer.from(invitation.slice(10), 'base64url')).connectToken); }
    catch { clean = false; }
  }
  const inspect = directory => {
    for (const entry of fs.readdirSync(directory, { withFileTypes: true })) {
      const file = path.join(directory, entry.name);
      if (entry.isDirectory()) inspect(file);
      else if (entry.isFile() && /\.(log|json)$/.test(file)) {
        let text = fs.readFileSync(file, 'utf8');
        for (const value of values.filter(value => typeof value === 'string' && value.length > 20)) {
          if (text.includes(value)) { text = text.split(value).join('[REDACTED]'); clean = false; }
        }
        if (!clean) fs.writeFileSync(file, text);
      }
    }
  };
  inspect(output); return clean;
}

async function run() {
  fs.mkdirSync(output, { recursive: true });
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'editchain-multiplayer-ui-'));
  fs.chmodSync(root, 0o700);
  const children = [];
  let passed = false, cleanupDone = false, credentialsOmitted = false;
  try {
    const vsix = path.resolve(process.env.EDITCHAIN_MULTIPLAYER_UI_VSIX || path.join(extension, '../../outputs/editchain-history-multiplayer.vsix'));
    execFileSync('unzip', ['-q', vsix, '-d', path.join(root, 'package')], { stdio: 'pipe' });
    fs.cpSync(path.join(extension, 'test/vscode/multiplayer-probe'), path.join(root, 'probe'), { recursive: true });
    fs.mkdirSync(path.join(root, 'sessions'));
    for (const role of ['host', 'guest']) {
      const workspace = path.join(root, role); fs.mkdirSync(workspace);
      fs.mkdirSync(path.join(output, role), { recursive: true });
      const manifest = JSON.parse(fs.readFileSync(path.join(root, 'package/extension/package.json'), 'utf8'));
      const installed = path.join(root, role + '-profile/extensions', `${manifest.publisher}.${manifest.name}-${manifest.version}`);
      fs.cpSync(path.join(root, 'package/extension'), installed, { recursive: true });
      execFileSync('git', ['-c', 'init.defaultBranch=main', 'init', '--quiet', workspace], { stdio: 'pipe' });
      fs.writeFileSync(path.join(workspace, '.gitignore'), '.editchain/\n');
      for (const name of ['from-host.ts', 'from-guest.ts']) fs.writeFileSync(path.join(workspace, name), `export const owner = '${role}'; // \n`);
    }
    const token = execFileSync('gh', ['auth', 'token', '--hostname', 'github.com'], { encoding: 'utf8', stdio: ['ignore', 'pipe', 'pipe'] }).trim();
    fs.writeFileSync(path.join(root, 'github-token'), token, { mode: 0o600 });
    console.log('Starting two isolated VS Code instances with the packaged extension and real relay.');
    const results = await Promise.all(['host', 'guest'].map(role => new Promise(resolve => {
      const log = fs.openSync(path.join(output, role, 'runner.log'), 'w', 0o600);
      const child = spawn('xvfb-run', ['-a', '-s', '-screen 0 1440x1000x24', path.join(extension, 'node_modules/.bin/wdio'), 'run', './test/vscode/wdio.multiplayer.conf.ts'], {
        cwd: extension, detached: true, stdio: ['ignore', log, log],
        env: { ...process.env, EDITCHAIN_MULTIPLAYER_UI_FIXTURE: root, EDITCHAIN_MULTIPLAYER_UI_ROLE: role, EDITCHAIN_MULTIPLAYER_UI_OUTPUT: output,
          EDITCHAIN_MULTIPLAYER_UI_PROXY: path.join(extension, 'node_modules/wdio-vscode-service/dist/proxy/index.js') },
      });
      fs.closeSync(log); children.push(child);
      const timeout = setTimeout(() => { try { process.kill(-child.pid, 'SIGTERM'); } catch {} }, 360_000);
      child.once('error', () => { clearTimeout(timeout); fs.writeFileSync(path.join(root, role + '.failed'), 'start'); resolve(1); });
      child.once('exit', code => { clearTimeout(timeout); if (code !== 0) fs.writeFileSync(path.join(root, role + '.failed'), 'exit'); resolve(code ?? 1); });
    })));
    passed = results.every(code => code === 0);
  } finally {
    for (const child of children) { try { process.kill(-child.pid, 'SIGTERM'); } catch {} }
    credentialsOmitted = privateArtifacts(root);
    passed = passed && credentialsOmitted;
    try { await cleanup(path.join(root, 'package/extension'), recoveryMarkers(root)); cleanupDone = true; }
    finally {
      fs.rmSync(root, { recursive: true, force: true });
      fs.writeFileSync(path.join(output, 'run.json'), JSON.stringify({ passed, tunnelCleanupComplete: cleanupDone, credentialsOmitted,
        scope: 'two VS Code instances, same machine/account, test-only GitHub provider, real relay', finishedAt: new Date().toISOString() }, null, 2));
    }
  }
  if (!passed) throw new Error('VS Code multiplayer test failed; inspect trace/multiplayer role logs');
  console.log('PASS: two-window packaged VS Code E2E; temporary tunnels deleted. Artifacts: ' + output);
}
run().catch(error => { console.error(error instanceof Error && /^(Temporary|VS Code)/.test(error.message) ? error.message : 'E2E setup failed; credential and service details omitted.'); process.exitCode = 1; });
