'use strict';
const { test } = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs/promises');
const os = require('node:os');
const path = require('node:path');
const { unsignedIdentity, workspaceIdentity } = require('../../out/humanIdentity');

test('concurrent first starts and later reloads retain one durable unsigned GUID', async () => {
  const directory = await fs.mkdtemp(path.join(os.tmpdir(), 'editchain-identity-'));
  try {
    const values = await Promise.all(Array.from({ length: 16 }, () => unsignedIdentity(directory)));
    assert.equal(new Set(values).size, 1);
    assert.equal(await unsignedIdentity(directory), values[0]);
    assert.deepEqual(await fs.readdir(directory), ['unsigned-human-identity.json']);
    const original = workspaceIdentity(values[0], 'file:///workspace', '/workspace', '.editchain');
    assert.deepEqual(workspaceIdentity(values[0], 'file:///workspace', '/workspace', '/workspace/.editchain'), original);
    assert.notEqual(workspaceIdentity(values[0], 'file:///other', '/other', '.editchain').stream, original.stream);
    assert.notEqual(workspaceIdentity(values[0], 'file:///workspace', '/workspace', '.another-chain').stream, original.stream);
    assert.equal(original.kind, 'unsigned');
  } finally { await fs.rm(directory, { recursive: true, force: true }); }
});

test('invalid persisted identity fails visibly instead of silently creating another person', async () => {
  const directory = await fs.mkdtemp(path.join(os.tmpdir(), 'editchain-identity-'));
  try {
    const location = path.join(directory, 'unsigned-human-identity.json');
    for (const raw of ['{broken', JSON.stringify({ schema: 1, kind: 'unsigned', guid: 'not-a-guid' })]) {
      await fs.writeFile(location, raw);
      await assert.rejects(unsignedIdentity(directory));
      assert.equal(await fs.readFile(location, 'utf8'), raw);
    }
  } finally { await fs.rm(directory, { recursive: true, force: true }); }
});
