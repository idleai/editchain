'use strict';
const { test } = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs/promises');
const path = require('node:path');
const os = require('node:os');
const { LiveSync } = require('../../out/liveSync');
const { captureSources, importSources } = require('../../out/liveSources');
const flush = () => new Promise(resolve => setTimeout(resolve, 5));
const deferred = () => { let resolve; const promise = new Promise(r => { resolve = r; }); return { promise, resolve }; };
async function until(predicate) {
  for (let i = 0; i < 100 && !predicate(); i++) await flush();
  assert.ok(predicate(), 'live transition completed');
}

test('growth during import and publication coalesces without overlapping work or losing the next edit', async t => {
  let stamp = 'first'; let history = 'empty';
  const importing = deferred(); const publishing = deferred();
  const imports = []; let publishes = 0;
  const loop = new LiveSync({
    capture: async () => ({ sessions: new Map([['rollout-a.jsonl', stamp]]), titles: '', history }),
    importFiles: async files => { imports.push(files); if (imports.length === 1) await importing.promise; history = stamp; },
    publish: async () => { publishes++; if (publishes === 1) await publishing.promise; },
    status() {},
  }, 60_000);
  t.after(() => loop.dispose());
  loop.wake(); await until(() => imports.length === 1);
  stamp = 'second';
  for (let i = 0; i < 30; i++) loop.wake();
  importing.resolve(); await until(() => publishes === 1);
  assert.equal(imports.length, 1, 'publication owns the same serial barrier as import');
  publishing.resolve(); await until(() => imports.length === 2);
  assert.deepEqual(imports, [['rollout-a.jsonl'], ['rollout-a.jsonl']]);
  await flush();
  assert.equal(publishes, 1, 'identical history stamps do not republish');
});

test('initial catch-up publishes each bounded batch and returns to a growing recent session before older backlog', async t => {
  const sessions = new Map([['recent', '1'], ...Array.from({ length: 64 }, (_, i) => [`archive-${i}`, '1'])]);
  const firstPublication = deferred();
  const imports = []; const events = []; let history = 0; let publications = 0;
  const loop = new LiveSync({
    capture: async () => ({ sessions: new Map(sessions), titles: '', history: String(history) }),
    importFiles: async files => { imports.push(files); events.push('import'); history++; },
    publish: async () => { events.push('publish'); if (++publications === 1) await firstPublication.promise; },
    status() {},
  }, 60_000);
  t.after(() => { loop.dispose(); firstPublication.resolve(); });
  loop.wake(); await until(() => publications === 1);
  assert.equal(imports.length, 1, 'the first update is visible before draining the archive');
  assert.equal(imports[0].length, 32);
  sessions.set('recent', '2');
  firstPublication.resolve(); await until(() => publications === 3);
  assert.equal(imports[1][0], 'recent', 'growth has priority on the next pass');
  assert.deepEqual(events, ['import', 'publish', 'import', 'publish', 'import', 'publish']);
  const counts = new Map();
  for (const file of imports.flat()) counts.set(file, (counts.get(file) || 0) + 1);
  assert.equal(counts.size, sessions.size, 'unprocessed files were not incorrectly checkpointed');
  assert.equal(counts.get('recent'), 2);
  assert.equal([...counts].filter(([file, count]) => file !== 'recent' && count !== 1).length, 0);
});

test('failed import retains source checkpoints and retries; failed publication does not repeat successful imports', async t => {
  let imports = 0; let publishes = 0; const errors = [];
  const loop = new LiveSync({
    capture: async () => ({ sessions: new Map([['a', '1']]), titles: '', history: '1' }),
    importFiles: async () => { if (++imports === 1) throw new Error('writer busy'); },
    publish: async () => { if (++publishes === 1) throw new Error('service restarted'); },
    status: text => { if (text.startsWith('Live retry')) errors.push(text); },
  }, 60_000);
  t.after(() => loop.dispose());
  loop.wake(); await until(() => errors.length === 1);
  loop.wake(); await until(() => errors.length === 2);
  loop.wake(); await until(() => publishes === 2);
  assert.equal(imports, 2);
});

test('dispose aborts its importer and suppresses later publication', async () => {
  const importing = deferred(); let signal; let publishes = 0;
  const loop = new LiveSync({
    capture: async () => ({ sessions: new Map([['a', '1']]), titles: '', history: '1' }),
    importFiles: async (_files, observed) => { signal = observed; await importing.promise; },
    publish: async () => { publishes++; }, status() {},
  });
  loop.wake(); await until(() => signal !== undefined);
  loop.dispose(); importing.resolve(); await flush();
  assert.equal(signal.aborted, true);
  assert.equal(publishes, 0);
});

test('Git or external chain changes publish without a Codex import', async t => {
  let history = 'first'; let publishes = 0;
  const loop = new LiveSync({
    capture: async () => ({ sessions: new Map(), titles: '', history }),
    importFiles: async () => assert.fail('no source changed'),
    publish: async () => { publishes++; }, status() {},
  }, 60_000);
  t.after(() => loop.dispose());
  loop.wake(); await until(() => publishes === 1);
  history = 'git-ref-moved'; loop.wake(); await until(() => publishes === 2);
  loop.wake(); await flush(); assert.equal(publishes, 2);
});

test('filesystem capture notices rollout replacement, root eclog growth, and worktree shared refs', async t => {
  const root = await fs.mkdtemp(path.join(os.tmpdir(), 'editchain-live-'));
  t.after(() => fs.rm(root, { recursive: true, force: true }));
  const paths = { workspace: path.join(root, 'worktree'), chain: path.join(root, 'chain'), sessions: path.join(root, 'sessions') };
  await fs.mkdir(paths.workspace); await fs.mkdir(paths.chain); await fs.mkdir(paths.sessions);
  const common = path.join(root, 'git'); const git = path.join(common, 'worktrees', 'live');
  await fs.mkdir(git, { recursive: true }); await fs.mkdir(path.join(common, 'refs', 'heads'), { recursive: true });
  await fs.writeFile(path.join(paths.workspace, '.git'), `gitdir: ${git}\n`);
  await fs.writeFile(path.join(git, 'commondir'), '../..\n');
  await fs.writeFile(path.join(git, 'HEAD'), 'ref: refs/heads/main\n');
  const ref = path.join(common, 'refs', 'heads', 'main'); await fs.writeFile(ref, 'one');
  const rollout = path.join(paths.sessions, 'rollout-1.jsonl'); await fs.writeFile(rollout, '{}\n');
  const first = await captureSources(paths);
  await fs.writeFile(path.join(paths.chain, '000000.eclog'), 'durable operations');
  const imported = await captureSources(paths); assert.notEqual(imported.history, first.history);
  await fs.writeFile(ref, 'two'); const committed = await captureSources(paths);
  assert.notEqual(committed.history, imported.history);
  await fs.writeFile(rollout + '.tmp', '{}\n'); await fs.rename(rollout + '.tmp', rollout);
  const replaced = await captureSources(paths);
  assert.notEqual(replaced.sessions.get(rollout), first.sessions.get(rollout));
  await fs.writeFile(path.join(paths.sessions, 'session_index.jsonl'), '{}\n');
  const renamed = await captureSources(paths);
  assert.notEqual(renamed.titles, replaced.titles);
  const older = path.join(paths.sessions, 'rollout-z-older.jsonl');
  await fs.writeFile(older, '{}\n'); await fs.utimes(older, 100, 100);
  const newest = await captureSources(paths);
  assert.deepEqual([...newest.sessions.keys()], [rollout, older], 'recent modifications precede older sessions regardless of filename order');
});

test('import batches selected workspace rollouts with the original root, using argument arrays', async t => {
  const root = await fs.mkdtemp(path.join(os.tmpdir(), 'editchain live paths '));
  t.after(() => fs.rm(root, { recursive: true, force: true }));
  const workspace = path.join(root, 'workspace'); const sessions = path.join(root, 'sessions');
  await fs.mkdir(workspace); await fs.mkdir(sessions);
  await fs.writeFile(path.join(workspace, 'import'), `require('fs').appendFileSync('calls.jsonl', JSON.stringify(process.argv.slice(2)) + '\\n');`);
  const files = ['rollout-one.jsonl', 'rollout-two.jsonl', 'rollout-foreign.jsonl'].map(name => path.join(sessions, name));
  for (let index = 0; index < files.length; index++) {
    await fs.writeFile(files[index], JSON.stringify({ type: 'session_meta', payload: { cwd: index < 2 ? workspace : root } }) + '\n');
  }
  await importSources({ workspace, sessions, cli: process.execPath, helper: 'unused', chain: path.join(root, 'chain') }, files, new AbortController().signal);
  const calls = (await fs.readFile(path.join(workspace, 'calls.jsonl'), 'utf8')).trim().split('\n').map(JSON.parse);
  assert.equal(calls.length, 1);
  assert.equal(calls[0][calls[0].indexOf('--sessions-dir') + 1], sessions);
  assert.deepEqual(calls[0].slice(calls[0].indexOf('--codex-rollout')), ['--codex-rollout', files[0], '--codex-rollout', files[1]]);
  assert.equal(calls[0].includes(files[2]), false);
});

test('native importer output reaches diagnostics while the child is still running', async t => {
  const root = await fs.mkdtemp(path.join(os.tmpdir(), 'editchain-live-progress-'));
  const sessions = path.join(root, 'sessions'); await fs.mkdir(sessions);
  const file = path.join(sessions, 'rollout-live.jsonl');
  await fs.writeFile(file, JSON.stringify({ type: 'session_meta', payload: { cwd: root } }) + '\n');
  await fs.writeFile(path.join(root, 'import'), `
process.stdout.write('Import phase reached\\n');
const timer = setInterval(() => {
  if (require('fs').existsSync('release')) {
    clearInterval(timer);
    process.stderr.write('Snapshot completed\\n');
  }
}, 5);
`);
  const abort = new AbortController(); const lines = []; let finished = false;
  const work = importSources({ workspace: root, sessions, cli: process.execPath, helper: 'unused', chain: path.join(root, 'chain') }, [file], abort.signal, line => lines.push(line)).finally(() => { finished = true; });
  t.after(async () => { abort.abort(); await work.catch(() => {}); await fs.rm(root, { recursive: true, force: true }); });
  await until(() => lines.some(line => line.includes('Import phase reached')));
  assert.equal(finished, false, 'progress is not withheld until process exit');
  await fs.writeFile(path.join(root, 'release'), ''); await work;
  assert.ok(lines.some(line => line.includes('Snapshot completed')));
});
