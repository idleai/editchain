import * as vscode from 'vscode';
import * as path from 'node:path';
import { createHash } from 'node:crypto';
import { EditorCapture } from './editorCapture';
import { EditorOutbox } from './editorOutbox';
import { observeEditorContext } from './editorContext';
import { StdioClient, resolveServicePath } from './stdioClient';

type Recorder = { capture: EditorCapture; context: { dispose(): void }; outbox: EditorOutbox; client: StdioClient; folder: vscode.WorkspaceFolder; chain: string };

/** Capture lifecycle is independent of whether the History panel is open. */
export class HumanWorkHost {
  private recorders: Recorder[] = [];
  private lifecycle = Promise.resolve();
  private disposed = false;
  private readonly status: vscode.StatusBarItem;
  private reportText = '';

  constructor(private readonly context: vscode.ExtensionContext, private readonly log: vscode.OutputChannel) {
    this.status = vscode.window.createStatusBarItem(vscode.StatusBarAlignment.Left, 99);
    this.status.command = 'editchain-history.humanWork';
    context.subscriptions.push(this.status,
      vscode.workspace.registerTextDocumentContentProvider('editchain-work', { provideTextDocumentContent: () => this.reportText }),
      vscode.commands.registerCommand('editchain-history.humanWork', () => this.showReport()),
      vscode.commands.registerCommand('editchain-history.startTracking', async () => {
        await vscode.workspace.getConfiguration('editchain-history').update('tracking.enabled', true, vscode.ConfigurationTarget.Workspace);
        await this.restart();
      }),
      vscode.commands.registerCommand('editchain-history.stopTracking', async () => {
        await vscode.workspace.getConfiguration('editchain-history').update('tracking.enabled', false, vscode.ConfigurationTarget.Workspace);
        await this.restart();
      }),
      vscode.workspace.onDidChangeWorkspaceFolders(() => { void this.restart(); }),
      vscode.workspace.onDidChangeConfiguration(event => {
        if (['tracking', 'chainDir', 'servicePath'].some(key => event.affectsConfiguration(`editchain-history.${key}`))) void this.restart();
      }),
      { dispose: () => { void this.stop(); } },
    );
    if (vscode.workspace.isTrusted === false) {
      context.subscriptions.push(vscode.workspace.onDidGrantWorkspaceTrust(() => { void this.restart(); }));
    }
    void this.restart();
  }

  private restart(): Promise<void> {
    this.lifecycle = this.lifecycle.then(async () => {
      await this.stopRecorders();
      if (this.disposed || !vscode.workspace.isTrusted || !this.context.storageUri) return;
      const configuration = vscode.workspace.getConfiguration('editchain-history');
      if (!configuration.get<boolean>('tracking.enabled', true)) {
        this.updateStatus('Human-work tracking paused'); return;
      }
      for (const folder of vscode.workspace.workspaceFolders ?? []) {
        if (folder.uri.scheme !== 'file') continue;
        const chain = configuration.get<string>('chainDir', '.editchain');
        const namespace = createHash('sha256').update(folder.uri.toString() + '\0' + chain).digest('hex').slice(0, 24);
        const client = new StdioClient();
        client.setLog(line => this.log.appendLine(`[capture] ${line}`));
        const outbox = new EditorOutbox(path.join(this.context.storageUri.fsPath, 'editor-outbox', namespace),
          folder.uri.fsPath, chain, body => {
            client.ensureStarted(resolveServicePath());
            return client.request(body, { timeoutMs: 30000 });
          }, message => this.updateStatus(message));
        const dwell = Math.max(500, Math.min(30000, configuration.get<number>('tracking.readDwellMs', 2000)));
        const maxBytes = Math.max(1024, Math.min(524288, configuration.get<number>('tracking.maxFileBytes', 262144)));
        const capture = new EditorCapture(folder, dwell, maxBytes, event => outbox.push(event));
        const context = observeEditorContext(capture, () => {
          client.ensureStarted(resolveServicePath());
          return client.request({ GetEditorContext: { workspace_path: folder.uri.fsPath, chain_dir: chain } }, { timeoutMs: 30000 });
        }, message => this.log.appendLine(`[capture] ${message}`));
        this.recorders.push({ capture, context, outbox, client, folder, chain });
      }
      if (this.recorders.length) this.updateStatus('Tracking human work');
    }).catch(error => this.updateStatus(`Tracking failed: ${String(error)}`));
    return this.lifecycle;
  }

  private updateStatus(message: string): void {
    const changed = this.status.tooltip !== message;
    this.status.text = message === 'Tracking human work' ? '$(edit) EditChain tracking' : '$(info) EditChain tracking';
    this.status.tooltip = message;
    if (!this.disposed) this.status.show();
    if (changed) this.log.appendLine(`[capture] ${message}`);
  }

  private async showReport(): Promise<unknown> {
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
      const client = recorder?.client ?? new StdioClient();
      client.ensureStarted(resolveServicePath());
      let response;
      try {
        response = await client.request({ GetHumanWork: { workspace_path: folder.uri.fsPath,
          chain_dir: vscode.workspace.getConfiguration('editchain-history').get<string>('chainDir', '.editchain') } }, { timeoutMs: 120000 });
      } finally { if (!recorder) client.stop(); }
      if (!response?.Ok || response.Ok.schema !== 1) throw new Error(JSON.stringify(response?.Error ?? response));
      this.reportText = formatReport(response.Ok);
      const document = await vscode.workspace.openTextDocument(vscode.Uri.parse(`editchain-work:/human-work-${Date.now()}.md`));
      await vscode.window.showTextDocument(document, { preview: true });
      return response.Ok;
    } catch (error) {
      this.log.appendLine(`[capture report] ${String(error)}`);
      await vscode.window.showErrorMessage(`EditChain human work: ${String(error)}`);
      return undefined;
    }
  }

  private async stopRecorders(): Promise<void> {
    const recorders = this.recorders;
    this.recorders = [];
    for (const recorder of recorders) { recorder.context.dispose(); recorder.capture.dispose(); }
    for (const recorder of recorders) {
      try { await recorder.outbox.stop(); } finally { recorder.client.stop(); }
    }
  }

  async stop(): Promise<void> {
    this.disposed = true;
    await this.lifecycle;
    await this.stopRecorders();
  }
}

export function formatReport(report: any): string {
  const coverage = (value: number) => report.ai_lines ? `${value} / ${report.ai_lines} (${(100 * value / report.ai_lines).toFixed(1)}%)` : 'Not yet measurable';
  const escape = (value: string) => value.replace(/[|\r\n]/g, ' ');
  return ['# Human work on AI-generated code', '', report.basis, '',
    `- AI-origin lines matched in current files: **${report.ai_lines}**`,
    `- Reading indicator: **${coverage(report.read_lines)}**`,
    `- Human edited: **${coverage(report.edited_lines)}**`,
    `- Both read indicator and edited: **${coverage(report.read_and_edited_lines)}**`,
    `- Visible at any duration (includes skimming): **${coverage(report.exposed_lines)}**`, '',
    '| File | Matched AI lines | Read indicator | Human edited |', '| --- | ---: | ---: | ---: |',
    ...report.files.map((file: any) => `| ${escape(file.path)} | ${file.ai_lines} | ${file.read_lines} | ${file.edited_lines} |`), '',
    `Historical evidence: ${report.historical_ai_lines} generated lines; ${report.historical_ai_lines_read} with reading indicators; ${report.historical_ai_lines_edited} edited (including subsequently deleted lines).`,
    `Captured ${report.human_changes} human-indicated changes and ${report.events} events.`, '',
    `Coverage gaps: ${report.capture_gaps}. Unsupported AI changes: ${report.unsupported_ai_changes}. Unavailable files: ${report.unavailable_files}.`, '',
    ...report.limitations.map((note: string) => `- ${note}`), '',
    'Run **EditChain: Show Human Work Coverage** again to refresh. Tracking can be paused or resumed from the Command Palette.', ''].join('\n');
}
