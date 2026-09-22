'use strict';
const { test } = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs/promises');
const os = require('node:os');
const path = require('node:path');
const { HistoryArchive, archiveDirectory, archiveDay, archiveFileName, historyArchiveLine,
  HISTORY_ARCHIVE_FORMAT, HISTORY_ARCHIVE_SCHEMA } = require('../../out/historyArchive');
const { HISTORY_ARCHIVE_QUEUE_BYTES } = require('../../out/editorLimits');

const WORKSPACE = '/original/workspace';
const SESSION = '11111111-1111-4111-8111-111111111111';
const event = (sequence, extra = {}) => ({ schema: 1, session: SESSION,
  identity: { kind: 'unsigned', guid: '22222222-2222-4222-8222-222222222222', stream: 'a'.repeat(24) },
  user_name: 'tester', sequence, time_ms: 1700000000000 + sequence,
  event: { type: sequence === 1 ? 'tracking_started' : 'document_snapshot', ...extra } });

const directory = () => fs.mkdtemp(path.join(os.tmpdir(), 'editchain-archive-'));
const day = () => archiveDay(new Date());

function writer(directoryPath, options = {}) {
  const logs = [], reports = [];
  const archive = new HistoryArchive({ directory: directoryPath, log: line => logs.push(line),
    report: message => reports.push(message), ...options });
  return { archive, logs, reports };
}

async function records(file) {
  return (await fs.readFile(file, 'utf8')).split('\n').filter(Boolean).map(line => JSON.parse(line));
}

async function allocated(archive) {
  for (let attempt = 0; attempt < 400 && !archive.location; attempt++) {
    await new Promise(resolve => setTimeout(resolve, 5));
  }
  return archive.location;
}

test('each line is the agreed source record carrying the full editor event', async () => {
  const dir = await directory();
  const { archive, logs } = writer(dir);
  try {
    const full = event(3, { text: 'one\ntwo\n', document: { id: '1', uri: 'file:///original/workspace/a.ts', path: 'a.ts', version: 2 } });
    archive.append(WORKSPACE, full);
    await archive.stop();
    const file = archive.location;
    assert.equal(path.dirname(file), dir);
    assert.equal(path.basename(file), archiveFileName(day(), 1));
    const [line] = await records(file);
    assert.deepEqual(Object.keys(line), ['format', 'schema', 'workspace_path', 'event']);
    assert.equal(line.format, HISTORY_ARCHIVE_FORMAT);
    assert.equal(line.schema, HISTORY_ARCHIVE_SCHEMA);
    assert.equal(line.workspace_path, WORKSPACE);
    assert.deepEqual(line.event, full, 'snapshots, attribution, context and lifecycle survive verbatim');
    assert.equal((await fs.readFile(file, 'utf8')).endsWith('\n'), true);
    assert.equal(historyArchiveLine(WORKSPACE, full).length, (await fs.stat(file)).size);
    assert.ok(logs.some(entry => entry.includes('Human history archive: ')));
  } finally { await fs.rm(dir, { recursive: true, force: true }); }
});

test('a reload allocates the next session file instead of overwriting', async () => {
  const dir = await directory();
  try {
    const first = writer(dir);
    first.archive.append(WORKSPACE, event(1, { text: 'first' }));
    await first.archive.stop();
    const second = writer(dir);
    second.archive.append(WORKSPACE, event(1, { text: 'second' }));
    await second.archive.stop();
    const names = (await fs.readdir(dir)).sort();
    assert.deepEqual(names, [archiveFileName(day(), 1), archiveFileName(day(), 2)]);
    assert.deepEqual((await records(path.join(dir, names[0]))).map(line => line.event.event.text), ['first']);
    assert.deepEqual((await records(path.join(dir, names[1]))).map(line => line.event.event.text), ['second']);
  } finally { await fs.rm(dir, { recursive: true, force: true }); }
});

test('the next counter stays above the day maximum so replay order is preserved', async () => {
  const dir = await directory();
  try {
    await fs.writeFile(path.join(dir, archiveFileName(day(), 7)), '');
    await fs.writeFile(path.join(dir, archiveFileName(day(), 1)), '');
    await fs.writeFile(path.join(dir, archiveFileName('2020-01-01', 99)), '');
    const { archive } = writer(dir);
    archive.append(WORKSPACE, event(1));
    await archive.stop();
    assert.equal(path.basename(archive.location), archiveFileName(day(), 8));
  } finally { await fs.rm(dir, { recursive: true, force: true }); }
});

test('a file keeps its allocation day and name across midnight', async () => {
  const dir = await directory();
  let clock = new Date(2026, 8, 21, 23, 59, 30);
  const { archive } = writer(dir, { now: () => clock });
  try {
    archive.append(WORKSPACE, event(1, { text: 'before' }));
    const file = await allocated(archive);
    clock = new Date(2026, 8, 22, 0, 0, 30);
    archive.append(WORKSPACE, event(2, { text: 'after' }));
    await archive.stop();
    assert.equal(archive.location, file);
    assert.equal(path.basename(file), '2026-09-21-session-0001.jsonl');
    assert.deepEqual((await records(file)).map(line => line.event.event.text), ['before', 'after']);
  } finally { await fs.rm(dir, { recursive: true, force: true }); }
});

test('concurrent windows allocate distinct files and keep ordered lines', async () => {
  const dir = await directory();
  try {
    const windows = [writer(dir), writer(dir)];
    await Promise.all(windows.map(async ({ archive }, index) => {
      for (let sequence = 1; sequence <= 40; sequence++) {
        archive.append(WORKSPACE, event(sequence, { text: `window-${index}-${sequence}` }));
        if (sequence % 7 === 0) await new Promise(resolve => setImmediate(resolve));
      }
      await archive.stop();
    }));
    const names = (await fs.readdir(dir)).sort();
    assert.deepEqual(names, [archiveFileName(day(), 1), archiveFileName(day(), 2)]);
    const texts = await Promise.all(names.map(async name => (await records(path.join(dir, name))).map(line => line.event.event.text)));
    const owners = texts.map(sequence => sequence[0].split('-')[1]);
    assert.notEqual(owners[0], owners[1], 'each window keeps its own file');
    for (const [index, sequence] of texts.entries()) {
      assert.deepEqual(sequence, Array.from({ length: 40 }, (_, position) => `window-${owners[index]}-${position + 1}`));
    }
  } finally { await fs.rm(dir, { recursive: true, force: true }); }
});

test('the directory setting follows the documented resolution rules', () => {
  assert.deepEqual(archiveDirectory('', ['/one'], '/storage/human-history'), { directory: '/storage/human-history' });
  assert.deepEqual(archiveDirectory('   ', [], '/storage/human-history'), { directory: '/storage/human-history' });
  assert.deepEqual(archiveDirectory('/abs/archives', [], 'x'), { directory: path.normalize('/abs/archives') });
  assert.deepEqual(archiveDirectory('~/archives', [], 'x'), { directory: path.join(os.homedir(), 'archives') });
  assert.deepEqual(archiveDirectory('archives', ['/one'], 'x'), { directory: path.resolve('/one', 'archives') });
  assert.match(archiveDirectory('archives', ['/one', '/two'], 'x').error, /2 workspace folders/);
  assert.match(archiveDirectory('archives', [], 'x').error, /no workspace folder/);
});

test('an unusable destination is reported once and never claims later events', async () => {
  const root = await directory();
  const blocker = path.join(root, 'blocked');
  await fs.writeFile(blocker, 'not a directory');
  const { archive, reports } = writer(path.join(blocker, 'archives'));
  try {
    archive.append(WORKSPACE, event(1));
    await archive.stop();
    assert.equal(archive.failed, true);
    assert.equal(reports.length, 1);
    assert.match(reports[0], /ENOTDIR|not a directory/i);
    archive.append(WORKSPACE, event(2));
    await archive.stop();
    assert.equal(reports.length, 1, 'a stopped archive does not silently accept more events');
  } finally { await fs.rm(root, { recursive: true, force: true }); }
});

test('a capacity failure still settles the in-flight write and closes the file', async () => {
  const dir = await directory();
  const { archive, reports } = writer(dir);
  const chunk = 'x'.repeat(Math.ceil(HISTORY_ARCHIVE_QUEUE_BYTES / 2) + 1024);
  const large = event(1, { text: chunk });
  try {
    archive.append(WORKSPACE, large);
    archive.append(WORKSPACE, large);
    archive.append(WORKSPACE, large);
    assert.equal(archive.failed, true, 'pending writes above the local limit stop archiving');
    assert.equal(reports.length, 1);
    await archive.stop();
    const names = await fs.readdir(dir);
    assert.deepEqual(names, [archiveFileName(day(), 1)], 'the write already in flight is still completed');
    const [line] = await records(path.join(dir, names[0]));
    assert.equal(line.event.event.text.length, chunk.length, 'shutdown waited for the pending write');
  } finally { await fs.rm(dir, { recursive: true, force: true }); }
});

test('only this directory\'s archive files are excluded from capture', async () => {
  const dir = await directory();
  const { archive } = writer(dir);
  try {
    assert.equal(archive.excludes(path.join(dir, archiveFileName(day(), 1))), true);
    assert.equal(archive.excludes(path.join(dir, 'notes.txt')), false);
    assert.equal(archive.excludes(path.join(os.tmpdir(), 'elsewhere', archiveFileName(day(), 1))), false);
    assert.equal(archive.excludes(path.join(dir, '2026-09-21-session-1.jsonl')), false);
  } finally { await fs.rm(dir, { recursive: true, force: true }); }
});
