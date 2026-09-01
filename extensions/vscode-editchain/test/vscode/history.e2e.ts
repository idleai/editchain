// End-to-end test for the EditChain History extension in real VS Code.
//
// Uses WebdriverIO's global `expect` (injected by @wdio/globals), not an
// explicit import — importing expect-webdriverio directly conflicts with the
// injected global ("Cannot redefine property: soft").
//
// Launched by wdio-vscode-service (see wdio.conf.ts). Validates the pieces the
// standalone Puppeteer harness cannot: extension activation, native Rust service
// spawn, the message bridge, and the webview/panel lifecycle.
//
// It also injects the same text-only layout probe (test/harness/layoutProbe.js)
// into the webview so the identical textual checks run inside real VS Code.

import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const PROBE_SRC = fs.readFileSync(
  path.join(__dirname, '..', 'harness', 'layoutProbe.js'),
  'utf8'
);

/**
 * Smoothly animate the webview's #rows container from its current scrollTop to
 * a target, in small steps per animation frame. This makes the scroll visible
 * in a recorded video (vs. an instant jump).
 *
 * Runs inside the webview frame (call after webview.open()).
 */
async function smoothScrollTo(targetTop, durationMs) {
  await browser.execute((target, duration) => {
    const rows = document.getElementById('rows');
    const start = rows.scrollTop;
    const delta = target - start;
    const t0 = performance.now();
    return new Promise((resolve) => {
      function step(now) {
        const p = Math.min(1, (now - t0) / duration);
        // easeInOutCubic for a natural feel.
        const eased = p < 0.5 ? 4 * p * p * p : 1 - Math.pow(-2 * p + 2, 3) / 2;
        rows.scrollTop = start + delta * eased;
        if (p < 1) requestAnimationFrame(step);
        else resolve();
      }
      requestAnimationFrame(step);
    });
  }, targetTop, durationMs);
}

/**
 * Scroll the webview's #rows container to the bottom smoothly, repeatedly,
 * until no more rows load. The renderer fetches 500-row pages on scroll; we
 * animate down, let the async fetch + re-render settle, and repeat until
 * scrollHeight stops growing (all pages loaded).
 *
 * Runs inside the webview frame (call after webview.open()).
 */
async function scrollHistoryToBottomSmooth() {
  let lastHeight = -1;
  let stable = 0;
  const STABLE_ROUNDS = 4; // stop after this many no-growth rounds
  while (stable < STABLE_ROUNDS) {
    const h = await browser.execute(() => {
      const rows = document.getElementById('rows');
      return rows.scrollHeight; // read current height
    });
    if (h === lastHeight) {
      stable++;
    } else {
      lastHeight = h;
      stable = 0;
    }
    // Animate to the bottom over ~1.5s so it's visible in the recording.
    await smoothScrollTo(h, 1500);
    // Give the service round-trip + DOM rebuild time between passes.
    await browser.pause(400);
  }
}

describe('EditChain History Explorer', () => {
  it('loads VS Code with the extension', async () => {
    const workbench = await browser.getWorkbench();
    // The Extension Development Host title includes our workspace name.
    // getTitle() returns the title bar's HTML; check for the workspace label.
    const title = await workbench.getTitleBar().getTitle();
    expect(title).toContain('editchain');
  });

  it('opens the history explorer webview and renders rows', async () => {
    const workbench = await browser.getWorkbench();

    // Keep the editor area dedicated to the history capture: VS Code can open
    // Agent/Chat in the auxiliary bar by default, but that is workbench chrome,
    // not part of the extension. Close it before opening the webview.
    await browser.executeWorkbench(async (vscode) => {
      await vscode.commands.executeCommand('workbench.action.closeAuxiliaryBar');
      await vscode.commands.executeCommand('notifications.clearAll');
      await vscode.commands.executeCommand('notifications.hideToasts');
      await vscode.commands.executeCommand('editchain-history.open');
    });

    // Find the webview panel and switch into its iframe.
    const webview = await workbench.getWebviewByTitle('EditChain History');
    await webview.open();

    // Wait for rows to render. Opening the 119k-node chain can take 20s+ (the
    // service builds blobs/diagnostics on Open), so the deadline is long — the
    // outer mocha timeout bounds the run, not a fixed service deadline.
    await browser.$('.row').waitForExist({ timeout: 120000 });

    const rowCount = await browser.$$('.row').length;
    console.log('[e2e] rows rendered:', rowCount);
    expect(rowCount).toBeGreaterThan(0);

    // Inject the text-only layout probe into the webview frame.
    await browser.execute((src) => {
      // eslint-disable-next-line no-eval
      (0, eval)(src);
      return typeof window.__editchainDebug;
    }, PROBE_SRC);

    // Wait for the UI to settle deterministically, then run textual checks.
    const idle = await browser.execute(() => window.__editchainDebug.whenIdle(60000));
    console.log('[e2e] idle:', JSON.stringify(idle));

    const assertion = await browser.execute(() => window.__editchainDebug.assertLayout());
    console.log('[e2e] checks pass=' + assertion.passCount + ' fail=' + assertion.failCount);
    assertion.checks.forEach((c) =>
      console.log('[e2e]   ' + c.name + ' ' + (c.pass ? 'PASS' : 'FAIL') + ' — ' + c.detail));

    // The probe must have executed and produced a well-formed result, and every
    // layout check must pass. The harness checks were corrected so scenario
    // gaps are skipped rather than false-failing, and per-scenario checks are
    // now test-blocking: a layout regression fails the e2e run instead of being
    // recorded for later.
    expect(typeof assertion.passCount).toBe('number');
    expect(assertion.failCount).toBe(0);

    // Deterministic Pulse capture from the REAL VS Code webview. Pulse is now
    // the production presentation: no prototype treatment switch and no side
    // panel, just one uninterrupted history surface.
    const presentation = await browser.execute(() => ({
      treatment: document.body.dataset.treatment,
      treatmentControl: !!document.getElementById('treatment-control'),
      productMark: !!document.getElementById('product-mark'),
      singlePane: !document.getElementById('detail') &&
        !document.getElementById('layout').classList.contains('has-detail'),
    }));
    expect(presentation.treatment).toBe('pulse');
    expect(presentation.treatmentControl).toBe(false);
    expect(presentation.productMark).toBe(false);
    expect(presentation.singlePane).toBe(true);

    // Clear any startup notifications that arrived while the service loaded,
    // and assert the right auxiliary bar is physically absent from the frame
    // before taking the full-workbench screenshot.
    await webview.close();
    await browser.executeWorkbench(async (vscode) => {
      await vscode.commands.executeCommand('workbench.action.closeAuxiliaryBar');
      await vscode.commands.executeCommand('notifications.clearAll');
      await vscode.commands.executeCommand('notifications.hideToasts');
    });
    const auxiliaryBarHidden = await browser.execute(() => {
      const auxiliary = document.querySelector('.part.auxiliarybar');
      return !auxiliary || getComputedStyle(auxiliary).display === 'none' ||
        auxiliary.getBoundingClientRect().width < 1;
    });
    expect(auxiliaryBarHidden).toBe(true);
    await webview.open();
    await browser.$('.row').waitForExist({ timeout: 10000 });

    const fullShot = path.join(__dirname, '..', '..', 'trace', 'e2e-history-pulse.png');
    const paneShot = path.join(__dirname, '..', '..', 'trace', 'e2e-history-pulse-webview.png');
    const canonicalShot = path.join(__dirname, '..', '..', 'trace', 'e2e-history.png');
    await browser.saveScreenshot(fullShot);
    await browser.$('body').saveScreenshot(paneShot);
    await browser.saveScreenshot(canonicalShot);
    console.log('[e2e] screenshot ->', fullShot);
    console.log('[e2e] webview screenshot ->', paneShot);

    // Leave the webview context.
    await webview.close();
  });

  it('keeps row selection inline; double-click explicitly opens raw JSON', async () => {
    const workbench = await browser.getWorkbench();

    await browser.executeWorkbench((vscode) => {
      vscode.commands.executeCommand('editchain-history.open');
    });
    const webview = await workbench.getWebviewByTitle('EditChain History');
    await webview.open();
    await browser.$('.row').waitForExist({ timeout: 120000 });

    // Move away from the initial viewport so the assertion covers cached row
    // identity and scroll restoration, not merely a coincidentally identical
    // top-of-chain render.
    await browser.execute(() => {
      const rows = document.getElementById('rows');
      rows.scrollTop = Math.min(3_400, Math.max(0, rows.scrollHeight - rows.clientHeight));
    });
    await browser.waitUntil(async () => {
      return browser.execute(() => {
        const rows = document.getElementById('rows');
        return rows.scrollTop > 0 && document.querySelectorAll('.row-placeholder').length === 0;
      });
    }, { timeout: 30000, interval: 100 });

    const before = await browser.execute(() => {
      const rows = document.getElementById('rows');
      const rendered = Array.from(document.querySelectorAll('.row'));
      const candidate = rendered.find((element) => {
        const abs = Number(element.getAttribute('data-row'));
        const row = window.__editchainRowAt?.(abs);
        return row && !row.is_subop && !(row.sub_ops || []).length &&
          (row.op_id || row.git_oid);
      });
      if (!candidate) throw new Error('no rendered raw-JSON-capable row');
      const keys = rendered.slice(0, 8).map((row) => row.getAttribute('data-key'));
      const result = {
        rendererInstanceId: window.__editchainRendererInstanceId,
        scrollTop: rows.scrollTop,
        keys,
        clickedKey: candidate.getAttribute('data-key'),
      };
      candidate.click();
      return result;
    });
    expect(before.rendererInstanceId).toBeTruthy();

    // An ordinary row click only selects within the history surface.
    const inline = await browser.execute(() => {
      return {
        secondaryPane: !!document.getElementById('detail') ||
          document.getElementById('layout').classList.contains('has-detail'),
        selected: !!document.querySelector('.row.row-selected'),
      };
    });
    console.log('[e2e] inline selection after row click:', JSON.stringify(inline));
    expect(inline.secondaryPane).toBe(false);
    expect(inline.selected).toBe(true);

    // Double-click is the explicit pointer path to the read-only raw JSON
    // editor; no secondary pane is introduced inside the webview.
    await browser.execute(() => {
      const selected = document.querySelector('.row.row-selected');
      if (!selected) throw new Error('no inline-selected row for raw JSON activation');
      selected.dispatchEvent(new MouseEvent('dblclick', { bubbles: true }));
    });

    // Leave the iframe after the explicit action asks the extension host to
    // show the read-only JSON editor, then wait until that editor is active.
    await webview.close();
    await browser.waitUntil(async () => {
      const tab = await workbench.getEditorView().getActiveTab();
      return !!tab && (await tab.getTitle()) !== 'EditChain History';
    }, { timeout: 30000, interval: 100 });

    await browser.executeWorkbench(async (vscode) => {
      await vscode.commands.executeCommand('workbench.action.navigateBack');
    });

    // Inspect the first frame presented after Back. No wait-for-row is used
    // here: a retained page must already contain its rows and must never expose
    // the renderer's initial Loading message.
    const restoredWebview = await workbench.getWebviewByTitle('EditChain History');
    await restoredWebview.open();
    const after = await browser.execute(() => {
      const rows = document.getElementById('rows');
      return {
        rendererInstanceId: window.__editchainRendererInstanceId,
        scrollTop: rows.scrollTop,
        keys: Array.from(document.querySelectorAll('.row')).slice(0, 8)
          .map((row) => row.getAttribute('data-key')),
        rowCount: document.querySelectorAll('.row').length,
        message: document.querySelector('.view-message')?.textContent || '',
      };
    });

    console.log('[e2e] raw-json/back before:', JSON.stringify(before));
    console.log('[e2e] raw-json/back after:', JSON.stringify(after));
    expect(after.rendererInstanceId).toBe(before.rendererInstanceId);
    expect(after.scrollTop).toBe(before.scrollTop);
    expect(after.keys).toEqual(before.keys);
    expect(after.rowCount).toBeGreaterThan(0);
    expect(after.message).not.toContain('Loading');
    await restoredWebview.close();
  });

  it('switches Activity/Raw profiles and supports keyboard activation', async () => {
    const workbench = await browser.getWorkbench();

    await browser.executeWorkbench((vscode) => {
      vscode.commands.executeCommand('editchain-history.open');
    });
    const webview = await workbench.getWebviewByTitle('EditChain History');
    await webview.open();
    await browser.$('.row').waitForExist({ timeout: 120000 });

    // The segmented control exists and defaults to Activity.
    const defaults = await browser.execute(() => ({
      activityPressed: document.getElementById('profile-activity')?.getAttribute('aria-pressed'),
      rawPressed: document.getElementById('profile-raw')?.getAttribute('aria-pressed'),
      profile: typeof window.__editchainGetProfile === 'function'
        ? window.__editchainGetProfile() : null,
      hasControl: !!document.getElementById('profile-control'),
    }));
    console.log('[e2e] profile defaults:', JSON.stringify(defaults));
    expect(defaults.hasControl).toBe(true);
    expect(defaults.activityPressed).toBe('true');
    expect(defaults.rawPressed).toBe('false');

    // Switch to Raw: the control updates and the view resets coherently
    // (scroll to the top, rows re-render under the new profile). The real
    // service may ignore hide_trace until the r4 backend lands, so the
    // assertions are chain-agnostic: control state, profile, scroll reset,
    // and a re-rendered bounded window.
    await browser.execute(() => {
      document.getElementById('profile-raw').click();
    });
    // The reset must clear readiness and drop the stale grid SYNCHRONOUSLY —
    // before the new profile's window arrives. This is the product fix for the
    // stale-DOM race (Raw -> Activity left old rows interactive and
    // readiness-satisfying; Enter then hit a stale row whose abs index was
    // absent from the cleared cache and raw activation was swallowed).
    const resetState = await browser.execute(() => ({
      dataReady: window.__editchainDataReady,
      rowCount: document.querySelectorAll('.row').length,
      loading: (document.querySelector('.view-message')?.textContent || ''),
      profile: window.__editchainGetProfile(),
    }));
    console.log('[e2e] raw reset state:', JSON.stringify(resetState));
    expect(resetState.dataReady).toBe(false);
    expect(resetState.rowCount).toBe(0);
    expect(resetState.loading).toContain('Loading');
    expect(resetState.profile).toBe('raw');
    // Wait for the NEW generation on authoritative readiness + current-profile
    // cache only: every rendered row must be backed by the cache, so a stale
    // DOM can never satisfy the wait while the new window is in flight.
    await browser.waitUntil(async () => browser.execute(() => {
      const rows = Array.from(document.querySelectorAll('.row'));
      return window.__editchainDataReady === true &&
        document.getElementById('rows').scrollTop === 0 &&
        rows.length > 0 &&
        rows.every((r) => {
          const abs = Number(r.getAttribute('data-row'));
          return Number.isFinite(abs) && window.__editchainRowAt(abs) != null;
        });
    }), { timeout: 30000, interval: 100 });
    const rawState = await browser.execute(() => ({
      profile: typeof window.__editchainGetProfile === 'function'
        ? window.__editchainGetProfile() : null,
      activityPressed: document.getElementById('profile-activity')?.getAttribute('aria-pressed'),
      rawPressed: document.getElementById('profile-raw')?.getAttribute('aria-pressed'),
      rowCount: document.querySelectorAll('.row').length,
      scrollTop: document.getElementById('rows').scrollTop,
    }));
    console.log('[e2e] raw state:', JSON.stringify(rawState));
    expect(rawState.profile).toBe('raw');
    expect(rawState.activityPressed).toBe('false');
    expect(rawState.rawPressed).toBe('true');
    expect(rawState.rowCount).toBeGreaterThan(0);
    expect(rawState.scrollTop).toBe(0);

    // Switch back to Activity through the control.
    await browser.execute(() => {
      document.getElementById('profile-activity').click();
    });
    await browser.waitUntil(async () => browser.execute(() => {
      const rows = Array.from(document.querySelectorAll('.row'));
      return window.__editchainDataReady === true &&
        document.getElementById('rows').scrollTop === 0 &&
        rows.length > 0 &&
        rows.every((r) => {
          const abs = Number(r.getAttribute('data-row'));
          return Number.isFinite(abs) && window.__editchainRowAt(abs) != null;
        });
    }), { timeout: 30000, interval: 100 });
    const activityState = await browser.execute(() => ({
      profile: window.__editchainGetProfile ? window.__editchainGetProfile() : null,
      rowCount: document.querySelectorAll('.row').length,
    }));
    console.log('[e2e] activity state:', JSON.stringify(activityState));
    expect(activityState.profile).toBe('activity');
    expect(activityState.rowCount).toBeGreaterThan(0);

    // Keyboard: Enter selects inline and explicitly opens the raw JSON editor.
    const keyboard = await browser.execute(() => {
      const row = document.querySelector('.row:not(.row-placeholder)');
      if (!row) throw new Error('no rendered row for keyboard probe');
      const abs = Number(row.getAttribute('data-row'));
      row.focus();
      row.dispatchEvent(new KeyboardEvent('keydown', { key: 'Enter', bubbles: true }));
      return {
        focused: document.activeElement === row,
        abs,
        cacheBacked: window.__editchainRowAt(abs) != null,
        selected: row.classList.contains('row-selected'),
        secondaryPane: !!document.getElementById('detail') ||
          document.getElementById('layout').classList.contains('has-detail'),
      };
    });
    expect(keyboard.focused).toBe(true);
    // Enter must target a CACHE-BACKED row — the stale-DOM race delivered Enter
    // to an old row whose abs index was absent from the cleared cache, and the
    // activation was swallowed.
    expect(keyboard.cacheBacked).toBe(true);
    expect(keyboard.selected).toBe(true);
    expect(keyboard.secondaryPane).toBe(false);
    await webview.close();
    await browser.waitUntil(async () => {
      const tab = await workbench.getEditorView().getActiveTab();
      return !!tab && (await tab.getTitle()) !== 'EditChain History';
    }, { timeout: 30000, interval: 100 });
    await browser.executeWorkbench(async (vscode) => {
      await vscode.commands.executeCommand('workbench.action.navigateBack');
    });
    const restoredWebview = await workbench.getWebviewByTitle('EditChain History');
    await restoredWebview.open();
    const restoredSinglePane = await browser.execute(() =>
      !document.getElementById('detail') &&
      !document.getElementById('layout').classList.contains('has-detail'));
    expect(restoredSinglePane).toBe(true);
    await restoredWebview.close();
  });

  it('scrolls through the full history with a bounded viewport', async () => {
    const workbench = await browser.getWorkbench();

    // Open the webview (reuses the existing panel if still open).
    await browser.executeWorkbench((vscode) => {
      vscode.commands.executeCommand('editchain-history.open');
    });
    const webview = await workbench.getWebviewByTitle('EditChain History');
    await webview.open();

    // Wait for the first window to render (long deadline — see above).
    await browser.$('.row').waitForExist({ timeout: 120000 });

    // Scroll to the bottom smoothly until no more rows load (visible in video).
    await scrollHistoryToBottomSmooth();

    // The webview is a thin viewport: it renders only a slice around the scroll
    // position, NOT the whole history. So the DOM row count must stay bounded
    // (viewport + buffer) and far below the server-reported total. The total
    // comes from the renderer (chain-agnostic) rather than a hardcoded chain
    // size, so the assertion holds for any workspace.
    const rowCount = await browser.$$('.row').length;
    const total = await browser.execute(() =>
      typeof window.__editchainGetTotal === 'function' ? window.__editchainGetTotal() : -1);
    console.log('[e2e] viewport rows rendered:', rowCount);
    console.log('[e2e] server total:', total);
    expect(rowCount).toBeGreaterThan(0);
    expect(total).toBeGreaterThan(0);
    expect(rowCount).toBeLessThan(total);

    // Confirm we reached the true bottom. The exact genesis node id depends on
    // the imported session (reimports renumber it), so assert chain-agnostically:
    // the scroll position must reach maxScroll and the deepest rendered slice
    // must contain real (non-placeholder) rows — no hardcoded id or chain size.
    const bottom = await browser.execute(() => {
      const rows = document.querySelectorAll('.row');
      const keys = Array.from(rows).slice(-5).map((r) => r.getAttribute('data-key'));
      const rowsEl = document.getElementById('rows');
      return {
        keys,
        scrollTop: rowsEl.scrollTop,
        scrollHeight: rowsEl.scrollHeight,
        clientHeight: rowsEl.clientHeight,
        rowCount: rows.length,
        placeholders: document.querySelectorAll('.row-placeholder').length,
      };
    });
    console.log('[e2e] bottom state:', JSON.stringify(bottom));
    // The viewport must have reached the true bottom (within one viewport of
    // maxScroll), and the deepest slice must be fully hydrated.
    expect(bottom.scrollHeight - bottom.scrollTop).toBeLessThanOrEqual(bottom.clientHeight + 5);
    expect(bottom.rowCount).toBeGreaterThan(0);
    expect(bottom.placeholders).toBe(0);

    // Leave the webview context.
    await webview.close();
  });

  it('keeps the Activity work-unit/bundle/promotion layer coherent with the wire and gates it off in Raw', async () => {
    const workbench = await browser.getWorkbench();

    await browser.executeWorkbench((vscode) => {
      vscode.commands.executeCommand('editchain-history.open');
    });
    const webview = await workbench.getWebviewByTitle('EditChain History');
    await webview.open();
    await browser.$('.row').waitForExist({ timeout: 120000 });

    // Inject the text-only layout probe so its contract helpers + textual
    // checks run inside real VS Code (same probe the harness uses).
    await browser.execute((src) => {
      // eslint-disable-next-line no-eval
      (0, eval)(src);
      return typeof window.__editchainDebug;
    }, PROBE_SRC);
    await browser.execute(() => window.__editchainDebug.whenIdle(60000));

    // Deterministic, chain-agnostic invariants over REAL rows: wherever a
    // rendered row carries work_unit / promoted / activity_bundle wire
    // metadata, the DOM layer must agree exactly (class + data attrs + count
    // text). No opaque ids are asserted — the chain's content is irrelevant,
    // only the wire-to-DOM correspondence.
    const activity = await browser.execute(() => {
      const rows = Array.from(document.querySelectorAll('.row:not(.row-placeholder)'));
      const problems = [];
      let typedBundles = 0;
      let startRows = 0;
      for (const el of rows) {
        const abs = Number(el.getAttribute('data-row'));
        const row = window.__editchainRowAt ? window.__editchainRowAt(abs) : null;
        if (!row) continue;
        if (row.work_unit) {
          const startDom = el.classList.contains('row-work-unit-start');
          // A single-row unit is BOTH is_start and is_end; the renderer's
          // end marker is optional and yields to the start header, so the
          // DOM expectation is `is_end && !is_start`.
          const endDom = el.classList.contains('row-work-unit-end');
          if (startDom !== row.work_unit.is_start) {
            problems.push('work_unit.is_start mismatch on ' + abs);
          }
          if (endDom !== (row.work_unit.is_end && !row.work_unit.is_start)) {
            problems.push('work_unit.is_end mismatch on ' + abs);
          }
          if (row.work_unit.is_start) {
            startRows++;
            const countEl = el.querySelector('.work-unit-count');
            const text = countEl ? (countEl.textContent || '').trim() : '';
            const expectsCount = row.activity_kind !== 'source_control' &&
              row.work_unit.count > 1;
            if (!!countEl !== expectsCount) {
              problems.push('work-unit count visibility mismatch on ' + abs);
            } else if (countEl && (!/^\d+/.test(text) ||
                Number(/^\d+/.exec(text)![0]) !== row.work_unit.count ||
                !/entr(?:y|ies)$/.test(text))) {
              problems.push('work-unit entry count text "' + text + '" != ' + row.work_unit.count + ' on ' + abs);
            }
          }
        }
        if (row.activity_bundle && row.activity_bundle.kind === 'execute-run') {
          typedBundles++;
          if (el.getAttribute('data-activity-bundle') !== 'execute-run') {
            problems.push('typed bundle missing data-activity-bundle=execute-run on ' + abs);
          }
          if (el.getAttribute('data-bundle-count') !== String(row.activity_bundle.member_count)) {
            problems.push('data-bundle-count mismatch on ' + abs);
          }
          const countEl = el.querySelector('.bundle-count');
          const text = countEl ? (countEl.textContent || '').trim() : '';
          if (!countEl || Number(/^\d+/.exec(text)?.[0]) !== row.activity_bundle.member_count) {
            problems.push('bundle-count text mismatch on ' + abs);
          }
          const statusEl = el.querySelector('.bundle-status');
          const statusText = statusEl ? (statusEl.textContent || '').trim() : '';
          if (row.outcome === 'success') {
            if (!statusEl || statusText !== '✓' ||
                !statusEl.classList.contains('bundle-status-success')) {
              problems.push('successful bundle missing quiet success check on ' + abs);
            }
          } else if (statusEl) {
            problems.push('unknown-outcome bundle renders noisy status on ' + abs);
          }
        } else if (row.activity_bundle) {
          // Forward-compatible unknown bundle kind: never styled as execute-run.
          if (el.getAttribute('data-activity-bundle') !== null ||
              el.classList.contains('row-activity-bundle')) {
            problems.push('unknown-kind bundle styled on ' + abs);
          }
        }
        if (row.promoted === true && !el.classList.contains('row-promoted')) {
          problems.push('promoted row missing .row-promoted on ' + abs);
        }
        if (row.promoted === false && el.classList.contains('row-promoted')) {
          problems.push('non-promoted row has .row-promoted on ' + abs);
        }
      }
      return {
        problems,
        rowsChecked: rows.length,
        startRows,
        typedBundles,
        profile: typeof window.__editchainGetProfile === 'function'
          ? window.__editchainGetProfile() : null,
      };
    });
    console.log('[e2e] activity wire/DOM coherence:', JSON.stringify(activity));
    expect(activity.profile).toBe('activity');
    expect(activity.rowsChecked).toBeGreaterThan(0);
    expect(activity.startRows).toBeGreaterThan(0);
    expect(activity.problems).toEqual([]);

    // Raw gating: after switching through the REAL control path, no rendered
    // row may carry any grouping/promotion class, descendant, or data attr —
    // even when its cached row still carries the wire metadata.
    await browser.execute(() => {
      document.getElementById('profile-raw').click();
    });
    await browser.waitUntil(async () => browser.execute(() => {
      return document.querySelectorAll('.row:not(.row-placeholder)').length > 0 &&
        window.__editchainDataReady === true;
    }), { timeout: 60000, interval: 200 });
    const raw = await browser.execute(() => {
      const rows = Array.from(document.querySelectorAll('.row:not(.row-placeholder)'));
      const groupingSel = '.row-work-unit-start, .row-work-unit-end, .work-unit-ribbon, ' +
        '.work-unit-count, .row-activity-bundle, .bundle-count, .bundle-status, ' +
        '.row-promoted, [data-activity-bundle], [data-bundle-count]';
      const leakRows = rows.filter((el) =>
        el.querySelector(groupingSel) !== null || el.matches(groupingSel));
      const cacheHasMetadata = rows.some((el) => {
        const abs = Number(el.getAttribute('data-row'));
        const row = window.__editchainRowAt ? window.__editchainRowAt(abs) : null;
        return row && (!!row.work_unit || row.promoted || row.activity_bundle);
      });
      return {
        profile: typeof window.__editchainGetProfile === 'function'
          ? window.__editchainGetProfile() : null,
        rows: rows.length,
        leakRows: leakRows.length,
        cacheHasMetadata,
      };
    });
    console.log('[e2e] raw gating:', JSON.stringify(raw));
    expect(raw.profile).toBe('raw');
    expect(raw.rows).toBeGreaterThan(0);
    expect(raw.leakRows).toBe(0);
    // Raw may legitimately lack metadata (older service), but when it is
    // present the gating must hold — never rendered.
    if (raw.cacheHasMetadata) {
      expect(raw.leakRows).toBe(0);
    }

    await webview.close();
  });
});
