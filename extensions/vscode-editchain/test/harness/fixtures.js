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

    // A fork + subagent-reconnect graph, mirroring the geometry the importer
    // emits via ForkOf/SubagentOf/ReconnectsTo relationship notes (SPEC §1.1):
    // a shared root forks into two continuations on distinct lanes, and the
    // parent's completion result reconnects into the subagent branch — a
    // cross-lane merge at the top. Each window row carries explicit lane /
    // above / below / transitions (the shape the renderer's per-row graph
    // cells read directly), so the fork draws two diverging columns and the
    // reconnection draws a horizontal merge connector.
    fork() {
      // Newest-first rows. n:0 is the reconnected completion result (on lane 0,
      // with a cross-lane merge connector to the subagent branch on lane 1).
      const local = [
        // completion result reconnecting to the subagent's last op (lane 1)
        [0, 'node:f:0', 'completion result', { above: [0, 1], below: [0, 1], transitions: [[1, 0]] }],
        // subagent's last op — the subagent branch, lane 1
        [1, 'node:f:1', 'subagent last op', { above: [1], below: [1] }],
        // subagent's first op — forks off the shared root
        [1, 'node:f:2', 'subagent first op', { above: [0, 1], below: [1] }],
        // Agent tool_use call — the parent's spawn point, lane 0
        [0, 'node:f:3', 'Agent tool call', { above: [0], below: [0, 1] }],
        // shared root on lane 0 (both branches descend from it)
        [0, 'node:f:4', 'shared root', { above: [], below: [0] }],
      ];
      // Drawn parent edges (child -> parent), the single source of truth for
      // the fork geometry: the reconnect, the subagent chain, the SubagentOf
      // spawn edge (subagent first op -> spawn point), the fork edge (subagent
      // first op -> shared root), and the trunk chain. Each row's `parents`
      // below is derived from this list so badges can never reference a parent
      // edge the layout does not draw.
      const edges = [
        { child: 'node:f:0', parent: 'node:f:1', points: [{ row: 0, lane: 1 }, { row: 1, lane: 1 }] },
        { child: 'node:f:1', parent: 'node:f:2', points: [{ row: 1, lane: 1 }, { row: 2, lane: 1 }] },
        { child: 'node:f:2', parent: 'node:f:3', points: [{ row: 2, lane: 1 }, { row: 3, lane: 0 }] },
        { child: 'node:f:2', parent: 'node:f:4', points: [{ row: 2, lane: 1 }, { row: 4, lane: 0 }] },
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
    // budget), so the renderer never drops or clips a service lane.
    highLanes() {
      const N = 200; // lanes 0..199 — exceeds 128
      const rows = [];
      const layoutRows = [];
      for (let i = 0; i < N; i++) {
        const key = 'git:lane:' + i;
        const r = gitRow(key, 'lane row ' + i, { ts: NOW - i * 1000 });
        r.lane = i;
        r.above = [];
        r.below = [];
        r.transitions = [];
        rows.push(r);
        layoutRows.push({ node: key, lane: i });
      }
      return {
        rows, layoutRows, edges: [],
        max_lane: N - 1,
        subOpCounts: rows.map(() => 0),
      };
    },
  };

  window.__editchainFixtures = scenarios;
})();
