// End-to-end test for the Rust/WASM history renderer (per-row SVG graph
// fragments) in real VS Code.
//
// Uses WebdriverIO's global `expect` (injected by @wdio/globals), exactly like
// history.e2e.ts — no explicit import (importing expect-webdriverio directly
// conflicts with the injected global).
//
// Launched by wdio.gpu.conf.ts (real Extension Development Host + native Rust
// service). The DEFAULT command `editchain-history.open` opens ONE panel
// titled "EditChain History"; that panel loads the exact production scaffold
// (media/main.css + media/gpu-preview/gpu-preview.css) and
// media/rust-history/loader.js as its ONLY script. The Rust shell owns the
// runtime (window/frame/lane presentation as per-row SVG graph fragments
// inside each .graph-cell; no canvas surface is created) and exposes
// window.__editchainGpuDebug (loader: 'rust-history', dataReady, lastError,
// backend: 'svg', snapshot, metrics, whenIdle)
// plus the __editchainGetProfile/GetTotal/RowAt compatibility hooks. This
// spec exercises the pieces the standalone harness cannot: the default open
// command, the single panel title, the production control paths (Activity/Raw
// profile, find-in-chain submit + navigation + clear, scrolling, inline
// selection/keyboard roving) inside the Rust-backed webview, and the debug
// renderer contract (loader identity, backend 'svg', renderCount > 0,
// vertexCount 0, zero canvases, one aria-hidden svg.graph-row-fragment per
// hydrated row). There is deliberately NO
// second panel and NO side-by-side capture: CPU-vs-GPU fixture parity is the
// offscreen regression oracle (test/harness/functionalParity.test.js +
// scripts/ui-gpu-preview.mjs), not a shipped two-panel UI. This spec does NOT
// open the raw-JSON editor (disruptive to framing); the harness covers the
// exact openJson envelope.

import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const TRACE_DIR = path.join(__dirname, '..', '..', 'trace');

// Real-service Open + the first render window can take 20s+ on the 119k-node
// chain (the service builds blobs/diagnostics on Open), so the deadline
// mirrors the history e2e's 120s — the outer mocha timeout bounds the run,
// not a fixed service deadline.
const ROW_TIMEOUT_MS = 120000;
const IDLE_TIMEOUT_MS = 60000;

describe('EditChain Rust history renderer (per-row SVG)', () => {
  it('loads VS Code with the extension', async () => {
    const workbench = await browser.getWorkbench();
    const title = await workbench.getTitleBar().getTitle();
    expect(title).toContain('editchain');
  });

  it('opens the default history panel with the per-row SVG renderer, drives the shared production controls, and captures the single-panel frame', async function () {
    // The first find lazily builds the real service's lexical index, so this
    // test needs a larger budget than the config default (mirrors history.e2e).
    this.timeout(420000);
    const workbench = await browser.getWorkbench();

    // Keep the editor area dedicated to the capture: close auxiliary bar and
    // notifications, then run the DEFAULT history command — it must open ONE
    // panel titled "EditChain History" with the per-row SVG renderer inside it.
    await browser.executeWorkbench(async (vscode) => {
      await vscode.commands.executeCommand('workbench.action.closeAuxiliaryBar');
      await vscode.commands.executeCommand('notifications.clearAll');
      await vscode.commands.executeCommand('notifications.hideToasts');
      await vscode.commands.executeCommand('editchain-history.open');
    });

    // Exactly one history panel must exist, titled "EditChain History" — no
    // companion/side-by-side panel with a second title may appear.
    const titles = await browser.executeWorkbench((vscode) =>
      vscode.window.tabGroups.all.flatMap((g: any) => g.tabs.map((t: any) => t.label)));
    console.log('[gpu-e2e] panel titles:', JSON.stringify(titles));
    expect(titles).toContain('EditChain History');
    expect(titles.filter((t) => String(t).includes('EditChain History'))).toHaveLength(1);
    expect(titles).not.toContain('EditChain History — Rust/WASM GPU');

    // Open the history webview frame and wait for production rows to appear:
    // the Rust shell renders into #rows, one .row[data-row] element per
    // visible row, each carrying data-key.
    let historyWebview: Awaited<ReturnType<typeof workbench.getWebviewByTitle>> | undefined;
    await browser.waitUntil(async () => {
      try {
        historyWebview = await workbench.getWebviewByTitle('EditChain History');
        return true;
      } catch {
        return false;
      }
    }, {
      timeout: 30000,
      timeoutMsg: 'history tab existed but its webview frame never mounted',
    });
    if (!historyWebview) throw new Error('history webview did not mount');
    await historyWebview.open();
    let startupError: string | null = null;
    await browser.waitUntil(async () => {
      const state = await browser.execute(() => ({
        rows: document.querySelectorAll('#rows .row[data-key]').length,
        error: window.__editchainGpuDebug?.lastError || null,
      }));
      startupError = state.error;
      return state.rows > 0 || startupError !== null;
    }, {
      timeout: ROW_TIMEOUT_MS,
      timeoutMsg: 'history panel produced neither rows nor an explicit error',
    });
    if (startupError !== null) {
      throw new Error('Rust renderer startup failed: ' + startupError);
    }

    // Assert the __editchainGpuDebug contract in the DEFAULT panel: the
    // rust-history loader facade, the 'svg' backend, a healthy snapshot over
    // the production DOM, zero canvases/vertices, and one aria-hidden
    // svg.graph-row-fragment per hydrated row, centred on the row.
    const debug = await browser.execute(() => {
      const g = window.__editchainGpuDebug;
      if (!g || typeof g.snapshot !== 'function') {
        throw new Error('window.__editchainGpuDebug missing or incomplete');
      }
      const snap = g.snapshot();
      const metrics = typeof g.metrics === 'function' ? g.metrics() : null;
      // Per-row SVG fragments live inside each .graph-cell; #gpu-canvas-host
      // stays an empty inert scaffold; #gpu-rows mirrors the frame rows.
      const graphCanvases = document.querySelectorAll('#gpu-canvas-host canvas');
      const hydratedRows = Array.from(document.querySelectorAll(
        '#rows .row[data-row][data-key]:not(.row-placeholder)'));
      let fragmentCount = 0;
      const fragmentIssues: Array<{
        row: number; count: number; ariaHidden: string | null;
      }> = [];
      let maxAlignDelta = 0;
      const alignExamples: Array<{ row: number; delta: number }> = [];
      for (const el of hydratedRows) {
        const fragments = el.querySelectorAll('svg.graph-row-fragment');
        const fragment = fragments[0] ?? null;
        if (fragments.length !== 1 || !fragment ||
            fragment.getAttribute('aria-hidden') !== 'true') {
          if (fragmentIssues.length < 5) {
            fragmentIssues.push({
              row: Number(el.getAttribute('data-row')),
              count: fragments.length,
              ariaHidden: fragment ? fragment.getAttribute('aria-hidden') : null,
            });
          }
          continue;
        }
        fragmentCount++;
        const rowBox = el.getBoundingClientRect();
        const svgBox = fragment.getBoundingClientRect();
        const shapes = Array.from(fragment.querySelectorAll(
          '.graphDot, .graphBundleCapsule'));
        let minY = Infinity;
        let maxY = -Infinity;
        let any = false;
        for (const shape of shapes) {
          const b = shape.getBoundingClientRect();
          if (b.width <= 0 && b.height <= 0) continue;
          any = true;
          if (b.top < minY) minY = b.top;
          if (b.bottom > maxY) maxY = b.bottom;
        }
        const center = any ? (minY + maxY) / 2 : svgBox.top + svgBox.height / 2;
        const delta = Math.abs(center - (rowBox.top + rowBox.height / 2));
        if (delta > maxAlignDelta) maxAlignDelta = delta;
        if (delta > 1 && alignExamples.length < 5) {
          alignExamples.push({
            row: Number(el.getAttribute('data-row')),
            delta: Math.round(delta * 100) / 100,
          });
        }
      }
      return {
        loader: g.loader || null,
        backend: typeof g.backend === 'function' ? g.backend() : (snap.backend || null),
        dataReady: g.dataReady === true,
        lastError: g.lastError || null,
        rows: Array.isArray(snap.rows) ? snap.rows.length : 0,
        total: typeof snap.total === 'number' ? snap.total : -1,
        domRows: hydratedRows.length,
        mirrorRows: document.querySelectorAll('#gpu-rows [data-row][data-key]').length,
        hasCanvas: graphCanvases.length > 0,
        canvasCount: graphCanvases.length,
        foreignCanvasCount: document.querySelectorAll('canvas:not(#gpu-canvas-host canvas)').length,
        canvasWidth: graphCanvases[0]?.width || 0,
        canvasHeight: graphCanvases[0]?.height || 0,
        fragmentCount,
        fragmentMissing: hydratedRows.length - fragmentCount,
        fragmentIssues,
        maxAlignDelta: Math.round(maxAlignDelta * 100) / 100,
        alignExamples,
        renderCount: metrics?.renderCount ?? 0,
        vertexCount: metrics?.vertexCount ?? 0,
      };
    });
    console.log('[gpu-e2e] debug:', JSON.stringify(debug));

    expect(debug.loader).toBe('rust-history');
    expect(debug.backend).toBe('svg');
    expect(debug.dataReady).toBe(true);
    expect(debug.lastError).toBeNull();
    expect(debug.rows).toBeGreaterThan(0);
    expect(debug.domRows).toBeGreaterThan(0);
    expect(debug.mirrorRows).toBe(debug.rows);
    expect(debug.total).toBeGreaterThan(0);
    expect(debug.hasCanvas).toBe(false);
    expect(debug.canvasCount).toBe(0);
    expect(debug.foreignCanvasCount).toBe(0);
    expect(debug.canvasWidth).toBe(0);
    expect(debug.canvasHeight).toBe(0);
    expect(debug.fragmentCount).toBe(debug.domRows);
    expect(debug.fragmentMissing).toBe(0);
    expect(debug.maxAlignDelta).toBeLessThanOrEqual(1);
    expect(debug.renderCount).toBeGreaterThan(0);
    expect(debug.vertexCount).toBe(0);

    // Deterministic settle before driving controls: whenIdle resolves only when
    // the renderer reports no in-flight work and stable frames.
    const idle = await browser.execute((timeout) => window.__editchainGpuDebug.whenIdle(timeout), IDLE_TIMEOUT_MS);
    console.log('[gpu-e2e] idle:', JSON.stringify(idle));

    // --- Production control path inside the Rust-backed history panel --------
    // Activity/Raw profile switch through the real segmented control. Raw
    // (hide_trace=false) must refetch and re-render through the Rust shell.
    const profileDefaults = await browser.execute(() => ({
      profile: typeof window.__editchainGetProfile === 'function'
        ? window.__editchainGetProfile() : null,
      rawPressed: document.getElementById('profile-raw')?.getAttribute('aria-pressed'),
    }));
    console.log('[gpu-e2e] profile defaults:', JSON.stringify(profileDefaults));
    expect(profileDefaults.profile).toBe('activity');
    await browser.execute(() => {
      document.getElementById('profile-raw').click();
    });
    await browser.waitUntil(async () => browser.execute(() =>
      typeof window.__editchainGetProfile === 'function' &&
      window.__editchainGetProfile() === 'raw' &&
      document.getElementById('rows').scrollTop === 0 &&
      document.querySelectorAll('#rows .row:not(.row-placeholder)').length > 0), {
      timeout: ROW_TIMEOUT_MS,
      timeoutMsg: 'history panel did not re-render under the Raw profile',
    });
    await browser.execute((timeout) => window.__editchainGpuDebug.whenIdle(timeout), IDLE_TIMEOUT_MS);
    const rawTotal = await browser.execute(() => window.__editchainGetTotal());
    // Pin Activity back for the deterministic find-in-chain query below.
    await browser.execute(() => {
      document.getElementById('profile-activity').click();
    });
    await browser.waitUntil(async () => browser.execute(() =>
      typeof window.__editchainGetProfile === 'function' &&
      window.__editchainGetProfile() === 'activity' &&
      document.querySelectorAll('#rows .row:not(.row-placeholder)').length > 0), {
      timeout: ROW_TIMEOUT_MS,
      timeoutMsg: 'history panel did not re-render under the Activity profile',
    });
    await browser.execute((timeout) => window.__editchainGpuDebug.whenIdle(timeout), IDLE_TIMEOUT_MS);
    const activityTotal = await browser.execute(() => window.__editchainGetTotal());
    expect(rawTotal).toBeGreaterThan(activityTotal);
    expect(activityTotal).toBeGreaterThan(0);

    // Find-in-chain through the real keyboard path: type the query and press
    // Enter. The Rust shell forwards the read-only FindInHistory request.
    const QUERY = 'find in chain';
    await browser.$('#search').setValue(QUERY);
    await browser.keys('Enter');
    await browser.waitUntil(async () => browser.execute(() => {
      const text = (document.getElementById('search-counter')?.textContent || '').trim();
      if (text === '0 of 0' || text === 'error') return true;
      if (!/^1 of \d+\+?$/.test(text)) return false;
      const cur = document.querySelector('.row-find-current');
      return !!cur && !!cur.getAttribute('data-key');
    }), { timeout: 300000, interval: 200, timeoutMsg: 'Rust find did not settle' });
    const findState = await browser.execute(() => {
      const counter = document.getElementById('search-counter');
      const cur = document.querySelector('.row-find-current');
      return {
        counter: (counter?.textContent || '').trim(),
        findRow: cur ? Number(cur.getAttribute('data-row')) : -1,
        findKey: cur ? cur.getAttribute('data-key') : null,
        total: window.__editchainGetTotal(),
      };
    });
    console.log('[gpu-e2e] find settled:', JSON.stringify(findState));
    expect(findState.findKey).toBeTruthy();
    expect(findState.total).toBe(activityTotal);
    // Next-match navigation stays in the real chain and updates the counter.
    await browser.$('#search-next').click();
    await browser.waitUntil(async () => browser.execute(() => {
      const text = (document.getElementById('search-counter')?.textContent || '').trim();
      return /^2 of \d+\+?$/.test(text) && !!document.querySelector('.row-find-current');
    }), { timeout: 30000, timeoutMsg: 'Rust find did not advance to match 2' });
    // Clear the find session through the input handler (no reload, no JSON).
    await browser.execute(() => {
      const input = document.getElementById('search');
      input.value = '';
      input.dispatchEvent(new Event('input', { bubbles: true }));
    });
    await browser.waitUntil(async () => browser.execute(() => {
      const text = (document.getElementById('search-counter')?.textContent || '').trim();
      return text === '' && !document.querySelector('.row-find-current');
    }), { timeout: 30000, timeoutMsg: 'Rust find did not clear' });

    // Scrolling: page the production virtual window (fetch + render on scroll)
    // and return to the top.
    await browser.execute(() => {
      const rows = document.getElementById('rows');
      rows.scrollTop = 2500;
    });
    await browser.waitUntil(async () => browser.execute(() => {
      const rows = document.getElementById('rows');
      const target = document.querySelector('.row[data-row="70"]');
      return rows.scrollTop > 2000 &&
        !!target && !target.classList.contains('row-placeholder') &&
        typeof window.__editchainRowAt === 'function' &&
        window.__editchainRowAt(70) != null;
    }), { timeout: ROW_TIMEOUT_MS, timeoutMsg: 'history panel did not page on scroll' });
    await browser.execute(() => {
      document.getElementById('rows').scrollTop = 0;
    });
    await browser.waitUntil(async () => browser.execute(() => {
      const top = document.querySelector('.row[data-row="0"]');
      return !!top && !top.classList.contains('row-placeholder');
    }), { timeout: ROW_TIMEOUT_MS, timeoutMsg: 'history panel did not return to the top' });

    // Inline selection + keyboard roving (safe: no raw-JSON activation).
    await browser.execute(() => {
      const row = document.querySelector('.row[data-row="1"]');
      if (!row) throw new Error('no .row[data-row="1"] to select');
      row.click();
    });
    const selection = await browser.execute(() => {
      const sel = document.querySelector('.row.row-selected');
      return {
        selectedRow: sel ? Number(sel.getAttribute('data-row')) : null,
        selectedKey: sel ? sel.getAttribute('data-key') : null,
        ariaSelected: sel ? sel.getAttribute('aria-selected') : null,
      };
    });
    console.log('[gpu-e2e] selection:', JSON.stringify(selection));
    expect(selection.selectedRow).toBe(1);
    expect(selection.ariaSelected).toBe('true');
    expect(selection.selectedKey).toBeTruthy();
    await browser.execute(() => {
      const row = document.querySelector('.row[data-row="1"]');
      if (!row) throw new Error('no .row[data-row="1"] to focus');
      row.focus();
      row.dispatchEvent(new KeyboardEvent('keydown', { key: 'ArrowDown', bubbles: true, cancelable: true }));
    });
    const roving = await browser.execute(() => {
      const active = document.activeElement?.closest('.row');
      return active ? Number(active.getAttribute('data-row')) : null;
    });
    expect(roving).toBe(2);

    // --- Single-panel contract artifact --------------------------------------
    // Record the Rust renderer contract exercised above. Renderer equivalence
    // against the CPU/SVG oracle renderer is NOT asserted here: it is the
    // offscreen regression oracle (test/harness/functionalParity.test.js +
    // scripts/ui-gpu-preview.mjs).
    fs.mkdirSync(TRACE_DIR, { recursive: true });
    fs.writeFileSync(path.join(TRACE_DIR, 'e2e-history-gpu-contract.json'), JSON.stringify({
      backend: debug.backend,
      dataReady: debug.dataReady,
      total: debug.total,
      rows: debug.rows,
      mirrorRows: debug.mirrorRows,
      canvasCount: debug.canvasCount,
      foreignCanvasCount: debug.foreignCanvasCount,
      fragmentCount: debug.fragmentCount,
      fragmentMissing: debug.fragmentMissing,
      maxAlignDelta: debug.maxAlignDelta,
      renderCount: debug.renderCount,
      vertexCount: debug.vertexCount,
      profile: { activityTotal, rawTotal },
      find: findState,
    }, null, 2));

    // --- Single-panel capture -------------------------------------------------
    // Tidy the workbench chrome and capture ONLY the one history panel frame:
    // there is no side-by-side workbench screenshot anymore.
    await browser.executeWorkbench(async (vscode) => {
      await vscode.commands.executeCommand('workbench.action.closeAuxiliaryBar');
      await vscode.commands.executeCommand('notifications.clearAll');
      await vscode.commands.executeCommand('notifications.hideToasts');
    });

    const panelShot = path.join(TRACE_DIR, 'e2e-history-webview.png');
    await browser.$('#rows .row[data-key]').waitForExist({ timeout: ROW_TIMEOUT_MS });
    await browser.$('body').saveScreenshot(panelShot);
    console.log('[gpu-e2e] history webview screenshot ->', panelShot);

    // Leave the webview context (clean frame teardown).
    await historyWebview.close();
  });
});
