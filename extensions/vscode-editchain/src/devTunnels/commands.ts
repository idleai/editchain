import * as vscode from 'vscode';
import type { SpikeJournal, SpikeResult } from './spike';

const PENDING_PREFIX = 'editchain.devTunnels.pending.';

export type SpikeCommandResult = { ok: true; result?: SpikeResult } | { ok: false; message: string };

/** Registration performs no sign-in, SDK loading, or network activity. */
export function registerDevTunnelsCommands(context: vscode.ExtensionContext): void {
  let active: vscode.CancellationTokenSource | undefined;
  let output: vscode.OutputChannel | undefined;

  const execute = async (cleanupOnly: boolean): Promise<SpikeCommandResult> => {
    if (active) return { ok: false, message: 'A Dev Tunnels spike is already running in this window.' };
    const cancellation = new vscode.CancellationTokenSource();
    active = cancellation;
    output ??= vscode.window.createOutputChannel('EditChain Dev Tunnels Spike');
    const out = output;
    out.show(true);
    const log = (line: string) => out.appendLine(line);
    let stage = 'GitHub authentication';
    try {
      return await vscode.window.withProgress({
        location: vscode.ProgressLocation.Notification,
        title: cleanupOnly ? 'EditChain: Cleaning up spike tunnels' : 'EditChain: Dev Tunnels spike',
        cancellable: true,
      }, async (progress, userCancellation) => {
        const subscription = userCancellation.onCancellationRequested(() => cancellation.cancel());
        if (userCancellation.isCancellationRequested) cancellation.cancel();
        try {
          const sdk = await import('./spike');
          const session = await sdk.bounded(
            async () => vscode.authentication.getSession('github', sdk.GITHUB_SCOPES, { createIfNone: true }),
            cancellation.token, 300_000
          );
          if (cancellation.token.isCancellationRequested) throw new sdk.ProbeError('Spike cancelled.');
          log(`GitHub account: ${session.account.label}; scopes: ${sdk.GITHUB_SCOPES.join(', ')}`);
          log(`SDK ${sdk.SDK_VERSION}; host and client run in this extension host.`);
          const services = sdk.createSpikeServices(async () => {
            const current = await vscode.authentication.getSession('github', sdk.GITHUB_SCOPES, { silent: true });
            if (!current || current.account.id !== session.account.id) {
              throw new sdk.ProbeError('The authorized GitHub session changed. Run the command again.');
            }
            return current.accessToken;
          });
          const journal: SpikeJournal = {
            remember: async name => { await context.globalState.update(PENDING_PREFIX + name, session.account.id); },
            forget: async name => { await context.globalState.update(PENDING_PREFIX + name, undefined); },
          };
          const pending = context.globalState.keys().filter(key =>
            key.startsWith(PENDING_PREFIX) && context.globalState.get(key) === session.account.id);
          stage = 'Cleaning up previous spike resources';
          try {
            for (const key of pending) {
              await sdk.cleanupSpike(services.management, key.slice(PENDING_PREFIX.length), journal);
              log(`Removed pending spike resource: ${key.slice(PENDING_PREFIX.length)}`);
            }
          } catch (error) {
            await services.management.dispose();
            throw error;
          }
          if (cleanupOnly) {
            await services.management.dispose();
            log('PASS: pending spike resources cleaned up for this account.');
            return { ok: true };
          }
          stage = 'Dev Tunnels spike';
          const result = await sdk.runSpike(services, journal, message => {
            log(message);
            progress.report({ message });
          }, cancellation.token);
          log(`PASS: encrypted bidirectional relay probe; ${JSON.stringify(result)}`);
          log('This measures a same-account relay path. Cross-account and cross-network tests remain separate.');
          void vscode.window.showInformationMessage('Dev Tunnels spike passed. Results are in EditChain Dev Tunnels Spike.');
          return { ok: true, result };
        } finally { subscription.dispose(); }
      });
    } catch (error) {
      const { safeFailure } = await import('./spike');
      const message = safeFailure(stage, error);
      log(`FAIL: ${message}`);
      void vscode.window.showErrorMessage(`Dev Tunnels spike: ${message}`);
      return { ok: false, message };
    } finally {
      cancellation.dispose();
      active = undefined;
    }
  };

  context.subscriptions.push(
    vscode.commands.registerCommand('editchain-history.devTunnelsSpike', () => execute(false)),
    vscode.commands.registerCommand('editchain-history.devTunnelsCleanup', () => execute(true)),
    { dispose: () => { active?.cancel(); output?.dispose(); } },
  );
}
