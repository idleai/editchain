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

    // Run inside VS Code: invoke the extension's command.
    await browser.executeWorkbench((vscode) => {
      vscode.commands.executeCommand('editchain-history.open');
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

    // Deterministic capture for the visual reviewer: taken only after whenIdle
    // + passing checks, so the screenshot shows a settled, rendered table.
    const shotPath = path.join(__dirname, '..', '..', 'trace', 'e2e-history.png');
    await browser.saveScreenshot(shotPath);
    console.log('[e2e] screenshot ->', shotPath);

    // Leave the webview context.
    await webview.close();
  });

  it('selects rows in the inspector; only explicit actions open an editor tab', async () => {
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
        return row && !row.is_subop && (row.op_id || row.git_oid);
      });
      if (!candidate) throw new Error('no rendered detail-capable row');
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

    // An ORDINARY row click must open the inspector — never an editor tab.
    const inspector = await browser.execute(() => {
      const detailEl = document.getElementById('detail');
      return {
        hasDetail: document.getElementById('layout').classList.contains('has-detail'),
        selected: !!document.querySelector('.row.row-selected'),
        title: detailEl.querySelector('.detail-title')?.textContent || '',
        hasActions: detailEl.querySelectorAll('.detail-btn').length > 0,
      };
    });
    console.log('[e2e] inspector after row click:', JSON.stringify(inspector));
    expect(inspector.hasDetail).toBe(true);
    expect(inspector.selected).toBe(true);
    // The details must have resolved (title rendered, actions present), not
    // left on the loading state.
    await browser.waitUntil(async () => browser.execute(() => {
      const detailEl = document.getElementById('detail');
      return detailEl.querySelectorAll('.detail-btn').length > 0;
    }), { timeout: 30000, interval: 100 });

    // The ONLY editor path is the inspector's explicit "Open raw JSON" action.
    await browser.execute(() => {
      const btns = Array.from(document.querySelectorAll('#detail .detail-btn'));
      const openBtn = btns.find((b) => b.textContent.includes('Open raw JSON'));
      if (!openBtn || openBtn.disabled) throw new Error('no enabled Open raw JSON action');
      openBtn.click();
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

    console.log('[e2e] inspector/back before:', JSON.stringify(before));
    console.log('[e2e] inspector/back after:', JSON.stringify(after));
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
    // absent from the cleared cache and the inspector never opened).
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

    // Keyboard: Enter on a focused row opens the inspector; the Close action
    // hides it and clears selection.
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
      };
    });
    expect(keyboard.focused).toBe(true);
    // Enter must target a CACHE-BACKED row — the stale-DOM race delivered Enter
    // to an old row whose abs index was absent from the cleared cache, and the
    // inspector never opened.
    expect(keyboard.cacheBacked).toBe(true);
    await browser.waitUntil(async () => browser.execute(() => {
      const sel = document.querySelector('.row.row-selected');
      return document.getElementById('layout').classList.contains('has-detail') &&
        !!sel &&
        window.__editchainRowAt(Number(sel.getAttribute('data-row'))) != null;
    }), { timeout: 30000, interval: 100 });
    await browser.execute(() => {
      const closeBtn = document.querySelector('#detail .detail-btn:first-child');
      if (closeBtn) closeBtn.click();
    });
    await browser.waitUntil(async () => browser.execute(() => {
      return !document.getElementById('layout').classList.contains('has-detail') &&
        !document.querySelector('.row.row-selected');
    }), { timeout: 30000, interval: 100 });

    await webview.close();
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
});
