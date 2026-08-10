// Deterministic protocol fixtures for the EditChain history webview harness.
//
// These model *protocol responses* (the shapes in crates/editchain-protocol),
// not copied DOM. The fixture bridge slices/dispatches them in response to
// requests from media/main.js.
//
// Row shape (HistoryRow): op_id?, git_oid?, repository?, summary, timestamp_ms,
//   group, node_key, parents[], is_submodule, is_system, author, commit_id, kind
// Layout shape (GraphLayout): { rows:[{node,lane}], edges:[{child,parent,points:[{row,lane}]}] }

(function () {
  'use strict';

  const NOW = Date.now();

  function gitRow(key, summary, opts) {
    opts = opts || {};
    return {
      op_id: null,
      git_oid: key,
      repository: opts.repository !== undefined ? opts.repository : 0,
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
      // Submodule + system rows present; "messages only" hides them client-side.
      const g = mergeGraph();
      g.rows[2].is_submodule = true; // feature branch as a submodule
      g.rows[3].is_system = true;
      return g;
    },

    error() {
      return { openError:'service unavailable' };
    },

    large() {
      return largeHistory();
    },
  };

  window.__editchainFixtures = scenarios;
})();