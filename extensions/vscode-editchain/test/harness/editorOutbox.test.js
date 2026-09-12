'use strict';
const { test } = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs/promises');
const os = require('node:os');
const path = require('node:path');
const { EditorOutbox } = require('../../out/editorOutbox');

const event = sequence => ({ schema: 1, session: '11111111-1111-4111-8111-111111111111',
  sequence, time_ms: 1234, event: { type: sequence === 1 ? 'tracking_started' : 'tracking_stopped' } });
const ack = request => ({ Ok: { schema: 1, ack: request.RecordEditorEvents.events.map(event => [event.session, event.sequence]) } });

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

test('capacity pauses emit an explicit final gap without a sequence hole', async () => {
  const directory = await fs.mkdtemp(path.join(os.tmpdir(), 'editchain-outbox-'));
  const delivered = [];
  const outbox = new EditorOutbox(directory, '/workspace', '.editchain', async request => {
    delivered.push(...request.RecordEditorEvents.events); return ack(request);
  }, () => {});
  try {
    assert.equal(outbox.push(event(1)), true);
    const oversized = event(2); oversized.event.text = 'x'.repeat(3 * 1024 * 1024);
    assert.equal(outbox.push(oversized), false);
    assert.equal(outbox.push(event(3)), false);
    await outbox.flush();
    assert.deepEqual(delivered.map(event => event.sequence), [1, 2]);
    assert.equal(delivered[1].event.type, 'tracking_gap');
  } finally { await outbox.stop(); await fs.rm(directory, { recursive: true, force: true }); }
});
