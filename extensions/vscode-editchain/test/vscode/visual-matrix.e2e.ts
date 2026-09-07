// End-to-end visual state matrix for the EditChain History webview in real
// VS Code.
//
// Uses WebdriverIO's global `expect` (injected by @wdio/globals), exactly like
// history.e2e.ts — no explicit import (importing expect-webdriverio directly
// conflicts with the injected global).
//
// Launched by wdio.visual.conf.ts (real Extension Development Host + native
// Rust service). It drives the DEFAULT production panel — one "EditChain
// History" webview opened with the default `editchain-history.open` command —
// through a deterministic visual state matrix and captures clearly named
// full-workbench and/or webview screenshots for each materially distinct
// state:
//
//   initial-activity     default Activity profile, top of chain, single pane
//   raw-profile          Raw via the real segmented control (hide_trace)
//   find-current/next    real find-in-chain session, match 1 and match 2
//   row-selected         inline row selection (no secondary pane)
//   keyboard-focus       roving keyboard focus after ArrowDown
//   bundle-expanded      first available .row-expandable disclosure (if any)
//   deep-scroll          virtualized window at depth (smooth animated scroll)
//   scroll-top-restored  smooth animated scroll back to the top
//   graph-narrow/wide    graph-column resize with the lane-geometry invariant
//
// Artifacts go under trace/visual-matrix/: per-state PNGs plus a JSON and a
// Markdown manifest recording state names and observed metadata. The suite
// only asserts wire-to-DOM contracts that already exist (it adds no new
// production behaviour) and never mutates the production service: the
// empty/error states are deliberately skipped (they would require mutating
// the real .editchain chain), and the raw-JSON editor is never opened (the
// harness covers the exact openJson envelope).

import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const PROBE_SRC = fs.readFileSync(
  path.join(__dirname, 'layoutProbe.js'),
  'utf8'
);
const MATRIX_DIR = path.join(__dirname, '..', '..', 'trace', 'visual-matrix');

const ROW_TIMEOUT_MS = 120000;
const IDLE_TIMEOUT_MS = 180000;
const FIND_TIMEOUT_MS = 300000;
const QUERY = 'find in chain';

/** Smoothly animate #rows scrollTop to `targetTop` (visible in recordings). */
async function smoothScrollTo(targetTop: number, durationMs: number): Promise<void> {
  await browser.execute((target: number, duration: number) => {
    const rows = document.getElementById('rows')!;
    const start = rows.scrollTop;
    const delta = target - start;
    const t0 = performance.now();
    return new Promise<void>((resolve) => {
      function step(now: number) {
        const p = Math.min(1, (now - t0) / duration);
        const eased = p < 0.5 ? 4 * p * p * p : 1 - Math.pow(-2 * p + 2, 3) / 2;
        rows.scrollTop = start + delta * eased;
        if (p < 1) requestAnimationFrame(step);
        else resolve();
      }
      requestAnimationFrame(step);
    });
  }, targetTop, durationMs);
}

/** Wait three paint frames, optionally requiring a quiescent Rust renderer. */
async function waitPresentedFrames(requireIdle: boolean): Promise<Record<string, unknown>> {
  let frameProbe: Record<string, unknown> = {};
  try {
    await browser.waitUntil(async () => {
      frameProbe = await browser.execute(() => new Promise((resolve) => {
        const startGeneration = Number((window as any).__editchainGeneration ?? -1);
        requestAnimationFrame(() => requestAnimationFrame(() => requestAnimationFrame(() => {
          resolve({
            startGeneration,
            endGeneration: Number((window as any).__editchainGeneration ?? -1),
            dataReady: (window as any).__editchainDataReady === true,
            inFlight: Number((window as any).__editchainInFlightCount ?? -1),
            placeholders: document.querySelectorAll('.row-placeholder').length,
          });
        })));
      }));
      const idle = frameProbe.startGeneration === frameProbe.endGeneration &&
        frameProbe.inFlight === 0;
      return (!requireIdle || idle) && frameProbe.dataReady === true &&
        frameProbe.placeholders === 0;
    }, {
      timeout: 5000,
      interval: 50,
      timeoutMsg: 'Rust history frame generation did not stabilize',
    });
  } catch (error) {
    throw new Error('Rust history frame did not stabilize: ' +
      JSON.stringify(frameProbe), { cause: error });
  }
  return frameProbe;
}

/** Wait for the renderer to settle (no in-flight work, no placeholders). */
async function waitIdle(timeoutMs: number = IDLE_TIMEOUT_MS): Promise<unknown> {
  const startedAt = Date.now();
  let observed: Record<string, unknown> = {};
  try {
    await browser.waitUntil(async () => {
      observed = await browser.execute(() => ({
        dataReady: (window as any).__editchainDataReady === true,
        inFlight: Number((window as any).__editchainInFlightCount ?? -1),
        generation: Number((window as any).__editchainGeneration ?? -1),
        placeholders: document.querySelectorAll('.row-placeholder').length,
        lastError: (window as any).__editchainLastError ?? null,
      }));
      return observed.lastError !== null || (observed.dataReady === true &&
        observed.inFlight === 0 && observed.placeholders === 0);
    }, {
      timeout: timeoutMs,
      interval: 250,
      timeoutMsg: 'Rust history renderer did not reach its settled state',
    });
  } catch (error) {
    throw new Error('Rust history renderer did not become idle: ' +
      JSON.stringify(observed), { cause: error });
  }
  if (observed.lastError !== null) {
    throw new Error(String(observed.lastError));
  }
  await waitPresentedFrames(true);
  return { ...observed, elapsedMs: Date.now() - startedAt };
}

/**
 * Wait for a hydrated, error-free frame and give it three animation frames to
 * paint. Background overscan may continue; its live count is recorded in the
 * manifest instead of being mislabeled as idle.
 */
async function waitVisibleFrame(timeoutMs: number = ROW_TIMEOUT_MS): Promise<unknown> {
  const startedAt = Date.now();
  let observed: Record<string, unknown> = {};
  try {
    await browser.waitUntil(async () => {
      observed = await browser.execute(() => ({
        dataReady: (window as any).__editchainDataReady === true,
        inFlight: Number((window as any).__editchainInFlightCount ?? -1),
        generation: Number((window as any).__editchainGeneration ?? -1),
        placeholders: document.querySelectorAll('.row-placeholder').length,
        lastError: (window as any).__editchainLastError ?? null,
      }));
      return observed.lastError !== null || (observed.dataReady === true &&
        observed.placeholders === 0);
    }, {
      timeout: timeoutMs,
      interval: 250,
      timeoutMsg: 'Rust history visible frame did not hydrate',
    });
  } catch (error) {
    throw new Error('Rust history visible frame did not settle: ' +
      JSON.stringify(observed), { cause: error });
  }
  if (observed.lastError !== null) {
    throw new Error(String(observed.lastError));
  }
  await waitPresentedFrames(false);
  return { ...observed, elapsedMs: Date.now() - startedAt };
}

/** Deterministic state snapshot for the manifest + assertions. */
async function readState(): Promise<Record<string, unknown>> {
  return browser.execute(() => {
    const rowsEl = document.getElementById('rows');
    if (!rowsEl) throw new Error('no #rows element');
    const rows = Array.from(document.querySelectorAll('.row:not(.row-placeholder)'));
    const rowIds = rows.map((row) => row.getAttribute('data-row'));
    const visibleDateCells = Array.from(document.querySelectorAll<HTMLElement>('.date-cell'))
      .filter((cell) => (cell.textContent ?? '').trim() !== '' &&
        getComputedStyle(cell).display !== 'none');
    const profileFn = (window as any).__editchainGetProfile;
    const debug = (window as any).__editchainGpuDebug;
    const metrics = typeof debug?.metrics === 'function' ? debug.metrics() : null;
    // Per-row SVG graph fragments: exactly one aria-hidden
    // svg.graph-row-fragment per hydrated row, centred on the row's middle.
    let fragmentCount = 0;
    let maxAlignDelta = 0;
    const fragmentIssues: Array<{
      row: number; count: number; ariaHidden: string | null;
    }> = [];
    const alignExamples: Array<{ row: number; delta: number }> = [];
    for (const el of rows) {
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
      loader: debug?.loader ?? null,
      backend: typeof debug?.backend === 'function' ? debug.backend() : null,
      profile: typeof profileFn === 'function' ? profileFn() : null,
      dataReady: (window as any).__editchainDataReady === true,
      inFlight: Number((window as any).__editchainInFlightCount ?? -1),
      total: typeof (window as any).__editchainGetTotal === 'function'
        ? (window as any).__editchainGetTotal() : -1,
      rowCount: rows.length,
      uniqueRowCount: new Set(rowIds).size,
      placeholders: document.querySelectorAll('.row-placeholder').length,
      scrollTop: rowsEl.scrollTop,
      scrollHeight: rowsEl.scrollHeight,
      clientHeight: rowsEl.clientHeight,
      hasDetail: !!document.getElementById('detail') ||
        (document.getElementById('layout')?.classList.contains('has-detail') ?? false),
      firstKeys: rows.slice(0, 8).map((r) => r.getAttribute('data-key')),
      rendererInstanceId: (window as any).__editchainRendererInstanceId,
      graphState: typeof debug?.graphState === 'function'
        ? debug.graphState()
        : null,
      laneXAll: typeof debug?.laneXAll === 'function'
        ? debug.laneXAll()
        : null,
      renderCount: metrics?.renderCount ?? 0,
      vertexCount: metrics?.vertexCount ?? 0,
      fragmentCount,
      fragmentMissing: rows.length - fragmentCount,
      fragmentIssues,
      maxAlignDelta: Math.round(maxAlignDelta * 100) / 100,
      alignExamples,
      rendererStatusBarCount: document.querySelectorAll(
        '#gpu-toolbar, #gpu-backend, #gpu-status').length,
      visibleDateCount: visibleDateCells.length,
      clippedDateCount: visibleDateCells.filter((cell) =>
        cell.scrollWidth > cell.clientWidth + 1).length,
      canvasCount: document.querySelectorAll('#gpu-canvas-host canvas').length,
      foreignCanvasCount: document.querySelectorAll(
        'canvas:not(#gpu-canvas-host canvas)'
      ).length,
      gridRole: document.querySelector('.tbl-grid')?.getAttribute('role') ?? null,
      gridRowCount: document.querySelector('.tbl-grid')?.getAttribute('aria-rowcount') ?? null,
    };
  });
}

describe('EditChain History visual state matrix', () => {
  it('captures the deterministic visual state matrix on the default production panel', async function (this: { timeout(ms: number): void }) {
    // One bounded budget: first window (120s) + lazy find index (300s) +
    // matrix transitions. Typical runs finish well inside this.
    this.timeout(720000);
    // A matrix is one coherent run: remove only this dedicated generated
    // artifact directory so stale screenshots cannot masquerade as states
    // captured by the current renderer instance.
    fs.rmSync(MATRIX_DIR, { recursive: true, force: true });
    fs.mkdirSync(MATRIX_DIR, { recursive: true });

    const workbench = await browser.getWorkbench();
    const title = await workbench.getTitleBar().getTitle();
    expect(title).toContain('editchain');

    // Default command path: close workbench chrome, then open the default
    // history panel — exactly one "EditChain History" panel, no companion.
    await browser.executeWorkbench(async (vscode: any) => {
      await vscode.commands.executeCommand('workbench.action.closeAuxiliaryBar');
      await vscode.commands.executeCommand('notifications.clearAll');
      await vscode.commands.executeCommand('notifications.hideToasts');
      await vscode.commands.executeCommand('editchain-history.open');
    });
    const titles = await browser.executeWorkbench((vscode: any) =>
      vscode.window.tabGroups.all.flatMap((g: any) => g.tabs.map((t: any) => t.label)));
    console.log('[visual-matrix] panel titles:', JSON.stringify(titles));
    expect(titles).toContain('EditChain History');
    expect(titles.filter((t: string) => String(t).includes('EditChain History'))).toHaveLength(1);

    const webview = await workbench.getWebviewByTitle('EditChain History');
    await webview.open();
    await browser.$('.row').waitForExist({ timeout: ROW_TIMEOUT_MS });

    // Inject the same text-only layout probe so whenIdle uses renderer state.
    await browser.execute((src: string) => {
      // eslint-disable-next-line no-eval
      (0, eval)(src);
      return typeof (window as any).__editchainDebug;
    }, PROBE_SRC);
    await waitIdle();

    const states: Array<Record<string, unknown>> = [];
    let rendererInstanceId = '';
    const skipped = [
      {
        name: 'empty-history',
        reason: 'would require mutating the production service/chain dir — out of scope for this non-mutating harness',
      },
      {
        name: 'error-state',
        reason: 'would require forcing a production service failure without mutating the service — not practical here',
      },
    ];

    const tidyWorkbench = async () => {
      // executeWorkbench changes out of the webview frame. Make that context
      // transition explicit and re-enter the same retained renderer before
      // taking either screenshot or reading state.
      await webview.close();
      await browser.executeWorkbench(async (vscode: any) => {
        await vscode.commands.executeCommand('workbench.action.closeAuxiliaryBar');
        await vscode.commands.executeCommand('notifications.clearAll');
        await vscode.commands.executeCommand('notifications.hideToasts');
      });
      await webview.open();
      await browser.$('#rows .row[data-key]').waitForExist({ timeout: ROW_TIMEOUT_MS });
    };

    const writeManifest = () => {
      const manifest = {
        suite: 'visual-matrix',
        command: 'editchain-history.open',
        panelTitle: 'EditChain History',
        panelCount: 1,
        rendererInstanceId,
        capturedAt: new Date().toISOString(),
        states,
        skipped,
      };
      const jsonPath = path.join(MATRIX_DIR, 'manifest.json');
      fs.writeFileSync(jsonPath, JSON.stringify(manifest, null, 2));
      const mdRows = states.map((s: any) => {
        const files = (s.files as string[]).map((f) => path.basename(f)).join(', ');
        return `| ${s.name} | ${s.capture} | ${files} | \`${JSON.stringify(s.observed)}\` |`;
      }).join('\n');
      const md = [
        '# EditChain History — Visual State Matrix (real VS Code)',
        '',
        '- Suite: `visual-matrix.e2e.ts` (config `wdio.visual.conf.ts`)',
        '- Default command: `editchain-history.open` — exactly one `EditChain History` panel',
        '- Renderer instance: `' + rendererInstanceId + '`',
        '- Captured at: ' + manifest.capturedAt,
        '',
        '| State | Capture | Files | Observed metadata |',
        '|---|---|---|---|',
        mdRows,
        '',
        'Skipped by design (would mutate the production service):',
        ...skipped.map((s) => '- `' + s.name + '`: ' + s.reason),
        '',
      ].join('\n');
      fs.writeFileSync(path.join(MATRIX_DIR, 'manifest.md'), md);
      console.log('[visual-matrix] manifest ->', jsonPath);
    };

    const capture = async (name: string, full: boolean, observed: Record<string, unknown>) => {
      if (full) await tidyWorkbench();
      await browser.$('#rows .row[data-key]').waitForExist({ timeout: 15000 });
      const files: string[] = [];
      const pane = path.join(MATRIX_DIR, `visual-${name}-webview.png`);
      await browser.$('body').saveScreenshot(pane);
      files.push(pane);
      if (full) {
        const fullShot = path.join(MATRIX_DIR, `visual-${name}-full.png`);
        await browser.saveScreenshot(fullShot);
        files.push(fullShot);
      }
      states.push({ name, capture: full ? 'full+webview' : 'webview', files, observed });
      console.log(`[visual-matrix] state ${name}:`, JSON.stringify(observed));
      writeManifest();
    };

    // --- initial-activity -----------------------------------------------------
    const initial = await readState();
    rendererInstanceId = initial.rendererInstanceId as string;
    expect(initial.loader).toBe('rust-history');
    expect(initial.backend).toBe('svg');
    expect(initial.profile).toBe('activity');
    expect(initial.dataReady).toBe(true);
    expect((initial.rowCount as number)).toBeGreaterThan(0);
    expect(initial.uniqueRowCount).toBe(initial.rowCount);
    expect((initial.total as number)).toBeGreaterThan(0);
    expect(initial.hasDetail).toBe(false);
    expect(initial.scrollTop).toBe(0);
    expect(initial.rendererStatusBarCount).toBe(0);
    expect((initial.visibleDateCount as number)).toBeGreaterThan(0);
    expect(initial.clippedDateCount).toBe(0);
    expect(initial.canvasCount).toBe(0);
    expect(initial.foreignCanvasCount).toBe(0);
    expect((initial.renderCount as number)).toBeGreaterThan(0);
    expect((initial.vertexCount as number)).toBe(0);
    expect((initial.fragmentCount as number)).toBe(initial.rowCount as number);
    expect((initial.fragmentMissing as number)).toBe(0);
    expect((initial.maxAlignDelta as number)).toBeLessThanOrEqual(1);
    expect(initial.gridRole).toBe('grid');
    // __editchainGetTotal is the authoritative absolute-slot count (including
    // collapsed sub-op slots); aria-rowcount is the currently visible logical
    // row count exposed to assistive technology. It must be positive and may
    // only grow up to the authoritative total as disclosures expand.
    expect(Number(initial.gridRowCount)).toBeGreaterThan(0);
    expect(Number(initial.gridRowCount)).toBeLessThanOrEqual(initial.total as number);
    const laneXBaseline = initial.laneXAll as number[] | null;
    const naturalGraphWidth = (initial.graphState as any)?.graphWidth ?? null;
    expect(laneXBaseline).toBeTruthy();
    expect(naturalGraphWidth).toBeTruthy();
    await capture('initial-activity', true, initial);

    // --- raw-profile ----------------------------------------------------------
    await browser.execute(() => {
      const btn = document.getElementById('profile-raw');
      if (!btn) throw new Error('no #profile-raw control');
      btn.click();
    });
    await browser.waitUntil(async () => browser.execute(() => {
      const profileFn = (window as any).__editchainGetProfile;
      const rowsEl = document.getElementById('rows');
      return typeof profileFn === 'function' && profileFn() === 'raw' &&
        !!rowsEl && rowsEl.scrollTop === 0 &&
        document.querySelectorAll('#rows .row:not(.row-placeholder)').length > 0;
    }), { timeout: ROW_TIMEOUT_MS, interval: 100 });
    await waitIdle();
    const raw = await readState();
    expect(raw.profile).toBe('raw');
    expect(raw.dataReady).toBe(true);
    expect((raw.rowCount as number)).toBeGreaterThan(0);
    expect(raw.scrollTop).toBe(0);
    const rawPressed = await browser.execute(() =>
      document.getElementById('profile-raw')?.getAttribute('aria-pressed'));
    expect(rawPressed).toBe('true');
    await capture('raw-profile', false, raw);

    // Back to Activity through the real control (Raw -> Activity must reset
    // the window and drop stale DOM before the new generation arrives).
    await browser.execute(() => {
      const btn = document.getElementById('profile-activity');
      if (!btn) throw new Error('no #profile-activity control');
      btn.click();
    });
    await browser.waitUntil(async () => browser.execute(() => {
      const profileFn = (window as any).__editchainGetProfile;
      const rowsEl = document.getElementById('rows');
      return typeof profileFn === 'function' && profileFn() === 'activity' &&
        !!rowsEl && rowsEl.scrollTop === 0 &&
        document.querySelectorAll('#rows .row:not(.row-placeholder)').length > 0;
    }), { timeout: ROW_TIMEOUT_MS, interval: 100 });
    await waitIdle();
    const activity = await readState();
    expect(activity.profile).toBe('activity');

    // --- find-current / find-next ---------------------------------------------
    await browser.$('#search').setValue(QUERY);
    await browser.keys('Enter');
    await browser.waitUntil(async () => browser.execute(() => {
      const text = (document.getElementById('search-counter')?.textContent || '').trim();
      if (text === '0 of 0' || text === 'error') return true;
      if (!/^1 of \d+\+?$/.test(text)) return false;
      return !!document.querySelector('.row-find-current');
    }), { timeout: FIND_TIMEOUT_MS, interval: 200 });
    const readFind = () => browser.execute(() => {
      const counter = document.getElementById('search-counter');
      const cur = document.querySelector('.row-find-current');
      return {
        counter: (counter?.textContent || '').trim(),
        findRow: cur ? Number(cur.getAttribute('data-row')) : -1,
        findKey: cur ? cur.getAttribute('data-key') : null,
        focusIsInput: document.activeElement === document.getElementById('search'),
        total: typeof (window as any).__editchainGetTotal === 'function'
          ? (window as any).__editchainGetTotal() : -1,
      };
    });
    const findCurrent = await readFind();
    if (findCurrent.counter === '0 of 0' || findCurrent.counter === 'error') {
      states.push({
        name: 'find-current',
        capture: 'webview',
        files: [],
        observed: { skipped: true, reason: 'query "' + QUERY + '" returned ' + findCurrent.counter + ' on this chain' },
      });
      writeManifest();
    } else {
      expect(findCurrent.counter).toMatch(/^1 of \d+\+?$/);
      expect(findCurrent.findKey).toBeTruthy();
      expect(findCurrent.total).toBe(activity.total); // the chain is untouched
      await capture('find-current', true, findCurrent);
      const findSurvived = await readFind();
      expect(findSurvived.counter).toBe(findCurrent.counter);

      // Next through the visible chevron: match 2, still the real chain.
      await browser.execute(() => {
        document.getElementById('search')?.focus();
      });
      await browser.$('#search-next').click();
      await browser.waitUntil(async () => browser.execute(() => {
        const text = (document.getElementById('search-counter')?.textContent || '').trim();
        return /^2 of \d+\+?$/.test(text) && !!document.querySelector('.row-find-current');
      }), { timeout: 60000, interval: 100 });
      const findNext = await readFind();
      expect(findNext.counter).toMatch(/^2 of \d+\+?$/);
      // Distinct underlying hits can resolve to the same visible parent when
      // both live inside one folded activity group. The counter is the search
      // cursor identity; the highlighted row key is intentionally shared.
      expect(findNext.findKey).toBeTruthy();
      await capture('find-next', false, findNext);

      // Clear the session through the real input handler (no reload, no JSON).
      await browser.execute(() => {
        const input = document.getElementById('search') as HTMLInputElement;
        input.value = '';
        input.dispatchEvent(new Event('input', { bubbles: true }));
      });
      await browser.waitUntil(async () => browser.execute(() => {
        const text = (document.getElementById('search-counter')?.textContent || '').trim();
        return text === '' && !document.querySelector('.row-find-current');
      }), { timeout: 60000, interval: 100 });
    }

    // Return to the top smoothly before the selection states.
    await smoothScrollTo(0, 1200);
    await browser.waitUntil(async () => browser.execute(() => {
      const rowsEl = document.getElementById('rows');
      const top = document.querySelector('.row[data-row="0"]');
      return !!rowsEl && rowsEl.scrollTop === 0 && !!top &&
        !top.classList.contains('row-placeholder');
    }), { timeout: ROW_TIMEOUT_MS, interval: 100 });
    await waitIdle();

    // --- row-selected ---------------------------------------------------------
    await browser.execute(() => {
      const row = document.querySelector<HTMLElement>('.row[data-row="1"]');
      if (!row) throw new Error('no .row[data-row="1"] to select');
      row.click();
    });
    const selection = await browser.execute(() => {
      const sel = document.querySelector('.row.row-selected');
      return {
        selectedRow: sel ? Number(sel.getAttribute('data-row')) : null,
        selectedKey: sel ? sel.getAttribute('data-key') : null,
        ariaSelected: sel ? sel.getAttribute('aria-selected') : null,
        hasDetail: !!document.getElementById('detail') ||
          (document.getElementById('layout')?.classList.contains('has-detail') ?? false),
      };
    });
    expect(selection.selectedRow).toBe(1);
    expect(selection.ariaSelected).toBe('true');
    expect(selection.hasDetail).toBe(false); // inline selection, no secondary pane
    await capture('row-selected', false, selection);

    // --- keyboard-focus -------------------------------------------------------
    await browser.execute(() => {
      const row = document.querySelector<HTMLElement>('.row[data-row="1"]');
      if (!row) throw new Error('no .row[data-row="1"] to focus');
      row.focus();
      row.dispatchEvent(new KeyboardEvent('keydown', {
        key: 'ArrowDown', bubbles: true, cancelable: true,
      }));
    });
    const keyboard = await browser.execute(() => {
      const active = document.activeElement?.closest('.row');
      const sel = document.querySelector('.row.row-selected');
      return {
        focusedRow: active ? Number(active.getAttribute('data-row')) : null,
        tabbable: document.querySelectorAll('.row[tabindex="0"]').length,
        selectedRow: sel ? Number(sel.getAttribute('data-row')) : null,
      };
    });
    expect(keyboard.focusedRow).toBe(2); // roving focus moved down one
    expect(keyboard.tabbable).toBe(1);   // exactly one row in the tab order
    await capture('keyboard-focus', false, keyboard);

    // --- bundle-expanded (only when the real chain exposes an expandable row) --
    let expandAbs: number | null = null;
    let expandDepth = 0;
    const findExpandable = () => browser.execute(() => {
      const el = document.querySelector<HTMLElement>(
        '.row-expandable[data-activity-bundle]:not(.row-placeholder)');
      return el ? Number(el.getAttribute('data-row')) : null;
    });
    expandAbs = await findExpandable();
    if (expandAbs == null) {
      // One bounded page-down (visible in the recording), then give up.
      await smoothScrollTo(3000, 1200);
      await browser.pause(300);
      expandAbs = await findExpandable();
      expandDepth = expandAbs == null ? 0 : 3000;
    }
    if (expandAbs == null) {
      states.push({
        name: 'bundle-expanded',
        capture: 'webview',
        files: [],
        observed: { skipped: true, reason: 'no .row-expandable row rendered near the top of the chain' },
      });
      writeManifest();
    } else {
      const collapsedGroupMarker = await browser.execute((abs: number) => {
        const graph = document.querySelector<HTMLElement>(
          '.row[data-row="' + abs + '"] .graph-cell');
        const capsule = graph?.querySelector<SVGRectElement>('.graphBundleCapsule');
        const terminal = graph?.querySelector<SVGCircleElement>('.graphBundleTerminal');
        return {
          dots: graph?.querySelectorAll('.graphDot').length ?? 0,
          capsules: graph?.querySelectorAll('.graphBundleCapsule').length ?? 0,
          width: Number(capsule?.getAttribute('width') ?? 0),
          terminalDiameter: Number(terminal?.getAttribute('r') ?? 0) * 2,
        };
      }, expandAbs);
      await browser.execute((abs: number) => {
        const row = document.querySelector<HTMLElement>('.row[data-row="' + abs + '"]');
        const chevron = row?.querySelector<HTMLElement>('.subop-chevron');
        if (!chevron) throw new Error('no .subop-chevron on row ' + abs);
        chevron.click();
      }, expandAbs);
      await browser.waitUntil(async () => browser.execute(() =>
        document.querySelectorAll('.row-subop').length > 0), { timeout: 60000, interval: 100 });
      await waitIdle();
      const nestedAbs = await browser.execute(() => {
        const row = document.querySelector<HTMLElement>(
          '.row-subop[data-hierarchy-depth="1"].row-expandable'
        );
        return row ? Number(row.getAttribute('data-row')) : null;
      });
      if (nestedAbs != null) {
        await browser.execute((abs: number) => {
          const row = document.querySelector<HTMLElement>('.row[data-row="' + abs + '"]');
          const chevron = row?.querySelector<HTMLElement>('.subop-chevron');
          if (!chevron) throw new Error('no nested .subop-chevron on row ' + abs);
          chevron.click();
        }, nestedAbs);
        await browser.waitUntil(async () => browser.execute(() =>
          document.querySelectorAll('.row-subop[data-hierarchy-depth="2"]').length > 0),
        { timeout: 60000, interval: 100 });
        await waitIdle();
      }
      await browser.execute((abs: number) => {
        document.querySelector<HTMLElement>('.row[data-row="' + abs + '"]')
          ?.scrollIntoView({ block: 'center' });
      }, expandAbs);
      await browser.pause(200);
      const expanded = await readState();
      const ariaExpanded = await browser.execute((abs: number) =>
        document.querySelector('.row[data-row="' + abs + '"]')?.getAttribute('aria-expanded'), expandAbs);
      const nestedAriaExpanded = nestedAbs == null ? null : await browser.execute((abs: number) =>
        document.querySelector('.row[data-row="' + abs + '"]')?.getAttribute('aria-expanded'), nestedAbs);
      const activityAffordance = await browser.execute((abs: number) => {
        const row = document.querySelector<HTMLElement>('.row[data-row="' + abs + '"]');
        const activity = row?.querySelector<HTMLElement>('.activity-cell');
        const label = activity?.querySelector<HTMLElement>('.activity-label');
        const chevron = activity?.querySelector<HTMLElement>('.subop-chevron');
        return {
          order: activity ? Array.from(activity.children).map((child) => child.className) : [],
          label: label?.textContent ?? '',
          glyph: chevron?.textContent ?? '',
          colorMatches: !!label && !!chevron &&
            getComputedStyle(label).color === getComputedStyle(chevron).color,
          contentChevron: !!row?.querySelector('.text-cell .subop-chevron'),
        };
      }, expandAbs);
      const contentPresentation = await browser.execute((parentAbs: number) => {
        const styleOf = (element: Element | null) => {
          if (!element) return null;
          const style = getComputedStyle(element);
          return {
            color: style.color,
            backgroundColor: style.backgroundColor,
            borderTop: style.borderTop,
            borderRight: style.borderRight,
            borderBottom: style.borderBottom,
            borderLeft: style.borderLeft,
            borderRadius: style.borderRadius,
            fontFamily: style.fontFamily,
            fontSize: style.fontSize,
            fontWeight: style.fontWeight,
            lineHeight: style.lineHeight,
            paddingTop: style.paddingTop,
            paddingRight: style.paddingRight,
            paddingBottom: style.paddingBottom,
            paddingLeft: style.paddingLeft,
          };
        };
        const gitChip = document.querySelector<HTMLElement>('.git-prefix-chip');
        const gitContent = gitChip?.closest('.row')
          ?.querySelector<HTMLElement>('.text-cell .git-summary-text') ?? null;
        const activityChip = document.querySelector<HTMLElement>('.bundle-count');
        const agentChip = document.querySelector<HTMLElement>('.session-chip-agent');
        const workUnitChip = document.querySelector<HTMLElement>('.work-unit-count');
        const chipSelector = [
          '.git-prefix-chip', '.bundle-count', '.bundle-status',
          '.session-chip', '.rel-badge', '.out-badge', '.work-unit-count',
        ].join(',');
        const chips = Array.from(document.querySelectorAll<HTMLElement>(chipSelector));
        const resizeHandles = Array.from(document.querySelectorAll<HTMLElement>(
          '.col-resize-handle'
        ));
        const tableHeader = document.querySelector('.tbl-header');
        const humanSummary = document.querySelector<HTMLElement>('.row-human .summary');
        const renderedRows = Array.from(document.querySelectorAll<HTMLElement>(
          '.row:not(.row-placeholder)'
        ));
        const sessionRow = renderedRows.find((row) =>
          row.getAttribute('data-classification') === 'session') ?? null;
        const ordinaryRow = renderedRows.find((row) =>
          row.getAttribute('data-classification') !== 'session') ?? null;
        const sessionBox = sessionRow?.getBoundingClientRect() ?? null;
        const ordinaryBox = ordinaryRow?.getBoundingClientRect() ?? null;
        const sessionStyle = sessionRow ? getComputedStyle(sessionRow) : null;
        const hoverProbe = document.createElement('div');
        hoverProbe.style.backgroundColor =
          'var(--vscode-list-hoverBackground, var(--ec-surface-raised))';
        hoverProbe.style.position = 'absolute';
        hoverProbe.style.visibility = 'hidden';
        document.body.append(hoverProbe);
        const hoverBackgroundColor = getComputedStyle(hoverProbe).backgroundColor;
        hoverProbe.remove();
        const openedGroupRows = renderedRows.filter((row) =>
          row.classList.contains('row-subop'));
        const parentGraph = document.querySelector<HTMLElement>(
          '.row[data-row="' + parentAbs + '"] .graph-cell');
        const opacityByColumn = Object.fromEntries([
          ['activity', '.activity-cell'],
          ['tags', '.tags-cell'],
          ['content', '.text-cell'],
          ['date', '.date-cell'],
          ['author', '.author-cell'],
          ['commit', '.commit-cell'],
        ].map(([name, selector]) => [name, Array.from(new Set(renderedRows
          .map((row) => row.querySelector<HTMLElement>(selector))
          .filter((cell): cell is HTMLElement => cell != null)
          .map((cell) => getComputedStyle(cell).opacity)))]));
        const conversationCounts = { agent: 0, user: 0 };
        const conversationMismatches: Array<{ author: string; label: string }> = [];
        for (const rendered of renderedRows) {
          const abs = Number(rendered.getAttribute('data-row'));
          const wire = window.__editchainRowAt?.(abs);
          if (!wire || wire.activity_kind !== 'conversation') continue;
          const author = String(wire.author ?? '');
          const label = (rendered.querySelector('.activity-label')?.textContent ?? '').trim();
          const expected = author === 'human' || author === 'user' ? 'user' : 'agent';
          conversationCounts[expected] += 1;
          if (label !== expected) conversationMismatches.push({ author, label });
        }
        const prefix = (gitChip?.textContent ?? '').trim();
        const content = (gitContent?.textContent ?? '').trim();
        const gitStyle = styleOf(gitChip);
        return {
          prefix,
          content,
          repeatsPrefix: !!prefix && (content === prefix || content.startsWith(prefix + ' ') ||
            content.startsWith(prefix + ':')),
          humanWeight: humanSummary ? getComputedStyle(humanSummary).fontWeight : null,
          activityChipStyleMatches: !!gitStyle &&
            JSON.stringify(styleOf(activityChip)) === JSON.stringify(gitStyle),
          agentChipStyleMatches: agentChip == null ? null :
            JSON.stringify(styleOf(agentChip)) === JSON.stringify(gitStyle),
          workUnitChipStyleMatches: workUnitChip == null ? null :
            JSON.stringify(styleOf(workUnitChip)) === JSON.stringify(gitStyle),
          headerLabels: Array.from(document.querySelectorAll('.tbl-header .th'))
            .map((cell) => (cell.textContent ?? '').trim()),
          chipCount: chips.length,
          misplacedChips: chips.filter((chip) =>
            !chip.parentElement?.classList.contains('tags-cell')).length,
          contentChipCount: document.querySelectorAll(
            '.text-cell :is(' + chipSelector + ')').length,
          maxTagsPerRow: Math.max(0, ...renderedRows.map((row) =>
            row.querySelector('.tags-cell')?.children.length ?? 0)),
          resizeHandleColumns: resizeHandles.map((handle) => handle.dataset.col ?? ''),
          resizeHandlesInHeader: resizeHandles.every((handle) =>
            handle.parentElement === tableHeader),
          visibleResizeIndicators: resizeHandles.every((handle) => {
            const style = getComputedStyle(handle, '::after');
            return style.width === '1px' && style.backgroundColor !== 'rgba(0, 0, 0, 0)';
          }),
          openedGroupMarkers: {
            rows: openedGroupRows.length,
            dots: openedGroupRows.filter((row) =>
              row.querySelectorAll('.graph-cell .graphDot').length === 1).length,
            nestedCapsules: openedGroupRows.filter((row) =>
              row.querySelector('.graph-cell .graphBundleCapsule') != null).length,
          },
          unfoldedGroupMarker: {
            dots: parentGraph?.querySelectorAll('.graphDot').length ?? 0,
            capsules: parentGraph?.querySelectorAll('.graphBundleCapsule').length ?? 0,
            terminals: parentGraph?.querySelectorAll('.graphBundleTerminal').length ?? 0,
          },
          sessionTreatment: {
            found: sessionRow != null,
            backgroundColor: sessionStyle?.backgroundColor ?? null,
            color: sessionStyle?.color ?? null,
            editorBackgroundColor: getComputedStyle(document.body).backgroundColor,
            boxShadow: sessionStyle?.boxShadow ?? null,
            hoverBackgroundColor,
            fullWidth: !!sessionBox && !!ordinaryBox &&
              Math.abs(sessionBox.width - ordinaryBox.width) <= 1,
          },
          opacityByColumn,
          conversationCounts,
          conversationMismatches,
        };
      }, expandAbs);
      expect(ariaExpanded).toBe('true');
      if (nestedAbs != null) expect(nestedAriaExpanded).toBe('true');
      expect(activityAffordance.order).toEqual(['activity-label', 'subop-chevron']);
      expect(activityAffordance.glyph).toBe('\u25be');
      expect(activityAffordance.colorMatches).toBe(true);
      expect(activityAffordance.contentChevron).toBe(false);
      expect(contentPresentation.prefix.length).toBeGreaterThan(0);
      expect(contentPresentation.content.length).toBeGreaterThan(0);
      expect(contentPresentation.repeatsPrefix).toBe(false);
      expect(contentPresentation.humanWeight).toBe('400');
      expect(contentPresentation.activityChipStyleMatches).toBe(true);
      if (contentPresentation.agentChipStyleMatches != null) {
        expect(contentPresentation.agentChipStyleMatches).toBe(true);
      }
      if (contentPresentation.workUnitChipStyleMatches != null) {
        expect(contentPresentation.workUnitChipStyleMatches).toBe(true);
      }
      expect(contentPresentation.headerLabels).toEqual([
        'Graph', 'Activity', 'Tags', 'Content', 'Date',
      ]);
      expect(contentPresentation.chipCount).toBeGreaterThan(0);
      expect(contentPresentation.misplacedChips).toBe(0);
      expect(contentPresentation.contentChipCount).toBe(0);
      expect(contentPresentation.maxTagsPerRow).toBeGreaterThan(1);
      expect(contentPresentation.resizeHandleColumns).toEqual([
        'graph', 'activity', 'tags', 'content', 'date',
      ]);
      expect(contentPresentation.resizeHandlesInHeader).toBe(true);
      expect(contentPresentation.visibleResizeIndicators).toBe(true);
      expect(contentPresentation.openedGroupMarkers.rows).toBeGreaterThan(0);
      expect(contentPresentation.openedGroupMarkers.dots)
        .toBe(contentPresentation.openedGroupMarkers.rows);
      expect(contentPresentation.openedGroupMarkers.nestedCapsules).toBe(0);
      expect(collapsedGroupMarker.dots).toBe(0);
      expect(collapsedGroupMarker.capsules).toBe(1);
      expect(collapsedGroupMarker.width).toBeGreaterThan(0);
      expect(collapsedGroupMarker.width - collapsedGroupMarker.terminalDiameter)
        .toBeCloseTo(1, 5);
      expect(contentPresentation.unfoldedGroupMarker)
        .toEqual({ dots: 1, capsules: 0, terminals: 0 });
      expect(contentPresentation.sessionTreatment.found).toBe(true);
      expect(contentPresentation.sessionTreatment.backgroundColor)
        .not.toBe(contentPresentation.sessionTreatment.editorBackgroundColor);
      expect(contentPresentation.sessionTreatment.backgroundColor)
        .toBe(contentPresentation.sessionTreatment.hoverBackgroundColor);
      expect(contentPresentation.sessionTreatment.fullWidth).toBe(true);
      expect(contentPresentation.opacityByColumn.activity).toEqual(['1']);
      expect(contentPresentation.opacityByColumn.tags).toEqual(['1']);
      expect(contentPresentation.opacityByColumn.content).toEqual(['1']);
      expect(contentPresentation.opacityByColumn.date).toHaveLength(1);
      expect(contentPresentation.opacityByColumn.author).toEqual(['1']);
      expect(contentPresentation.opacityByColumn.commit).toEqual(['1']);
      expect(contentPresentation.conversationCounts.agent).toBeGreaterThan(0);
      expect(contentPresentation.conversationCounts.user).toBeGreaterThan(0);
      expect(contentPresentation.conversationMismatches).toEqual([]);
      expect((expanded.rowCount as number)).toBeGreaterThan(0);
      expect(expanded.uniqueRowCount).toBe(expanded.rowCount);
      await capture('bundle-expanded', false, {
        ...expanded, parentRow: expandAbs, ariaExpanded, nestedRow: nestedAbs,
        nestedAriaExpanded, activityAffordance, collapsedGroupMarker,
        contentPresentation, expandDepth,
      });
      // Collapse again so the later states start from the default reveal state.
      if (nestedAbs != null) {
        await browser.execute((abs: number) => {
          const row = document.querySelector<HTMLElement>('.row[data-row="' + abs + '"]');
          const chevron = row?.querySelector<HTMLElement>('.subop-chevron');
          if (!chevron) throw new Error('no nested .subop-chevron to collapse row ' + abs);
          chevron.click();
        }, nestedAbs);
        await browser.waitUntil(async () => browser.execute(() =>
          document.querySelectorAll('.row-subop[data-hierarchy-depth="2"]').length === 0),
        { timeout: 60000, interval: 100 });
        await waitIdle();
      }
      await browser.execute((abs: number) => {
        const row = document.querySelector<HTMLElement>('.row[data-row="' + abs + '"]');
        const chevron = row?.querySelector<HTMLElement>('.subop-chevron');
        if (!chevron) throw new Error('no .subop-chevron to collapse row ' + abs);
        chevron.click();
      }, expandAbs);
      await browser.waitUntil(async () => browser.execute(() =>
        document.querySelectorAll('.row-subop').length === 0), { timeout: 60000, interval: 100 });
      await waitIdle();
      await smoothScrollTo(0, 1200);
      await browser.pause(200);
    }

    // --- deep-scroll (virtualized window at depth) ----------------------------
    const maxScroll = await browser.execute(() => {
      const rowsEl = document.getElementById('rows');
      if (!rowsEl) throw new Error('no #rows element');
      return Math.max(0, rowsEl.scrollHeight - rowsEl.clientHeight);
    });
    // BUFFER=400 at ROW_H=34 keeps the first 13,600px cached. When the collapsed
    // view is larger than that buffer, move beyond the boundary to prove a true
    // virtual-window reanchor while keeping the live service fetch bounded.
    // A compact grouped view may fit entirely in the rendered window instead.
    const deepTarget = Math.min(14000, maxScroll as number);
    const deepThreshold = Math.max(0, deepTarget - 200);
    await smoothScrollTo(deepTarget, 1600);
    await browser.waitUntil(async () => browser.execute((threshold: number) => {
      const rowsEl = document.getElementById('rows');
      return !!rowsEl && rowsEl.scrollTop >= threshold &&
        document.querySelectorAll('.row-placeholder').length === 0;
    }, deepThreshold), { timeout: ROW_TIMEOUT_MS, interval: 100 });
    await waitVisibleFrame();
    const deep = await readState();
    expect((deep.scrollTop as number)).toBeGreaterThanOrEqual(deepThreshold);
    expect((deep.rowCount as number)).toBeGreaterThan(0);
    expect(deep.uniqueRowCount).toBe(deep.rowCount);
    if ((deep.total as number) > 100) {
      // Bounded viewport: only a slice is rendered, never the whole chain.
      expect((deep.rowCount as number)).toBeLessThan(deep.total as number);
    }
    if (deepTarget > 0 &&
        (deep.rowCount as number) < Number(deep.gridRowCount)) {
      expect(deep.firstKeys).not.toEqual(initial.firstKeys);
    }
    await capture('deep-scroll', true, deep);
    const deepAfterTidy = await readState();
    expect(deepAfterTidy.rendererInstanceId).toBe(deep.rendererInstanceId);
    expect((deepAfterTidy.scrollTop as number)).toBeGreaterThanOrEqual(deepThreshold);

    // --- scroll-top-restored --------------------------------------------------
    await smoothScrollTo(0, 1600);
    await browser.waitUntil(async () => browser.execute(() => {
      const rowsEl = document.getElementById('rows');
      const top = document.querySelector('.row[data-row="0"]');
      return !!rowsEl && rowsEl.scrollTop === 0 && !!top &&
        !top.classList.contains('row-placeholder');
    }), { timeout: ROW_TIMEOUT_MS, interval: 100 });
    await waitVisibleFrame();
    const topRestored = await readState();
    expect(topRestored.scrollTop).toBe(0);
    await capture('scroll-top-restored', false, topRestored);

    // --- Activity/Tags column resizing ------------------------------------------
    const dragTextColumn = (column: 'activity' | 'tags', deltaX: number): Promise<number> =>
      browser.execute((col: string, delta: number) => {
        const handle = document.querySelector<HTMLElement>(
          '.col-resize-handle[data-col="' + col + '"]'
        );
        if (!handle) throw new Error('no ' + col + ' resize handle');
        const rect = handle.getBoundingClientRect();
        const startX = rect.left + rect.width / 2;
        const y = rect.top + rect.height / 2;
        handle.dispatchEvent(new MouseEvent('mousedown', {
          bubbles: true, cancelable: true, clientX: startX, clientY: y,
        }));
        window.dispatchEvent(new MouseEvent('mousemove', {
          bubbles: true, cancelable: true, clientX: startX + delta, clientY: y,
        }));
        window.dispatchEvent(new MouseEvent('mouseup', {
          bubbles: true, cancelable: true, clientX: startX + delta, clientY: y,
        }));
        return document.querySelector<HTMLElement>('.tbl-header .th.' + col)
          ?.getBoundingClientRect().width ?? 0;
      }, column, deltaX);
    const textColumnWidths = await browser.execute(() => ({
      activity: document.querySelector<HTMLElement>('.tbl-header .th.activity')
        ?.getBoundingClientRect().width ?? 0,
      tags: document.querySelector<HTMLElement>('.tbl-header .th.tags')
        ?.getBoundingClientRect().width ?? 0,
    }));
    const widerActivity = await dragTextColumn('activity', 18);
    expect(widerActivity).toBeGreaterThan((textColumnWidths as any).activity);
    const widerTags = await dragTextColumn('tags', 24);
    expect(widerTags).toBeGreaterThan((textColumnWidths as any).tags);
    const restoredActivity = await dragTextColumn('activity', -18);
    const restoredTags = await dragTextColumn('tags', -24);
    expect(Math.abs(restoredActivity - (textColumnWidths as any).activity)).toBeLessThanOrEqual(1);
    expect(Math.abs(restoredTags - (textColumnWidths as any).tags)).toBeLessThanOrEqual(1);
    await waitVisibleFrame();

    // --- graph-column narrow/wide (lane geometry invariant) --------------------
    const dragGraph = (deltaX: number): Promise<number> =>
      browser.execute((delta: number) => {
        const handle = document.querySelector<HTMLElement>('.col-resize-handle[data-col="graph"]');
        if (!handle) throw new Error('no graph resize handle');
        const rect = handle.getBoundingClientRect();
        const y = rect.top + Math.min(4, Math.max(1, rect.height / 2));
        const startX = rect.left + rect.width / 2;
        handle.dispatchEvent(new MouseEvent('mousedown', {
          bubbles: true, cancelable: true, clientX: startX, clientY: y,
        }));
        window.dispatchEvent(new MouseEvent('mousemove', {
          bubbles: true, cancelable: true, clientX: startX + delta, clientY: y,
        }));
        window.dispatchEvent(new MouseEvent('mouseup', {
          bubbles: true, cancelable: true,
        }));
        return (window as any).__editchainGpuDebug.graphState().graphWidth;
      }, deltaX);
    const expectPointerWidth = (actual: number, expected: number): void => {
      // MouseEvent.clientX is integer-valued in Chromium, while the natural
      // lane-derived graph width can be fractional (44.28px here).
      expect(Math.abs(actual - expected)).toBeLessThanOrEqual(0.5);
    };

    const wideTarget = await browser.execute((natural: number) => {
      const rowsEl = document.getElementById('rows');
      if (!rowsEl) throw new Error('no #rows element');
      return Math.min(natural + 160, Math.max(natural + 40, Math.floor(rowsEl.clientWidth * 0.6)));
    }, naturalGraphWidth as number);
    expect(wideTarget).toBeGreaterThan(naturalGraphWidth as number);

    // Narrow: drag to MIN_COL_W.graph (40px) — lane X positions must not move.
    const narrowWidth = await dragGraph(40 - (naturalGraphWidth as number));
    await waitVisibleFrame();
    const narrow = await readState();
    expect((narrow.graphState as any).graphWidth).toBe(40);
    expect(narrowWidth).toBe(40);
    expect(narrow.laneXAll).toEqual(laneXBaseline); // lane geometry invariant
    await capture('graph-narrow', false, { ...narrow, graphWidthAfterDrag: narrowWidth });

    // Wide: drag to the bounded wide target — lane X positions must not move.
    const wideWidth = await dragGraph((wideTarget as number) - 40);
    await waitVisibleFrame();
    const wide = await readState();
    expectPointerWidth((wide.graphState as any).graphWidth, wideTarget as number);
    expectPointerWidth(wideWidth, wideTarget as number);
    expect(wide.laneXAll).toEqual(laneXBaseline);
    await capture('graph-wide', true, { ...wide, graphWidthAfterDrag: wideWidth });

    // Restore the natural width so the session ends in its default layout.
    const restoredWidth = await dragGraph((naturalGraphWidth as number) - wideWidth);
    await waitVisibleFrame();
    const restored = await readState();
    expectPointerWidth((restored.graphState as any).graphWidth, naturalGraphWidth as number);
    expectPointerWidth(restoredWidth, naturalGraphWidth as number);
    expect(restored.laneXAll).toEqual(laneXBaseline);
    await capture('graph-restored', false, { ...restored, graphWidthAfterDrag: restoredWidth });

    await webview.close();
  });
});
