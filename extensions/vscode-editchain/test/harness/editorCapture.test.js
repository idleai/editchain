'use strict';
const { test } = require('node:test');
const assert = require('node:assert/strict');
const Module = require('node:module');

function harness(dwell = 2000, identity) {
  let now = 0;
  const timers = new Set();
  const clock = {
    setTimeout(callback, delay) {
      const timer = { callback, at: now + delay, unref() {} };
      timers.add(timer); return timer;
    },
    clearTimeout(timer) { timers.delete(timer); },
  };
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
    TabInputText: class { constructor(uri) { this.uri = uri; } },
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
  const tab = { input: new vscode.TabInputText(uri) };
  vscode.window.tabGroups.all = [{ tabs: [tab] }];
  const original = Module._load;
  const filename = require.resolve('../../out/editorCapture');
  delete require.cache[filename];
  Module._load = function(name, ...args) {
    if (name === 'vscode') return vscode;
    if (name === 'node:perf_hooks') return { performance: { now: () => now } };
    if (name === 'node:timers') return clock;
    return original.call(this, name, ...args);
  };
  let capture;
  try {
    const { EditorCapture } = require(filename);
    capture = new EditorCapture({ uri: { fsPath: '/workspace' }, index: 0 }, dwell, 262144, event => { events.push(event); return true; }, identity);
  } finally { Module._load = original; }
  const tick = ms => {
    const until = now + ms;
    for (;;) {
      const due = [...timers].filter(timer => timer.at <= until).sort((a, b) => a.at - b.at)[0];
      if (!due) break;
      timers.delete(due); now = Math.max(now, due.at); due.callback();
    }
    now = until;
  };
  return { capture, events, vscode, document, editor, tab, signals, range, timers, tick,
    elapse: ms => { now += ms; }, reads: () => events.filter(event => event.event.type === 'code_read') };
}

test('fresh capture sessions retain the same unsigned identity on every event', () => {
  const identity = { kind: 'unsigned', guid: '99999999-9999-4999-8999-999999999999', stream: 'a'.repeat(24) };
  const first = harness(2000, identity);
  first.tick(2500); first.capture.dispose();
  const second = harness(2000, identity);
  second.tick(2500); second.capture.dispose();
  assert.notEqual(first.events[0].session, second.events[0].session);
  for (const events of [first.events, second.events]) {
    assert.equal(events[0].sequence, 1);
    assert.equal(events[0].event.activity_schema, 3);
    assert.ok(events.every(event => JSON.stringify(event.identity) === JSON.stringify(identity)));
    assert.ok(events.some(event => event.event.type === 'code_read'));
  }
});

test('focus only gates local timing; only qualified reads with disjoint visible ranges are recorded', () => {
  const env = harness();
  try {
    env.tick(1000);
    env.vscode.window.state.focused = false; env.signals.focus();
    env.tick(60000); env.capture.checkpoint();
    env.vscode.window.state.focused = true; env.signals.focus();
    env.tick(2500); env.capture.checkpoint();
    assert.deepEqual(env.reads().map(event => event.event.duration_ms), [2000]);
    assert.deepEqual(env.reads()[0].event.ranges, [{ start: [0, 0], end: [1, 0] }, { start: [3, 0], end: [5, 0] }]);
    assert.ok(env.events.every(event => !['code_exposure', 'selection_changed', 'visible_ranges_changed'].includes(event.event.type)));
    assert.ok(env.events.every(event => !event.event.type.includes('focus')));
    assert.ok(!JSON.stringify(env.events).includes('focused'));
  } finally { env.capture.dispose(); }
});

test('a changed Git observation splits read intervals without changing buffer identity', () => {
  const env = harness();
  try {
    env.elapse(2200);
    env.capture.context({ observed_ms: 2200, repositories: [{ repository: '1', root: '/workspace', head: 'a'.repeat(40) }] });
    env.tick(2300); env.capture.checkpoint();
    const events = env.events.filter(event => ['code_read', 'workspace_context'].includes(event.event.type));
    assert.deepEqual(events.map(event => event.event.type), ['code_read', 'workspace_context', 'code_read']);
    assert.deepEqual([events[0].event.duration_ms, events[2].event.duration_ms], [2200, 2000]);
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
    assert.equal(env.reads().length, 1);
    assert.equal(env.reads()[0].event.document.version, 3, 'read is bound to the new buffer version');
    assert.ok(env.events.every(event => event.event.type !== 'selection_changed'));
  } finally { env.capture.dispose(); }
});

test('closed and reopened document objects receive a new incarnation and hidden tabs earn no reads', () => {
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
    assert.equal(env.reads().length, 0);
  } finally { env.capture.dispose(); }
});

test('ten minutes at an unchanged viewport emit one read automatically, including repeated checkpoints', () => {
  const env = harness();
  try {
    env.tick(1999); env.capture.checkpoint();
    assert.equal(env.reads().length, 0);
    env.tick(1);
    assert.equal(env.reads().length, 1, 'read arrives without a report or editor boundary');
    for (let minute = 0; minute < 10; minute++) {
      env.tick(minute === 0 ? 58000 : 60000);
      env.capture.checkpoint(); env.signals.viewport(); env.signals.focus();
    }
    assert.equal(env.reads().length, 1, 'no heartbeat or duplicate notification reads');
    assert.equal(env.timers.size, 0, 'no recurring viewing timer after qualification');
    assert.equal(env.events.length, 4, 'only startup, baseline, tab open, and one read');
  } finally { env.capture.dispose(); }
});

test('brief visits do not accumulate and duplicate viewport notifications preserve the active interval', () => {
  const env = harness();
  try {
    env.tick(1500);
    env.editor.visibleRanges = [env.range(2, 3)]; env.signals.viewport();
    env.tick(1500);
    env.editor.visibleRanges = [env.range(0, 1)]; env.signals.viewport();
    env.tick(1500); env.signals.viewport(); env.capture.checkpoint();
    assert.equal(env.reads().length, 0);
    env.tick(500);
    assert.deepEqual(env.reads().map(event => event.event.ranges), [[{ start: [0, 0], end: [1, 0] }]]);
  } finally { env.capture.dispose(); }
});

test('background edits and split-pane viewport changes do not reset the active read timer', () => {
  const env = harness();
  try {
    const document = { ...env.document, uri: { scheme: 'file', fsPath: '/workspace/b.ts', toString: () => 'file:///workspace/b.ts' } };
    const other = { document, visibleRanges: [env.range(0, 1)] };
    env.signals.open(document);
    env.vscode.window.visibleTextEditors.push(other); env.signals.visible();
    env.tick(1500);
    document.version++; document.text = 'automatic\n';
    env.signals.change({ document, contentChanges: [{ rangeOffset: 0, rangeLength: env.document.text.length, text: document.text }] });
    env.signals.viewport({ textEditor: other });
    env.tick(500);
    assert.equal(env.reads().length, 1);
    assert.equal(env.reads()[0].event.document.uri, env.document.uri.toString());
    assert.equal(env.reads()[0].event.duration_ms, 2000);
  } finally { env.capture.dispose(); }
});

test('tab close records lifecycle, flushes a delayed qualified read, and does not require document unload', () => {
  for (const duration of [100, 2200]) {
    const env = harness();
    try {
      env.elapse(duration);
      env.vscode.window.activeTextEditor = undefined;
      env.vscode.window.visibleTextEditors = [];
      env.signals.tabs({ opened: [], closed: [env.tab] });
      const opened = env.events.find(event => event.event.type === 'editor_opened');
      const closed = env.events.find(event => event.event.type === 'editor_closed');
      assert.equal(closed.event.editor, opened.event.editor);
      assert.equal(closed.event.uri, opened.event.uri);
      assert.equal(closed.event.path, 'a.ts');
      assert.equal(opened.event.path, 'a.ts');
      assert.equal(env.events[0].event.activity_schema, 2);
      assert.equal(env.reads().length, duration >= 2000 ? 1 : 0);
      if (env.reads().length) assert.ok(env.reads()[0].sequence < closed.sequence);
      env.tick(10000); env.capture.checkpoint();
      assert.equal(env.reads().length, duration >= 2000 ? 1 : 0, 'loaded document alone earns no reading');
    } finally { env.capture.dispose(); }
  }
});

test('split tabs have independent open/close identities and closing a background split preserves the active interval', () => {
  const env = harness();
  try {
    const tab = { input: new env.vscode.TabInputText(env.document.uri) };
    env.signals.tabs({ opened: [tab], closed: [] });
    env.tick(1500); env.signals.tabs({ opened: [], closed: [tab] });
    env.tick(500);
    const opened = env.events.filter(event => event.event.type === 'editor_opened');
    const closed = env.events.find(event => event.event.type === 'editor_closed');
    assert.notEqual(opened[0].event.editor, opened[1].event.editor);
    assert.equal(closed.event.editor, opened[1].event.editor);
    assert.equal(env.reads().length, 1);
    assert.equal(env.reads()[0].event.duration_ms, 2000);
  } finally { env.capture.dispose(); }
});

test('configured dwell boundaries, early timer callbacks, and orderly shutdown never lose or duplicate qualified reads', () => {
  for (const dwell of [500, 2000, 30000]) {
    const env = harness(dwell);
    env.elapse(dwell - 0.5);
    const timer = [...env.timers][0];
    env.timers.delete(timer); timer.callback();
    assert.equal(env.reads().length, 0);
    assert.equal(env.timers.size, 1, 'an early callback rearms the remaining dwell');
    env.elapse(1); env.capture.dispose();
    assert.equal(env.reads().length, 1, 'shutdown flushes even before the rearmed callback runs');
    env.tick(60000);
    assert.equal(env.reads().length, 1);
    assert.equal(env.events.at(-1).event.type, 'tracking_stopped');
    assert.equal(env.timers.size, 0);
  }
});
