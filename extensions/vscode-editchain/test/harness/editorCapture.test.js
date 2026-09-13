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
  const document = { uri, version: 1, isUntitled: false, text: 'one\ntwo\nthree\nfour\nfive\n', getText() { return this.text; },
    offsetAt(position) { return this.text.split('\n').slice(0, position.line).reduce((size, line) => size + line.length + 1, 0) + position.character; } };
  const range = (start, end) => ({ start: { line: start, character: 0 }, end: { line: end, character: 0 } });
  const editor = { document, visibleRanges: [range(0, 1), range(3, 5)] };
  const vscode = {
    version: '1.85.0',
    extensions: { getExtension: () => ({ packageJSON: { version: '0.1.6' } }) },
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
  delete require.cache[require.resolve('../../out/editorEdits')];
  delete require.cache[require.resolve('../../out/editorTabs')];
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
  const selection = (offset, end = offset) => ({ isEmpty: offset === end, active: { line: 0, character: end } });
  return { capture, events, vscode, document, editor, tab, signals, range, selection, timers, tick,
    elapse: ms => { now += ms; }, reads: () => events.filter(event => event.event.type === 'code_read') };
}

function type(env, text, explicit = false) {
  const offset = env.document.text.indexOf('\n');
  env.editor.selections = [env.selection(offset)];
  env.document.text = env.document.text.slice(0, offset) + text + env.document.text.slice(offset);
  env.document.version++;
  env.signals.change({ document: env.document, contentChanges: [{ rangeOffset: offset, rangeLength: 0, text }],
    ...(explicit ? { detailedReason: { source: 'cursor', metadata: { kind: 'type' } } } : {}) });
  env.signals.selection({ textEditor: env.editor, kind: 1, selections: [env.selection(offset + text.length)] });
  env.editor.selections = [env.selection(offset + text.length)];
}

const editEvents = env => env.events.filter(event => ['human_edit', 'human_edit_batch'].includes(event.event.type));
const receipts = env => editEvents(env).flatMap(event => event.event.type === 'human_edit' ? [event]
  : event.event.edits.map(edit => ({ ...event, event: { type: 'human_edit', ...edit } })));
const editGroups = env => {
  const groups = new Map();
  for (const event of editEvents(env)) {
    const key = event.event.group ?? event.sequence;
    groups.set(key, [...(groups.get(key) ?? []), ...(event.event.edits ?? [event.event])]);
  }
  return [...groups.values()];
};

test('corrections of just-typed text remain in one edit without claiming existing or agent-deleted code', () => {
  for (const explicitAgent of [false, true]) {
    const env = harness();
    try {
      type(env, '😀x');
      const offset = env.document.text.indexOf('\n') - 1;
      env.document.text = env.document.text.slice(0, offset) + env.document.text.slice(offset + 1);
      env.document.version++;
      env.signals.change({ document: env.document, contentChanges: [{ rangeOffset: offset, rangeLength: 1, text: '' }],
        ...(explicitAgent ? { detailedReason: { source: 'applyEdits' } } : {}) });
      env.tick(100);
      assert.deepEqual(receipts(env).map(value => value.event.signal),
        explicitAgent ? ['keyboard_selection'] : ['keyboard_selection', 'typing_correction']);
      assert.equal(editGroups(env).length, 1);
      env.document.text = env.document.text.slice(1); env.document.version++;
      env.signals.change({ document: env.document, contentChanges: [{ rangeOffset: 0, rangeLength: 1, text: '' }] });
      env.signals.selection({ textEditor: env.editor, kind: undefined, selections: [env.selection(0)] });
      env.signals.save(env.document);
      assert.equal(receipts(env).length, explicitAgent ? 1 : 2, 'deleting pre-existing code never inherits a typing receipt');
    } finally { env.capture.dispose(); }
  }
});

test('reading and background save/disposal do not end the active edit', () => {
  const env = harness();
  try {
    type(env, 'a'); env.tick(2200);
    assert.equal(env.reads()[0].event.group, editEvents(env)[0].event.group);
    const other = { ...env.document, uri: { scheme: 'file', fsPath: '/workspace/b.ts', toString: () => 'file:///workspace/b.ts' } };
    env.signals.open(other);
    const length = other.text.length;
    other.text = 'agent'; other.version++;
    env.signals.change({ document: other, contentChanges: [{ rangeOffset: 0, rangeLength: length, text: 'agent' }],
      detailedReason: { source: 'applyEdits' } });
    env.signals.save(other);
    env.signals.close(other);
    env.signals.close({ uri: { scheme: 'output', toString: () => 'output:internal' } });
    type(env, 'b'); env.tick(100);
    assert.equal(editGroups(env).length, 1);
    assert.equal(receipts(env).length, 2);
    env.signals.save(env.document);
    type(env, 'c');
    assert.equal(editGroups(env).length, 2, 'saving the edited file finishes that edit');
  } finally { env.capture.dispose(); }
});

test('a typing burst produces one edit with every raw revision, then one read without a heartbeat', () => {
  for (const explicit of [false, true]) {
    const env = harness();
    try {
      for (const char of 'humanwork') {
        type(env, char, explicit); env.signals.active(env.editor); env.tick(100);
      }
      assert.equal(receipts(env).length, 9, 'every input is published while typing');
      env.tick(1000);
      const changes = env.events.filter(event => event.event.type === 'document_changed');
      assert.equal(changes.length, 9);
      const edits = editGroups(env);
      assert.equal(edits.length, 1);
      assert.deepEqual(edits[0], changes.map(event => ({ change: event.sequence, signal: explicit ? 'editor_input' : 'keyboard_selection' })));
      env.signals.save(env.document);
      env.tick(600000); env.capture.checkpoint();
      assert.equal(editGroups(env).length, 1, 'save and elapsed time do not duplicate the edit');
      assert.equal(receipts(env).length, 9);
      assert.equal(env.reads().length, 1);
      assert.equal(env.timers.size, 0);
    } finally { env.capture.dispose(); }
  }
});

test('an automatic or unconfirmed mutation splits typing bursts without gaining human attribution', () => {
  for (const explicit of [false, true]) {
    const env = harness();
    try {
      type(env, 'a', explicit); env.tick(100); type(env, 'b', explicit);
      const offset = env.document.text.length;
      env.document.text += 'AGENT\n'; env.document.version++;
      env.signals.change({ document: env.document, contentChanges: [{ rangeOffset: offset, rangeLength: 0, text: 'AGENT\n' }],
        ...(explicit ? { detailedReason: { source: 'unknown' } } : {}) });
      env.signals.selection({ textEditor: env.editor, kind: undefined, selections: [env.selection(5)] });
      type(env, 'c', explicit); env.tick(100); type(env, 'd', explicit);
      env.signals.save(env.document);
      const changes = env.events.filter(event => event.event.type === 'document_changed');
      const edits = editGroups(env);
      assert.equal(edits.length, 2);
      assert.deepEqual(edits.map(group => group.map(edit => edit.change)),
        [[changes[0].sequence, changes[1].sequence], [changes[3].sequence, changes[4].sequence]]);
    } finally { env.capture.dispose(); }
  }
});

test('save, focus, editor, context, capture gaps and shutdown finalize pending typing', () => {
  for (const boundary of ['save', 'focus', 'editor', 'context', 'gap', 'shutdown']) {
    const env = harness();
    try {
      type(env, 'a'); env.tick(100); type(env, 'b');
      if (boundary === 'save') env.signals.save(env.document);
      if (boundary === 'focus') { env.vscode.window.state.focused = false; env.signals.focus(); }
      if (boundary === 'editor') env.signals.active(undefined);
      if (boundary === 'context') env.capture.context({ observed_ms: 100, repositories: [] });
      if (boundary === 'gap') {
        env.document.text = 'x'.repeat(262145); env.document.version++;
        env.signals.change({ document: env.document, contentChanges: [{ rangeOffset: 0, rangeLength: 26, text: env.document.text }] });
      }
      if (boundary === 'shutdown') env.capture.dispose();
      assert.equal(editGroups(env).length, 1, boundary);
      assert.equal(editGroups(env)[0].length, 2);
      env.tick(60000); assert.equal(editGroups(env).length, 1, 'no timer duplicates the completed burst');
    } finally { env.capture.dispose(); }
  }
});

test('uninterrupted typing publishes immediately and retains one group across ordinary pauses', () => {
  const env = harness();
  try {
    for (let index = 0; index < 60; index++) {
      type(env, 'x'); env.tick(100);
      assert.equal(receipts(env).length, index + 1, 'publication follows input within one frame');
      env.tick(400);
    }
    assert.equal(editGroups(env).length, 1);
    assert.equal(editGroups(env)[0].length, 60);
    type(env, 'y'); env.tick(1000);
    assert.equal(editGroups(env).length, 1);
    assert.equal(receipts(env).length, 61);
  } finally { env.capture.dispose(); }
});

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
    assert.equal(events[0].event.extension_version, '0.1.6');
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

test('a changed Git observation flushes a qualified read without rereading the unchanged view', () => {
  const env = harness();
  try {
    env.elapse(2200);
    env.capture.context({ observed_ms: 2200, repositories: [{ repository: '1', root: '/workspace', head: 'a'.repeat(40) }] });
    env.tick(2300); env.capture.checkpoint();
    const events = env.events.filter(event => ['code_read', 'workspace_context'].includes(event.event.type));
    assert.deepEqual(events.map(event => event.event.type), ['code_read', 'workspace_context']);
    assert.equal(events[0].event.duration_ms, 2200);
    assert.equal(env.timers.size, 0, 'Git metadata alone does not rearm reading');
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
    env.signals.selection({ textEditor: env.editor, kind: 1, selections: [env.selection(6)] });
    env.tick(300);
    env.document.text += 'automatic\n'; env.document.version = 3;
    env.signals.change({ document: env.document, contentChanges: [{ rangeOffset: before.length + 6, rangeLength: 0, text: 'automatic\n' }] });
    env.tick(300);
    env.signals.selection({ textEditor: env.editor, kind: 1, selections: [env.range(1, 1)] });
    const changes = env.events.filter(event => event.event.type === 'document_changed');
    const human = receipts(env);
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

test('rapid interleaved automatic and keyboard changes attribute only the matching revision once', () => {
  const env = harness();
  try {
    env.document.text = 'agent\n'; env.document.version++;
    env.signals.change({ document: env.document, contentChanges: [{ rangeOffset: 0, rangeLength: 24, text: 'agent\n' }] });
    const automatic = env.events.at(-1);
    env.document.text = 'agent!\n'; env.document.version++;
    env.signals.change({ document: env.document, contentChanges: [{ rangeOffset: 5, rangeLength: 0, text: '!' }] });
    const typed = env.events.at(-1);
    const event = { textEditor: env.editor, kind: 1, selections: [env.selection(6)] };
    env.signals.selection(event); env.signals.selection(event);
    env.signals.save(env.document);
    assert.deepEqual(receipts(env).map(event => event.event.change), [typed.sequence]);
    assert.equal(automatic.event.type, 'document_changed');
    assert.equal(env.events.at(-1).event.type, 'document_saved');
  } finally { env.capture.dispose(); }
});

test('unrelated navigation and interruptions cannot claim an earlier programmatic change', () => {
  for (const boundary of ['unknown selection', 'command selection', 'wrong caret', 'range selection', 'save', 'focus', 'activation', 'split', 'context']) {
    const env = harness();
    try {
      env.document.text = 'agent'; env.document.version++;
      env.signals.change({ document: env.document, contentChanges: [{ rangeOffset: 0, rangeLength: 24, text: 'agent' }] });
      const matching = { textEditor: env.editor, kind: 1, selections: [env.selection(5)] };
      if (boundary === 'unknown selection') env.signals.selection({ ...matching, kind: undefined });
      if (boundary === 'command selection') env.signals.selection({ ...matching, kind: 3 });
      if (boundary === 'wrong caret') env.signals.selection({ ...matching, selections: [env.selection(2)] });
      if (boundary === 'range selection') env.signals.selection({ ...matching, selections: [env.selection(0, 5)] });
      if (boundary === 'save') env.signals.save(env.document);
      if (boundary === 'focus') {
        env.vscode.window.state.focused = false; env.signals.focus(); env.vscode.window.state.focused = true;
      }
      if (boundary === 'activation') env.signals.active(undefined);
      if (boundary === 'context') env.capture.context({ observed_ms: 0, repositories: [] });
      if (boundary === 'split') env.signals.selection({ ...matching, textEditor: { ...env.editor } });
      env.signals.selection(matching);
      assert.equal(receipts(env).length, 0, boundary);
    } finally { env.capture.dispose(); }
  }
});

test('multi-cursor keyboard edits use the resulting UTF-16 caret positions', () => {
  const env = harness();
  try {
    env.document.text = 'A😀B'; env.document.version++;
    env.signals.change({ document: env.document, contentChanges: [{ rangeOffset: 0, rangeLength: 24, text: 'A😀B' }] });
    env.document.text = 'A😀!B?'; env.document.version++;
    env.signals.change({ document: env.document, contentChanges: [
      { rangeOffset: 4, rangeLength: 0, text: '?' }, { rangeOffset: 3, rangeLength: 0, text: '!' },
    ] });
    env.signals.active(env.editor); env.signals.focus();
    env.signals.selection({ textEditor: env.editor, kind: 1, selections: [env.selection(4), env.selection(6)] });
    env.tick(1000);
    assert.equal(receipts(env).length, 1);
  } finally { env.capture.dispose(); }
});

test('explicit editor input attributes the exact change without a selection or save', () => {
  for (const kind of ['type', 'paste', 'cut', 'compositionType', 'compositionEnd', 'executeCommand', 'executeCommands']) {
    const env = harness();
    try {
      env.document.text = 'ne\ntwo\nthree\nfour\nfive\n'; env.document.version++;
      env.signals.change({ document: env.document, contentChanges: [{ rangeOffset: 0, rangeLength: 1, text: '' }],
        detailedReason: { source: 'cursor', metadata: { kind, detailedSource: 'deleteRight' } } });
      const changed = env.events.find(event => event.event.type === 'document_changed');
      assert.equal(changed.event.origin.source, 'cursor');
      assert.equal(changed.event.origin.kind, kind);
      env.tick(1000);
      const human = receipts(env);
      assert.deepEqual(human.map(event => event.event), [{ type: 'human_edit', change: changed.sequence, signal: 'editor_input' }]);
      env.signals.selection({ textEditor: env.editor, kind: 1, selections: [env.selection(0)] });
      env.signals.save(env.document);
      assert.deepEqual(receipts(env), human, kind);
    } finally { env.capture.dispose(); }
  }
});

test('programmatic, provider, missing and unfamiliar origins cannot be promoted by nearby keyboard input', () => {
  for (const detailedReason of [undefined, null, 42,
    ...['unknown', 'reloadFromDisk', 'inlineCompletionAccept', 'inlineCompletionPartialAccept',
      'Chat.applyEdits', 'inlineChat.applyEdits', 'Chat.undoEdits', 'snippet', 'suggest', 'codeAction', 'future-source']
      .map(source => ({ source, metadata: { name: 'formatEditsCommand', $extensionId: 'fixture.agent' } })),
    { source: 'cursor', metadata: { kind: 'future-operation' } }, { source: 'cursor', metadata: null },
  ]) {
    const env = harness();
    try {
      env.document.text = 'agent'; env.document.version++;
      env.signals.change({ document: env.document, contentChanges: [{ rangeOffset: 0, rangeLength: 24, text: 'agent' }], detailedReason });
      env.signals.selection({ textEditor: env.editor, kind: 1, selections: [env.selection(5)] });
      env.signals.save(env.document);
      assert.equal(receipts(env).length, 0, JSON.stringify(detailedReason));
      assert.ok(env.events.find(event => event.event.type === 'document_changed').event.origin);
    } finally { env.capture.dispose(); }
  }
});

test('origin-based input still requires the focused active document; undo and redo retain their reason', () => {
  for (const mode of ['unfocused', 'background', 'undo', 'redo', 'agent undo']) {
    const env = harness();
    try {
      if (mode === 'unfocused') env.vscode.window.state.focused = false;
      if (mode === 'background') env.vscode.window.activeTextEditor = undefined;
      env.document.text = 'one'; env.document.version++;
      const reason = mode === 'redo' ? 2 : mode.includes('undo') ? 1 : undefined;
      const source = mode === 'agent undo' ? 'Chat.undoEdits' : reason ? 'applyEdits' : 'cursor';
      env.signals.change({ document: env.document, contentChanges: [{ rangeOffset: 3, rangeLength: 21, text: '' }],
        reason, detailedReason: { source, metadata: { kind: 'type' } } });
      assert.deepEqual(receipts(env).map(event => event.event.signal),
        mode === 'undo' || mode === 'redo' ? [mode] : [], mode);
    } finally { env.capture.dispose(); }
  }
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

test('an unchanged view stays read through ten minutes of focus, activation, Git changes, and editor switches', () => {
  const env = harness();
  try {
    env.tick(2000);
    for (let minute = 0; minute < 10; minute++) {
      env.signals.active(env.editor);
      env.vscode.window.state.focused = false; env.signals.focus();
      env.tick(1000);
      env.vscode.window.state.focused = true; env.signals.focus();
      env.vscode.window.activeTextEditor = undefined; env.signals.active(undefined);
      env.tick(1000);
      // VS Code may replace the TextEditor wrapper when a hidden tab returns.
      const editor = { ...env.editor };
      env.vscode.window.visibleTextEditors = [editor];
      env.vscode.window.activeTextEditor = editor; env.signals.active(editor);
      env.capture.context({ observed_ms: minute * 60000, repositories: [{ head: String(minute).repeat(40) }] });
      env.tick(58000); env.capture.checkpoint();
      assert.equal(env.reads().length, 1);
      assert.equal(env.timers.size, 0, 'already read view has no pending timer');
    }
  } finally { env.capture.dispose(); }
});

test('scroll and buffer revisions rearm reading, while repeated activation preserves pending dwell', () => {
  const env = harness();
  try {
    env.tick(1500); env.signals.active(env.editor); env.tick(500);
    assert.equal(env.reads().length, 1, 'same-editor notification cannot restart a pending timer');
    env.editor.visibleRanges = [env.range(2, 3)]; env.signals.viewport();
    env.tick(1999); assert.equal(env.reads().length, 1);
    env.tick(1); assert.equal(env.reads().length, 2, 'new viewport must qualify');
    const before = env.document.text;
    env.document.text = 'changed\n' + before; env.document.version = 2;
    env.signals.change({ document: env.document, contentChanges: [{ rangeOffset: 0, rangeLength: 0, text: 'changed\n' }] });
    env.tick(2000);
    assert.equal(env.reads().length, 3, 'new code revision can qualify at the same scroll position');
    assert.equal(env.reads().at(-1).event.document.version, 2);
    env.tick(600000); env.capture.checkpoint();
    assert.equal(env.reads().length, 3);
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
      env.vscode.window.tabGroups.all = [{ tabs: [] }];
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

test('split tabs and duplicate notifications produce one file lifecycle and preserve reading', () => {
  const env = harness();
  try {
    const tab = { input: new env.vscode.TabInputText(env.document.uri) };
    env.vscode.window.tabGroups.all[0].tabs.push(tab);
    env.signals.tabs({ opened: [tab], closed: [] });
    env.signals.tabs({ opened: [tab], closed: [] });
    env.tick(1500);
    env.vscode.window.tabGroups.all[0].tabs.pop();
    env.signals.tabs({ opened: [], closed: [tab] });
    env.tick(500);
    const opened = env.events.filter(event => event.event.type === 'editor_opened');
    const closed = env.events.find(event => event.event.type === 'editor_closed');
    assert.equal(opened.length, 1);
    assert.equal(opened[0].event.restored, true, 'startup inventory is not a fresh open action');
    assert.equal(closed, undefined, 'file remains open in the first split');
    assert.equal(env.reads().length, 1);
    assert.equal(env.reads()[0].event.duration_ms, 2000);
    env.vscode.window.tabGroups.all[0].tabs = [];
    env.signals.tabs({ opened: [], closed: [env.tab] });
    env.vscode.window.tabGroups.all[0].tabs = [tab];
    env.signals.tabs({ opened: [tab], closed: [] });
    env.signals.tabs({ opened: [tab], closed: [] });
    assert.equal(env.events.filter(event => event.event.type === 'editor_closed').length, 1);
    assert.equal(env.events.filter(event => event.event.type === 'editor_opened' && !event.event.restored).length, 1);
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
