import * as vscode from 'vscode';
import * as path from 'node:path';
import { existsSync } from 'node:fs';
import { randomUUID } from 'node:crypto';
import { resolveServicePath } from '../stdioClient';
import type { MultiplayerManager, SharingStatus } from './manager';
import { NativePeerError } from './native';
import { ProbeError } from '../devTunnels/probe';

class CommandError extends Error {}

const PENDING = 'editchain.multiplayer.pending.';
const SPACE = 'editchain.multiplayer.space.';
const SCOPES = ['read:user', 'read:org'];

export type MultiplayerCommands = { stop(): Promise<void> };

/** Registration has no account, network, or native-process side effects. */
export function registerMultiplayerCommands(context: vscode.ExtensionContext, received: () => void): MultiplayerCommands {
  let manager: MultiplayerManager | undefined;
  let folder: vscode.WorkspaceFolder | undefined;
  let output: vscode.OutputChannel | undefined;
  let status: vscode.StatusBarItem | undefined;
  let account: vscode.AuthenticationSession | undefined;
  let active = false;
  let stopVersion = 0;
  const owner = randomUUID();
  const ownedMarkers = new Set<string>();
  let leaseTimer: NodeJS.Timeout | undefined;
  let journalTail = Promise.resolve();
  type JournalRecord = { account: string; workspace?: string; owner: string; leaseUntil: number };
  const journalWrite = (action: () => Promise<void>) => {
    const work = journalTail.then(action);
    journalTail = work.catch(() => {});
    return work;
  };

  const update = (value: SharingStatus, durableChange: boolean) => {
    status ??= vscode.window.createStatusBarItem(vscode.StatusBarAlignment.Left, 99);
    status.command = 'editchain-history.multiplayerStatus';
    const live = value.peers.filter(peer => peer.state === 'Live').length;
    status.text = value.hosting || value.peers.length ? `$(broadcast) Sharing · ${live}/${value.peers.length} live` : '$(broadcast) Sharing stopped';
    status.tooltip = value.message || value.peers.map(peer => `${peer.fingerprint?.slice(0, 12) || 'Device'}: ${peer.state}`).join('\n');
    status.show();
    if (durableChange) received();
  };
  const journal = {
    remember: async (marker: string) => {
      if (!account) throw new CommandError('GitHub host session is unavailable.');
      const record: JournalRecord = { account: account.account.id, workspace: folder?.uri.toString(), owner, leaseUntil: Date.now() + 90_000 };
      await journalWrite(async () => { await context.globalState.update(PENDING + marker, record); ownedMarkers.add(marker); });
      leaseTimer ??= setInterval(() => {
        void journalWrite(async () => {
          for (const marker of ownedMarkers) {
            const current = context.globalState.get<JournalRecord>(PENDING + marker);
            if (current?.owner === owner) await context.globalState.update(PENDING + marker, { ...current, leaseUntil: Date.now() + 90_000 });
          }
        }).catch(() => {});
      }, 30_000);
    },
    forget: async (marker: string) => {
      await journalWrite(async () => { ownedMarkers.delete(marker); await context.globalState.update(PENDING + marker, undefined); });
      if (!ownedMarkers.size) { clearInterval(leaseTimer); leaseTimer = undefined; }
    },
  };

  const getManager = async (): Promise<MultiplayerManager> => {
    if (!vscode.workspace.isTrusted) throw new CommandError('Trust this workspace before enabling history sharing.');
    if (manager) return manager;
    const folders = vscode.workspace.workspaceFolders ?? [];
    folder = folders.length === 1 ? folders[0] : (await vscode.window.showQuickPick(folders.map(value => ({ label: value.name, description: value.uri.fsPath, folder: value })), { title: 'Choose the workspace history to share' }))?.folder;
    if (!folder || folder.uri.scheme !== 'file') throw new CommandError('Choose a local workspace folder for sharing.');
    const configuration = vscode.workspace.getConfiguration('editchain-history', folder.uri);
    const chain = path.resolve(folder.uri.fsPath, configuration.get<string>('chainDir', '.editchain'));
    const suffix = process.platform === 'win32' ? '.exe' : '';
    const sibling = path.join(path.dirname(resolveServicePath()), `editchain-peer${suffix}`);
    const bundled = context.asAbsolutePath(path.join('bin', `${process.platform}-${process.arch}`, `editchain-peer${suffix}`));
    const binary = configuration.get<string>('peerPath', '') || (existsSync(sibling) ? sibling : bundled);
    const key = SPACE + folder.uri.toString();
    const { MultiplayerManager } = await import('./manager');
    manager = new MultiplayerManager({ binary, chain,
      // Device credentials live in private application storage, never the workspace or VSIX.
      deviceDirectory: path.join(context.globalStorageUri.fsPath, 'multiplayer-device'),
      space: context.workspaceState.get<string>(key), saveSpace: async space => { await context.workspaceState.update(key, space); }, journal,
      githubToken: async () => {
        const current = await vscode.authentication.getSession('github', SCOPES, { silent: true });
        if (!current || !account || current.account.id !== account.account.id) throw new CommandError('GitHub host session changed. Start hosting again.');
        return current.accessToken;
      }, changed: update });
    return manager;
  };

  const backfill = async (): Promise<boolean | undefined> => {
    const choice = await vscode.window.showQuickPick([
      { label: 'Share records added from now on', detail: 'Existing records stay private unless they are already shared in this space.', value: false },
      { label: 'Include existing history', detail: 'Share all retained operations and their referenced content in this workspace history.', value: true },
    ], { title: `Share history from ${folder?.name || 'this workspace'}` });
    return choice?.value;
  };

  const execute = async (action: () => Promise<unknown>) => {
    if (active) return { ok: false, message: 'A multiplayer command is already running.' };
    active = true;
    output ??= vscode.window.createOutputChannel('EditChain Multiplayer');
    try { const value = await action(); return { ok: true, value }; }
    catch (error) {
      // Only UI and controlled adapter errors escape the manager.
      const message = error instanceof CommandError || error instanceof ProbeError || error instanceof NativePeerError
        ? error.message : 'Multiplayer command failed. Account and service details were omitted.';
      output.appendLine(message);
      void vscode.window.showErrorMessage(`EditChain Multiplayer: ${message}`);
      return { ok: false, message };
    } finally { active = false; }
  };
  const stop = async () => { stopVersion++; await manager?.stop(); };
  const command = (name: string, action: () => Promise<unknown>) => vscode.commands.registerCommand(`editchain-history.${name}`, () => execute(action));

  context.subscriptions.push(
    command('multiplayerRequest', async () => {
      const current = await getManager();
      await vscode.env.clipboard.writeText(await current.joinRequest());
      void vscode.window.showInformationMessage('Join request copied. Give it to the person hosting the shared history.');
    }),
    command('multiplayerHost', async () => {
      const version = stopVersion;
      const current = await getManager();
      const text = await vscode.window.showInputBox({ title: 'Host shared history', prompt: 'Paste the joining device’s EditChain join request', ignoreFocusOut: true });
      if (!text) return;
      const request = await current.inspectRequest(text);
      const include = await backfill(); if (include === undefined) return;
      const approved = await vscode.window.showWarningMessage(`Approve device ${request.device.fingerprint} to exchange history with ${folder!.name}?`, { modal: true }, 'Approve device');
      if (approved !== 'Approve device') return;
      account = await vscode.authentication.getSession('github', SCOPES, { createIfNone: true });
      if (version !== stopVersion) return;
      const invitation = await current.hostHistory(text, include);
      await vscode.env.clipboard.writeText(invitation);
      void vscode.window.showInformationMessage('Private invitation copied. Give it to the approved device. It expires in at most one hour.');
    }),
    command('multiplayerJoin', async () => {
      const version = stopVersion;
      const current = await getManager();
      const text = await vscode.window.showInputBox({ title: 'Join shared history', prompt: 'Paste the host’s private EditChain invitation', password: true, ignoreFocusOut: true });
      if (!text) return;
      const invitation = await current.inspectInvitation(text);
      const include = await backfill(); if (include === undefined) return;
      const approved = await vscode.window.showWarningMessage(`Join space ${invitation.space} with host device ${invitation.host.fingerprint}?`, { modal: true }, 'Join space');
      if (approved !== 'Join space') return;
      if (version !== stopVersion) return;
      await current.joinHistory(text, include);
    }),
    command('multiplayerStatus', async () => {
      const value = manager?.status() ?? { hosting: false, peers: [], message: 'Sharing is disabled.' };
      output!.show(true);
      output!.appendLine(JSON.stringify(value, null, 2));
      return value;
    }),
    command('multiplayerRemove', async () => {
      const current = await getManager();
      const device = await vscode.window.showQuickPick((await current.devices()).map(device => ({ label: device.fingerprint, device })), { title: 'Remove an approved device from this replica' });
      if (device) await current.revoke(device.device.fingerprint);
    }),
    // Stop is always available even while a sign-in or connection command waits.
    vscode.commands.registerCommand('editchain-history.multiplayerStop', () => stop().then(() => ({ ok: true }), () => {
      void vscode.window.showErrorMessage('Sharing stopped; run EditChain: Clean Up Multiplayer Tunnels to retry cleanup.');
      return { ok: false };
    })),
    command('multiplayerCleanup', async () => {
      if (manager?.status().hosting) throw new CommandError('Stop hosting in this window before cleaning up its tunnels.');
      account = await vscode.authentication.getSession('github', SCOPES, { createIfNone: true });
      const { managementClient, cleanupRelay } = await import('./relay');
      const management = managementClient(async () => account!.accessToken);
      try {
        const workspaces = new Set(vscode.workspace.workspaceFolders?.map(folder => folder.uri.toString()));
        const keys = context.globalState.keys().filter(key => {
          if (!key.startsWith(PENDING)) return false;
          const entry = context.globalState.get<JournalRecord>(key);
          return entry?.account === account!.account.id && workspaces.has(entry.workspace || '') &&
            (entry.owner === owner || entry.leaseUntil < Date.now());
        });
        for (const key of keys) await cleanupRelay(management, key.slice(PENDING.length), journal);
        void vscode.window.showInformationMessage('Inactive pending multiplayer tunnels cleaned up for this workspace.');
      } finally { await management.dispose(); }
    }),
    { dispose: () => { void stop().catch(() => {}); clearInterval(leaseTimer); status?.dispose(); output?.dispose(); } },
  );
  const reset = () => {
    const previous = manager;
    manager = undefined;
    folder = undefined;
    stopVersion++;
    void previous?.stop().catch(() => {});
  };
  context.subscriptions.push(
    vscode.workspace.onDidChangeWorkspaceFolders(reset),
    vscode.workspace.onDidChangeConfiguration(event => {
      if (['chainDir', 'servicePath', 'peerPath'].some(key => event.affectsConfiguration(`editchain-history.${key}`))) reset();
    }),
  );
  if (vscode.authentication.onDidChangeSessions) context.subscriptions.push(vscode.authentication.onDidChangeSessions(event => {
    if (event.provider.id !== 'github' || !account) return;
    void Promise.resolve(vscode.authentication.getSession('github', SCOPES, { silent: true })).then(current => {
      if (!current || current.account.id !== account?.account.id) return stop();
    }, () => stop()).catch(() => {});
  }));
  return { stop };
}
