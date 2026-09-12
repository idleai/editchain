'use strict';
const { test } = require('node:test');
const assert = require('node:assert/strict');
const Module = require('node:module');

function harness() {
  let now = 0;
  const events = [];
  const signals = {};
  const on = name => listener => { signals[name] = listener; return { dispose() { delete signals[name]; } }; };
  const uri = { scheme: 'file', fsPath: '/workspace/a.ts', toString: () => 'file:///workspace/a.ts' };
  const document = { uri, version: 1, isUntitled: false, text: 'one\ntwo\nthree\nfour\nfive\n', getText() { return this.text; } };
  const range = (start, end) => ({ start: { line: start, character: 0 }, end: { line: end, character: 0 } });
  const editor = { document, visibleRanges: [range(0, 1), range(3, 5)] };
  const vscode = {
    version: '1.85.0',
    TextDocumentChangeReason: { Undo: 1, Redo: 2 }, TextEditorSelectionChangeKind: { Keyboard: 1 },
    TabInputText: class {},
    workspace: {
      textDocuments: [document], onDidOpenTextDocument: on('open'), onDidCloseTextDocument: on('close'),
      onDidChangeTextDocument: on('change'), onDidSaveTextDocument: on('save'), onDidRenameFiles: on('rename'),
    },
    window: {
      state: { focused: true }, activeTextEditor: editor, visibleTextEditors: [editor],
      onDidChangeActiveTextEditor: on('active'), onDidChangeVisibleTextEditors: on('visible'),
      onDidChangeTextEditorVisibleRanges: on('viewport'), onDidChangeTextEditorSelection: on('selection'),
      onDidChangeWindowState: on('focus'), tabGroups: { all: [], onDidChangeTabs: on('tabs') },
    },
  };
  const original = Module._load;
  const filename = require.resolve('../../out/editorCapture');
  delete require.cache[filename];
  Module._load = function(name, ...args) {
    if (name === 'vscode') return vscode;
    if (name === 'node:perf_hooks') return { performance: { now: () => now } };
    return original.call(this, name, ...args);
  };
  let capture;
  try {
    const { EditorCapture } = require(filename);
    capture = new EditorCapture({ uri: { fsPath: '/workspace' }, index: 0 }, 2000, 262144, event => { events.push(event); return true; });
  } finally { Module._load = original; }
  return { capture, events, vscode, document, editor, signals, range, tick: ms => { now += ms; } };
}

test('focus only gates exposure timing; no focus event or folded gap is recorded', () => {
  const env = harness();
  try {
    env.tick(1000);
    env.vscode.window.state.focused = false; env.signals.focus();
    env.tick(60000); env.capture.checkpoint();
    env.vscode.window.state.focused = true; env.signals.focus();
    env.tick(2500); env.capture.checkpoint();
    const exposures = env.events.filter(event => event.event.type === 'code_exposure');
    assert.deepEqual(exposures.map(event => event.event.duration_ms), [1000, 2500]);
    assert.deepEqual(exposures[1].event.ranges, [{ start: [0, 0], end: [1, 0] }, { start: [3, 0], end: [5, 0] }]);
    assert.ok(env.events.every(event => !event.event.type.includes('focus')));
    assert.ok(!JSON.stringify(env.events).includes('focused'));
  } finally { env.capture.dispose(); }
});

test('a changed Git observation splits exposure intervals without changing buffer identity', () => {
  const env = harness();
  try {
    env.tick(1200);
    env.capture.context({ observed_ms: 1200, repositories: [{ repository: '1', root: '/workspace', head: 'a'.repeat(40) }] });
    env.tick(2300); env.capture.checkpoint();
    const events = env.events.filter(event => ['code_exposure', 'workspace_context'].includes(event.event.type));
    assert.deepEqual(events.map(event => event.event.type), ['code_exposure', 'workspace_context', 'code_exposure']);
    assert.deepEqual([events[0].event.duration_ms, events[2].event.duration_ms], [1200, 2300]);
    assert.deepEqual(events[0].event.document, events[2].event.document);
    assert.ok(events.every((event, i) => i === 0 || event.sequence > events[i - 1].sequence));
  } finally { env.capture.dispose(); }
});

test('human indicators reference raw versioned changes; automatic changes stay unattributed', () => {
  const env = harness();
  try {
    env.tick(300);
    const before = env.document.text;
    env.document.text = 'human ' + before; env.document.version = 2;
    env.signals.change({ document: env.document, contentChanges: [{ rangeOffset: 0, rangeLength: 0, text: 'human ' }] });
    env.signals.selection({ textEditor: env.editor, kind: 1, selections: [env.range(0, 0)] });
    env.tick(300);
    env.document.text += 'automatic\n'; env.document.version = 3;
    env.signals.change({ document: env.document, contentChanges: [{ rangeOffset: before.length + 6, rangeLength: 0, text: 'automatic\n' }] });
    env.tick(300);
    env.signals.selection({ textEditor: env.editor, kind: 1, selections: [env.range(1, 1)] });
    const changes = env.events.filter(event => event.event.type === 'document_changed');
    const human = env.events.filter(event => event.event.type === 'human_edit');
    assert.equal(changes.length, 2);
    assert.equal(changes[0].event.before, before);
    assert.equal(changes[0].event.before_version, 1);
    assert.equal(changes[0].event.document.version, 2);
    assert.deepEqual(human.map(event => event.event.change), [changes[0].sequence]);
    env.tick(2500); env.capture.checkpoint();
    assert.equal(env.events.at(-1).event.document.version, 3, 'exposure is bound to the new buffer version');
  } finally { env.capture.dispose(); }
});

test('closed and reopened document objects receive a new incarnation and hidden tabs earn no exposure', () => {
  const env = harness();
  try {
    const initial = env.events.find(event => event.event.type === 'document_snapshot');
    env.signals.close(env.document);
    env.signals.open(env.document);
    const snapshots = env.events.filter(event => event.event.type === 'document_snapshot');
    assert.notEqual(snapshots[1].event.document.id, initial.event.document.id, 'language changes can reuse the same API object');
    env.vscode.window.activeTextEditor = undefined; env.vscode.window.visibleTextEditors = [];
    env.signals.active(undefined);
    env.tick(5000); env.capture.checkpoint();
    assert.equal(env.events.filter(event => event.event.type === 'code_exposure').length, 0);
  } finally { env.capture.dispose(); }
});
