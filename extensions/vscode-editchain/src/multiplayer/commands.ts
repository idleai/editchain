import * as vscode from 'vscode';
import * as path from 'node:path';
import { existsSync } from 'node:fs';
import { randomUUID } from 'node:crypto';
import { resolveServicePath } from '../stdioClient';
import type { MultiplayerManager, SavedSharing, SharingStatus } from './manager';
import { NativePeerError } from './native';
import { ProbeError } from '../devTunnels/probe';
import type { DirectorySync, DiscoveryStatus } from './discovery';
import { MultiplayerStatusOutput } from './statusOutput';
import { sharingDetails, sharingLabel } from './statusBar';
import { describeScope, ScopeChoice } from './scope';
import { HUMAN_ACCOUNT_SCOPES as SCOPES } from '../humanAccount';

class CommandError extends Error {}

const PENDING = 'editchain.multiplayer.pending.';
const SPACE = 'editchain.multiplayer.space.';
const SESSION = 'editchain.multiplayer.session.';
const ENABLED = 'editchain.multiplayer.enabled.';
const DIRECTORY = 'editchain.multiplayer.directory.';
type JournalRecord = { account: string; workspace?: string; owner: string; leaseUntil: number; process?: number };

function liveLease(record: JournalRecord): boolean {
  if (record.leaseUntil <= Date.now()) return false;
  if (!Number.isSafeInteger(record.process) || record.process! <= 0) return true;
  try { process.kill(record.process!, 0); return true; }
  catch (error) {
    // Signal zero only checks existence. Other errors (including permissions)
    // retain the lease; PID reuse conservatively waits for ordinary expiry.
    return (error as NodeJS.ErrnoException).code !== 'ESRCH';
  }
}

export type MultiplayerCommands = { stop(): Promise<void>; suspend(): Promise<void> };

/** Registration has no account, network, or native-process side effects. */
export function registerMultiplayerCommands(context: vscode.ExtensionContext, received: () => void,
  identified: (account: vscode.AuthenticationSessionAccountInformation) => void = () => {}): MultiplayerCommands {
  let manager: MultiplayerManager | undefined;
  let folder: vscode.WorkspaceFolder | undefined;
  let output: vscode.OutputChannel | undefined;
  let liveOutput: MultiplayerStatusOutput | undefined;
  let status: vscode.StatusBarItem | undefined;
  let account: vscode.AuthenticationSession | undefined;
  let directory: DirectorySync | undefined;
  let directoryStatus: DiscoveryStatus | undefined;
  let active = false;
  let stopVersion = 0;
  let configuredChain: string | undefined;
  let transition = Promise.resolve();
  let resumeAfterSettings = false;
  const retiring = new Map<MultiplayerManager, number>();
  const owner = randomUUID();
  const ownedMarkers = new Set<string>();
  let leaseTimer: NodeJS.Timeout | undefined;
  let journalTail = Promise.resolve();
  const journalWrite = (action: () => Promise<void>) => {
    const work = journalTail.then(action);
    journalTail = work.catch(() => {});
    return work;
  };

  const update = (value: SharingStatus, durableChange: boolean) => {
    liveOutput?.update({ ...value, discovery: directoryStatus });
    status ??= vscode.window.createStatusBarItem(vscode.StatusBarAlignment.Left, 99);
    status.command = 'editchain-history.multiplayerStatus';
    status.text = `$(broadcast) ${sharingLabel(value)}`;
    status.tooltip = [sharingDetails(value),
      directoryStatus ? `Discovery: ${directoryStatus.state}` : ''].filter(Boolean).join('\n');
    status.show();
    if (durableChange) received();
  };

  const startDirectory = async (current: MultiplayerManager) => {
    const version = stopVersion, selected = folder;
    if (!selected || manager !== current || !current.status().enabled) return;
    const settings = context.workspaceState.get<{ repository: string; account: string }>(DIRECTORY + selected.uri.toString());
    if (!settings) return;
    if (directory) { await directory.refresh(); return; }
    const { GitHubDirectory, DirectorySync } = await import('./discovery');
    if (version !== stopVersion || manager !== current || !current.status().enabled) return;
    const github = new GitHubDirectory(settings.repository, async () => {
      const session = await vscode.authentication.getSession('github', ['repo'], { silent: true });
      if (!session || session.account.id !== settings.account) throw new CommandError('Repository discovery account is unavailable.');
      return session.accessToken;
    });
    const sync = new DirectorySync(github, { describe: () => current.describe(), discover: values => current.discover(values),
      space: () => current.status().space }, value => {
      if (directory === sync) { directoryStatus = value; update(current.status(), false); }
    });
    directory = sync;
    await sync.start();
  };
  const journal = {
    remember: async (marker: string, workspace = folder?.uri.toString()) => {
      if (!account) throw new CommandError('GitHub host session is unavailable.');
      const record: JournalRecord = { account: account.account.id, workspace, owner, leaseUntil: Date.now() + 90_000, process: process.pid };
      await journalWrite(async () => {
        const current = context.globalState.get<JournalRecord>(PENDING + marker);
        if (current && current.owner !== owner && liveLease(current)) throw new CommandError('This hosting session is active in another window. Close that window before resuming here.');
        await context.globalState.update(PENDING + marker, record); ownedMarkers.add(marker);
      });
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

  const getManager = async (selected?: vscode.WorkspaceFolder): Promise<MultiplayerManager> => {
    if (!vscode.workspace.isTrusted) throw new CommandError('Trust this workspace before enabling history sharing.');
    if (manager) return manager;
    const version = stopVersion;
    const folders = vscode.workspace.workspaceFolders ?? [];
    const chosen = selected ?? (folders.length === 1 ? folders[0] : (await vscode.window.showQuickPick(folders.map(value => ({ label: value.name, description: value.uri.fsPath, folder: value })), { title: 'Choose the workspace history to share' }))?.folder);
    if (version !== stopVersion) throw new CommandError('Sharing was stopped.');
    folder = chosen;
    if (!folder || folder.uri.scheme !== 'file') throw new CommandError('Choose a local workspace folder for sharing.');
    const configuration = vscode.workspace.getConfiguration('editchain-history', folder.uri);
    const chain = path.resolve(folder.uri.fsPath, configuration.get<string>('chainDir', '.editchain'));
    const suffix = process.platform === 'win32' ? '.exe' : '';
    const sibling = path.join(path.dirname(resolveServicePath()), `editchain-peer${suffix}`);
    const bundled = context.asAbsolutePath(path.join('bin', `${process.platform}-${process.arch}`, `editchain-peer${suffix}`));
    const binary = configuration.get<string>('peerPath', '') || (existsSync(sibling) ? sibling : bundled);
    const key = SPACE + folder.uri.toString();
    const sessionKey = SESSION + folder.uri.toString();
    const enabledKey = ENABLED + folder.uri.toString();
    const { MultiplayerManager } = await import('./manager');
    if (version !== stopVersion) throw new CommandError('Sharing was stopped.');
    const workspace = folder.uri.toString();
    const created: MultiplayerManager = new MultiplayerManager({ binary, chain,
      // Device credentials live in private application storage, never the workspace or VSIX.
      deviceDirectory: path.join(context.globalStorageUri.fsPath, 'multiplayer-device'),
      space: context.workspaceState.get<string>(key), saveSpace: async space => { await context.workspaceState.update(key, space); },
      journal: { remember: marker => journal.remember(marker, workspace), forget: journal.forget },
      saveSession: async session => {
        // A retired manager (Stop, chain change, window replacement) must never
        // rewrite the saved session or the auto-resume flag of a newer manager.
        if (manager !== created) return;
        if (session) {
          await context.secrets.store(sessionKey, JSON.stringify({ account: account?.account.id, session }));
          await context.workspaceState.update(enabledKey, true);
        } else {
          await context.workspaceState.update(enabledKey, undefined);
          await context.secrets.delete(sessionKey);
        }
      },
      githubToken: async () => {
        const current = await vscode.authentication.getSession('github', SCOPES, { silent: true });
        if (!current || !account || current.account.id !== account.account.id) throw new CommandError('GitHub host session changed. Start hosting again.');
        return current.accessToken;
      }, changed: (value, durable) => { if (manager === created) update(value, durable); } });
    configuredChain = chain;
    manager = created;
    return manager;
  };

  const backfill = async (current: MultiplayerManager, changing = false): Promise<ScopeChoice | undefined> => {
    const scope = await current.sharingScope();
    const choices: { label: string; detail: string; value: ScopeChoice }[] = [
      { label: 'Share records added from now on', detail: 'Set a new cutoff for this device. Earlier records stop being sent; copies already received by others remain.', value: false },
      { label: 'Include existing history', detail: 'Share all retained operations and their referenced content in this workspace history.', value: true },
    ];
    if (scope?.active && !changing) choices.unshift({ label: 'Keep current sharing scope', detail: describeScope(scope), value: 'keep' });
    const choice = await vscode.window.showQuickPick(choices, { title: `${changing ? 'Change outgoing history scope for' : 'Share history from'} ${folder?.name || 'this workspace'}` });
    return choice?.value;
  };

  const execute = async (action: () => Promise<unknown>) => {
    if (active) return { ok: false, message: 'A multiplayer command is already running.' };
    active = true;
    const version = stopVersion;
    output ??= vscode.window.createOutputChannel('EditChain Multiplayer');
    try {
      await transition;
      if (version !== stopVersion) return { ok: true };
      const value = await action();
      return { ok: true, value };
    }
    catch (error) {
      if (version !== stopVersion) return { ok: true };
      // Only UI and controlled adapter errors escape the manager.
      const message = error instanceof CommandError || error instanceof ProbeError || error instanceof NativePeerError
        ? error.message : 'Multiplayer command failed. Account and service details were omitted.';
      output.appendLine(message);
      void vscode.window.showErrorMessage(`EditChain Multiplayer: ${message}`);
      return { ok: false, message };
    } finally { active = false; }
  };
  const stop = async () => {
    stopVersion++;
    resumeAfterSettings = false;
    const previous = directory; directory = undefined; directoryStatus = undefined;
    const current = new Set([...retiring.keys(), ...(manager ? [manager] : [])]);
    // Cancel synchronously so Stop always wins, then make the awaited cleanup part of
    // the transition every execute() waits on. A Host started while cleanup is pending
    // must wait for retirement instead of grabbing the stopped manager and being
    // discarded (or leaked) by this Stop's cleanup.
    const cleanup = Promise.allSettled([previous?.stop(), ...[...current].map(value => value.stop())])
      .then(results => {
        if (results.some(result => result.status === 'rejected')) throw new CommandError('Sharing cleanup is pending.');
      })
      .finally(() => {
        // An explicit Stop retires the session for good. Clearing the manager keeps a
        // later settings change from suspending it and promising a resume it cannot honor.
        if (manager && current.has(manager)) { manager = undefined; folder = undefined; configuredChain = undefined; }
      });
    transition = transition.then(() => cleanup, () => cleanup).catch(() => {});
    await cleanup;
  };
  const suspend = async () => {
    stopVersion++;
    resumeAfterSettings = false;
    const previous = directory; directory = undefined;
    const current = new Set([...retiring.keys(), ...(manager ? [manager] : [])]);
    await Promise.allSettled([previous?.stop(), ...[...current].map(value => value.suspend())]);
    clearInterval(leaseTimer); leaseTimer = undefined;
    await journalWrite(async () => {
      for (const marker of ownedMarkers) {
        const current = context.globalState.get<JournalRecord>(PENDING + marker);
        if (current?.owner === owner) await context.globalState.update(PENDING + marker, { ...current, leaseUntil: 0 });
      }
    });
  };
  const restore = async (interactive: boolean, selected?: vscode.WorkspaceFolder) => {
    const version = stopVersion;
    const current = await getManager(selected);
    const stored = await context.secrets.get(SESSION + folder!.uri.toString());
    if (!stored) throw new CommandError('No saved sharing session. Host or join to enable sharing.');
    if (stored.length > 512 * 1024) throw new CommandError('Saved sharing session exceeds the limit.');
    let envelope: { account?: string; session: SavedSharing };
    try { envelope = JSON.parse(stored); } catch { throw new CommandError('Saved sharing session is invalid.'); }
    if (envelope.session?.host) {
      account = await vscode.authentication.getSession('github', SCOPES, interactive ? { createIfNone: true } : { silent: true });
      if (!account || account.account.id !== envelope.account) throw new CommandError('Sign in with the original host GitHub account, then resume sharing.');
    }
    const identity = envelope.session?.host ? account : interactive
      ? await vscode.authentication.getSession('github', SCOPES, { createIfNone: true }) : undefined;
    if (version !== stopVersion) return;
    if (identity) identified(identity.account);
    const wasEnabled = current.status().enabled;
    await current.resume(envelope.session);
    if (version !== stopVersion) return;
    if (wasEnabled) await current.reconnect();
    if (version === stopVersion) await startDirectory(current);
  };
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
      const include = await backfill(current); if (include === undefined) return;
      const approved = await vscode.window.showWarningMessage(`Approve device ${request.device.fingerprint} to exchange history with ${folder!.name}?`, { modal: true }, 'Approve device');
      if (approved !== 'Approve device') return;
      account = await vscode.authentication.getSession('github', SCOPES, { createIfNone: true });
      if (version !== stopVersion) return;
      identified(account.account);
      const invitation = await current.hostHistory(text, include);
      await vscode.env.clipboard.writeText(invitation);
      void vscode.window.showInformationMessage('Private invitation copied. Give it to the approved device. It expires in at most one hour.');
      if (version === stopVersion) await startDirectory(current);
    }),
    command('multiplayerJoin', async () => {
      const version = stopVersion;
      const current = await getManager();
      const text = await vscode.window.showInputBox({ title: 'Join shared history', prompt: 'Paste the host’s private EditChain invitation', password: true, ignoreFocusOut: true });
      if (!text) return;
      const invitation = await current.inspectInvitation(text);
      const include = await backfill(current); if (include === undefined) return;
      const approved = await vscode.window.showWarningMessage(`Join space ${invitation.space} with host device ${invitation.host.fingerprint}?`, { modal: true }, 'Join space');
      if (approved !== 'Join space') return;
      const identity = await vscode.authentication.getSession('github', SCOPES, { createIfNone: true });
      if (version !== stopVersion) return;
      identified(identity.account);
      await current.joinHistory(text, include);
      if (version === stopVersion) await startDirectory(current);
    }),
    command('multiplayerStatus', async () => {
      if (manager) await manager.sharingScope();
      const value = { ...(manager?.status() ?? { hosting: false, peers: [], message: 'Sharing is disabled.' }), discovery: directoryStatus };
      output!.show(true);
      output!.appendLine(JSON.stringify(value, null, 2));
      liveOutput ??= new MultiplayerStatusOutput(line => output!.appendLine(line));
      liveOutput.show(value);
      return value;
    }),
    command('multiplayerScope', async () => {
      const version = stopVersion;
      const current = await getManager();
      const include = await backfill(current, true);
      if (include === undefined || include === 'keep' || version !== stopVersion) return;
      await current.changeScope(include);
      if (version === stopVersion) void vscode.window.showInformationMessage(describeScope(current.status().scope));
    }),
    command('multiplayerRemove', async () => {
      const current = await getManager();
      const device = await vscode.window.showQuickPick((await current.devices()).map(device => ({ label: device.fingerprint, device })), { title: 'Remove an approved device from this replica' });
      if (device) await current.revoke(device.device.fingerprint);
    }),
    command('multiplayerResume', () => restore(true)),
    command('multiplayerDiscovery', async () => {
      const version = stopVersion;
      const current = await getManager();
      if (!current.status().space || !current.status().enabled) throw new CommandError('Host or join a space before enabling discovery.');
      const choice = await vscode.window.showQuickPick([
        { label: 'Enable repository discovery', value: true }, { label: 'Disable repository discovery', value: false },
      ], { title: 'Optional GitHub peer discovery' });
      if (!choice) return;
      const key = DIRECTORY + folder!.uri.toString();
      if (!choice.value) {
        if (version !== stopVersion) return;
        await context.workspaceState.update(key, undefined);
        const previous = directory; directory = undefined; directoryStatus = undefined;
        await previous?.stop();
        return;
      }
      const text = await vscode.window.showInputBox({ title: 'GitHub repository for discovery', prompt: 'owner/repository (collaborator access required)', ignoreFocusOut: true });
      if (!text) return;
      const { repositoryName } = await import('./discovery');
      const repository = repositoryName(text.trim());
      const approved = await vscode.window.showWarningMessage(`Publish this space’s public device identity and relay endpoint in ${repository}? Repository discovery requests GitHub repo access. Invitations still control peer enrollment.`, { modal: true }, 'Enable discovery');
      if (approved !== 'Enable discovery') return;
      const session = await vscode.authentication.getSession('github', ['repo'], { createIfNone: true });
      if (version !== stopVersion) return;
      await context.workspaceState.update(key, { repository, account: session.account.id });
      const previous = directory; directory = undefined; directoryStatus = undefined;
      await previous?.stop();
      if (version === stopVersion) await startDirectory(current);
    }),
    // Stop is always available even while a sign-in or connection command waits.
    vscode.commands.registerCommand('editchain-history.multiplayerStop', () => stop().then(() => ({ ok: true }), () => {
      void vscode.window.showErrorMessage('Sharing stopped; run EditChain: Clean Up Multiplayer Tunnels to retry cleanup.');
      return { ok: false };
    })),
    command('multiplayerCleanup', async () => {
      if (manager?.status().enabled) throw new CommandError('Stop sharing in this window before cleaning up its tunnels.');
      account = await vscode.authentication.getSession('github', SCOPES, { createIfNone: true });
      const { managementClient, cleanupRelay } = await import('./relay');
      const management = managementClient(async () => account!.accessToken);
      try {
        const workspaces = new Set(vscode.workspace.workspaceFolders?.map(folder => folder.uri.toString()));
        const keys = context.globalState.keys().filter(key => {
          if (!key.startsWith(PENDING)) return false;
          const entry = context.globalState.get<JournalRecord>(key);
          return entry?.account === account!.account.id && workspaces.has(entry.workspace || '') &&
            (entry.owner === owner || !liveLease(entry));
        });
        for (const key of keys) await cleanupRelay(management, key.slice(PENDING.length), journal);
        void vscode.window.showInformationMessage('Inactive pending multiplayer tunnels cleaned up for this workspace.');
      } finally { await management.dispose(); }
    }),
    { dispose: () => { liveOutput?.dispose(); void suspend().catch(() => {}); clearInterval(leaseTimer); status?.dispose(); output?.dispose(); } },
  );
  const reset = (preserve: boolean) => {
    const previous = manager, selected = folder;
    if (!previous || !selected) { stopVersion++; return; }
    const version = ++stopVersion;
    resumeAfterSettings = preserve && (resumeAfterSettings || !!previous.status().enabled);
    const discovery = directory; directory = undefined; directoryStatus = undefined;
    retiring.set(previous, (retiring.get(previous) ?? 0) + 1);
    transition = transition.then(async () => {
      const results = await Promise.allSettled([preserve ? previous.suspend() : previous.stop(), discovery?.stop()]);
      if (version !== stopVersion) return;
      if (results.some(result => result.status === 'rejected')) throw new CommandError('Sharing could not restart. Check the worker settings, then resume sharing.');
      manager = undefined; folder = undefined; configuredChain = undefined;
      if (resumeAfterSettings && vscode.workspace.isTrusted && vscode.workspace.workspaceFolders?.some(value => value.uri.toString() === selected.uri.toString())) {
        await restore(false, selected);
      }
      if (version === stopVersion) resumeAfterSettings = false;
    }).catch(() => {
      if (version === stopVersion) void vscode.window.showErrorMessage('EditChain Multiplayer: Sharing could not restart. Check the worker settings, then resume sharing.');
    }).finally(() => {
      const count = (retiring.get(previous) ?? 1) - 1;
      if (count) retiring.set(previous, count); else retiring.delete(previous);
    });
  };
  context.subscriptions.push(
    vscode.workspace.onDidChangeWorkspaceFolders(event => {
      if (folder && event.removed.some(value => value.uri.toString() === folder!.uri.toString())) reset(false);
    }),
    vscode.workspace.onDidChangeConfiguration(event => {
      if (!folder) return;
      const affects = (key: string) => event.affectsConfiguration(`editchain-history.${key}`, folder!.uri);
      const chain = path.resolve(folder.uri.fsPath, vscode.workspace.getConfiguration('editchain-history', folder.uri).get<string>('chainDir', '.editchain'));
      if (affects('chainDir') && chain !== configuredChain) reset(false);
      else if (affects('servicePath') || affects('peerPath')) reset(true);
    }),
  );
  if (vscode.authentication.onDidChangeSessions) context.subscriptions.push(vscode.authentication.onDidChangeSessions(event => {
    if (event.provider.id !== 'github' || !account) return;
    void Promise.resolve(vscode.authentication.getSession('github', SCOPES, { silent: true })).then(current => {
      if (!current || current.account.id !== account?.account.id) return stop();
    }, () => stop()).catch(() => {});
  }));
  // Only a workspace explicitly enabled earlier can initiate background activity.
  const selected = vscode.workspace.workspaceFolders?.find(value => context.workspaceState.get<boolean>(ENABLED + value.uri.toString()));
  if (selected && vscode.workspace.isTrusted) void execute(() => restore(false, selected));
  return { stop, suspend };
}
