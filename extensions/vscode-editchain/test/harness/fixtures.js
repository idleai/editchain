// Deterministic protocol fixtures for the EditChain history webview harness.
//
// These model *protocol responses* (the shapes in crates/editchain-protocol),
// not copied DOM. The fixture bridge slices/dispatches them in response to
// requests from media/main.js.
//
// Row shape (HistoryRow): op_id?, git_oid?, repository?, summary, timestamp_ms,
//   group, node_key, parents[], is_submodule, is_system, author, commit_id, kind
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
      // internal bookkeeping rows hidden by hide_trace — record_role has no
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
  function linearChain() {
    const keys = ['git:d', 'git:c', 'git:b', 'git:a'];
    const rows = keys.map((k, i) =>
      gitRow(k, 'commit ' + k.slice(4), {
        parents: i < keys.length - 1 ? [keys[i + 1]] : [],
        ts: NOW - i * 60_000,
      })
    );
    const layoutRows = keys.map((k) => ({ node: k, lane: 0 }));
    const edges = [];
    for (let i = 0; i < keys.length - 1; i++) {
      edges.push({
        child: keys[i],
        parent: keys[i + 1],
        points: [
          { row: i, lane: 0 },
          { row: i + 1, lane: 0 },
        ],
      });
    }
    return { rows, layoutRows, edges };
  }

  // A fork + merge graph with two lanes and crossing edges.
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
  function mixedHistory() {
    const rows = [
      opRow('node:s1:a', 'Agent message one', { group:'session:s1', kind:'message', author:'agent' }),
      opRow('node:s1:h', 'Human message one', { group:'session:s1', kind:'message', author:'human' }),
      opRow('node:s1:b', 'Tool call result',   { group:'session:s1', kind:'tool', is_system:true }),
      opRow('node:s2:c', 'Second session note',{ group:'session:s2', kind:'command' }),
      gitRow('git:x', 'repo commit x', { group:'repo:x' }),
    ];
    const layoutRows = rows.map((r) => ({ node: r.node_key, lane: r.node_key.startsWith('node:s2') ? 1 : 0 }));
    const edges = [
      {
        child:'node:s1:a', parent:'node:s1:b',
        points:[{row:-1,lane:-1}],
      },
    ];
    return { rows, layoutRows, edges };
  }

  // A combined op carrying bundled metadata sub-ops (the exact list from the
  // request), to exercise inline reveal. The server emits the parent + one row
  // per sub-op as a fixed fully-expanded flat list.
  function combinedOp() {
    const parent = opRow('node:c:1', 'Agent turn with metadata', {
      group: 'session:s1', kind: 'message', author: 'agent',
    });
    const subKinds = [
      'system', 'last-prompt', 'custom-title', 'agent-name',
      'mode', 'permission-mode', 'file-history-snapshot',
    ];
    parent.sub_ops = subKinds.map((k, i) => ({
      op_id: 'node:c:1::sub:' + i,
      summary: k,
      kind: k,
      timestamp_ms: NOW - i * 1000,
    }));
    // A second plain row after it, so expansion shifts it down.
    const after = opRow('node:c:2', 'Plain row after', { group: 'session:s1', kind: 'message' });
    const rows = [parent, after];
    const layoutRows = rows.map((r) => ({ node: r.node_key, lane: 0 }));
    return { rows, layoutRows, edges: [], subOpCounts: [parent.sub_ops.length, 0] };
  }

  // A large virtual history window (600 rows) for scroll/overflow checks.
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

    linear() {
      return linearChain();
    },

    merge() {
      return mergeGraph();
    },

    mixed() {
      return mixedHistory();
    },

    filtered() {
      // Submodule + system rows present; "messages only" (an INCLUSIVE kind
      // constraint) and hide-submodules are applied server-side by the bridge.
      const g = mergeGraph();
      g.rows[2].is_submodule = true; // feature branch as a submodule
      g.rows[3].is_system = true;
      return g;
    },

    undated() {
      // A chain with some undated (timestamp_ms == 0) rows, so the "Hide
      // undated" filter can be exercised.
      const rows = [
        opRow('node:u:1', 'dated newest', { ts: NOW - 1000 }),
        opRow('node:u:2', 'undated middle', { ts: 0 }),
        opRow('node:u:3', 'dated oldest', { ts: NOW - 2000 }),
      ];
      const layoutRows = rows.map((r) => ({ node: r.node_key, lane: 0 }));
      const edges = [
        { child:'node:u:1', parent:'node:u:2', points:[{row:0,lane:0},{row:1,lane:0}] },
        { child:'node:u:2', parent:'node:u:3', points:[{row:1,lane:0},{row:2,lane:0}] },
      ];
      return { rows, layoutRows, edges };
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
    multigroup() {
      const rows = [];
      for (let i = 0; i < 600; i++) {
        const k = 'git:G' + String(i).padStart(4, '0');
        if (i < 200) {
          rows.push(gitRow(k, 'multi-group commit #' + i, {
            group: i < 100 ? 'repo:a' : 'repo:b',
            parents: i > 0 ? [rows[i - 1].node_key] : [],
            ts: NOW - i * 1000,
          }));
        } else {
          rows.push(opRow('node:G' + String(i).padStart(4, '0'), 'multi-group op #' + i, {
            group: 'session:s1',
            kind: i % 2 ? 'message' : 'command',
            parents: [rows[i - 1].node_key],
            ts: NOW - i * 1000,
          }));
        }
      }
      const layoutRows = rows.map((r) => ({ node: r.node_key, lane: 0 }));
      const edges = [];
      for (let i = 0; i < rows.length - 1; i++) {
        edges.push({
          child: rows[i].node_key,
          parent: rows[i + 1].node_key,
          points: [
            { row: i, lane: 0 },
            { row: i + 1, lane: 0 },
          ],
        });
      }
      return {
        rows,
        layoutRows,
        edges,
        subOpCounts: rows.map(() => 0),
      };
    },

    longsummary() {
      // A row whose summary is ~1024 chars, to exercise the ellipsis/truncation
      // behaviour when the content column is resized.
      const long = 'word '.repeat(200); // ~1000 chars
      const rows = [
        opRow('node:l:1', long, { group:'session:s1', kind:'message', author:'human' }),
        opRow('node:l:2', 'short row', { group:'session:s1', kind:'message' }),
      ];
      const layoutRows = rows.map((r) => ({ node: r.node_key, lane: 0 }));
      return { rows, layoutRows, edges: [] };
    },

    combined() {
      return combinedOp();
    },

    // A turn + tool chain with internal trace-VISIBILITY records (the r4
    // Activity/Raw profile contract). Activity (hide_trace=true, the default)
    // removes every visibility==="trace" row via the server-side chain
    // filter; Raw (hide_trace=false) shows the full stream. The bridge
    // mirrors the filter for both GetWindow and GetLayout, so switching
    // profiles must refetch a coherent, smaller/larger view with the same
    // single-lane topology. All taxonomy fields use the exact Rust wire
    // values.
    traced() {
      const rows = [
        opRow('node:t:0', 'Agent reply (build fixed)', {
          group: 'session:s1', kind: 'message', author: 'agent',
          record_role: 'narrative', activity_kind: 'conversation',
          visibility: 'primary', outcome: 'success', turn_id: 't1',
        }),
        opRow('node:t:1', 'tool result: 3 files updated', {
          group: 'session:s1', kind: 'tool', is_system: true,
          record_role: 'result', activity_kind: 'execute',
          visibility: 'trace', outcome: 'success', turn_id: 't1',
        }),
        opRow('node:t:2', 'Run tests', {
          group: 'session:s1', kind: 'command',
          record_role: 'action', activity_kind: 'execute',
          visibility: 'primary', outcome: 'unknown', turn_id: 't0',
        }),
        opRow('node:t:3', 'tool result: 42 passed', {
          group: 'session:s1', kind: 'tool', is_system: true,
          record_role: 'result', activity_kind: 'execute',
          visibility: 'trace', outcome: 'success', turn_id: 't0',
        }),
        opRow('node:t:4', 'User asks to fix the build', {
          group: 'session:s1', kind: 'message', author: 'human',
          record_role: 'narrative', activity_kind: 'conversation',
          visibility: 'primary', outcome: 'unknown', turn_id: 't0',
        }),
      ];
      rows[0].parents = ['node:t:1'];
      rows[1].parents = ['node:t:2'];
      rows[2].parents = ['node:t:3'];
      rows[3].parents = ['node:t:4'];
      const layoutRows = rows.map((r) => ({ node: r.node_key, lane: 0 }));
      const edges = [];
      for (let i = 0; i < rows.length - 1; i++) {
        edges.push({
          child: rows[i].node_key,
          parent: rows[i + 1].node_key,
          points: [
            { row: i, lane: 0 },
            { row: i + 1, lane: 0 },
          ],
        });
      }
      return {
        rows,
        layoutRows,
        edges,
        subOpCounts: rows.map(() => 0),
      };
    },

    // A full-coverage scenario for the EXACT r4 taxonomy (Rust wire enums).
    // Every whitelisted activity_kind and outcome appears so the badge probe
    // can assert the exact labels render — execute/source_control/failure in
    // particular — and that conversation rows stay badge-free. The legacy
    // tool_call/command/edit/commit/review and error/interrupted vocabulary
    // must never leak back into badges.
    badges() {
      const rows = [
        opRow('node:b:exec', 'Run the test suite', {
          group: 'session:s1', kind: 'command',
          record_role: 'action', activity_kind: 'execute',
          visibility: 'primary', outcome: 'failure', turn_id: 't0',
        }),
        gitRow('git:b:sc', 'fix layout regression', {
          group: 'repo:x',
          record_role: 'artifact', activity_kind: 'source_control',
          visibility: 'primary', outcome: 'success',
        }),
        opRow('node:b:chg', 'Update main.css', {
          group: 'session:s1', kind: 'file',
          record_role: 'artifact', activity_kind: 'change',
          visibility: 'primary', outcome: 'warning', turn_id: 't1',
        }),
        opRow('node:b:plan', 'Stop the plan run', {
          group: 'session:s1', kind: 'reflection',
          record_role: 'narrative', activity_kind: 'plan',
          visibility: 'primary', outcome: 'cancelled', turn_id: 't2',
        }),
        opRow('node:b:exp', 'Search the workspace', {
          group: 'session:s1', kind: 'tool',
          record_role: 'action', activity_kind: 'explore',
          visibility: 'primary', outcome: 'unknown', turn_id: 't3',
        }),
        opRow('node:b:ver', 'Check test results', {
          group: 'session:s1', kind: 'tool',
          record_role: 'result', activity_kind: 'verify',
          visibility: 'primary', outcome: 'unknown', turn_id: 't3',
        }),
        opRow('node:b:diag', 'Investigate the failure', {
          group: 'session:s1', kind: 'error',
          record_role: 'result', activity_kind: 'diagnose',
          visibility: 'primary', outcome: 'failure', turn_id: 't4',
        }),
        opRow('node:b:coord', 'Delegate to a subagent', {
          group: 'session:s1', kind: 'message', author: 'agent',
          record_role: 'narrative', activity_kind: 'coordinate',
          visibility: 'primary', outcome: 'unknown', turn_id: 't5',
        }),
        opRow('node:b:ext', 'External agent echo', {
          group: 'session:s1', kind: 'message', is_system: true,
          record_role: 'echo', activity_kind: 'external',
          visibility: 'primary', outcome: 'unknown', turn_id: 't6',
        }),
        opRow('node:b:sys', 'Chain transport record', {
          group: 'session:s1', kind: 'note', is_system: true,
          record_role: 'lifecycle', activity_kind: 'system',
          visibility: 'primary', outcome: 'unknown', turn_id: 't6',
        }),
        opRow('node:b:conv', 'User asks a question', {
          group: 'session:s1', kind: 'message', author: 'human',
          record_role: 'narrative', activity_kind: 'conversation',
          visibility: 'primary', outcome: 'unknown', turn_id: 't7',
        }),
      ];
      for (let i = 0; i < rows.length - 1; i++) {
        rows[i].parents = [rows[i + 1].node_key];
      }
      const layoutRows = rows.map((r) => ({ node: r.node_key, lane: 0 }));
      const edges = [];
      for (let i = 0; i < rows.length - 1; i++) {
        edges.push({
          child: rows[i].node_key,
          parent: rows[i + 1].node_key,
          points: [
            { row: i, lane: 0 },
            { row: i + 1, lane: 0 },
          ],
        });
      }
      return {
        rows,
        layoutRows,
        edges,
        subOpCounts: rows.map(() => 0),
      };
    },

    // A fork + subagent-reconnect graph, mirroring the geometry the importer
    // emits via ForkOf/SubagentOf/ReconnectsTo relationship notes (SPEC §1.1):
    // a shared root forks into two continuations on distinct lanes, and the
    // parent's completion result reconnects into the subagent branch — a
    // cross-lane transition at the top. Each window row carries explicit lane /
    // above / below / transitions (the shape the renderer's per-row graph
    // cells read directly), so the fork draws two diverging columns and the
    // reconnection draws a rounded cross-lane transition.
    //
    // The per-row geometry below is exactly what the production layout emits
    // for the edge list that follows (see LayoutContext::new): for each edge
    // (child_row, child_lane) -> (parent_row, parent_lane), the child lane runs
    // vertically down to parent_row-1, jogs onto the parent lane at that row,
    // and the parent lane runs down to the parent row. Transitions are
    // (child_lane, parent_lane) order, so the reconnect at row 0 jogs FROM the
    // completion lane (0) TO the subagent lane (1), and the fork jogs at rows 2
    // and 3 go FROM the subagent lane (1) TO lane 0.
    fork() {
      // Newest-first rows. n:0 is the reconnected completion result (on lane 0,
      // with a cross-lane transition to the subagent branch on lane 1).
      const local = [
        // completion result reconnecting to the subagent's last op (lane 1):
        // the 0 -> 1 transition begins at THIS row's own dot (the child node
        // lives here — no synthetic top half above the dot and no source-lane
        // bottom stub) and ends on lane 1 at the row boundary, where row 1's
        // above=[1] continues the line into the next dot.
        [0, 'node:f:0', 'completion result', { above: [], below: [1], transitions: [[0, 1]] }],
        // subagent's last op — the subagent branch, lane 1
        [1, 'node:f:1', 'subagent last op', { above: [1], below: [1] }],
        // subagent's first op — forks off the spawn point (lane 0, next row)
        // and off the shared root (lane 0, two rows below); both jogs go 1 -> 0
        [1, 'node:f:2', 'subagent first op', { above: [1], below: [0, 1], transitions: [[1, 0]] }],
        // Agent tool_use call — the parent's spawn point, lane 0
        [0, 'node:f:3', 'Agent tool call', { above: [0, 1], below: [0], transitions: [[1, 0]] }],
        // shared root on lane 0 (both branches descend from it)
        [0, 'node:f:4', 'shared root', { above: [0], below: [] }],
      ];
      // Drawn parent edges (child -> parent), the single source of truth for
      // the fork geometry: the reconnect, the subagent chain, the SubagentOf
      // spawn edge (subagent first op -> spawn point), the fork edge (subagent
      // first op -> shared root), and the trunk chain. Each row's `parents`
      // below is derived from this list so badges can never reference a parent
      // edge the layout does not draw.
      const edges = [
        { child: 'node:f:0', parent: 'node:f:1', points: [{ row: 0, lane: 0 }, { row: 0, lane: 1 }, { row: 1, lane: 1 }] },
        { child: 'node:f:1', parent: 'node:f:2', points: [{ row: 1, lane: 1 }, { row: 2, lane: 1 }] },
        { child: 'node:f:2', parent: 'node:f:3', points: [{ row: 2, lane: 1 }, { row: 2, lane: 0 }, { row: 3, lane: 0 }] },
        { child: 'node:f:2', parent: 'node:f:4', points: [{ row: 2, lane: 1 }, { row: 3, lane: 1 }, { row: 3, lane: 0 }, { row: 4, lane: 0 }] },
        { child: 'node:f:3', parent: 'node:f:4', points: [{ row: 3, lane: 0 }, { row: 4, lane: 0 }] },
      ];
      const parentsByKey = new Map();
      for (const e of edges) {
        const ps = parentsByKey.get(e.child) || [];
        ps.push(e.parent);
        parentsByKey.set(e.child, ps);
      }
      const rows = local.map(([lane, key, summary, geo], i) => {
        // The completion result is a tool-kind system row (like the collab
        // tool call the service derives ReconnectsTo from), so the badge
        // probe can verify badges never inherit the tool row's dimmed opacity.
        const r = opRow(key, summary, {
          group: 'session:s1',
          kind: key === 'node:f:0' ? 'tool' : 'message',
          is_system: key === 'node:f:0',
        });
        r.lane = geo.lane !== undefined ? geo.lane : lane;
        r.above = geo.above;
        r.below = geo.below;
        r.transitions = geo.transitions || [];
        r.parents = parentsByKey.get(key) || [];
        return r;
      });
      // Structural parent relations mirroring what the service derives from
      // SubagentOf / ReconnectsTo / ForkOf notes: the completion result row
      // RETURNS into the subagent branch (reconnect), the subagent's first op
      // row STARTS the branch off the spawn marker (subagent) and forks off
      // the shared root (fork). Every relation's parent is one of the row's
      // `parents` (and therefore one of the drawn edges above). The renderer
      // must badge these rows without parsing any provider JSON.
      const relsByKey = {
        'node:f:0': [
          { parent: 'node:f:1', kind: 'reconnect' },
          // Prototype-property wire values must be ignored by the viewer's
          // own-property whitelist (and never become a garbage badge).
          { parent: 'node:f:1', kind: 'constructor' },
        ],
        'node:f:2': [
          { parent: 'node:f:3', kind: 'subagent' },
          { parent: 'node:f:4', kind: 'fork' },
        ],
      };
      for (const r of rows) {
        r.parent_relations = relsByKey[r.node_key] || [];
      }
      const layoutRows = local.map(([lane, key, ,], i) => ({ node: key, lane }));
      return {
        rows, layoutRows, edges,
        max_lane: 1,
        subOpCounts: rows.map(() => 0),
      };
    },

    // A chain with MORE concurrent lanes than the former 128-lane clipping cap.
    // Every lane must still be drawn inside the graph column (lane spacing
    // compresses, then lane centres distribute proportionally across the graph
    // budget), so the renderer never drops or clips a service lane. The first
    // five rows form a connected production-like zigzag — consecutive nodes on
    // alternating lanes (0,1,0,1,0) with an adjacent transition at each row,
    // exactly as LayoutContext::new emits for a chain that weaves across two
    // lanes: each transition begins at its row's own dot, ends on the next
    // lane at the row boundary, and the following row's `above` continues it.
    // This exercises the rounded transition paths under heavy compression,
    // where the corner radius clamps to the lane distance (and, at extreme
    // spacing, the renderer falls back to a straight orthogonal jog).
    highLanes() {
      const N = 200; // lanes 0..199 — exceeds 128
      const rows = [];
      const layoutRows = [];
      const zigzag = [
        { lane: 0, above: [], below: [1], transitions: [[0, 1]] },
        { lane: 1, above: [1], below: [0], transitions: [[1, 0]] },
        { lane: 0, above: [0], below: [1], transitions: [[0, 1]] },
        { lane: 1, above: [1], below: [0], transitions: [[1, 0]] },
        { lane: 0, above: [0], below: [], transitions: [] },
      ];
      for (let i = 0; i < N; i++) {
        const key = 'git:lane:' + i;
        const r = gitRow(key, 'lane row ' + i, { ts: NOW - i * 1000 });
        const z = zigzag[i];
        r.lane = z ? z.lane : i;
        r.above = z ? z.above : [];
        r.below = z ? z.below : [];
        r.transitions = z ? z.transitions : [];
        rows.push(r);
        layoutRows.push({ node: key, lane: r.lane });
      }
      return {
        rows, layoutRows, edges: [],
        max_lane: N - 1,
        subOpCounts: rows.map(() => 0),
      };
    },

    // Round-two Activity-view wire contract: deterministic work-unit markers
    // (`work_unit`), conservative promotion (`promoted`), and typed execute-run
    // bundles (`activity_bundle`) modelled as the SERVICE's projection output
    // (crates/editchain-project/src/activity.rs + editchain-vscode-service),
    // newest first in each list:
    //
    //   `rows`    — the Activity profile view: trace rows filtered out,
    //               eligible execute runs folded into ONE top-level bundle row
    //               that carries `activity_bundle` and its members as
    //               `sub_ops` (expandable through the existing sub-ops model);
    //   `rawRows` — the Raw profile stream (hide_trace=false on the wire):
    //               bundles unfolded back to their member rows, activity_bundle
    //               None on every row, trace rows kept. Raw rows still carry
    //               the additive work_unit/promoted wire fields (the service
    //               annotates raw views too — see the service's
    //               activity_view_bundles_execute_runs..._raw_profile... test).
    //
    // Three logical units interleave in display order so the client must rely
    // on the explicit markers, never on adjacency: unit `t1` (titled, count 6
    // in Activity / 11 in Raw), unit `t2` (titled, count 4 / 5), and the
    // fallback `ops` unit (group-key id, NO narrative evidence -> title null,
    // count 2 / 2). Each id has exactly one is_start and one is_end.
    //
    // Rows cover every promotion reason (unit-final narrative, failure,
    // change, verify) and every bundle shape the client must distinguish
    // WITHOUT parsing summaries:
    //   - a clean unknown-outcome execute-run bundle (member_count 3);
    //   - an all-success execute-run bundle (member_count 2);
    //   - an ordinary execute row WITH sub-ops but NO activity_bundle (its
    //     summary deliberately reads like an execute run);
    //   - one activity_bundle with a forward-compatible unknown kind
    //     ('checkpoint' — the bridge coerces it through the wire enum to
    //     'unknown', and the renderer must never style it as execute-run).
    workUnits() {
      const SESSION = 'session:s1';
      const OPS = 'repo:ops';
      const act = []; // Activity top-level rows, newest first
      const raw = []; // Raw top-level rows, newest first

      // Deterministic per-row summaries (the Activity projection reuses the
      // raw summaries for members; bundle rows get their own summary-like
      // text below — the renderer must style from activity_bundle, never the
      // summary).
      const SUMMARY = {
        'wu:req1': 'User asks to fix the build',
        'wu:req2': 'User asks to check the result',
        'wu:a1': 'tool result: apply patch 1',
        'wu:a2': 'tool result: apply patch 2',
        'wu:a3': 'tool result: apply patch 3',
        'wu:b1': 'tool result: run tests 1',
        'wu:b2': 'tool result: run tests 2',
        'wu:fail': 'Run the test suite',
        'wu:chg': 'Update main.css',
        'wu:ver': 'Check test results',
        'wu:execsub': 'tool result: execute run (2 steps)',
        'wu:x1': 'tool result: unknown bundle member 1',
        'wu:x2': 'tool result: unknown bundle member 2',
        'wu:x3': 'tool result: unknown bundle member 3',
        'wu:x4': 'tool result: unknown bundle member 4',
        'wu:ops1': 'chore: ops one',
        'wu:req1b': 'User asks to fix the build',
        'wu:ops2': 'chore: ops two',
      };
      // Bundle folds, in display order: [bundleKey, kind, [memberKeys], tsOffset]
      const bundles = [
        ['wu:run1', 'execute-run', ['a1', 'a2', 'a3'], 10],
        ['wu:run2', 'execute-run', ['b1', 'b2'], 25],
        ['wu:xbundle', 'checkpoint', ['x1', 'x2', 'x3', 'x4'], 55],
      ];
      // --- Raw stream (newest first): the exact unbundled service output ---
      const rawDefs = [
        // [key, unit, group, tsOffset, kind, activity, outcome, promoted]
        ['wu:req1', 't1', SESSION, 0, 'message', 'conversation', 'unknown', true],
        ['wu:req2', 't2', SESSION, 5, 'message', 'conversation', 'unknown', true],
        ['wu:a1', 't1', SESSION, 10, 'tool', 'execute', 'unknown', false],
        ['wu:a2', 't1', SESSION, 15, 'tool', 'execute', 'unknown', false],
        ['wu:a3', 't1', SESSION, 20, 'tool', 'execute', 'unknown', false],
        ['wu:b1', 't2', SESSION, 25, 'tool', 'execute', 'success', false],
        ['wu:b2', 't2', SESSION, 30, 'tool', 'execute', 'success', false],
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
      for (const def of rawDefs) {
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
        raw.push(row);
      }
      // Chain parents (newest -> older) so the layout stays connected.
      for (let i = 0; i + 1 < raw.length; i++) raw[i].parents = [raw[i + 1].node_key];
      const rawByKey = new Map(raw.map((r) => [r.node_key, r]));

      // --- Activity projection (newest-first display order) ----------------
      // Walk the raw stream once: fold each marked run into its bundle row at
      // the FIRST member's display position (members are skipped), replace
      // execsub with its sub-op-carrying twin, and pass every other row
      // through unchanged. The bundle rows' summaries deliberately read like
      // execute runs with step counts — styling must come from the typed
      // activity_bundle metadata, never from parsing the display summary.
      const execsub = rawByKey.get('wu:execsub');
      const bundleByMemberKey = new Map(); // 'wu:ax' -> bundle def
      for (const b of bundles) {
        for (const m of b[2]) bundleByMemberKey.set('wu:' + m, b);
      }
      for (const r of raw) {
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
          const anchor = rawByKey.get('wu:' + memberKeys[0]);
          const memberRows = memberKeys.map((m) => rawByKey.get('wu:' + m));
          const allSuccess = memberRows.every((m) => m.outcome === 'success');
          act.push({
            ...anchor,
            op_id: 'node:' + bundleKey,
            git_oid: null,
            repository: null,
            summary: 'tool result: execute run (' + memberRows.length + ' steps)',
            timestamp_ms: NOW - off * 1000,
            node_key: bundleKey,
            is_system: true,
            author: '',
            commit_id: bundleKey,
            kind: 'command',
            record_role: 'action',
            activity_kind: 'execute',
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
              kind: 'tool',
              timestamp_ms: m.timestamp_ms,
            })),
          });
          continue;
        }
        act.push({ ...r });
      }
      // Work-unit markers are view-wide: recompute over EACH profile's own
      // top-level list (counts differ between Activity and Raw).
      const annotate = (rows) => {
        const unitId = (r) => r.turn_id ? 'session:s1/turn:' + r.turn_id : (r.group === OPS ? 'repo:ops' : r.group);
        const first = new Map();
        const last = new Map();
        const counts = new Map();
        const titles = new Map();
        rows.forEach((r, i) => {
          const id = unitId(r);
          if (!first.has(id)) first.set(id, i);
          last.set(id, i);
          counts.set(id, (counts.get(id) || 0) + 1);
          // The unit title is the OLDEST primary narrative row's summary (the
          // initiating request); the last narrative encountered in
          // newest-first order wins, exactly like annotate_activity_rows.
          if (r.record_role === 'narrative') titles.set(id, r.summary);
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
          if (!r.record_role) r.record_role = 'narrative';
          return r;
        });
      };
      // Promotion flags ride on the authored rows (raw + Activity share the
      // same flags); the execsub row and bundle rows are explicitly NOT
      // promoted.
      const promotedKeys = new Set(['wu:req1', 'wu:req2', 'wu:fail', 'wu:chg', 'wu:ver']);
      const rawAnnotated = annotate(raw);
      for (const r of rawAnnotated) r.promoted = promotedKeys.has(r.node_key);
      // Activity rows: preserve authored promotion flags (bundle rows were
      // created with promoted:false; fold-visible rows keep their raw flag).
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
      const rawChain = chain(rawAnnotated);
      // Emit the additive fields with the same defaults serde applies, so
      // fixture rows are always fully shaped on the wire (activity_bundle
      // None on every ordinary row, never a missing property).
      const finalize = (rows) => rows.map((r) => {
        if (r.work_unit === undefined) r.work_unit = null;
        if (r.promoted === undefined) r.promoted = false;
        if (r.activity_bundle === undefined) r.activity_bundle = null;
        return r;
      });
      return {
        rows: finalize(actAnnotated),
        rawRows: finalize(rawAnnotated),
        layoutRows: actChain.layoutRows,
        layoutRowsRaw: rawChain.layoutRows,
        edges: actChain.edges,
        edgesRaw: rawChain.edges,
        subOpCounts: actAnnotated.map((r) => (r.sub_ops || []).length),
        rawSubOpCounts: rawAnnotated.map(() => 0),
      };
    },

    // A tall version of the workUnits scenario (96 repeated blocks) so the
    // virtual-scroll prepend/trim paths run against work-unit boundaries.
    // Each block repeats the exact 12-row unit structure (one start/end per
    // id, titled + fallback units, typed execute-run + unknown-kind bundles,
    // promoted rows) with BLOCK-SCOPED unit ids, so any rendered window slice
    // sees complete units: prepending rows above or trimming rows below can
    // never invent a duplicate start. Activity = 1,152 top-level rows, Raw =
    // 1,728 top-level rows.
    workUnitsDeep() {
      const small = window.__editchainFixtures.workUnits();
      const BLOCKS = 96; // 1152 activity rows — larger than viewport + 2*BUFFER, so prepend/trim engage
      const actRows = [];
      const rawRows = [];
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
        for (const r of small.rawRows) rawRows.push(clone(r, b));
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
      const rawA = annotate(rawRows);
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
      const rawChain = chain(rawA);
      return {
        rows: actA,
        rawRows: rawA,
        layoutRows: actChain.layoutRows,
        layoutRowsRaw: rawChain.layoutRows,
        edges: actChain.edges,
        edgesRaw: rawChain.edges,
        subOpCounts: actA.map((r) => (r.sub_ops || []).length),
        rawSubOpCounts: rawA.map(() => 0),
      };
    },
  };

  window.__editchainFixtures = scenarios;
})();
