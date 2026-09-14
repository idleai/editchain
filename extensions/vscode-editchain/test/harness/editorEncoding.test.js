'use strict';
const { test } = require('node:test');
const assert = require('node:assert/strict');
const { EditorEncoding } = require('../../out/editorEncoding');

// Deliberately put event first; JSON field order must not affect the encoder.
const observation = event => ({ event, schema: 1, session: 'recorder', sequence: 1, time_ms: 1234 });

test('incremental full-snapshot JSON matches complete serialization at every UTF-16 boundary', () => {
  const before = 'a😀"\\\n\r\t\u0001🦀éz';
  const encoding = new EditorEncoding();
  for (let offset = 0; offset <= before.length; offset++) {
    for (let end = offset; end <= before.length; end++) {
      for (const text of ['', 'X', '🙂', '\u0001"\\\r\n', '\ud800', '\udc00']) {
        const baseline = observation({ type: 'document_snapshot', text: before });
        assert.deepEqual(JSON.parse(encoding.encode(baseline)), baseline);
        const after = before.slice(0, offset) + text + before.slice(end);
        const event = observation({ type: 'document_changed', before, after,
          changes: [{ offset, length: end - offset, text }], document: { version: 2 } });
        assert.deepEqual(JSON.parse(encoding.encode(event)), event, `replacement ${offset}..${end}`);
      }
    }
  }
});

test('snapshots remain independent across consecutive edits, document changes, and fallback encoding', () => {
  const encoding = new EditorEncoding();
  let before = 'prefix "😀"\n' + 'unchanged source\n'.repeat(100000) + 'end';
  const baseline = observation({ type: 'document_snapshot', text: before });
  encoding.encode(baseline);
  const retained = [];
  for (const offset of [0, 10, Math.floor(before.length / 2), before.length]) {
    const text = 'human 🦀\n';
    const after = before.slice(0, offset) + text + before.slice(offset);
    const event = observation({ type: 'document_changed', before, after, changes: [{ offset, length: 0, text }] });
    retained.push([encoding.encode(event), event]);
    before = after;
  }
  // A mismatched delta must preserve the supplied snapshot for native validation.
  for (const changes of [[{ offset: 0, length: 0, text: 'wrong' }], [], [{ offset: -1, length: 0, text: '' }]]) {
    const event = observation({ type: 'document_changed', before: 'another document', after: 'exact after', changes });
    retained.push([encoding.encode(event), event]);
  }
  for (const [bytes, event] of retained) assert.deepEqual(JSON.parse(bytes), event);
});
