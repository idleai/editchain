// Graph-data probe for the EditChain history webview (harness-only).
//
// A memory-conscious structural checker for ~150k-row chains. Loaded by
// test/harness/index.html AFTER layoutProbe.js; defines window.__editchainGraph
// and is inert until the host calls runGraphProbe().
//
// What it does, end to end:
//   1. Pages the ENTIRE filtered dataset through the real service bridge using
//      the exact GetWindow DTOs the production renderer sends (same
//      hide_submodules/filter payload construction as media/main.js). Only
//      probe-relevant fields are retained per row — never the full DTOs — so
//      heap stays bounded on ~150k rows.
//   2. Runs structural checks over the paged dataset: node-key uniqueness,
//      sub-op block structure (key format, parent_row, expanded-slot layout,
//      sub_op_counts consistency), parent resolution, relation validity + kind
//      reporting, duplicate/quarantine diagnostics from the Open response,
//      lane bounds, lane continuity (including page boundaries), lane-reuse
//      pathologies, tip/root boundaries, sub-op lane inheritance, and
//      transition anchor coherence. chain_generation must be IDENTICAL across
//      every paged response — a change mid-scan fails the run (the pages are
//      not one coherent snapshot).
//   3. Dumps the RENDERED window (production main.js DOM) and checks
//      rendered-row math (data-row/data-key integrity, uniform ROW_H, bounded
//      DOM), dot geometry (one dot per top-level row, inside the graph cell,
//      lane-consistent cx), and render/data geometry coherence (rendered keys
//      match the dataset at the same absolute slot; renderer total and max_lane
//      match the dataset) — at the initial viewport AND after scrolling to the
//      top, middle, and bottom (waiting for idle each time), so a scroll path
//      that leaks DOM rows or fails to re-anchor fails the run.
//   4. runSelfTests() verifies the probe's own failure detection with an
//      in-page SIMULATED service: a generation change mid-scan must be
//      detected, and a hung GetWindow must reject after the configured
//      timeout instead of hanging.
//
// The Node host (scripts/ui-graph.mjs) calls runGraphProbe() once and then
// streams the compact dataset out in chunks via datasetChunk() so neither the
// page nor the host ever materializes the full dataset as one JSON blob.
//
// Checks follow the layoutProbe convention: { name, pass, detail }.

(function () {
  'use strict';

  // Absolute-index dataset (compact rows; index = expanded slot).
  const rows = [];
  // Every node key seen (top-level AND sub-op), for uniqueness checks.
  const allKeys = new Set();
  // Top-level node keys only, for parent resolution.
  const topKeys = new Set();
  // key -> first two absolute slots where it repeats (duplicate pathology).
  const dupSlots = new Map();

  let openBody = null;       // the Open response seen by the page (or {Error})
  let readyFired = false;

  // Capture the same handshake the renderer consumes, so diagnostics
  // (duplicate/quarantine counts) come from the real Open response.
  window.addEventListener('message', (event) => {
    const msg = event.data;
    if (!msg || typeof msg !== 'object') return;
    if (msg.id === 'open') openBody = msg.body;
    else if (msg.id === 'ready') readyFired = true;
  });

  // --------------------------------------------------------------------------
  // Exact renderer request construction (mirrors media/main.js)
  // --------------------------------------------------------------------------

  // The renderer's temporary fixed filter while the filtering UI is absent.
  function filterPayload() {
    return {
      summary_pattern: '',
      kind_pattern: '',
      include_kind_pattern: '',
      hide_undated: false,
      splice: true,
    };
  }

  // Nested Git repositories/submodules are hidden in the temporary fixed view.
  function hideSubmodules() {
    return true;
  }

  // --------------------------------------------------------------------------
  // Bridge request plumbing (independent id namespace so the renderer ignores
  // probe responses; the service bridge dispatches { id, body } to all
  // listeners, and main.js drops ids it does not own).
  // --------------------------------------------------------------------------

  let reqSeq = 0x70000000;
  const pending = new Map();

  window.addEventListener('message', (event) => {
    const msg = event.data;
    if (!msg || typeof msg.id !== 'number') return;
    const entry = pending.get(msg.id);
    if (!entry) return;
    pending.delete(msg.id);
    entry.resolve(msg.body);
  });

  // Default bound for a page-side probe request. The host threads its
  // `--row-timeout` value in via runGraphProbe({ requestTimeoutMs }) and
  // pageAll passes it to every GetWindow request, so a hung service can never
  // stall the probe for the hardcoded default.
  const DEFAULT_REQUEST_TIMEOUT_MS = 120000;

  function request(body, timeoutMs) {
    const id = ++reqSeq;
    const effective = (typeof timeoutMs === 'number' && timeoutMs > 0)
      ? Math.floor(timeoutMs)
      : DEFAULT_REQUEST_TIMEOUT_MS;
    return new Promise((resolve, reject) => {
      const timer = setTimeout(() => {
        pending.delete(id);
        reject(new Error('graph probe request timed out after ' + effective + 'ms: ' +
          JSON.stringify(body).slice(0, 120)));
      }, effective);
      pending.set(id, {
        resolve: (body2) => { clearTimeout(timer); resolve(body2); },
        reject: (err) => { clearTimeout(timer); reject(err); },
      });
      window.vscode.postMessage({ id, body });
    });
  }

  // --------------------------------------------------------------------------
  // Compact row retention (memory-conscious: probe fields only)
  // --------------------------------------------------------------------------

  function compactRow(r) {
    return {
      node_key: r.node_key,
      kind: r.kind,
      lane: r.lane,
      above: (r.above || []).slice(),
      below: (r.below || []).slice(),
      transitions: (r.transitions || []).map((t) => [t[0], t[1]]),
      parents: (r.parents || []).slice(),
      relations: (r.parent_relations || []).map((x) => ({ parent: x.parent, kind: x.kind })),
      is_subop: !!r.is_subop,
      parent_row: (typeof r.parent_row === 'number') ? r.parent_row : null,
    };
  }

  // --------------------------------------------------------------------------
  // Full-dataset paging (exact GetWindow DTOs)
  // --------------------------------------------------------------------------

  async function pageAll(limit, requestTimeoutMs) {
    const meta = {
      total: -1,
      maxLane: -1,
      chainGeneration: -1,
      subOpCounts: null,
      pagesFetched: 0,
      pageLimit: limit,
      pageTotals: [],
      pageGenerations: [],
    };
    let offset = 0;
    while (true) {
      const body = await request({
        GetWindow: {
          offset,
          limit,
          hide_submodules: hideSubmodules(),
          filter: filterPayload(),
        },
      }, requestTimeoutMs);
      if (!body || body.Ok === undefined || body.Ok === null) {
        throw new Error('GetWindow error at offset ' + offset + ': ' +
          JSON.stringify(body).slice(0, 300));
      }
      const w = body.Ok;
      if (meta.total === -1) meta.total = w.total;
      meta.pageTotals.push(w.total);
      if (typeof w.max_lane === 'number') {
        meta.maxLane = Math.max(meta.maxLane, w.max_lane);
      }
      if (typeof w.chain_generation === 'number') {
        meta.chainGeneration = w.chain_generation;
      }
      // Record EVERY page's generation, not just the last: the dataset is only
      // one coherent snapshot if every paged response reports the same chain
      // generation (a change mid-scan means the chain was rewritten while the
      // probe was reading it).
      meta.pageGenerations.push(typeof w.chain_generation === 'number' ? w.chain_generation : null);
      // The offset-0 window carries the global expansion snapshot.
      if (offset === 0 && Array.isArray(w.sub_op_counts)) {
        meta.subOpCounts = w.sub_op_counts;
      }
      for (let i = 0; i < w.rows.length; i++) {
        const r = w.rows[i];
        const abs = offset + i;
        const key = r.node_key;
        if (allKeys.has(key)) {
          const list = dupSlots.get(key) || [];
          if (list.length < 2) list.push(abs);
          dupSlots.set(key, list);
        } else {
          allKeys.add(key);
        }
        if (!r.is_subop) topKeys.add(key);
        rows[abs] = compactRow(r);
      }
      meta.pagesFetched++;
      offset += w.rows.length;
      if (w.rows.length === 0) break;
      if (offset >= meta.total) break;
    }
    if (meta.total === -1) meta.total = rows.length;
    return meta;
  }

  // --------------------------------------------------------------------------
  // Structural checks over the paged dataset
  // --------------------------------------------------------------------------

  const KNOWN_RELATION_KINDS = new Set(['subagent', 'reconnect', 'fork', 'unknown']);

  function datasetChecks(meta, checks) {
    const total = meta.total;
    const ok = (name, pass, detail) => checks.push({ name, pass, detail });

    // Paging integrity.
    let pageTotalStable = true;
    let firstTotal = null;
    for (const t of meta.pageTotals) {
      if (firstTotal === null) firstTotal = t;
      else if (t !== firstTotal) pageTotalStable = false;
    }
    ok('PAGING_TOTAL_STABLE', pageTotalStable,
      pageTotalStable ? 'every page reported total=' + firstTotal
        : 'page totals differed: ' + JSON.stringify(meta.pageTotals));
    ok('PAGING_TOTAL_MATCH', rows.length === total,
      'paged ' + rows.length + ' rows, reported total=' + total +
      ' (limit=' + meta.pageLimit + ', pages=' + meta.pagesFetched + ')');

    // chain_generation must be identical across EVERY paged response: a change
    // mid-scan means the chain was rewritten while the probe was reading it,
    // so the paged dataset is not one coherent snapshot. Absent generations
    // are a failure too (the service contract ships one per window).
    const gens = meta.pageGenerations || [];
    const genStable = gens.length > 0 &&
      gens.every((g) => typeof g === 'number' && g === gens[0]);
    let genDetail;
    if (gens.length === 0) {
      genDetail = 'no page reported chain_generation';
    } else if (genStable) {
      genDetail = 'chain_generation=' + gens[0] + ' across all ' + gens.length + ' pages';
    } else {
      genDetail = 'chain_generation changed mid-scan: pages ' +
        gens.map((g, i) => (i + 1) + '->' + g).join(', ');
    }
    ok('CHAIN_GENERATION_STABLE', genStable, genDetail);

    // Node-key uniqueness / presence.
    ok('NODE_KEY_PRESENT', rows.every((r) => typeof r.node_key === 'string' && r.node_key.length > 0),
      'all ' + rows.length + ' rows carry a non-empty node_key');
    const dups = Array.from(dupSlots.keys());
    ok('NODE_KEY_UNIQUE', dups.length === 0,
      dups.length === 0 ? 'all ' + allKeys.size + ' node keys unique'
        : 'duplicate node_key(s): ' + dups.slice(0, 5).map((k) => k + '@' + dupSlots.get(k).join(',')).join('; ') +
          (dups.length > 5 ? ' (+' + (dups.length - 5) + ' more)' : ''));

    // Open diagnostics consistency (duplicates/quarantines are structural
    // facts about the chain; their rates are reported in graph.json).
    const diag = openBody && openBody.Ok ? (openBody.Ok.diagnostics || null) : null;
    if (diag && diag.chain) {
      const c = diag.chain;
      const sum = (c.accepted || 0) + (c.duplicates || 0) + (c.quarantined || 0);
      ok('CHAIN_DIAGNOSTICS_CONSISTENT', (c.records || 0) === sum,
        'records=' + c.records + ' accepted=' + c.accepted +
        ' duplicates=' + c.duplicates + ' quarantined=' + c.quarantined +
        ' (sum=' + sum + ')');
    } else {
      ok('CHAIN_DIAGNOSTICS_CONSISTENT', false,
        'Open response carried no chain diagnostics: ' + JSON.stringify(openBody).slice(0, 200));
    }

    // Sub-op block structure: keys, parent_row, expanded-slot layout, and the
    // offset-0 sub_op_counts snapshot all agree.
    const subopCounts = meta.subOpCounts;
    let subopFormatOk = true;
    let subopStructureOk = true;
    let countsOk = true;
    let subopRowsCount = 0;
    let topLevelCount = 0;
    let firstSubopBad = null;
    const SUBOP_KEY = /^(.+)::sub:(\d+)$/;
    for (let abs = 0; abs < rows.length; abs++) {
      const r = rows[abs];
      if (!r.is_subop) { topLevelCount++; continue; }
      subopRowsCount++;
      const m = SUBOP_KEY.exec(r.node_key);
      if (!m) {
        subopFormatOk = false;
        firstSubopBad = firstSubopBad || { abs, issue: 'bad key format: ' + r.node_key };
        continue;
      }
      const parentKey = m[1];
      const i = parseInt(m[2], 10);
      const expectedParentRow = abs - 1 - i;
      if (r.parent_row !== expectedParentRow) {
        subopStructureOk = false;
        firstSubopBad = firstSubopBad || { abs, issue: 'parent_row=' + r.parent_row + ' expected=' + expectedParentRow };
      }
      const parent = rows[r.parent_row];
      if (!parent || parent.is_subop || parent.node_key !== parentKey) {
        subopStructureOk = false;
        firstSubopBad = firstSubopBad || { abs, issue: 'parent row ' + r.parent_row + ' is not top-level ' + parentKey };
      }
      // Every slot between the parent and this sub-op must belong to the same
      // block (no interleaved top-level row, and the index suffix must be the
      // sub-op's position within the block).
      for (let j = r.parent_row + 1; j < abs; j++) {
        const mid = rows[j];
        if (!mid || mid.is_subop !== true || mid.node_key !== parentKey + '::sub:' + (j - r.parent_row - 1)) {
          subopStructureOk = false;
          firstSubopBad = firstSubopBad || { abs, issue: 'slot ' + j + ' breaks ' + parentKey + ' block' };
          break;
        }
      }
      if (parent && !parent.is_subop) {
        // The sub-op must occupy exactly the slot its own suffix claims.
        if (i !== abs - r.parent_row - 1) {
          subopStructureOk = false;
          firstSubopBad = firstSubopBad || { abs, issue: 'suffix ' + i + ' != slot offset ' + (abs - r.parent_row - 1) };
        }
      }
    }
    ok('SUBOP_KEY_FORMAT', subopFormatOk,
      subopFormatOk ? 'all ' + subopRowsCount + ' sub-op keys match <parent>::sub:<i>'
        : 'first bad=' + JSON.stringify(firstSubopBad));
    ok('SUBOP_BLOCK_STRUCTURE', subopStructureOk,
      subopStructureOk ? 'sub-op parent_row/slots consistent for ' + subopRowsCount + ' sub-op rows'
        : 'first bad=' + JSON.stringify(firstSubopBad));
    if (subopCounts !== null) {
      const countSum = subopCounts.reduce((a, b) => a + b, 0);
      countsOk = subopCounts.length === topLevelCount &&
        countSum === subopRowsCount &&
        total === topLevelCount + subopRowsCount;
      ok('SUBOP_COUNTS_CONSISTENT', countsOk,
        'sub_op_counts len=' + subopCounts.length + ' sum=' + countSum +
        ' (topLevel=' + topLevelCount + ' subop=' + subopRowsCount + ' total=' + total + ')');
    } else {
      ok('SUBOP_COUNTS_CONSISTENT', true,
        'no sub_op_counts snapshot on the offset-0 window (legacy service) — skipped');
    }

    // Parent resolution: every parent is a real top-level node key; no
    // self-parents; no repeated parents within a row.
    let parentsOk = true;
    let firstParentBad = null;
    for (let abs = 0; abs < rows.length; abs++) {
      const r = rows[abs];
      const seen = new Set();
      for (const p of r.parents) {
        if (!topKeys.has(p)) {
          parentsOk = false;
          firstParentBad = firstParentBad || { abs, key: r.node_key, parent: p, issue: 'unresolved' };
        } else if (p === r.node_key) {
          parentsOk = false;
          firstParentBad = firstParentBad || { abs, key: r.node_key, parent: p, issue: 'self-parent' };
        } else if (seen.has(p)) {
          parentsOk = false;
          firstParentBad = firstParentBad || { abs, key: r.node_key, parent: p, issue: 'repeated parent' };
        }
        seen.add(p);
      }
    }
    ok('PARENT_RESOLUTION', parentsOk,
      parentsOk ? 'every parent key resolves to a top-level node' : 'first bad=' + JSON.stringify(firstParentBad));

    // Relation validity: relations reference drawn parents, carry a known
    // kind, and are not duplicated per row.
    let relationsOk = true;
    let firstRelationBad = null;
    const relationKindCounts = {};
    let rowsWithRelations = 0;
    for (let abs = 0; abs < rows.length; abs++) {
      const r = rows[abs];
      if (!r.relations.length) continue;
      rowsWithRelations++;
      const seen = new Set();
      for (const rel of r.relations) {
        relationKindCounts[rel.kind] = (relationKindCounts[rel.kind] || 0) + 1;
        if (r.parents.indexOf(rel.parent) === -1) {
          relationsOk = false;
          firstRelationBad = firstRelationBad || { abs, key: r.node_key, parent: rel.parent, issue: 'relation parent not in parents[]' };
        } else if (!KNOWN_RELATION_KINDS.has(rel.kind)) {
          relationsOk = false;
          firstRelationBad = firstRelationBad || { abs, key: r.node_key, kind: rel.kind, issue: 'unknown relation kind' };
        } else if (seen.has(rel.parent)) {
          relationsOk = false;
          firstRelationBad = firstRelationBad || { abs, key: r.node_key, parent: rel.parent, issue: 'duplicate relation' };
        }
        seen.add(rel.parent);
      }
    }
    ok('RELATION_VALIDITY', relationsOk,
      relationsOk ? relationKindCounts['unknown']
        ? 'all relations reference drawn parents with known kinds (unknown=' + relationKindCounts['unknown'] + ')'
        : 'all relations reference drawn parents with known kinds'
        : 'first bad=' + JSON.stringify(firstRelationBad));

    // Lane bounds: every lane reference (dot lane, above, below, transition
    // endpoints) stays inside [0, max_lane].
    let laneBoundsOk = true;
    let firstLaneBad = null;
    for (let abs = 0; abs < rows.length; abs++) {
      const r = rows[abs];
      const checkLane = (v, what) => {
        if (typeof v !== 'number' || !Number.isInteger(v) || v < 0 || v > meta.maxLane) {
          laneBoundsOk = false;
          firstLaneBad = firstLaneBad || { abs, key: r.node_key, what, v };
        }
      };
      checkLane(r.lane, 'lane');
      for (const v of r.above) checkLane(v, 'above');
      for (const v of r.below) checkLane(v, 'below');
      for (const t of r.transitions) { checkLane(t[0], 'transition.from'); checkLane(t[1], 'transition.to'); }
    }
    ok('LANE_BOUNDS', laneBoundsOk,
      laneBoundsOk ? 'all lane references within [0,' + meta.maxLane + ']'
        : 'first bad=' + JSON.stringify(firstLaneBad));

    // Lane continuity across adjacent slots (including page boundaries — the
    // dataset is one contiguous array). A vertical segment in a row's bottom
    // half must meet either the next row's top half or the next row's dot, and
    // vice versa.
    let continuityOk = true;
    let firstContinuityBad = null;
    for (let abs = 0; abs + 1 < rows.length; abs++) {
      const a = rows[abs];
      const b = rows[abs + 1];
      for (const L of a.below) {
        if (b.above.indexOf(L) === -1 && b.lane !== L) {
          continuityOk = false;
          firstContinuityBad = firstContinuityBad || { abs, key: a.node_key, lane: L, issue: 'below lane not continued at row ' + (abs + 1) };
        }
      }
      for (const L of b.above) {
        if (a.below.indexOf(L) === -1 && a.lane !== L) {
          continuityOk = false;
          firstContinuityBad = firstContinuityBad || { abs: abs + 1, key: b.node_key, lane: L, issue: 'above lane not continued from row ' + abs };
        }
      }
    }
    ok('LANE_CONTINUITY', continuityOk,
      continuityOk ? 'vertical lane segments meet at every row boundary' : 'first bad=' + JSON.stringify(firstContinuityBad));

    // Lane-reuse pathology: when a lane passes from a top-level dot into the
    // NEXT top-level dot with no edge between them, two unrelated nodes share
    // one vertical line. Layout is newest-first, so the UPPER dot is the child
    // and the line runs down to its parent: the lower dot must appear in the
    // upper node's parents[].
    let reuseOk = true;
    let firstReuseBad = null;
    let prevTop = -1;
    for (let abs = 0; abs < rows.length; abs++) {
      const r = rows[abs];
      if (r.is_subop) continue;
      if (prevTop !== -1) {
        const upper = rows[prevTop];
        if (upper.lane === r.lane && upper.below.indexOf(r.lane) !== -1 && r.above.indexOf(r.lane) !== -1) {
          if (upper.parents.indexOf(r.node_key) === -1) {
            reuseOk = false;
            firstReuseBad = firstReuseBad || {
              abs, key: r.node_key, lane: r.lane,
              child: upper.node_key, childAbs: prevTop,
              issue: 'lane continues onto a dot that is not the child\'s parent',
            };
          }
        }
      }
      prevTop = abs;
    }
    ok('LANE_REUSE', reuseOk,
      reuseOk ? 'no lane reuse without a parent edge' : 'first bad=' + JSON.stringify(firstReuseBad));

    // Tip/root boundaries: the newest row has no lines entering from above;
    // the oldest row has no lines leaving below.
    let boundaryOk = true;
    let boundaryDetail = '';
    if (rows.length > 0) {
      const first = rows[0];
      const last = rows[rows.length - 1];
      if (first.above.length !== 0) {
        boundaryOk = false;
        boundaryDetail += ' first row above=' + JSON.stringify(first.above);
      }
      if (last.below.length !== 0) {
        boundaryOk = false;
        boundaryDetail += ' last row below=' + JSON.stringify(last.below);
      }
      boundaryDetail = boundaryOk ? 'tip above=[], root below=[]' : boundaryDetail;
    } else {
      boundaryDetail = 'no rows';
    }
    ok('TIP_ROOT_BOUNDARIES', boundaryOk, boundaryDetail);

    // Sub-op lanes inherit their parent's lane.
    let inheritOk = true;
    let firstInheritBad = null;
    for (let abs = 0; abs < rows.length; abs++) {
      const r = rows[abs];
      if (!r.is_subop) continue;
      const parent = rows[r.parent_row];
      if (!parent || parent.lane !== r.lane) {
        inheritOk = false;
        firstInheritBad = firstInheritBad || { abs, key: r.node_key, lane: r.lane, parentRow: r.parent_row, parentLane: parent ? parent.lane : null };
      }
    }
    ok('SUBOP_LANE_INHERITANCE', inheritOk,
      inheritOk ? 'sub-op rows share their parent lane' : 'first bad=' + JSON.stringify(firstInheritBad));

    // Transition anchors must be backed by the row's own dot or by the
    // above/below geometry (mirrors buildGraphCell's anchor resolution; a
    // transition with no anchor is a dangling stub in the DTO).
    let transitionsOk = true;
    let firstTransitionBad = null;
    for (let abs = 0; abs < rows.length; abs++) {
      const r = rows[abs];
      for (const t of r.transitions) {
        const startOk = t[0] === r.lane || r.above.indexOf(t[0]) !== -1;
        const endOk = r.below.indexOf(t[1]) !== -1 || r.lane === t[1];
        if (!startOk || !endOk) {
          transitionsOk = false;
          firstTransitionBad = firstTransitionBad || { abs, key: r.node_key, from: t[0], to: t[1], startOk, endOk };
        }
      }
    }
    ok('TRANSITION_COHERENCE', transitionsOk,
      transitionsOk ? 'every transition is anchored by a dot or boundary geometry' : 'first bad=' + JSON.stringify(firstTransitionBad));

    const chainDiag = diag && diag.chain ? diag.chain : null;
    const records = chainDiag ? (chainDiag.records || 0) : 0;

    // Kind and lane histograms (data-derived — never hardcoded chain counts).
    const kindHistogram = {};
    const subopKindHistogram = {};
    const laneHistogram = {};
    for (let abs = 0; abs < rows.length; abs++) {
      const r = rows[abs];
      const bucket = r.is_subop ? subopKindHistogram : kindHistogram;
      bucket[r.kind] = (bucket[r.kind] || 0) + 1;
      if (!r.is_subop) laneHistogram[r.lane] = (laneHistogram[r.lane] || 0) + 1;
    }
    return {
      relationKindCounts,
      rowsWithRelations,
      topLevelCount,
      subopRowsCount,
      duplicateKeys: dups.length,
      duplicateRates: {
        records,
        accepted: chainDiag ? (chainDiag.accepted || 0) : 0,
        duplicates: chainDiag ? (chainDiag.duplicates || 0) : 0,
        quarantined: chainDiag ? (chainDiag.quarantined || 0) : 0,
        duplicate_rate: records > 0 ? ((chainDiag.duplicates || 0) / records) : 0,
        quarantine_rate: records > 0 ? ((chainDiag.quarantined || 0) / records) : 0,
        node_key_duplicates: dups.length,
      },
      blobs: diag && diag.blobs ? diag.blobs : null,
      kindHistogram,
      subopKindHistogram,
      laneHistogram,
    };
  }

  // --------------------------------------------------------------------------
  // Rendered-window (DOM) checks and render/data coherence
  // --------------------------------------------------------------------------

  function renderedChecks(meta, checks, prefix) {
    const ok = (name, pass, detail) => checks.push({ name: (prefix || '') + name, pass, detail });
    const total = meta.total;
    const ROW_H = 34;
    const viewMessageEl = document.querySelector('.view-message');

    if (total === 0) {
      // Empty chain: the renderer shows an explicit empty state; no rows to
      // validate. Skip the row-based checks and confirm the state message.
      const msg = viewMessageEl ? (viewMessageEl.textContent || '').trim() : '';
      ok('EMPTY_CHAIN_STATE', msg.length > 0,
        msg ? 'empty chain — view message: "' + msg.slice(0, 60) + '"'
          : 'empty chain but no view message rendered');
      return;
    }

    // --- rendered-row math --------------------------------------------------
    const rowEls = Array.from(document.querySelectorAll('#rows .row'));
    const rendered = [];
    let mathOk = true;
    let firstMathBad = null;
    let prevAbs = -1;
    const seenRows = new Set();
    const seenKeys = new Set();
    let heightOk = true;
    let firstHeightBad = null;
    for (const el of rowEls) {
      const absRaw = el.getAttribute('data-row');
      const key = el.getAttribute('data-key');
      const abs = absRaw === null ? NaN : parseInt(absRaw, 10);
      const isSubop = el.classList.contains('row-subop');
      const isPlaceholder = el.classList.contains('row-placeholder');
      const h = el.getBoundingClientRect().height;
      const dot = el.querySelector('circle.graphDot');
      const svg = el.querySelector('svg.graphCell');
      rendered.push({
        abs: Number.isFinite(abs) ? abs : null,
        key,
        isSubop,
        isPlaceholder,
        h: Math.round(h * 100) / 100,
        dotCx: dot ? parseFloat(dot.getAttribute('cx')) : null,
        dotCy: dot ? parseFloat(dot.getAttribute('cy')) : null,
        svgW: svg ? parseFloat(svg.getAttribute('width')) : null,
      });
      if (absRaw === null || !Number.isFinite(abs) || abs < 0 || abs >= total) {
        mathOk = false;
        firstMathBad = firstMathBad || { issue: 'bad data-row', absRaw };
      }
      if (abs <= prevAbs) {
        mathOk = false;
        firstMathBad = firstMathBad || { issue: 'data-row not strictly increasing', abs, prevAbs };
      }
      if (seenRows.has(abs)) {
        mathOk = false;
        firstMathBad = firstMathBad || { issue: 'duplicate data-row', abs };
      }
      if (!key || seenKeys.has(key)) {
        mathOk = false;
        firstMathBad = firstMathBad || { issue: 'missing/duplicate data-key', key };
      }
      if (Math.abs(h - ROW_H) > 0.5) {
        heightOk = false;
        firstHeightBad = firstHeightBad || { abs, key, h: Math.round(h * 100) / 100 };
      }
      seenRows.add(abs);
      seenKeys.add(key);
      prevAbs = abs;
    }
    ok('RENDERED_ROWS_PRESENT', rendered.length > 0,
      rendered.length > 0 ? rendered.length + ' rows rendered' : 'no .row elements in #rows');
    ok('RENDERED_ROW_MATH', mathOk,
      mathOk ? 'data-row/data-key integrity OK across ' + rendered.length + ' rows'
        : 'first bad=' + JSON.stringify(firstMathBad));
    ok('RENDERED_ROW_HEIGHT', heightOk,
      heightOk ? 'all rows exactly ROW_H=' + ROW_H : 'first bad=' + JSON.stringify(firstHeightBad));
    ok('RENDERED_NO_PLACEHOLDERS', !rendered.some((r) => r.isPlaceholder),
      rendered.some((r) => r.isPlaceholder) ? 'placeholder rows remain after idle' : 'no placeholder rows');

    // --- bounded DOM math ---------------------------------------------------
    // The renderer keeps roughly viewport + 2*BUFFER visible rows; a rendered
    // DOM far larger than that is a memory pathology. BUFFER=400, ROW_H=34.
    const BUFFER = 400;
    const viewportRows = Math.ceil(window.innerHeight / ROW_H);
    const bound = viewportRows + 2 * BUFFER + 64;
    ok('RENDER_WINDOW_BOUNDED', rendered.length <= bound,
      'rendered=' + rendered.length + ' rows, bound=' + bound +
      ' (viewport=' + viewportRows + ' + 2*BUFFER=' + (2 * BUFFER) + ' + 64)');

    // --- dot geometry -------------------------------------------------------
    let dotsOk = true;
    let firstDotBad = null;
    let topLevelDots = 0;
    let subopDots = 0;
    for (const r of rendered) {
      const cell = rows[r.abs];
      const dot = r.dotCx;
      if (r.isSubop) {
        if (dot !== null) { subopDots++; dotsOk = false; firstDotBad = firstDotBad || { abs: r.abs, issue: 'sub-op row drew a dot' }; }
        continue;
      }
      if (dot === null) {
        dotsOk = false;
        firstDotBad = firstDotBad || { abs: r.abs, issue: 'top-level row missing dot' };
        continue;
      }
      topLevelDots++;
      if (r.svgW === null || dot < 0 || dot > r.svgW + 0.5 || r.dotCy === null || Math.abs(r.dotCy - ROW_H / 2) > 1.5) {
        dotsOk = false;
        firstDotBad = firstDotBad || { abs: r.abs, issue: 'dot outside graph cell', dotCx: dot, svgW: r.svgW, dotCy: r.dotCy };
      }
    }
    ok('RENDERED_DOT_BOUNDS', dotsOk,
      dotsOk ? 'dots inside cells, cy=midY, 1 per top-level row (' + topLevelDots + '), none on sub-ops'
        : 'first bad=' + JSON.stringify(firstDotBad));

    // --- render/data geometry coherence -------------------------------------
    let keyCoherenceOk = true;
    let firstKeyBad = null;
    for (const r of rendered) {
      const d = rows[r.abs];
      if (!d || d.node_key !== r.key || d.is_subop !== r.isSubop) {
        keyCoherenceOk = false;
        firstKeyBad = firstKeyBad || {
          abs: r.abs,
          domKey: r.key,
          domSubop: r.isSubop,
          dataKey: d ? d.node_key : null,
          dataSubop: d ? d.is_subop : null,
        };
      }
    }
    ok('RENDER_DATA_KEY_COHERENCE', keyCoherenceOk,
      keyCoherenceOk ? 'every rendered row matches the dataset at its absolute slot' : 'first bad=' + JSON.stringify(firstKeyBad));

    // Lane-consistent dot x: rows on the same lane must share a dot cx; dot cx
    // must be strictly increasing with lane (laneX is monotonic).
    let laneDotsOk = true;
    let firstLaneDotBad = null;
    const cxByLane = new Map();
    for (const r of rendered) {
      if (r.isSubop || r.dotCx === null) continue;
      const lane = rows[r.abs] ? rows[r.abs].lane : null;
      if (lane === null) continue;
      if (!cxByLane.has(lane)) cxByLane.set(lane, r.dotCx);
      else if (Math.abs(cxByLane.get(lane) - r.dotCx) > 0.5) {
        laneDotsOk = false;
        firstLaneDotBad = firstLaneDotBad || { issue: 'same lane, different dot x', lane, a: cxByLane.get(lane), b: r.dotCx, abs: r.abs };
      }
    }
    const lanes = Array.from(cxByLane.keys()).sort((a, b) => a - b);
    for (let i = 1; i < lanes.length; i++) {
      if (cxByLane.get(lanes[i]) <= cxByLane.get(lanes[i - 1])) {
        laneDotsOk = false;
        firstLaneDotBad = firstLaneDotBad || { issue: 'dot x not monotonic with lane', laneA: lanes[i - 1], xA: cxByLane.get(lanes[i - 1]), laneB: lanes[i], xB: cxByLane.get(lanes[i]) };
      }
    }
    ok('RENDERED_LANE_DOTS_CONSISTENT', laneDotsOk,
      laneDotsOk ? 'distinct lanes -> distinct monotonic dot x (' + lanes.length + ' lanes rendered)' : 'first bad=' + JSON.stringify(firstLaneDotBad));

    // Renderer state coherence: total and max_lane must agree with the dataset.
    const rendererTotal = (typeof window.__editchainGetTotal === 'function') ? window.__editchainGetTotal() : null;
    const graphState = (typeof window.__editchainGraphState === 'function') ? window.__editchainGraphState() : null;
    ok('RENDER_TOTAL_MATCH', rendererTotal === total,
      'renderer total=' + rendererTotal + ' dataset total=' + total);
    const rendererMaxLane = graphState ? graphState.maxLane : null;
    ok('RENDER_MAX_LANE_MATCH', rendererMaxLane === meta.maxLane,
      'renderer maxLane=' + rendererMaxLane + ' dataset maxLane=' + meta.maxLane);
    if (graphState) {
      const valid = Number.isInteger(graphState.renderTop) && Number.isInteger(graphState.renderBottom) &&
        graphState.renderTop >= 0 && graphState.renderBottom >= graphState.renderTop;
      ok('RENDERER_STATE_VALID', valid,
        'renderTop=' + graphState.renderTop + ' renderBottom=' + graphState.renderBottom +
        ' maxLane=' + graphState.maxLane + ' graphWidth=' + graphState.graphWidth);
    } else {
      ok('RENDERER_STATE_VALID', false, 'window.__editchainGraphState() unavailable');
    }
  }

  // --------------------------------------------------------------------------
  // Scroll sampling: re-run the rendered checks after scrolling to at least
  // the top, middle, and bottom of the chain — not just the initial viewport —
  // waiting for idle after each jump so the renderer's own fetch/sync work is
  // done. The bounded-DOM check runs at every sample, so a scroll path that
  // accumulates rows (or fails to re-anchor after a full traversal) fails
  // here. Uses only production renderer hooks (#rows scroll + the existing
  // __editchain* state accessors).
  // --------------------------------------------------------------------------

  async function scrollSamples(meta, checks, idleTimeoutMs) {
    const rowsEl = document.getElementById('rows');
    const samples = [];
    if (!rowsEl) return samples;
    const scrollable = rowsEl.scrollHeight > rowsEl.clientHeight + 1;
    if (!scrollable) {
      checks.push({
        name: 'SCROLL_SAMPLING_SCOPE',
        pass: true,
        detail: 'chain not scrollable (scrollHeight=' + rowsEl.scrollHeight +
          ' clientHeight=' + rowsEl.clientHeight + ') — initial-viewport sampling only',
      });
      return samples;
    }
    const targets = [
      { position: 'top', scrollTop: 0 },
      { position: 'middle', scrollTop: Math.max(0, Math.floor(rowsEl.scrollHeight / 2)) },
      { position: 'bottom', scrollTop: Math.max(0, rowsEl.scrollHeight - rowsEl.clientHeight) },
      // After a full traversal the renderer must re-anchor cleanly at the top
      // with the DOM still bounded (no accumulated rows).
      { position: 'top-return', scrollTop: 0 },
    ];
    for (const t of targets) {
      rowsEl.scrollTop = t.scrollTop;
      // The renderer syncs on the scroll event; dispatch it explicitly so the
      // sample reflects the target position even before the browser's own
      // (async) scroll event lands. fetchWindow/syncWindow are idempotent, so
      // a later real scroll event re-syncing the same position is harmless.
      rowsEl.dispatchEvent(new Event('scroll'));
      await window.__editchainDebug.whenIdle(idleTimeoutMs);
      const prefix = 'SCROLL_' + t.position.toUpperCase().replace('-', '_') + '_';
      renderedChecks(meta, checks, prefix);
      const graphState = (typeof window.__editchainGraphState === 'function')
        ? window.__editchainGraphState() : null;
      samples.push({
        position: t.position,
        scrollTop: rowsEl.scrollTop,
        domRows: document.querySelectorAll('#rows .row').length,
        renderTop: graphState ? graphState.renderTop : null,
        renderBottom: graphState ? graphState.renderBottom : null,
        maxLane: graphState ? graphState.maxLane : null,
      });
    }
    return samples;
  }

  // --------------------------------------------------------------------------
  // Public API
  // --------------------------------------------------------------------------

  window.__editchainGraph = {
    /** Run one full graph-probe pass.
     *
     * opts: { limit, idleTimeoutMs, requestTimeoutMs }
     * Returns { open, datasetMeta, rendered, rendererState, scrollSamples,
     *           checks, summary }
     * where datasetMeta carries aggregates (relation kinds, duplicate rates,
     * lane/kind histograms) and the compact dataset stays in-page for
     * datasetChunk() streaming.
     */
    async runGraphProbe(opts) {
      opts = opts || {};
      const limit = (opts.limit && opts.limit > 0) ? Math.floor(opts.limit) : 2000;
      const idleTimeoutMs = opts.idleTimeoutMs || 30000;
      const requestTimeoutMs = (opts.requestTimeoutMs && opts.requestTimeoutMs > 0)
        ? Math.floor(opts.requestTimeoutMs)
        : 120000;

      const checks = [];
      let open = null;
      let datasetMeta = null;
      let scrollSamplesArr = [];
      try {
        // Wait for the renderer to settle before paging so the bridge is warm
        // and the offset-0 snapshot is established.
        await window.__editchainDebug.whenIdle(idleTimeoutMs);
        datasetMeta = await pageAll(limit, requestTimeoutMs);
        const extras = datasetChecks(datasetMeta, checks);
        datasetMeta.relationKinds = extras.relationKindCounts;
        datasetMeta.rowsWithRelations = extras.rowsWithRelations;
        datasetMeta.topLevelRows = extras.topLevelCount;
        datasetMeta.subopRows = extras.subopRowsCount;
        datasetMeta.duplicateNodeKeys = extras.duplicateKeys;
        datasetMeta.duplicateRates = extras.duplicateRates;
        datasetMeta.blobs = extras.blobs;
        datasetMeta.kindHistogram = extras.kindHistogram;
        datasetMeta.subopKindHistogram = extras.subopKindHistogram;
        datasetMeta.laneHistogram = extras.laneHistogram;
        // Settle again (the renderer's own loader may still be buffering) so
        // the DOM dump reflects a stable window.
        await window.__editchainDebug.whenIdle(idleTimeoutMs);
        open = {
          body: openBody,
          readyFired,
          error: (openBody && openBody.Error !== undefined) ? openBody.Error : null,
        };
        checks.push({
          name: 'OPEN_OK',
          pass: !!openBody && openBody.Ok !== undefined && openBody.Ok !== null,
          detail: openBody
            ? (openBody.Ok !== undefined ? 'open succeeded (nodes=' + (openBody.Ok.nodes !== undefined ? openBody.Ok.nodes : '?') + ', repos=' + (openBody.Ok.repos !== undefined ? openBody.Ok.repos : '?') + ')' : 'open error: ' + String(openBody.Error).slice(0, 200))
            : 'no open response observed',
        });
        renderedChecks(datasetMeta, checks);
        // Sample rendered/data coherence at top, middle, and bottom (plus a
        // top re-anchor after the full traversal), waiting for idle each time.
        scrollSamplesArr = await scrollSamples(datasetMeta, checks, idleTimeoutMs);
      } catch (err) {
        checks.push({
          name: 'GRAPH_PROBE_RUN',
          pass: false,
          detail: 'probe failed: ' + String(err && err.message || err),
        });
      }

      const failed = checks.filter((c) => !c.pass);
      const summary = {
        passCount: checks.length - failed.length,
        failCount: failed.length,
      };
      return {
        open,
        datasetMeta,
        rendererState: {
          total: (typeof window.__editchainGetTotal === 'function') ? window.__editchainGetTotal() : null,
          graphState: (typeof window.__editchainGraphState === 'function') ? window.__editchainGraphState() : null,
          dataReady: window.__editchainDataReady === true,
          loadedRows: window.__editchainLoadedRows || 0,
          domRows: document.querySelectorAll('#rows .row').length,
        },
        rendered: (function () {
          const out = [];
          document.querySelectorAll('#rows .row').forEach((el) => {
            const abs = parseInt(el.getAttribute('data-row'), 10);
            const dot = el.querySelector('circle.graphDot');
            const svg = el.querySelector('svg.graphCell');
            out.push({
              abs: Number.isFinite(abs) ? abs : null,
              key: el.getAttribute('data-key'),
              isSubop: el.classList.contains('row-subop'),
              isPlaceholder: el.classList.contains('row-placeholder'),
              h: Math.round(el.getBoundingClientRect().height * 100) / 100,
              dotCx: dot ? parseFloat(dot.getAttribute('cx')) : null,
              dotCy: dot ? parseFloat(dot.getAttribute('cy')) : null,
              svgW: svg ? parseFloat(svg.getAttribute('width')) : null,
            });
          });
          return out;
        })(),
        scrollSamples: scrollSamplesArr,
        checks,
        summary,
      };
    },

    /** Self-tests for the probe's OWN failure detection, driven by an in-page
     * SIMULATED service (no real service, no fixture bridge): the paging path
     * must detect a chain_generation change mid-scan, and a hung GetWindow
     * must reject after the configured timeout instead of hanging.
     *
     * Returns { results, pass, failCount } where each result is
     * { name, pass, detail }.
     */
    async runSelfTests() {
      const results = [];
      const push = (name, pass, detail) => results.push({ name, pass, detail });

      // Install an in-page simulated service: every probe GetWindow request is
      // answered by dispatching a synthetic message event from fixture data.
      function installSimulator(handler) {
        window.vscode.postMessage = function (msg) {
          if (msg && msg.body !== undefined && typeof msg.id === 'number' && handler) {
            handler(msg.id, msg.body);
          }
          return undefined;
        };
      }

      // A small linear chain (newest first) that passes the structural checks,
      // so the self-tests isolate generation/timeout behaviour. `prefix` keeps
      // the module-level key sets disjoint between runs.
      function fixtureRows(n, prefix) {
        const out = [];
        for (let i = 0; i < n; i++) {
          out.push({
            node_key: prefix + ':' + i,
            kind: 'message',
            lane: 0,
            above: i === 0 ? [] : [0],
            below: i === n - 1 ? [] : [0],
            transitions: [],
            parents: i < n - 1 ? [prefix + ':' + (i + 1)] : [],
            parent_relations: [],
            is_subop: false,
            sub_ops: [],
          });
        }
        return out;
      }

      // Simulated GetWindow response over fixture rows.
      function windowFor(req, rows, generationFor) {
        const offset = req.offset || 0;
        const limit = req.limit || 0;
        return {
          Ok: {
            rows: rows.slice(offset, offset + limit),
            total: rows.length,
            max_lane: 0,
            chain_generation: generationFor(offset),
            sub_op_counts: offset === 0 ? rows.map(() => 0) : null,
          },
        };
      }

      // 1 + 2. chain_generation stability: identical across pages must pass;
      // a change mid-scan must FAIL and name the offending pages.
      try {
        const stableRows = fixtureRows(12, 'selftest:a');
        installSimulator((id, body) => {
          window.dispatchEvent(new MessageEvent('message', {
            data: { id, body: windowFor(body.GetWindow, stableRows, () => 7) },
          }));
        });
        const stableMeta = await pageAll(5, 2000);
        const stableChecks = [];
        datasetChecks(stableMeta, stableChecks);
        const stableGen = stableChecks.find((c) => c.name === 'CHAIN_GENERATION_STABLE');
        push('generation-stable-accepted', !!stableGen && stableGen.pass === true,
          stableGen ? stableGen.detail : 'CHAIN_GENERATION_STABLE check missing');

        const changedRows = fixtureRows(12, 'selftest:b');
        installSimulator((id, body) => {
          window.dispatchEvent(new MessageEvent('message', {
            data: { id, body: windowFor(body.GetWindow, changedRows, (offset) => (offset >= 5 ? 42 : 7)) },
          }));
        });
        const changedMeta = await pageAll(5, 2000);
        const changedChecks = [];
        datasetChecks(changedMeta, changedChecks);
        const changedGen = changedChecks.find((c) => c.name === 'CHAIN_GENERATION_STABLE');
        push('generation-change-fails', !!changedGen && changedGen.pass === false &&
          /changed mid-scan/.test(changedGen.detail || ''),
          changedGen ? changedGen.detail : 'CHAIN_GENERATION_STABLE check missing');
      } catch (err) {
        push('generation-self-tests', false, String(err && err.message || err));
      }

      // 3 + 4. Timeout handling: a GetWindow that never resolves must reject
      // after the configured timeout, and pageAll must propagate it as a
      // failed probe run (never hang).
      try {
        installSimulator(() => { /* never respond */ });
        const t0 = Date.now();
        let rejected = null;
        try {
          await request({ GetWindow: { offset: 0, limit: 5, hide_submodules: true, filter: {} } }, 120);
        } catch (err) {
          rejected = err;
        }
        const elapsed = Date.now() - t0;
        push('getwindow-timeout-rejects',
          !!rejected && /timed out/.test(String(rejected && rejected.message || rejected)) && elapsed < 5000,
          rejected
            ? 'request rejected after ' + elapsed + 'ms: ' + String(rejected.message)
            : 'request did not time out within 5s');

        const t1 = Date.now();
        let pagingRejected = null;
        try {
          await pageAll(5, 120);
        } catch (err) {
          pagingRejected = err;
        }
        const elapsedPage = Date.now() - t1;
        push('paging-timeout-propagates',
          !!pagingRejected && /timed out/.test(String(pagingRejected && pagingRejected.message || pagingRejected)) && elapsedPage < 5000,
          pagingRejected
            ? 'pageAll rejected after ' + elapsedPage + 'ms: ' + String(pagingRejected.message)
            : 'pageAll did not time out within 5s');
      } catch (err) {
        push('timeout-self-tests', false, String(err && err.message || err));
      }

      const failed = results.filter((r) => !r.pass);
      return { results, pass: failed.length === 0, failCount: failed.length };
    },

    /** Stream the compact dataset out in slices (JSON array strings), so the
     * Node host can write a machine-readable NDJSON dataset without ever
     * materializing the full dataset as one blob in either process. */
    datasetChunk(start, end) {
      const slice = [];
      const from = Math.max(0, start || 0);
      const to = Math.min(rows.length, (typeof end === 'number' ? end : rows.length));
      for (let i = from; i < to; i++) {
        if (rows[i] !== undefined) slice.push(rows[i]);
      }
      return JSON.stringify(slice);
    },

    datasetLength() {
      return rows.length;
    },
  };
})();
