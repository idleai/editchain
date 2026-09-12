// Test-only observer. It records synthetic fixture documents in memory; it is
// never loaded by the shipped extension and makes no authorship/read claims.
const vscode = require('vscode');

function activate(context) {
  // These IDs identify API objects, NOT production document incarnations.
  // A language change can emit close/open with the very same document object.
  const ids = new WeakMap();
  const beforeStates = new Map();
  const events = [];
  let nextId = 0;
  let label = 'bootstrap';
  let subscriptions = [];
  const started = process.hrtime.bigint();
  let windowActiveSupported = false;
  let windowActiveProbeError = null;
  try {
    windowActiveSupported = typeof vscode.window.state.active === 'boolean';
  } catch (error) {
    // In 1.85, active exists as a getter that throws unless the windowActivity
    // proposal is enabled. A plain typeof/optional-chain feature check throws.
    if (!String(error).includes('API proposal: windowActivity')) throw error;
    windowActiveProbeError = String(error);
  }
  const id = object => {
    if (!ids.has(object)) ids.set(object, ++nextId);
    return ids.get(object);
  };
  const tracked = document => document.uri.scheme === 'untitled'
    || Boolean(vscode.workspace.getWorkspaceFolder(document.uri));
  const range = value => ({ start: [value.start.line, value.start.character],
    end: [value.end.line, value.end.character] });
  const documentState = document => ({
    id: id(document), uri: document.uri.toString(), version: document.version,
    languageId: document.languageId, eol: document.eol, dirty: document.isDirty,
    untitled: document.isUntitled, closed: document.isClosed, text: document.getText(),
  });
  const editorState = editor => editor ? ({
    id: id(editor), documentId: id(editor.document), uri: editor.document.uri.toString(),
    version: editor.document.version, column: editor.viewColumn ?? null,
    ranges: editor.visibleRanges.map(range), selections: editor.selections.map(range),
  }) : null;
  const tabState = tab => ({
    id: id(tab), groupId: id(tab.group), label: tab.label, active: tab.isActive,
    preview: tab.isPreview, dirty: tab.isDirty, pinned: tab.isPinned,
    kind: tab.input?.constructor.name,
    uri: tab.input?.uri?.toString() ?? null,
    original: tab.input?.original?.toString() ?? null,
    modified: tab.input?.modified?.toString() ?? null,
  });
  const windowState = () => ({ focused: vscode.window.state.focused,
    active: windowActiveSupported ? vscode.window.state.active : null });
  const snapshot = () => ({
    documents: vscode.workspace.textDocuments.filter(tracked).map(documentState),
    editors: vscode.window.visibleTextEditors.map(editorState),
    activeEditor: editorState(vscode.window.activeTextEditor), window: windowState(),
    groups: vscode.window.tabGroups.all.map(group => ({
      id: id(group), column: group.viewColumn, active: group.isActive,
      tabs: group.tabs.map(tabState),
    })),
  });
  const emit = (type, data) => events.push({
    seq: events.length + 1, label, type, observedAt: new Date().toISOString(),
    elapsedMs: Number(process.hrtime.bigint() - started) / 1e6, ...data,
  });
  const stop = () => {
    for (const subscription of subscriptions) subscription.dispose();
    subscriptions = [];
  };
  const start = () => {
    stop();
    beforeStates.clear();
    subscriptions = [
      vscode.workspace.onDidOpenTextDocument(document => {
        if (!tracked(document)) return;
        const state = documentState(document);
        beforeStates.set(document, state);
        emit('document_open', { document: state });
      }),
      vscode.workspace.onDidCloseTextDocument(document => {
        if (!tracked(document)) return;
        emit('document_close', { document: documentState(document) });
        beforeStates.delete(document);
      }),
      vscode.workspace.onDidChangeTextDocument(event => {
        if (!tracked(event.document)) return;
        const entryAt = process.hrtime.bigint();
        const after = documentState(event.document);
        const data = {
          before: beforeStates.get(event.document) ?? null, after,
          keys: Object.keys(event).sort(), reason: event.reason ?? null,
          detailedReason: event.detailedReason ?? null,
          changes: event.contentChanges.map(change => ({
            range: range(change.range), offset: change.rangeOffset,
            length: change.rangeLength, text: change.text,
          })),
        };
        beforeStates.set(event.document, after);
        emit('document_change', data);
        events[events.length - 1].callbackMs = Number(process.hrtime.bigint() - entryAt) / 1e6;
      }),
      vscode.workspace.onWillSaveTextDocument(event => {
        if (tracked(event.document)) emit('will_save', {
          document: documentState(event.document), reason: event.reason,
        });
      }),
      vscode.workspace.onDidSaveTextDocument(document => {
        if (tracked(document)) emit('did_save', { document: documentState(document) });
      }),
      vscode.workspace.onDidRenameFiles(event => emit('files_renamed', {
        files: event.files.map(file => ({ oldUri: file.oldUri.toString(), newUri: file.newUri.toString() })),
      })),
      vscode.window.onDidChangeActiveTextEditor(editor => emit('active_editor', { editor: editorState(editor) })),
      vscode.window.onDidChangeVisibleTextEditors(editors => emit('visible_editors', { editors: editors.map(editorState) })),
      vscode.window.onDidChangeTextEditorSelection(event => emit('selection', {
        editor: editorState(event.textEditor), kind: event.kind ?? null,
      })),
      vscode.window.onDidChangeTextEditorVisibleRanges(event => emit('viewport', {
        editor: editorState(event.textEditor), ranges: event.visibleRanges.map(range), keys: Object.keys(event).sort(),
      })),
      vscode.window.onDidChangeTextEditorViewColumn(event => emit('editor_column', { editor: editorState(event.textEditor) })),
      vscode.window.onDidChangeWindowState(() => emit('window_state', windowState())),
      vscode.window.tabGroups.onDidChangeTabs(event => emit('tabs', {
        opened: event.opened.map(tabState), changed: event.changed.map(tabState), closed: event.closed.map(tabState),
      })),
      vscode.window.tabGroups.onDidChangeTabGroups(() => emit('tab_groups', { groups: snapshot().groups })),
    ];
    const state = snapshot();
    for (const document of vscode.workspace.textDocuments.filter(tracked)) {
      beforeStates.set(document, documentState(document));
    }
    emit('baseline', state);
    return state;
  };
  context.subscriptions.push({ dispose: stop },
    vscode.languages.registerDocumentFormattingEditProvider({ scheme: 'file', pattern: '**/format.txt' }, {
      provideDocumentFormattingEdits(document) {
        return [vscode.TextEdit.replace(new vscode.Range(document.positionAt(0), document.positionAt(document.getText().length)),
          'formatted = true\n')];
      },
    }),
    vscode.languages.registerInlineCompletionItemProvider({ scheme: 'file', pattern: '**/completion.txt' }, {
      provideInlineCompletionItems(document, position) {
        if (document.getText() !== 'const answer = ') return [];
        return [new vscode.InlineCompletionItem('42;', new vscode.Range(position, position))];
      },
    }),
  );
  return {
    start, snapshot, stop,
    mark(value) { label = value; emit('marker', {}); },
    getEvents(scenario, type) {
      return events.filter(event => event.label === scenario && (!type || event.type === type));
    },
    getTrace() {
      return { metadata: {
        vscode: vscode.version, platform: process.platform, arch: process.arch,
        versions: process.versions, remoteName: vscode.env.remoteName ?? null,
        windowActiveSupported, windowActiveProbeError,
        proposedReasonRequested: context.extension.packageJSON.enabledApiProposals?.includes('textDocumentChangeReason') ?? false,
      }, events, current: snapshot() };
    },
  };
}

module.exports = { activate };
