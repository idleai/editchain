'use strict';

const { test } = require('node:test');
const assert = require('node:assert/strict');
const { FrameDecoder } = require('../../out/frameDecoder');

function frame(payload) {
  const header = Buffer.alloc(4);
  header.writeUInt32LE(payload.length);
  return Buffer.concat([header, payload]);
}

test('frames survive arbitrary boundaries, empty payloads, and coalesced messages', () => {
  const payloads = [Buffer.from('a→b'), Buffer.alloc(0), Buffer.from('{"revision":2}')];
  const stream = Buffer.concat(payloads.map(frame));
  for (let split = 0; split <= stream.length; split++) {
    const decoder = new FrameDecoder();
    assert.deepEqual([...decoder.push(stream.subarray(0, split)), ...decoder.push(stream.subarray(split))], payloads);
  }
  const decoder = new FrameDecoder();
  assert.deepEqual(Array.from(stream).flatMap(byte => [...decoder.push(Buffer.from([byte]))]), payloads);
});

test('large fragmented snapshots copy at most one frame worth of bytes', () => {
  const payload = Buffer.from(JSON.stringify({ blocks: 'session→git'.repeat(60000) }));
  const stream = frame(payload);
  const decoder = new FrameDecoder();
  const messages = [];
  // Count actual copies rather than imposing a machine-dependent timing limit.
  const copy = Buffer.prototype.copy;
  const concat = Buffer.concat;
  let copied = 0;
  Buffer.prototype.copy = function (...args) {
    const count = copy.apply(this, args);
    copied += count;
    return count;
  };
  Buffer.concat = function (buffers, length) {
    const result = concat.call(this, buffers, length);
    copied += result.length;
    return result;
  };
  try {
    for (let offset = 0; offset < stream.length; offset += 1023) {
      messages.push(...decoder.push(stream.subarray(offset, offset + 1023)));
      if (offset + 1023 < stream.length) assert.equal(messages.length, 0);
    }
  } finally {
    Buffer.prototype.copy = copy;
    Buffer.concat = concat;
  }
  assert.deepEqual(messages, [payload]);
  assert.ok(copied <= stream.length, `${copied} bytes copied for ${stream.length} input bytes`);
});
