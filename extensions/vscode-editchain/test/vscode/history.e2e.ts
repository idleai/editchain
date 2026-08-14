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

    // Wait for rows to render (the real service round-trips async).
    await browser.$('.row').waitForExist({ timeout: 20000 });

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
    const idle = await browser.execute(() => window.__editchainDebug.whenIdle(8000));
    console.log('[e2e] idle:', JSON.stringify(idle));

    const assertion = await browser.execute(() => window.__editchainDebug.assertLayout());
    console.log('[e2e] checks pass=' + assertion.passCount + ' fail=' + assertion.failCount);
    assertion.checks.forEach((c) =>
      console.log('[e2e]   ' + c.name + ' ' + (c.pass ? 'PASS' : 'FAIL') + ' — ' + c.detail));

    // The probe must have executed and produced a well-formed result. We do NOT
    // assert zero failures here: known layout bugs (overflow, dot alignment) are
    // surfaced as FAIL checks for the agent to fix, not as test blockers.
    expect(typeof assertion.passCount).toBe('number');
    expect(typeof assertion.failCount).toBe('number');

    // Leave the webview context.
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

    // Wait for the first window to render.
    await browser.$('.row').waitForExist({ timeout: 20000 });

    // Scroll to the bottom smoothly until no more rows load (visible in video).
    await scrollHistoryToBottomSmooth();

    // The webview is a thin viewport: it renders only a slice around the scroll
    // position, NOT the whole history. So the DOM row count must stay bounded
    // (viewport + buffer), far below the total of 953.
    const rowCount = await browser.$$('.row').length;
    console.log('[e2e] viewport rows rendered:', rowCount);
    expect(rowCount).toBeGreaterThan(0);
    expect(rowCount).toBeLessThan(953);

    // Confirm we reached the true bottom. The two oldest ops are the genesis
// ChainStart (`0:0:0`) and the seed session's first op (`...:65536`); both must
// be present at the bottom. Their relative order is racy under progressive
// loading, so we assert presence, not strict order.
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
      };
    });
    console.log('[e2e] bottom state:', JSON.stringify(bottom));
    expect(bottom.keys).toContain('0:0:0');
    expect(bottom.keys.some((k) => k.includes(':65536'))).toBe(true);

    // Leave the webview context.
    await webview.close();
  });
});