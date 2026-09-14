'use strict';

const { test } = require('node:test');
const assert = require('node:assert/strict');
const { LiveQueue } = require('../../out/liveQueue');

test('interactions pass queued background work while preserving the active revision and every toggle', async () => {
  const queue = new LiveQueue();
  const order = [];
  let release;
  const active = queue.enqueue(() => new Promise(resolve => { release = resolve; order.push('active'); }));
  const background = queue.enqueue(async () => { order.push('background'); });
  const first = queue.enqueue(async () => { order.push('open'); }, true);
  const second = queue.enqueue(async () => { order.push('close'); }, true);
  assert.deepEqual(order, ['active']);
  release();
  await Promise.all([active, background, first, second]);
  assert.deepEqual(order, ['active', 'open', 'close', 'background']);
});

test('a failed action rejects its caller and the next queued operation still runs', async () => {
  const queue = new LiveQueue();
  const failed = queue.enqueue(async () => { throw new Error('failed native request'); }, true);
  const next = queue.enqueue(async () => 'next revision', true);
  await assert.rejects(failed, /failed native request/);
  assert.equal(await next, 'next revision');
});
