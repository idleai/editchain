'use strict';
const { test } = require('node:test');
const assert = require('node:assert/strict');

test('local editor-origin enablement preserves JSONC comments, other settings, and existing opt-ins', async () => {
  const { withEditorOrigins } = await import('../../scripts/runtime-arguments.mjs');
  const original = '{\n  // existing preference\n  "disable-hardware-acceleration": true,\n  "enable-proposed-api": ["another.extension"],\n}\n';
  const result = withEditorOrigins(original, 'ambientlight.editchain-history');
  assert.ok(result.includes('// existing preference'));
  assert.ok(result.includes('"disable-hardware-acceleration": true'));
  const { parse } = require('jsonc-parser');
  assert.deepEqual(parse(result)['enable-proposed-api'], ['another.extension', 'ambientlight.editchain-history']);
  assert.equal(withEditorOrigins(result, 'ambientlight.editchain-history'), result);
  assert.deepEqual(parse(withEditorOrigins('{"enable-proposed-api":[]}', 'ambientlight.editchain-history'))['enable-proposed-api'],
    ['ambientlight.editchain-history'], 'argv.json needs an explicit extension ID, including when the list was empty');
  assert.throws(() => withEditorOrigins('{broken', 'ambientlight.editchain-history'), /valid JSONC/);
  assert.throws(() => withEditorOrigins('{"enable-proposed-api":true}', 'ambientlight.editchain-history'), /Invalid/);
});
