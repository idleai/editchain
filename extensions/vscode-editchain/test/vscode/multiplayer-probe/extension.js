'use strict';
// Disposable test extension, excluded from the VSIX. Never modifies production
// commands or their approval dialogs. Clipboard secrets stay inside VS Code.
const vscode = require('vscode');
const fs = require('node:fs');
const path = require('node:path');
const { pathToFileURL } = require('node:url');

exports.activate = context => {
  const root = process.env.EDITCHAIN_MULTIPLAYER_UI_FIXTURE;
  if (!root) throw new Error('Multiplayer fixture directory is required');
  // VS Code refuses modal prompts when extensionTestsPath is set. Run the
  // existing automation bridge from this helper in an ordinary app window.
  const proxy = process.env.EDITCHAIN_MULTIPLAYER_UI_PROXY;
  if (!proxy) throw new Error('Multiplayer automation bridge is required');
  void import(pathToFileURL(proxy).href).then(module => module.run(vscode)).catch(() => {
    console.error('Multiplayer automation bridge failed to start');
  });
  const events = new vscode.EventEmitter();
  // Distinct display metadata exercises attribution; the real relay token is still same-account.
  const session = scopes => ({ id: 'relay-fixture', account: { id: 'relay-fixture-account', label: process.env.EDITCHAIN_MULTIPLAYER_UI_ROLE + '-user' },
    scopes: [...(scopes || ['read:user', 'read:org'])], accessToken: fs.readFileSync(path.join(root, 'github-token'), 'utf8') });
  context.subscriptions.push(events, vscode.authentication.registerAuthenticationProvider('github', 'GitHub relay test fixture', {
    onDidChangeSessions: events.event,
    getSessions: async scopes => [session(scopes)], createSession: async scopes => session(scopes), removeSession: async () => {},
  }, { supportsMultipleAccounts: false }));
  let result;
  context.subscriptions.push(vscode.commands.registerCommand('editchain-multiplayer-test.run', name => {
    if (!['multiplayerHost', 'multiplayerJoin', 'multiplayerScope'].includes(name)) throw new Error('Unsupported fixture command');
    result = undefined;
    void vscode.commands.executeCommand('editchain-history.' + name).then(value => {
      result = { ok: value?.ok === true, message: value?.message };
    }, () => { result = { ok: false, message: 'Command rejected' }; });
    return true;
  }));
  context.subscriptions.push(vscode.commands.registerCommand('editchain-multiplayer-test.result', () => result));
  context.subscriptions.push(vscode.commands.registerCommand('editchain-multiplayer-test.clipboard', async (name, write) => {
    if (!['request', 'invitation'].includes(name)) throw new Error('Unsupported exchange file');
    const file = path.join(root, name);
    if (write) fs.writeFileSync(file, await vscode.env.clipboard.readText(), { mode: 0o600 });
    else await vscode.env.clipboard.writeText(fs.readFileSync(file, 'utf8'));
    return true;
  }));
};
