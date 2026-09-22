import * as vscode from 'vscode';
import * as path from 'node:path';
import { promises as fs } from 'node:fs';
import { createHash } from 'node:crypto';
import { EditorCapture } from './editorCapture';
import { EditorOutbox } from './editorOutbox';
import { observeEditorContext } from './editorContext';
import { StdioClient, resolveServicePath } from './stdioClient';
import { unsignedIdentity, workspaceIdentity } from './humanIdentity';
import { EditorHealth } from './editorHealth';
import { MAX_EDITOR_BUFFER_BYTES } from './editorLimits';
import { HumanAccount } from './humanAccount';
import { HistoryArchive, archiveDirectory } from './historyArchive';

/** Settings that change capture itself; archive settings are handled separately. */
const RESTART_SETTINGS = ['tracking.enabled', 'tracking.readDwellMs', 'tracking.maxFileBytes', 'chainDir', 'servicePath'];

type Recorder = { capture: EditorCapture; context: { dispose(): void }; contextClient: StdioClient; outbox: EditorOutbox; client: StdioClient; folder: vscode.WorkspaceFolder; chain: string; health: EditorHealth };

/** Capture lifecycle is independent of whether the History panel is open. */
export class HumanWorkHost {
  private recorders: Recorder[] = [];
  private lifecycle = Promise.resolve();
  private disposed = false;
  private readonly status: vscode.StatusBarItem;
  private reportText = '';
  private configurationKey: string | undefined;
  private transportStatus = 'Tracking human work';
  private reporting: Promise<unknown> | undefined;
  private reportClient: StdioClient | undefined;
  private stopping: Promise<void> | undefined;
  private readonly archives = new Map<string, HistoryArchive>();
  private archive: HistoryArchive | undefined;
  private archiveKey: string | undefined;
  private readonly account: HumanAccount;

  constructor(private readonly context: vscode.ExtensionContext, private readonly log: vscode.OutputChannel,
    private readonly delivered: () => void = () => {}) {
    this.account = new HumanAccount(name => {
      for (const recorder of this.recorders) recorder.capture.setUserName(name);
    });
    this.status = vscode.window.createStatusBarItem(vscode.StatusBarAlignment.Left, 99);
    this.status.command = 'editchain-history.showTrackingStatus';
    context.subscriptions.push(this.status,
      vscode.workspace.registerTextDocumentContentProvider('editchain-work', { provideTextDocumentContent: () => this.reportText }),
      vscode.commands.registerCommand('editchain-history.humanWork', () => this.showReport()),
      vscode.commands.registerCommand('editchain-history.showTrackingStatus', () => this.showStatus()),
      vscode.commands.registerCommand('editchain-history.trackingStatus', async () => {
        await this.lifecycle;
        return this.recorders.map(({ folder, health }) => ({ workspace: folder.uri.fsPath, ...health }));
      }),
      vscode.commands.registerCommand('editchain-history.startTracking', () => this.setTracking(true)),
      vscode.commands.registerCommand('editchain-history.stopTracking', () => this.setTracking(false)),
      vscode.workspace.onDidChangeWorkspaceFolders(() => { void this.restart(); }),
      vscode.workspace.onDidChangeConfiguration(event => {
        if (RESTART_SETTINGS.some(name => event.affectsConfiguration(`editchain-history.${name}`))) void this.restart();
        // A newly enabled archive or a new destination must begin with complete
        // recorder sequences, so capture restarts after the archive switches.
        else if (event.affectsConfiguration('editchain-history.tracking.jsonl')) void this.restart(true);
      }),
      this.account, { dispose: () => { void this.stop(); } },
    );
    if (vscode.workspace.isTrusted === false) {
      context.subscriptions.push(vscode.workspace.onDidGrantWorkspaceTrust(() => { void this.account.refresh(); void this.restart(); }));
    }
    void this.restart();
  }

  private async setTracking(enabled: boolean): Promise<void> {
    const configuration = vscode.workspace.getConfiguration('editchain-history');
    if (configuration.get<boolean>('tracking.enabled', true) === enabled) return await this.restart(true);
    await configuration.update('tracking.enabled', enabled, vscode.ConfigurationTarget.Workspace);
    await this.restart();
  }

  private restart(force = false): Promise<void> {
    this.lifecycle = this.lifecycle.then(async () => {
      const configuration = vscode.workspace.getConfiguration('editchain-history');
      const key = JSON.stringify([vscode.workspace.isTrusted, (vscode.workspace.workspaceFolders ?? []).map(folder => folder.uri.toString()),
        ...RESTART_SETTINGS.map(name => configuration.get(name))]);
      if (!force && key === this.configurationKey) return;
      this.configurationKey = undefined;
      await this.stopRecorders();
      if (this.disposed || !vscode.workspace.isTrusted || !this.context.storageUri) return;
      // Stop the old recorders first so a replaced destination keeps its
      // session's final event; the new destination starts a fresh session.
      await this.syncArchive();
      if (!configuration.get<boolean>('tracking.enabled', true)) {
        this.configurationKey = key;
        this.updateStatus('Human-work tracking paused'); return;
      }
      const guid = await unsignedIdentity(this.context.globalStorageUri.fsPath);
      for (const folder of vscode.workspace.workspaceFolders ?? []) {
        if (folder.uri.scheme !== 'file') continue;
        const chain = configuration.get<string>('chainDir', '.editchain');
        const namespace = createHash('sha256').update(folder.uri.toString() + '\0' + chain).digest('hex').slice(0, 24);
        const client = new StdioClient();
        client.setLog(line => this.log.appendLine(`[capture] ${line}`));
        const contextClient = new StdioClient();
        contextClient.setLog(line => this.log.appendLine(`[capture context] ${line}`));
        const outbox = new EditorOutbox(path.join(this.context.storageUri.fsPath, 'editor-outbox', namespace),
          folder.uri.fsPath, chain, body => {
            client.ensureStarted(resolveServicePath());
            return client.requestJson(body, { timeoutMs: 30000 });
          }, message => this.updateStatus(message), this.delivered,
          timing => this.log.appendLine(`[capture] delivery ${JSON.stringify(timing)}`),
          // Archive the exact event the outbox admits, so the archive and the
          // chain agree even when a capacity pause replaces an event with its
          // gap. The archive still runs before any native delivery.
          (workspace, event) => this.archive?.append(workspace, event));
        const dwell = Math.max(500, Math.min(30000, configuration.get<number>('tracking.readDwellMs', 2000)));
        const maxBytes = Math.max(1024, Math.min(MAX_EDITOR_BUFFER_BYTES,
          configuration.get<number>('tracking.maxFileBytes', MAX_EDITOR_BUFFER_BYTES)));
        const health = new EditorHealth();
        const capture = new EditorCapture(folder, dwell, maxBytes, event => {
          const accepted = outbox.push(event);
          if (accepted) {
            const previous = health.mode;
            health.observe(event);
            if (previous !== health.mode) {
              this.log.appendLine(`[capture] Edit attribution: ${health.mode === 'direct' ? 'direct document change reasons' : 'limited; unconfirmed edits remain visible as unattributed'}`);
              this.updateStatus(this.transportStatus);
            }
          }
          return accepted;
        },
          workspaceIdentity(guid, folder.uri.toString(), folder.uri.fsPath, chain), this.account.name,
          file => this.excludesArchive(file));
        const context = observeEditorContext(capture, () => {
          contextClient.ensureStarted(resolveServicePath());
          return contextClient.request({ GetEditorContext: { workspace_path: folder.uri.fsPath, chain_dir: chain } }, { timeoutMs: 30000 });
        }, message => this.log.appendLine(`[capture] ${message}`));
        this.recorders.push({ capture, context, contextClient, outbox, client, folder, chain, health });
      }
      this.configurationKey = key;
      if (this.recorders.length) this.updateStatus('Tracking human work');
    }).catch(error => this.updateStatus(`Tracking failed: ${String(error)}`));
    return this.lifecycle;
  }

  private updateStatus(message: string): void {
    this.transportStatus = message;
    const limited = this.recorders.some(recorder => recorder.health.mode === 'limited');
    const attribution = limited ? 'Limited attribution: enable direct editor reasons to count all supported human input. Unconfirmed edits remain visible as unattributed.'
      : this.recorders.some(recorder => recorder.health.mode === 'direct') ? 'Attribution uses direct VS Code document-change reasons.'
      : 'Waiting for the first document change to verify input attribution.';
    const tooltip = message === 'Tracking human work' ? `${message}\n${attribution}` : message;
    const changed = this.status.tooltip !== tooltip;
    this.status.text = message === 'Tracking human work' && !limited ? '$(edit) EditChain tracking'
      : limited && message === 'Tracking human work' ? '$(info) EditChain · limited attribution' : '$(info) EditChain tracking';
    this.status.tooltip = tooltip;
    if (!this.disposed) this.status.show();
    if (changed) this.log.appendLine(`[capture] ${tooltip.replaceAll('\n', ' · ')}`);
  }

  private async showStatus(): Promise<void> {
    await this.lifecycle;
    this.log.appendLine(`[capture] Runtime ${JSON.stringify({ vscode: vscode.version,
      client: vscode.env?.appHost, remote: vscode.env?.remoteName,
      extension: this.context.extension?.packageJSON.version,
      enabled_proposals: this.context.extension?.packageJSON.enabledApiProposals ?? [],
      workspaces: this.recorders.map(({ folder, health }) => ({ workspace: folder.uri.fsPath, ...health })),
    })}`);
    this.log.appendLine(`[capture] ${this.transportStatus}`);
    if (this.recorders.some(recorder => recorder.health.mode === 'limited')) {
      this.log.appendLine('[capture] Direct input metadata is absent in this session. In the desktop client, use Preferences: Configure Runtime Arguments, add "enable-proposed-api": ["ambientlight.editchain-history"], then fully quit and reopen the client. Workspace settings and a remote host\'s argv.json do not configure a different client.');
    }
    this.log.show(true);
  }

  private showReport(): Promise<unknown> {
    this.reporting ??= this.buildReport().finally(() => { this.reporting = undefined; });
    return this.reporting;
  }

  private async buildReport(): Promise<unknown> {
    await this.lifecycle;
    try {
      const folders = vscode.workspace.workspaceFolders ?? [];
      const folder = folders.length === 1 ? folders[0] : await vscode.window.showWorkspaceFolderPick();
      if (!folder || !vscode.workspace.isTrusted) return;
      const recorder = this.recorders.find(item => item.folder.uri.toString() === folder.uri.toString());
      if (recorder) {
        recorder.capture.checkpoint();
        if (!await recorder.outbox.flush()) throw new Error('Capture is still pending. Restore the service and retry the report.');
      }
      if (this.disposed) return undefined;
      // Coverage replays the complete history. Never put it ahead of live input
      // on the recorder's serial native connection, even while the report waits.
      const client = new StdioClient();
      this.reportClient = client;
      client.setLog(line => this.log.appendLine(`[capture report] ${line}`));
      let response;
      try {
        client.ensureStarted(resolveServicePath());
        for (let attempt = 0; attempt < 3 && !this.disposed; attempt++) {
          response = await client.request({ GetHumanWork: { workspace_path: folder.uri.fsPath,
            chain_dir: vscode.workspace.getConfiguration('editchain-history').get<string>('chainDir', '.editchain') } }, { timeoutMs: 120000 });
          if (response?.Error?.code !== 'stale_snapshot') break;
        }
      } finally { client.stop(); this.reportClient = undefined; }
      if (this.disposed) return undefined;
      if (!response?.Ok || response.Ok.schema !== 1) throw new Error(JSON.stringify(response?.Error ?? response));
      this.reportText = formatReport(response.Ok);
      const document = await vscode.workspace.openTextDocument(vscode.Uri.parse(`editchain-work:/human-work-${Date.now()}.md`));
      await vscode.window.showTextDocument(document, { preview: true });
      return response.Ok;
    } catch (error) {
      this.log.appendLine(`[capture report] ${String(error)}`);
      if (!this.disposed) void vscode.window.showErrorMessage(`EditChain human work: ${String(error)}`);
      return undefined;
    }
  }

  private archiveFallback(): string | undefined {
    return this.context.globalStorageUri ? path.join(this.context.globalStorageUri.fsPath, 'human-history') : undefined;
  }

  private archiveWorkspaces(): string[] {
    return (vscode.workspace.workspaceFolders ?? [])
      .filter(folder => folder.uri.scheme === 'file').map(folder => folder.uri.fsPath);
  }

  private archiveDestination(): { directory: string } | { error: string } {
    const fallback = this.archiveFallback();
    if (!fallback) return { error: 'extension global storage is unavailable' };
    const setting = vscode.workspace.getConfiguration('editchain-history').get<string>('tracking.jsonl.directory', '');
    return archiveDirectory(setting, this.archiveWorkspaces(), fallback);
  }

  /** One session file per activation and destination; restarts reuse it. */
  private async syncArchive(): Promise<void> {
    const enabled = vscode.workspace.getConfiguration('editchain-history').get<boolean>('tracking.jsonl.enabled', false)
      && vscode.workspace.isTrusted;
    const resolved = this.archiveDestination();
    const key = JSON.stringify([enabled, 'error' in resolved ? resolved.error : resolved.directory]);
    if (key === this.archiveKey) return;
    this.archiveKey = key;
    // Disabling keeps the destination's writer for this activation, so
    // re-enabling appends to the same file instead of rotating it.
    if (!enabled) { this.archive = undefined; return; }
    if ('error' in resolved) { this.archive = undefined; this.reportArchive(resolved.error); return; }
    const retained = this.archives.get(resolved.directory);
    if (retained) {
      // A failed destination keeps its file and handle until shutdown: the
      // error asks for a reload, and replacing it would allocate a second
      // same-activation file and leak the failed writer.
      this.archive = retained;
      return;
    }
    try { await fs.mkdir(resolved.directory, { recursive: true }); }
    catch (error) {
      this.archive = undefined;
      this.reportArchive(`directory ${resolved.directory} is unavailable: ${String(error)}`);
      return;
    }
    const archive = new HistoryArchive({ directory: resolved.directory,
      log: line => this.log.appendLine(line), report: message => this.reportArchive(message) });
    this.archives.set(resolved.directory, archive);
    this.archive = archive;
    this.log.appendLine(`[capture] Human history archive: ${resolved.directory}`);
  }

  /** No archive file this activation wrote may be recorded back into the chain. */
  private excludesArchive(fsPath: string): boolean {
    for (const archive of this.archives.values()) if (archive.excludes(fsPath)) return true;
    return false;
  }

  private async stopArchives(): Promise<void> {
    const archives = [...this.archives.values()];
    this.archives.clear();
    this.archive = undefined;
    this.archiveKey = undefined;
    for (const archive of archives) await archive.stop();
  }

  private reportArchive(message: string): void {
    this.log.appendLine(`[capture] Human history archive: ${message}`);
    if (!this.disposed) void vscode.window.showErrorMessage(`EditChain human history archive: ${message}`);
  }

  private async stopRecorders(): Promise<void> {
    const recorders = this.recorders;
    this.recorders = [];
    for (const recorder of recorders) {
      recorder.context.dispose(); recorder.contextClient.stop(); recorder.capture.dispose();
    }
    for (const recorder of recorders) {
      try { await recorder.outbox.stop(); } finally { recorder.client.stop(); }
    }
  }

  /** Subscription disposal and deactivate share one shutdown completion. */
  stop(): Promise<void> {
    this.disposed = true;
    this.stopping ??= this.shutdown();
    return this.stopping;
  }

  private async shutdown(): Promise<void> {
    this.account.dispose();
    this.reportClient?.stop();
    await this.lifecycle;
    await this.stopRecorders();
    // Drain every destination used by this activation after the recorders
    // published their final events.
    await this.stopArchives();
  }

  useAccount(account: vscode.AuthenticationSessionAccountInformation): void { this.account.use(account); }
}

export function formatReport(report: any): string {
  const coverage = (value: number) => report.ai_lines ? `${value} / ${report.ai_lines} (${(100 * value / report.ai_lines).toFixed(1)}%)` : 'Not yet measurable';
  const escape = (value: string) => value.replace(/[|\r\n]/g, ' ');
  return ['# Human work on AI-generated code', '', report.basis, '',
    `- AI-origin lines matched in current files: **${report.ai_lines}**`,
    `- Reading indicator: **${coverage(report.read_lines)}**`,
    `- Human edited: **${coverage(report.edited_lines)}**`,
    `- Both read indicator and edited: **${coverage(report.read_and_edited_lines)}**`, '',
    '| File | Matched AI lines | Read indicator | Human edited |', '| --- | ---: | ---: | ---: |',
    ...report.files.map((file: any) => `| ${escape(file.path)} | ${file.ai_lines} | ${file.read_lines} | ${file.edited_lines} |`), '',
    `Historical evidence: ${report.historical_ai_lines} generated lines; ${report.historical_ai_lines_read} with reading indicators; ${report.historical_ai_lines_edited} edited (including subsequently deleted lines).`,
    `Captured ${report.human_changes} human-indicated changes and ${report.events} events.`, '',
    `Observed changes without human attribution: ${report.unattributed_changes ?? 0}.`, '',
    `Coverage gaps: ${report.capture_gaps}. Unsupported AI changes: ${report.unsupported_ai_changes}. Unavailable files: ${report.unavailable_files}.`, '',
    ...report.limitations.map((note: string) => `- ${note}`), '',
    'Run **EditChain: Show Human Work Coverage** again to refresh. Tracking can be paused or resumed from the Command Palette.', ''].join('\n');
}
