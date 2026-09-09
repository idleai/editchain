// Deterministic protocol fixtures for the EditChain history webview harness.
//
// These model *protocol responses* (the shapes in crates/editchain-protocol),
// not copied DOM. The fixture bridge slices/dispatches them in response to
// requests from the renderer bootstrap.
//
// Row shape (HistoryRow): op_id?, git_oid?, repository?, summary, timestamp_ms,
//   group, group_end, node_key, parents[], is_submodule, is_system, author,
//   commit_id, kind
// Layout shape (GraphLayout): { rows:[{node,lane}], edges:[{child,parent,points:[{row,lane}]}] }
//
// Identifier contract (editchain-protocol): op_id is "node:boot:seq", git_oid
// is lowercase hex, and repository is an exact DECIMAL RepositoryId string.
// u64 identifiers above 2^53 (e.g. 9007199254740993) must never be numbers in
// protocol payloads — JavaScript doubles would round them. Fixture git rows
// use a large exact repository string below so every git row click exercises
// the exact-string navigation path.

(function () {
  'use strict';

  // Fixed deterministic clock (2026-01-15T12:00:00Z). Fixture timestamps must
  // be stable across runs and hosts so harness assertions never depend on the
  // wall clock. Date rendering still depends on the host timezone/locale — the
  // layout probe computes expectations with explicit Intl options instead of
  // hardcoding a timezone-specific string.
  const NOW = Date.UTC(2026, 0, 15, 12, 0, 0);

  function gitRow(key, summary, opts) {
    opts = opts || {};
    return {
      op_id: null,
      git_oid: key,
      repository: opts.repository !== undefined ? opts.repository : '9007199254740993',
      summary,
      timestamp_ms: opts.ts !== undefined ? opts.ts : NOW - key.length * 1000,
      group: opts.group !== undefined ? opts.group : 'repo:0',
      node_key: key,
      parents: opts.parents || [],
      is_submodule: !!opts.is_submodule,
      is_system: !!opts.is_system,
      author: opts.author || 'ambientlight',
      commit_id: key.slice(0, 7),
      kind: opts.kind || 'git',
      // Additive r4 contract fields (HistoryRow): EXACT Rust wire taxonomy
      // (crates/editchain-project/taxonomy.rs) — lowercase snake_case,
      // defaulted to the conservative values the service emits.
      record_role: opts.record_role || 'artifact',
      activity_kind: opts.activity_kind || 'source_control',
      visibility: opts.visibility || 'primary',
      outcome: opts.outcome || 'unknown',
      turn_id: opts.turn_id || '',
    };
  }

  function opRow(key, summary, opts) {
    opts = opts || {};
    return {
      op_id: key,
      git_oid: null,
      repository: null,
      summary,
      timestamp_ms: opts.ts !== undefined ? opts.ts : NOW - key.length * 1000,
      group: opts.group !== undefined ? opts.group : 'session:s1',
      node_key: key,
      parents: opts.parents || [],
      is_submodule: false,
      is_system: !!opts.is_system,
      author: opts.author || '',
      commit_id: key,
      kind: opts.kind || 'message',
      // Additive r4 contract fields (HistoryRow): EXACT Rust wire taxonomy
      // (crates/editchain-project/taxonomy.rs). Ops default by kind: messages
      // are narrative/conversation turns; tool and command rows are execute
      // activity (tool = result, command = action). Trace VISIBILITY marks
      // internal bookkeeping rows hidden from Activity — record_role has no
      // "trace" variant.
      record_role: opts.record_role !== undefined ? opts.record_role
        : (opts.kind === 'tool' ? 'result' : opts.kind === 'command' ? 'action' : 'narrative'),
      activity_kind: opts.activity_kind !== undefined ? opts.activity_kind
        : (opts.kind === 'tool' || opts.kind === 'command') ? 'execute' : 'conversation',
      visibility: opts.visibility || 'primary',
      outcome: opts.outcome || 'unknown',
      turn_id: opts.turn_id !== undefined ? opts.turn_id : key,
    };
  }

  // A linear chain of git commits a -> b -> c -> d (newest first).

  function mergeGraph() {
    const keys = ['git:m3', 'git:m2', 'git:f1', 'git:m1', 'git:m0'];
    const rows = [
      gitRow('git:m3', 'merge feature into main', { parents: ['git:m2', 'git:f1'], ts: NOW }),
      gitRow('git:m2', 'main work two', { parents: ['git:m1'], ts: NOW - 60_000 }),
      gitRow('git:f1', 'feature work one', { parents: ['git:m1'], ts: NOW - 90_000 }),
      gitRow('git:m1', 'main work one', { parents: ['git:m0'], ts: NOW - 120_000 }),
      gitRow('git:m0', 'initial commit', { parents: [], ts: NOW - 180_000 }),
    ];
    const layoutRows = [
      { node: 'git:m3', lane: 0 },
      { node: 'git:m2', lane: 0 },
      { node: 'git:f1', lane: 1 },
      { node: 'git:m1', lane: 0 },
      { node: 'git:m0', lane: 0 },
    ];
    const edges = [
      {
        child:'git:m3', parent:'git:m2',
        points:[{row:0,lane:0},{row:1,lane:0}],
      },
      {
        child:'git:m3', parent:'git:f1',
        points:[{row:0,lane:0},{row:1,lane:1},{row:2,lane:1}],
      },
      {
        child:'git:m2', parent:'git:m1',
        points:[{row:1,lane:0},{row:3,lane:0}],
      },
      {
        child:'git:f1', parent:'git:m1',
        points:[{row:2,lane:1},{row:3,lane:0}],
      },
      {
        child:'git:m1', parent:'git:m0',
        points:[{row:3,lane:0},{row:4,lane:0}],
      },
    ];
    return { rows, layoutRows, edges };
  }

  // Mixed EditChain ops + git commits across two sessions/repos.

  function largeHistory() {
    const keys = [];
    const rows = [];
    for (let i = 0; i < 600; i++) {
      const k = 'git:L' + String(i).padStart(4, '0');
      keys.push(k);
      rows.push(gitRow(k, 'large history commit #' + i, {
        parents:i>0?[keys[i-1]]:[], ts:NOW-i*1000 }));
    }
    const layoutRows = keys.map((k,i)=>({node:k,lane:i%4}));
    const edges=[];
    for(let i=0;i<keys.length-1;i++){
      edges.push({
        child:i%4===3?keys[i]:keys[i+1],
        parent:i%4===3?keys[i+1]:keys[i],
        points:[{row:i,lane:i%4},{row:i+1,lane:(i+1)%4}],
      });
    }
    return { rows, layoutRows, edges };
  }

  // --- Scenario registry -----------------------------------------------------
  //
  // Each scenario returns a fixture object:
  //   openError?: string            -> respond to Open with an Error
  //   rows / layoutRows / edges     -> full dataset (bridge slices by offset/limit)
  //   total?: number                -> override reported total (default rows.length)

  const scenarios = {
    empty() {
      return { rows:[], layoutRows:[], edges:[], total:-1 };
    },


    merge() {
      return mergeGraph();
    },


    error() {
      return { openError:'service unavailable' };
    },

    warned() {
      // A healthy chain whose Open response also reports data-integrity issues
      // (missing blob payloads). Rows must still render below a non-blocking
      // warning banner — the warning must never be silently discarded.
      const g = mergeGraph();
      return {
        rows: g.rows,
        layoutRows: g.layoutRows,
        edges: g.edges,
        openWarnings: ['6131 blob payload(s) missing from the durable store'],
        diagnostics: {
          blobs: { corrupt: 0, hydrated: 0, missing: 6131, unresolved: 0 },
          chain: { accepted: 118601, duplicates: 0, quarantined: 0, records: 118601 },
        },
      };
    },

    large() {
      return largeHistory();
    },

    // A long virtual window with three explicit group runs at KNOWN absolute
    // boundaries (rows 0..99 = repo:a, 100..199 = repo:b, 200+ = session:s1).
    // The deterministic prepend regression scrolls down past the boundary and
    // back up, so prependRowsAbove rebuilds the rows above a boundary; the
    // group-start chip must land on the FIRST row of each run (0, 100, 200)
    // before AND after a full reanchor rebuild.

    fileEdits() {
      const commit = gitRow('0123456789abcdef0123456789abcdef01234567', 'show changed files', {
        group: 'repo:files',
        parents: ['node:agent:edit'],
        ts: NOW,
        outcome: 'success',
      });
      const gitChange = {
        source: 'git',
        path: 'crates/editchain-git/src/diff.rs',
        status: 'modified',
        binary: false,
        partial: false,
        repository: commit.repository,
        repository_path: 'crates/editchain-git/src/diff.rs',
        commit_oid: commit.git_oid,
        old_oid: '1111111111111111111111111111111111111111',
        new_oid: '2222222222222222222222222222222222222222',
        old_mode: 'blob',
        new_mode: 'blob',
      };
      commit.sub_ops = [{
        op_id: '',
        summary: gitChange.path,
        kind: 'file',
        timestamp_ms: commit.timestamp_ms,
        file_change: gitChange,
      }];

      const agent = opRow('node:agent:edit', 'Edited the VS Code bridge', {
        group: 'session:files',
        kind: 'tool',
        author: 'agent',
        parents: ['node:files:root'],
        ts: NOW - 60_000,
      });
      const agentChange = {
        source: 'agent',
        path: 'extensions/vscode-editchain/src/extension.ts',
        status: 'modified',
        binary: false,
        partial: true,
        op_id: 'node:agent:edit:normalized',
      };
      agent.sub_ops = [{
        op_id: agentChange.op_id,
        summary: agentChange.path,
        kind: 'file',
        timestamp_ms: agent.timestamp_ms,
        file_change: agentChange,
      }];
      const root = opRow('node:files:root', 'Review requested', {
        group: 'session:files',
        kind: 'message',
        author: 'human',
        parents: [],
        ts: NOW - 120_000,
      });
      const rows = [commit, agent, root];
      rows.forEach((row) => {
        row.lane = 0;
        row.above = row === commit ? [] : [0];
        row.below = row === root ? [] : [0];
        row.transitions = [];
      });
      return {
        rows,
        layoutRows: rows.map((row) => ({ node: row.node_key, lane: 0 })),
        edges: [],
        subOpCounts: rows.map((row) => (row.sub_ops || []).length),
      };
    },

    // A turn + tool chain projected into the service's fixed Activity view.
    // All taxonomy fields use the exact Rust wire values.

    workUnits() {
      const SESSION = 'session:s1';
      const OPS = 'repo:ops';
      const act = []; // Activity top-level rows, newest first
      const sourceRows = []; // Unbundled service rows, newest first

      // Deterministic per-row summaries (the Activity projection reuses the
      // source summaries for members; bundle rows get their own summary-like
      // text below — the renderer must style from activity_bundle, never the
      // summary).
      const SUMMARY = {
        'wu:req1': '**User asks** to fix `the build`',
        'wu:req2': 'User asks to check the result',
        'wu:a1': 'tool result: apply patch 1',
        'wu:a2': 'tool result: apply patch 2',
        'wu:a3': 'tool result: apply patch 3',
        'wu:b1': 'tool result: run tests 1',
        'wu:b2': 'tool result: run tests 2',
        'wu:p1': '**Planning build and dry-run import steps**',
        'wu:p2': 'Planning   build and dry-run import steps',
        'wu:p3': '__Planning build and dry-run import steps__',
        'wu:fail': 'Run the test suite',
        'wu:chg': 'Update main.css',
        'wu:ver': 'Check test results',
        'wu:execsub': 'tool result: execute run (2 steps)',
        'wu:x1': 'tool result: unknown bundle member 1',
        'wu:x2': 'tool result: unknown bundle member 2',
        'wu:x3': 'tool result: unknown bundle member 3',
        'wu:x4': 'tool result: unknown bundle member 4',
        'wu:ops1': 'chore: ops one',
        'wu:req1b': '**User asks** to fix `the build`',
        'wu:ops2': 'ops two without a prefix',
      };
      // Bundle folds, in display order: [bundleKey, kind, [memberKeys], tsOffset]
      const bundles = [
        ['wu:run1', 'execute-run', ['a1', 'a2', 'a3'], 10],
        ['wu:run2', 'execute-run', ['b1', 'b2'], 25],
        ['wu:plans', 'plan-repeat', ['p1', 'p2', 'p3'], 32],
        ['wu:xbundle', 'checkpoint', ['x1', 'x2', 'x3', 'x4'], 55],
      ];
      // --- Source stream (newest first): unbundled service output -----------
      const sourceDefs = [
        // [key, unit, group, tsOffset, kind, activity, outcome, promoted]
        ['wu:req1', 't1', SESSION, 0, 'message', 'conversation', 'unknown', true],
        ['wu:req2', 't2', SESSION, 5, 'message', 'conversation', 'unknown', true],
        ['wu:a1', 't1', SESSION, 10, 'tool', 'execute', 'unknown', false],
        ['wu:a2', 't1', SESSION, 15, 'tool', 'execute', 'unknown', false],
        ['wu:a3', 't1', SESSION, 20, 'tool', 'execute', 'unknown', false],
        ['wu:b1', 't2', SESSION, 25, 'tool', 'execute', 'success', false],
        ['wu:b2', 't2', SESSION, 30, 'tool', 'execute', 'success', false],
        ['wu:p1', 't1', SESSION, 32, 'reflection', 'plan', 'unknown', false],
        ['wu:p2', 't1', SESSION, 33, 'reflection', 'plan', 'unknown', false],
        ['wu:p3', 't1', SESSION, 34, 'reflection', 'plan', 'unknown', false],
        ['wu:fail', 't1', SESSION, 35, 'command', 'execute', 'failure', true],
        ['wu:chg', 't2', SESSION, 40, 'file', 'change', 'warning', true],
        ['wu:ver', 't2', SESSION, 45, 'tool', 'verify', 'success', true],
        ['wu:execsub', 't1', SESSION, 50, 'tool', 'execute', 'unknown', false],
        ['wu:x1', 't1', SESSION, 55, 'tool', 'execute', 'unknown', false],
        ['wu:x2', 't1', SESSION, 60, 'tool', 'execute', 'unknown', false],
        ['wu:x3', 't1', SESSION, 65, 'tool', 'execute', 'unknown', false],
        ['wu:x4', 't1', SESSION, 70, 'tool', 'execute', 'unknown', false],
        ['wu:ops1', 'ops', OPS, 80, 'git', 'source_control', 'success', false],
        ['wu:req1b', 't1', SESSION, 90, 'message', 'conversation', 'unknown', false],
        ['wu:ops2', 'ops', OPS, 100, 'git', 'source_control', 'success', false],
      ];
      const recordRoleFor = (kind, activity) => {
        if (activity === 'change') return 'artifact';
        if (activity === 'verify') return 'result';
        if (activity === 'execute') return kind === 'tool' ? 'result' : 'action';
        if (activity === 'source_control') return 'artifact';
        return 'narrative';
      };
      for (const def of sourceDefs) {
        const [key, unit, group, off, kind, activity, outcome] = def;
        const ts = NOW - off * 1000;
        const isGit = kind === 'git';
        const row = isGit
          ? gitRow(key, SUMMARY[key], { group, ts, outcome })
          : opRow(key, SUMMARY[key], { group, kind, ts, outcome });
        row.activity_kind = activity;
        row.record_role = isGit ? 'artifact' : recordRoleFor(kind, activity);
        row.visibility = 'primary';
        row.turn_id = unit === 'ops' ? '' : unit;
        if (group === SESSION) {
          row.session_meta = {
            model_provider: 'sglang_dsv4',
            agent_nickname: 'Harvey',
          };
        }
        sourceRows.push(row);
      }
      // Chain parents (newest -> older) so the layout stays connected.
      for (let i = 0; i + 1 < sourceRows.length; i++) {
        sourceRows[i].parents = [sourceRows[i + 1].node_key];
      }
      const sourceByKey = new Map(sourceRows.map((r) => [r.node_key, r]));

      // --- Activity projection (newest-first display order) ----------------
      // Walk the source stream once: fold each marked run into its bundle row at
      // the FIRST member's display position (members are skipped), replace
      // execsub with its sub-op-carrying twin, and pass every other row
      // through unchanged. The bundle rows' summaries deliberately read like
      // execute runs with step counts — styling must come from the typed
      // activity_bundle metadata, never from parsing the display summary.
      const execsub = sourceByKey.get('wu:execsub');
      const bundleByMemberKey = new Map(); // 'wu:ax' -> bundle def
      for (const b of bundles) {
        for (const m of b[2]) bundleByMemberKey.set('wu:' + m, b);
      }
      for (const r of sourceRows) {
        if (r.node_key === 'wu:execsub') {
          act.push({
            ...execsub,
            summary: 'tool result: execute run (2 steps)',
            sub_ops: [
              { op_id: execsub.op_id + '::sub:0', summary: 'custom-title', kind: 'custom-title', timestamp_ms: execsub.timestamp_ms - 1000 },
              { op_id: execsub.op_id + '::sub:1', summary: 'mode', kind: 'mode', timestamp_ms: execsub.timestamp_ms - 2000 },
            ],
          });
          continue;
        }
        const bundleDef = bundleByMemberKey.get(r.node_key);
        if (bundleDef) {
          // Emit the bundle only at its FIRST member's display position.
          if (r.node_key !== 'wu:' + bundleDef[2][0]) continue;
          const [bundleKey, kind, memberKeys, off] = bundleDef;
          const anchor = sourceByKey.get('wu:' + memberKeys[0]);
          const memberRows = memberKeys.map((m) => sourceByKey.get('wu:' + m));
          const allSuccess = memberRows.every((m) => m.outcome === 'success');
          const isPlanRepeat = kind === 'plan-repeat';
          act.push({
            ...anchor,
            op_id: 'node:' + bundleKey,
            git_oid: null,
            repository: null,
            summary: isPlanRepeat
              ? anchor.summary
              : 'tool result: execute run (' + memberRows.length + ' steps)',
            timestamp_ms: NOW - off * 1000,
            node_key: bundleKey,
            is_system: !isPlanRepeat,
            author: isPlanRepeat ? 'agent' : '',
            commit_id: bundleKey,
            kind: isPlanRepeat ? 'reflection' : 'command',
            record_role: isPlanRepeat ? 'narrative' : 'action',
            activity_kind: isPlanRepeat ? 'plan' : 'execute',
            visibility: 'primary',
            outcome: allSuccess ? 'success' : 'unknown',
            promoted: false,
            work_unit: null, // recomputed below over the Activity view
            activity_bundle: {
              kind,
              member_count: memberRows.length,
            },
            sub_ops: memberRows.map((m) => ({
              op_id: m.op_id,
              summary: m.summary,
              kind: isPlanRepeat ? 'reflection' : 'tool',
              timestamp_ms: m.timestamp_ms,
            })),
          });
          continue;
        }
        act.push({ ...r });
      }
      // Work-unit markers are view-wide: recompute over the projected rows.
      const annotate = (rows) => {
        const unitId = (r) => r.turn_id ? 'session:s1/turn:' + r.turn_id : (r.group === OPS ? 'repo:ops' : r.group);
        const first = new Map();
        const last = new Map();
        const counts = new Map();
        const titles = new Map();
        const sessionFirst = new Map();
        const sessionCounts = new Map();
        rows.forEach((r, i) => {
          const id = unitId(r);
          if (!first.has(id)) first.set(id, i);
          last.set(id, i);
          counts.set(id, (counts.get(id) || 0) + 1);
          // The unit title is the OLDEST primary narrative row's summary (the
          // initiating request); the last narrative encountered in
          // newest-first order wins, exactly like annotate_activity_rows.
          if (r.record_role === 'narrative') titles.set(id, r.summary);
          if (r.group.startsWith('session:')) {
            if (!sessionFirst.has(r.group)) sessionFirst.set(r.group, i);
            sessionCounts.set(r.group, (sessionCounts.get(r.group) || 0) + 1);
          }
        });
        return rows.map((r, i) => {
          const id = unitId(r);
          const marker = {
            id,
            is_start: first.get(id) === i,
            is_end: last.get(id) === i,
            title: titles.has(id) ? titles.get(id) : null,
            count: counts.get(id),
          };
          r.work_unit = marker;
          r.session_summary = r.group.startsWith('session:') && sessionFirst.get(r.group) === i
            ? { count: sessionCounts.get(r.group) }
            : null;
          if (!r.record_role) r.record_role = 'narrative';
          return r;
        });
      };
      // Promotion flags ride on authored rows; the execsub row and bundle
      // rows are explicitly not promoted.
      const promotedKeys = new Set(['wu:req1', 'wu:req2', 'wu:fail', 'wu:chg', 'wu:ver']);
      // Activity rows: preserve authored promotion flags (bundle rows were
      // created with promoted:false; fold-visible rows keep their source flag).
      const actAnnotated = annotate(act);
      for (const r of actAnnotated) r.promoted = promotedKeys.has(r.node_key);
      for (const r of actAnnotated) {
        if (r.activity_bundle) r.promoted = false; // bundles are never promoted
      }

      const chain = (rows) => {
        const layoutRows = rows.map((r) => ({ node: r.node_key, lane: 0 }));
        const edges = [];
        for (let i = 0; i + 1 < rows.length; i++) {
          edges.push({
            child: rows[i].node_key,
            parent: rows[i + 1].node_key,
            points: [
              { row: i, lane: 0 },
              { row: i + 1, lane: 0 },
            ],
          });
        }
        return { layoutRows, edges };
      };
      const actChain = chain(actAnnotated);
      // Emit the additive fields with the same defaults serde applies, so
      // fixture rows are always fully shaped on the wire (activity_bundle
      // None on every ordinary row, never a missing property).
      const finalize = (rows) => rows.map((r) => {
        if (r.work_unit === undefined) r.work_unit = null;
        if (r.session_summary === undefined) r.session_summary = null;
        if (r.promoted === undefined) r.promoted = false;
        if (r.activity_bundle === undefined) r.activity_bundle = null;
        return r;
      });
      return {
        rows: finalize(actAnnotated),
        layoutRows: actChain.layoutRows,
        edges: actChain.edges,
        subOpCounts: actAnnotated.map((r) => (r.sub_ops || []).length),
      };
    },

    // A tall version of the workUnits scenario (96 repeated blocks) so the
    // virtual-scroll prepend/trim paths run against work-unit boundaries.
    // Each block repeats the exact 13-row unit structure (one start/end per
    // id, titled + fallback units, typed Activity + unknown-kind bundles,
    // promoted rows) with BLOCK-SCOPED unit ids, so any rendered window slice
    // sees complete units: prepending rows above or trimming rows below can
    // never invent a duplicate start. Activity = 1,248 top-level rows.
    workUnitsDeep() {
      const small = window.__editchainFixtures.workUnits();
      const BLOCKS = 96; // 1248 activity rows — larger than viewport + 2*BUFFER, so prepend/trim engage
      const actRows = [];
      const clone = (r, b) => {
        const key = 'wud:' + b + ':' + r.node_key;
        const off = (NOW - r.timestamp_ms) / 1000;
        return {
          ...r,
          node_key: key,
          op_id: r.op_id === r.node_key ? key : r.op_id,
          commit_id: r.git_oid ? r.commit_id : key,
          timestamp_ms: NOW - (b * 120000 + off * 1000),
          parents: [],
          _block: b,
        };
      };
      for (let b = 0; b < BLOCKS; b++) {
        for (const r of small.rows) actRows.push(clone(r, b));
      }
      // Block-scoped unit ids: 'block:N/session:s1/turn:t1' etc. so a window
      // slice always holds complete units (titles/counts recomputed per block
      // exactly like annotate_activity_rows).
      const unitId = (r) =>
        r._block + ':' + (r.turn_id ? 'session:s1/turn:' + r.turn_id : 'repo:ops');
      const annotate = (rows) => {
        const first = new Map();
        const last = new Map();
        const counts = new Map();
        const titles = new Map();
        rows.forEach((r, i) => {
          const id = unitId(r);
          if (!first.has(id)) first.set(id, i);
          last.set(id, i);
          counts.set(id, (counts.get(id) || 0) + 1);
          if (r.record_role === 'narrative') titles.set(id, r.summary);
        });
        return rows.map((r, i) => {
          const id = unitId(r);
          r.work_unit = {
            id,
            is_start: first.get(id) === i,
            is_end: last.get(id) === i,
            title: titles.has(id) ? titles.get(id) : null,
            count: counts.get(id),
          };
          delete r._block;
          return r;
        });
      };
      const actA = annotate(actRows);
      const chain = (rows) => {
        const layoutRows = [];
        const edges = [];
        rows.forEach((r, i) => {
          r.parents = i + 1 < rows.length ? [rows[i + 1].node_key] : [];
          layoutRows.push({ node: r.node_key, lane: 0 });
          if (i + 1 < rows.length) {
            edges.push({
              child: r.node_key,
              parent: rows[i + 1].node_key,
              points: [
                { row: i, lane: 0 },
                { row: i + 1, lane: 0 },
              ],
            });
          }
        });
        return { layoutRows, edges };
      };
      const actChain = chain(actA);
      return {
        rows: actA,
        layoutRows: actChain.layoutRows,
        edges: actChain.edges,
        subOpCounts: actA.map((r) => (r.sub_ops || []).length),
      };
    },
  };

  window.__editchainFixtures = scenarios;
})();
