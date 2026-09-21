'use strict';
const { test } = require('node:test');
const assert = require('node:assert/strict');
const Module = require('node:module');

function harness() {
  const requests = [], names = [];
  let changed, disposed = false;
  const vscode = {
    workspace: { isTrusted: true },
    authentication: {
      onDidChangeSessions: listener => { changed = listener; return { dispose() { disposed = true; } }; },
      getSession: (provider, scopes, options) => new Promise((resolve, reject) => requests.push({ provider, scopes, options, resolve, reject })),
    },
  };
  const filename = require.resolve('../../out/humanAccount');
  delete require.cache[filename];
  const load = Module._load;
  Module._load = function(name, ...args) { return name === 'vscode' ? vscode : load.call(this, name, ...args); };
  let account;
  try { account = new (require(filename).HumanAccount)(name => names.push(name)); }
  finally { Module._load = load; }
  return { account, requests, names, vscode, disposed: () => disposed,
    changed: () => changed({ provider: { id: 'github' } }) };
}

test('silent profile lookup cannot overwrite an explicit choice, and sign-out clears the name', async () => {
  const env = harness();
  try {
    assert.deepEqual(env.requests[0].options, { silent: true });
    env.account.use({ id: 'chosen', label: ' ambientlight ' });
    env.requests[0].resolve({ account: { id: 'stale', label: 'stale-name' }, accessToken: 'never-record-this' });
    await Promise.resolve();
    assert.deepEqual(env.names, ['ambientlight']);
    env.changed();
    env.requests[1].resolve(undefined);
    await Promise.resolve();
    assert.deepEqual(env.names, ['ambientlight', undefined]);
    env.changed();
    env.account.dispose();
    env.requests[2].resolve({ account: { id: 'late', label: 'late-name' } });
    await Promise.resolve();
    assert.equal(env.disposed(), true);
    assert.equal(env.account.name, undefined);
  } finally { env.account.dispose(); }
});

test('profile errors and unusable names leave capture anonymous', async () => {
  const env = harness();
  try {
    env.requests[0].reject(new Error('private provider error'));
    await Promise.resolve();
    assert.equal(env.account.name, undefined);
    for (const label of ['', '  ', 'line\nbreak', 'a'.repeat(81), 'a\u0085b']) {
      env.account.use({ id: 'user', label });
      assert.equal(env.account.name, undefined);
    }
    env.account.use({ id: 'user', label: 'Zoë 🦀 <user>' });
    assert.equal(env.account.name, 'Zoë 🦀 <user>');
    env.vscode.workspace.isTrusted = false;
    await env.account.refresh();
    assert.equal(env.requests.length, 1);
    assert.equal(env.account.name, undefined);
  } finally { env.account.dispose(); }
});
