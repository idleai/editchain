import type {} from '@wdio/types';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';

const version = process.env.EDITCHAIN_CAPTURE_VSCODE || '1.137.0';
if (!/^\d+\.\d+\.\d+$/.test(version)) throw new Error('EDITCHAIN_CAPTURE_VSCODE must be an exact release version');
const proposed = process.env.EDITCHAIN_CAPTURE_PROPOSED === '1';
const owner = !process.env.EDITCHAIN_CAPTURE_FIXTURE;
const fixture = process.env.EDITCHAIN_CAPTURE_FIXTURE || fs.mkdtempSync(path.join(os.tmpdir(), 'editchain-capture-'));
process.env.EDITCHAIN_CAPTURE_FIXTURE = fixture;
const workspace = path.join(fixture, 'workspace');
const extension = path.join(fixture, 'probe');
const output = path.resolve('trace', `capture-${version}${proposed ? '-proposed' : ''}`);
process.env.EDITCHAIN_CAPTURE_OUTPUT = output;
if (owner) fs.rmSync(output, { recursive: true, force: true });
fs.mkdirSync(output, { recursive: true });

if (owner) {
  fs.mkdirSync(workspace);
  fs.cpSync(path.join(__dirname, 'capture-probe'), extension, { recursive: true });
  if (proposed) {
    const manifestPath = path.join(extension, 'package.json');
    const manifest = JSON.parse(fs.readFileSync(manifestPath, 'utf8'));
    manifest.enabledApiProposals = ['textDocumentChangeReason'];
    fs.writeFileSync(manifestPath, JSON.stringify(manifest, null, 2));
  }
  const files: Record<string, string> = {
    'startup.txt': 'Already open before recording.\n', 'keyboard.txt': '',
    'unicode.txt': 'A😀B\r\né中\r\nlast\r\n', 'hidden.txt': 'Loaded without an editor.\n',
    'preview-a.txt': 'Preview A\n', 'preview-b.txt': 'Preview B\n',
    'rename.txt': 'Rename and external reload fixture.\n',
    'format.txt': 'formatted=true\n', 'completion.txt': 'const answer = ',
    'lifecycle.txt': 'Open, close, reopen, and move this tab.\n', 'autosave.txt': 'Before autosave.\n',
    'long.txt': Array.from({ length: 1200 }, (_, i) => `line ${String(i).padStart(4, '0')} ${'x'.repeat(160)}`).join('\n'),
    'fold.py': ['def outer():', ...Array.from({ length: 100 }, (_, i) => `    value_${i} = ${i}`), '', 'done = True', ''].join('\n'),
  };
  for (const [name, text] of Object.entries(files)) fs.writeFileSync(path.join(workspace, name), text);
}

export const config: WebdriverIO.Config = {
  outputDir: output, specs: ['./editor-capture.e2e.ts'], maxInstances: 1,
  capabilities: [{ browserName: 'vscode', browserVersion: version,
    'wdio:enforceWebDriverClassic': true,
    'wdio:vscodeOptions': {
      extensionPath: extension, workspacePath: workspace,
      storagePath: path.join(fixture, 'profile'),
      vscodeArgs: proposed ? { enableProposedApi: ['ambientlight.editchain-capture-probe'] } : {},
      userSettings: {
        'security.workspace.trust.enabled': false, 'telemetry.telemetryLevel': 'off',
        'workbench.editor.enablePreview': true, 'workbench.startupEditor': 'none',
        'files.autoSave': 'off', 'files.hotExit': 'off',
        'editor.minimap.enabled': false, 'editor.smoothScrolling': false,
        'editor.wordWrap': 'off', 'editor.fontSize': 14, 'editor.lineHeight': 20,
        'editor.quickSuggestions': false, 'editor.acceptSuggestionOnEnter': 'off',
        'editor.autoClosingBrackets': 'never', 'editor.autoClosingQuotes': 'never',
        'editor.formatOnType': false, 'editor.formatOnPaste': false, 'editor.formatOnSave': false,
        'editor.foldingStrategy': 'indentation', 'editor.stickyScroll.enabled': false,
        'editor.inlineSuggest.enabled': true,
        'window.commandCenter': false,
      },
    },
  }],
  services: ['vscode'], framework: 'mocha',
  mochaOpts: { ui: 'bdd', timeout: 120000 }, logLevel: 'warn',
  onComplete(exitCode) {
    fs.writeFileSync(path.join(output, 'run.json'), JSON.stringify({
      requestedVersion: version, proposed, exitCode, finishedAt: new Date().toISOString(),
    }, null, 2));
    if (owner) fs.rmSync(fixture, { recursive: true, force: true });
  },
};
