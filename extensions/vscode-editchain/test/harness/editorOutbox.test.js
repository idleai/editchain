'use strict';
const { test } = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs/promises');
const os = require('node:os');
const path = require('node:path');
const { EditorOutbox: ProductionOutbox } = require('../../out/editorOutbox');
class EditorOutbox extends ProductionOutbox {
  constructor(directory, workspace, chain, send, ...callbacks) {
    super(directory, workspace, chain, parts => send(JSON.parse(Buffer.concat(parts))), ...callbacks);
  }
}
const { MAX_EDITOR_EVENT_BYTES, MAX_EDITOR_BUFFER_BYTES } = require('../../out/editorLimits');
const { HistoryArchive } = require('../../out/historyArchive');

const event = sequence => ({ schema: 1, session: '11111111-1111-4111-8111-111111111111',
  sequence, time_ms: 1234, event: { type: sequence === 1 ? 'tracking_started' : 'tracking_stopped' } });
const ack = request => ({ Ok: { schema: 1, ack: request.RecordEditorEvents.events.map(event => [event.session, event.sequence]) } });

test('input and saves deliver without a report, and writer contention retries promptly', async () => {
  const directory = await fs.mkdtemp(path.join(os.tmpdir(), 'editchain-outbox-'));
  const delivered = [];
  let attempts = 0, notifications = 0;
  const outbox = new EditorOutbox(directory, '/workspace', '.editchain', async request => {
    if (++attempts === 1) throw new Error('operation would block');
    delivered.push(...request.RecordEditorEvents.events); return ack(request);
  }, () => {}, () => { notifications++; });
  try {
    const start = Date.now();
    outbox.push(event(1));
    outbox.push({ ...event(2), event: { type: 'document_saved', document: { id: 'buffer', version: 2 } } });
    while ((delivered.length < 2 || notifications < 1) && Date.now() - start < 500) await new Promise(resolve => setTimeout(resolve, 10));
    assert.deepEqual(delivered.map(value => value.sequence), [1, 2]);
    assert.equal(attempts, 2);
    assert.equal(notifications, 1, 'only acknowledged work wakes history');
  } finally { await outbox.stop(); await fs.rm(directory, { recursive: true, force: true }); }
});

test('a save arriving during acknowledgement is included in the requested flush', async () => {
  const directory = await fs.mkdtemp(path.join(os.tmpdir(), 'editchain-outbox-'));
  const delivered = [];
  const saved = { ...event(2), event: { type: 'document_saved', document: { id: 'buffer', version: 2 } } };
  const outbox = new EditorOutbox(directory, '/workspace', '.editchain', async request => {
    const events = request.RecordEditorEvents.events;
    delivered.push(...events);
    if (events.some(item => item.sequence === 1)) outbox.push(saved);
    return ack(request);
  }, () => {});
  try {
    outbox.push(event(1));
    assert.equal(await outbox.flush(), true, 'in-flight save must not produce a false pending/error report');
    assert.deepEqual(delivered.map(item => item.sequence), [1, 2]);
    assert.deepEqual(await fs.readdir(directory), []);
  } finally { await outbox.stop(); await fs.rm(directory, { recursive: true, force: true }); }
});

test('outbox preserves failed batches across recreation and verifies exact acknowledgement', async () => {
  const directory = await fs.mkdtemp(path.join(os.tmpdir(), 'editchain-outbox-'));
  let outbox;
  try {
    outbox = new EditorOutbox(directory, '/workspace', '.editchain', async () => { throw new Error('offline'); }, () => {});
    assert.equal(outbox.push(event(1)), true);
    assert.equal(await outbox.flush(), false);
    assert.equal((await fs.readdir(directory)).filter(name => name.endsWith('.json')).length, 1);
    await outbox.stop();
    outbox = new EditorOutbox(directory, '/workspace', '.editchain', async () => ({ Ok: { schema: 1, ack: [['wrong', 1]] } }), () => {});
    assert.equal(await outbox.flush(), false, 'an unrelated ack must not delete pending work');
    await outbox.stop();
    const delivered = [];
    outbox = new EditorOutbox(directory, '/workspace', '.editchain', async request => { delivered.push(request); return ack(request); }, () => {});
    assert.equal(outbox.push(event(2)), true);
    assert.equal(await outbox.flush(), true);
    assert.deepEqual(delivered.flatMap(request => request.RecordEditorEvents.events.map(event => event.sequence)), [1, 2]);
    assert.deepEqual(await fs.readdir(directory), []);
  } finally { await outbox?.stop(); await fs.rm(directory, { recursive: true, force: true }); }
});

test('journal short writes finish exactly and failed writes leave the observation pending', async () => {
  const directory = await fs.mkdtemp(path.join(os.tmpdir(), 'editchain-outbox-short-'));
  const open = fs.open;
  let stalled = true, writes = 0;
  fs.open = async (...args) => {
    const file = await open(...args);
    if (args[1] === 'wx') {
      const write = file.write.bind(file);
      file.write = async (buffer, offset, length) => {
        if (stalled) return { bytesWritten: 0 };
        writes++;
        return write(buffer, offset, Math.min(length, 37));
      };
    }
    return file;
  };
  const delivered = [];
  const outbox = new EditorOutbox(directory, '/workspace', '.editchain', async request => {
    const [name] = (await fs.readdir(directory)).filter(name => name.endsWith('.json'));
    const persisted = JSON.parse(await fs.readFile(path.join(directory, name), 'utf8'));
    assert.deepEqual(persisted.events, request.RecordEditorEvents.events, 'short writes preserve the actual journal bytes');
    delivered.push(...request.RecordEditorEvents.events); return ack(request);
  }, () => {});
  try {
    outbox.push(event(1));
    assert.equal(await outbox.flush(), false);
    assert.deepEqual(delivered, []);
    assert.deepEqual(await fs.readdir(directory), [], 'failed writes must not accumulate large temporary files');
    stalled = false;
    assert.equal(await outbox.flush(), true);
    assert.ok(writes > 1);
    assert.deepEqual(delivered, [event(1)]);
    assert.deepEqual(await fs.readdir(directory), []);
  } finally { stalled = false; fs.open = open; await outbox.stop(); await fs.rm(directory, { recursive: true, force: true }); }
});

test('the next durable batch reads during acknowledgement and a failed prefetch remains retryable', async () => {
  const directory = await fs.mkdtemp(path.join(os.tmpdir(), 'editchain-outbox-prefetch-'));
  const names = [1, 2, 3].map(sequence => `${event(sequence).session}-${String(sequence).padStart(16, '0')}.json`);
  for (const [index, name] of names.entries()) {
    await fs.writeFile(path.join(directory, name), JSON.stringify({ workspace_path: '/workspace', chain_dir: '.editchain', events: [event(index + 1)] }));
  }
  const readFile = fs.readFile, reads = [], delivered = [];
  let failing = true, release, entered, flush;
  const waiting = new Promise(resolve => { release = resolve; });
  const started = new Promise(resolve => { entered = resolve; });
  fs.readFile = async (file, ...args) => {
    if (path.dirname(file) === directory) {
      reads.push(path.basename(file));
      if (path.basename(file) === names[1] && failing) throw new Error('temporary journal read failure');
    }
    return readFile(file, ...args);
  };
  const outbox = new EditorOutbox(directory, '/workspace', '.editchain', async request => {
    delivered.push(...request.RecordEditorEvents.events.map(event => event.sequence));
    if (delivered.length === 1) { entered(); await waiting; }
    return ack(request);
  }, () => {});
  try {
    flush = outbox.flush();
    await started;
    await new Promise(resolve => setImmediate(resolve));
    assert.deepEqual(reads, names.slice(0, 2), 'only the next journal reads while the first acknowledgement waits');
    assert.deepEqual(delivered, [1]);
    assert.deepEqual((await fs.readdir(directory)).sort(), names, 'prefetch never acknowledges or removes evidence');
    release();
    assert.equal(await flush, false);
    assert.deepEqual((await fs.readdir(directory)).sort(), names.slice(1), 'a failed read keeps its journal and subsequent evidence');
    failing = false;
    assert.equal(await outbox.flush(), true);
    assert.deepEqual(delivered, [1, 2, 3]);
    assert.deepEqual(await fs.readdir(directory), []);
  } finally { failing = false; release(); await flush; fs.readFile = readFile; await outbox.stop(); await fs.rm(directory, { recursive: true, force: true }); }
});

test('new observations become durable while a service acknowledgement is pending', async () => {
  const directory = await fs.mkdtemp(path.join(os.tmpdir(), 'editchain-outbox-'));
  let release;
  const pending = new Promise(resolve => { release = resolve; });
  let entered;
  const started = new Promise(resolve => { entered = resolve; });
  const outbox = new EditorOutbox(directory, '/workspace', '.editchain', async request => {
    entered(); await pending; return ack(request);
  }, () => {});
  try {
    outbox.push(event(1));
    const first = outbox.flush();
    await started;
    outbox.push(event(2));
    const second = outbox.flush();
    let files = [];
    for (let i = 0; i < 100; i++) {
      files = (await fs.readdir(directory)).filter(name => name.endsWith('.json'));
      if (files.length === 2) break;
      await new Promise(resolve => setTimeout(resolve, 10));
    }
    assert.equal(files.length, 2, 'disk persistence must not wait for the service');
    release();
    await Promise.all([first, second]);
    await outbox.flush();
    assert.deepEqual(await fs.readdir(directory), []);
  } finally { release(); await outbox.stop(); await fs.rm(directory, { recursive: true, force: true }); }
});

test('continuing input cannot starve delivery behind a later disk write', async () => {
  const directory = await fs.mkdtemp(path.join(os.tmpdir(), 'editchain-outbox-persisting-'));
  const delivered = [];
  let release, entered;
  const waiting = new Promise(resolve => { release = resolve; });
  const writing = new Promise(resolve => { entered = resolve; });
  const outbox = new EditorOutbox(directory, '/workspace', '.editchain', async request => {
    delivered.push(...request.RecordEditorEvents.events.map(event => event.sequence)); return ack(request);
  }, () => {});
  const publish = outbox.publish.bind(outbox);
  outbox.publish = async (destination, content) => {
    if (JSON.parse(content).events[0].sequence === 2) { entered(); await waiting; }
    await publish(destination, content);
    if (JSON.parse(content).events[0].sequence === 1) outbox.push(event(2));
  };
  let flush;
  try {
    outbox.push(event(1));
    flush = outbox.flush();
    await writing;
    const started = Date.now();
    while (!delivered.length && Date.now() - started < 500) await new Promise(resolve => setTimeout(resolve, 10));
    assert.deepEqual(delivered, [1], 'the earlier durable event delivers while the newer snapshot write is blocked');
    release(); await flush;
    assert.equal(await outbox.flush(), true);
    assert.deepEqual(delivered, [1, 2]);
  } finally { release(); await flush; await outbox.stop(); await fs.rm(directory, { recursive: true, force: true }); }
});

test('capacity pauses emit an explicit final gap without a sequence hole', async () => {
  const directory = await fs.mkdtemp(path.join(os.tmpdir(), 'editchain-outbox-'));
  const delivered = [];
  const outbox = new EditorOutbox(directory, '/workspace', '.editchain', async request => {
    delivered.push(...request.RecordEditorEvents.events); return ack(request);
  }, () => {});
  try {
    assert.equal(outbox.push(event(1)), true);
    const oversized = event(2); oversized.event.text = 'x'.repeat(MAX_EDITOR_EVENT_BYTES);
    assert.equal(outbox.push(oversized), false);
    assert.equal(outbox.push(event(3)), false);
    await outbox.flush();
    assert.deepEqual(delivered.map(event => event.sequence), [1, 2]);
    assert.equal(delivered[1].event.type, 'tracking_gap');
  } finally { await outbox.stop(); await fs.rm(directory, { recursive: true, force: true }); }
});

test('the first large batch delivers before the rest of the same persistence pass', async () => {
  const directory = await fs.mkdtemp(path.join(os.tmpdir(), 'editchain-outbox-pipeline-'));
  let release, entered;
  const waiting = new Promise(resolve => { release = resolve; });
  const writing = new Promise(resolve => { entered = resolve; });
  const delivered = [];
  const outbox = new EditorOutbox(directory, '/workspace', '.editchain', async request => {
    delivered.push(...request.RecordEditorEvents.events.map(event => event.sequence)); return ack(request);
  }, () => {});
  const publish = outbox.publish.bind(outbox);
  outbox.publish = async (destination, content) => {
    if (JSON.parse(content).events[0].sequence === 2) { entered(); await waiting; }
    await publish(destination, content);
  };
  let flush;
  try {
    outbox.push({ ...event(1), event: { type: 'document_snapshot', text: 'x'.repeat(5 * 1024 * 1024) } });
    outbox.push(event(2));
    flush = outbox.flush();
    await writing;
    const started = Date.now();
    while (!delivered.length && Date.now() - started < 500) await new Promise(resolve => setTimeout(resolve, 10));
    assert.deepEqual(delivered, [1], 'delivery must not await the last snapshot in the persistence pass');
    release(); await flush;
    assert.deepEqual(delivered, [1, 2]);
  } finally { release(); await flush; await outbox.stop(); await fs.rm(directory, { recursive: true, force: true }); }
});

test('large snapshots retain exact contents and ordered retries in their own batches', async () => {
  const directory = await fs.mkdtemp(path.join(os.tmpdir(), 'editchain-outbox-large-'));
  const before = 'export const emoji = "😀";\n'.repeat(40000);
  const after = before.replace('emoji', 'human');
  const change = { ...event(2), event: { type: 'document_changed', before, after,
    changes: [{ offset: 13, length: 5, text: 'human' }] } };
  let serializations = 0;
  change.toJSON = () => { serializations++; return { ...event(2), event: change.event }; };
  let outbox;
  const delivered = [];
  try {
    outbox = new EditorOutbox(directory, '/workspace', '.editchain', async () => { throw new Error('offline'); }, () => {});
    assert.equal(outbox.push(event(1)), true);
    assert.equal(outbox.push(change), true);
    // An emitted observation is immutable even if the producer's object changes.
    change.event = { type: 'tracking_stopped' };
    assert.equal(outbox.push(event(3)), true);
    assert.equal(await outbox.flush(), false);
    await outbox.stop();
    assert.equal(serializations, 1, 'snapshot sizing and persistence reuse one encoding');
    outbox = new EditorOutbox(directory, '/workspace', '.editchain', async request => {
      delivered.push(request.RecordEditorEvents.events); return ack(request);
    }, () => {});
    assert.equal(await outbox.flush(), true);
    const events = delivered.flat();
    assert.deepEqual(events.map(event => event.sequence), [1, 2, 3]);
    assert.equal(events[1].event.before, before);
    assert.equal(events[1].event.after, after);
    assert.deepEqual(await fs.readdir(directory), []);
  } finally { await outbox?.stop(); await fs.rm(directory, { recursive: true, force: true }); }
});

test('a maximum-size escaped full replacement is delivered intact beyond the normal batch target', async () => {
  const directory = await fs.mkdtemp(path.join(os.tmpdir(), 'editchain-outbox-escaped-'));
  // A NUL-free control character uses six JSON bytes per UTF-8 byte.
  const before = '\u0001'.repeat(MAX_EDITOR_BUFFER_BYTES);
  const after = '\u0002'.repeat(MAX_EDITOR_BUFFER_BYTES);
  const batches = [];
  const outbox = new EditorOutbox(directory, '/workspace', '.editchain', async request => {
    batches.push(request.RecordEditorEvents.events.map(event => event.sequence));
    const change = request.RecordEditorEvents.events.find(event => event.sequence === 2);
    if (change) {
      assert.equal(change.event.before, before);
      assert.equal(change.event.after, after);
      assert.equal(change.event.changes[0].text, after);
    }
    return ack(request);
  }, () => {});
  try {
    assert.equal(outbox.push(event(1)), true);
    assert.equal(outbox.push({ ...event(2), event: { type: 'document_changed', before, after,
      changes: [{ offset: 0, length: before.length, text: after }] } }), true);
    assert.equal(outbox.push(event(3)), true);
    assert.equal(await outbox.flush(), true);
    assert.deepEqual(batches, [[1], [2], [3]], 'an oversized batch member neither splits nor blocks the following event');
  } finally { await outbox.stop(); await fs.rm(directory, { recursive: true, force: true }); }
});

test('the archive observes the admitted event, so a capacity gap cannot diverge from the chain', async () => {
  const directory = await fs.mkdtemp(path.join(os.tmpdir(), 'editchain-outbox-observe-'));
  const archives = await fs.mkdtemp(path.join(os.tmpdir(), 'editchain-outbox-observe-archive-'));
  const delivered = [], reports = [];
  const archive = new HistoryArchive({ directory: archives, log: () => {}, report: message => reports.push(message) });
  const outbox = new EditorOutbox(directory, '/workspace', '.editchain', async request => {
    delivered.push(...request.RecordEditorEvents.events); return ack(request);
  }, () => {}, () => {}, () => {}, (workspace, event) => archive.append(workspace, event));
  try {
    const changed = { ...event(1), event: { type: 'document_changed', document: { id: '1', uri: 'file:///workspace/a.ts', path: 'a.ts', version: 2 },
      before_version: 1, before: 'one\ntwo\n', after: 'one\nhuman\n', changes: [{ offset: 4, length: 3, text: 'human' }] } };
    assert.equal(outbox.push(changed), true);
    const oversized = event(2);
    oversized.event.text = 'x'.repeat(MAX_EDITOR_EVENT_BYTES);
    assert.equal(outbox.push(oversized), false, 'capacity refuses the event it cannot retain');
    assert.equal(outbox.push(event(3)), false, 'capture stays stopped after the capacity pause');
    await outbox.flush();
    await outbox.stop();
    await archive.stop();
    assert.deepEqual(delivered.map(item => item.sequence), [1, 2]);
    assert.equal(delivered[1].event.type, 'tracking_gap');
    assert.equal(delivered[1].session, oversized.session);
    assert.equal(delivered[1].sequence, oversized.sequence);
    assert.equal(reports.length, 0);
    const names = (await fs.readdir(archives)).filter(name => name.endsWith('.jsonl'));
    assert.equal(names.length, 1);
    const lines = (await fs.readFile(path.join(archives, names[0]), 'utf8')).trim().split('\n').map(line => JSON.parse(line));
    assert.deepEqual(lines.map(line => line.event), delivered,
      'the archived payload is exactly the payload the chain receives, gap included');
    assert.deepEqual(lines.map(line => line.workspace_path), ['/workspace', '/workspace']);
    assert.equal(lines[1].event.event.type, 'tracking_gap');
    assert.equal(lines.length, 2, 'nothing beyond the admitted events is archived');
  } finally {
    await outbox.stop(); await archive.stop();
    await fs.rm(directory, { recursive: true, force: true });
    await fs.rm(archives, { recursive: true, force: true });
  }
});

test('replaying a durable journal re-delivers without re-observing the event', async () => {
  const directory = await fs.mkdtemp(path.join(os.tmpdir(), 'editchain-outbox-replay-observe-'));
  const observed = [];
  const observe = (_workspace, event) => observed.push([event.session, event.sequence]);
  let outbox = new EditorOutbox(directory, '/workspace', '.editchain',
    async () => { throw new Error('offline'); }, () => {}, () => {}, () => {}, observe);
  try {
    assert.equal(outbox.push(event(1)), true);
    assert.equal(await outbox.flush(), false);
    assert.deepEqual(observed, [[event(1).session, 1]]);
    await outbox.stop();
    const delivered = [];
    outbox = new EditorOutbox(directory, '/workspace', '.editchain',
      async request => { delivered.push(...request.RecordEditorEvents.events); return ack(request); },
      () => {}, () => {}, () => {}, observe);
    assert.equal(await outbox.flush(), true);
    assert.deepEqual(delivered.map(item => item.sequence), [1], 'the durable journal is replayed to the service');
    assert.deepEqual(observed, [[event(1).session, 1]], 'a replayed batch is not a new observation');
  } finally { await outbox.stop(); await fs.rm(directory, { recursive: true, force: true }); }
});
