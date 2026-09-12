import * as vscode from 'vscode';
import * as path from 'path';
import { homedir } from 'os';
import { existsSync } from 'fs';
import { belongsToWorkspace, captureSources, LivePaths } from './liveSources';
import { LiveSync } from './liveSync';

// A disposed collector may still be completing a durable transaction.
// Replacement panels/configurations share this barrier.
let importsIdle = Promise.resolve();

function serializeSync(run: () => Promise<void>, signal: AbortSignal): Promise<void> {
  const work = importsIdle.then(() => {
    signal.throwIfAborted();
    return run();
  });
  importsIdle = work.catch(() => {});
  return work;
}

export interface LiveProviderRequest { sessions_root: string; helper: string; paths: string[] }

export function createLiveSync(service: string, synchronize: (provider?: LiveProviderRequest) => Promise<boolean | void>, status: (text: string) => void, log: (text: string) => void): LiveSync {
  const config = vscode.workspace.getConfiguration('editchain-history');
  const workspace = vscode.workspace.workspaceFolders?.[0]?.uri.fsPath;
  if (!workspace) throw new Error('Open a workspace folder to follow Codex sessions.');
  const helper = path.join(workspace, 'tools', 'codex-session-exporter', 'target', 'release', 'codex-session-exporter');
  const paths: LivePaths = {
    workspace,
    chain: path.resolve(workspace, config.get<string>('chainDir', '.editchain')),
    sessions: path.resolve(workspace, config.get<string>('live.sessionsPath', '') ||
      path.join(process.env.CODEX_HOME || path.join(homedir(), '.codex'), 'sessions')),
    cli: config.get<string>('live.cliPath', '') || path.join(path.dirname(service), process.platform === 'win32' ? 'editchain.exe' : 'editchain'),
    helper: config.get<string>('live.codexHelperPath', '') || (existsSync(helper) ? helper : 'codex-session-exporter'),
  };
  for (const [name, value] of Object.entries(paths)) log(`${name}: ${value}`);
  return new LiveSync({
    capture: () => captureSources(paths, true),
    importFiles: (files, signal) => serializeSync(async () => {
      const selected: string[] = [];
      for (const file of files) {
        signal.throwIfAborted();
        if (await belongsToWorkspace(file, paths.workspace)) selected.push(file);
      }
      log(`Syncing ${selected.length} changed Codex rollout(s) through the retained service.`);
      while (await synchronize({ sessions_root: paths.sessions, helper: paths.helper, paths: selected })) {
        signal.throwIfAborted();
      }
    }, signal),
    publish: async () => { await synchronize(); }, status, pollNative: true,
  }, 250);
}
