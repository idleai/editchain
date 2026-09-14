'use strict';
const { test } = require('node:test');
const assert = require('node:assert/strict');
const { observeEditorContext } = require('../../out/editorContext');
const settle = () => new Promise(resolve => setImmediate(resolve));

function timer(t) {
  let tick;
  const handle = { unref() {} };
  t.mock.method(global, 'setInterval', (callback, delay) => {
    assert.equal(delay, 15000); tick = callback; return handle;
  });
  const cleared = t.mock.method(global, 'clearInterval', id => assert.equal(id, handle));
  return { poll: () => tick(), cleared };
}

test('Git observation records context changes, clears stale anchors on failure, and recovers', async t => {
  const clock = timer(t);
  const events = [], logs = [];
  const first = [{ repository: '9007199254740993', root: '/workspace', head: 'a'.repeat(40) }];
  let answer = { Ok: { observed_ms: 1000, repositories: first } };
  const recorder = observeEditorContext({ context: event => events.push(event) }, async () => answer, message => logs.push(message));
  await settle();
  answer = { Ok: { observed_ms: 2000, repositories: first } };
  clock.poll(); await settle();
  assert.equal(events.length, 1, 'unchanged Git context does not create noise');
  assert.equal(events[0].repositories[0].repository, '9007199254740993', 'repository identity stays exact');
  answer = { Error: { message: 'repository temporarily unavailable' } };
  clock.poll(); await settle();
  clock.poll(); await settle();
  assert.equal(events.length, 2);
  assert.deepEqual(events[1].repositories, [], 'observation failure ends use of a stale baseline');
  assert.equal(logs.length, 2);
  answer = { Ok: { observed_ms: 4000, repositories: first } };
  clock.poll(); await settle();
  assert.equal(events.length, 3, 'recovery restores explicit context even at the same HEAD');
  recorder.dispose();
  assert.equal(clock.cleared.mock.callCount(), 1);
});

test('Git observation never overlaps requests or delivers a context after recorder disposal', async t => {
  const clock = timer(t);
  let resolve, calls = 0;
  const events = [];
  const recorder = observeEditorContext({ context: event => events.push(event) }, () => {
    calls++; return new Promise(done => { resolve = done; });
  }, () => {});
  clock.poll(); clock.poll();
  assert.equal(calls, 1);
  recorder.dispose();
  resolve({ Ok: { observed_ms: 1000, repositories: [] } });
  await settle();
  clock.poll();
  assert.equal(calls, 1);
  assert.equal(events.length, 0);
});
