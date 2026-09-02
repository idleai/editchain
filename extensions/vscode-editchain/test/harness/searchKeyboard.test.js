// Node tests for find-in-chain keyboard behaviour in media/main.js, driven
// against the REAL harness page (test/harness/index.html) in headless Chromium
// via puppeteer-core. The fixture bridge answers requests synchronously, so
// every regression is deterministic — no wall-clock sleeps decide the outcome.
//
// Find-in-chain is an IN-PLACE find over the real history view: submitting a
// query never replaces the chain DOM, graph, profile, expansion state, cache,
// or layout. Navigation keeps focus in the search input (ArrowDown/ArrowUp
// move next/previous and wrap; same-query Enter advances, Shift+Enter steps
// back), and the adjacent counter reports "i of N" ("1 of N+" when truncated),
// an animated spinner while pending (no visible text), "0 of 0", or "error".
//
//  1. history row keys/graph stay real and unchanged through search
//  2. initial auto-jump + "1 of N"; next/prev/wrap; same-query Enter/Shift+Enter
//  3. far/off-cache target fetch and scroll (sparse window, never the full chain)
//  4. absolute->visible mapping under expansion state; expansion/profile preserved
//  5. clear/Escape clears find state without refetching or moving the scroll
//  6. stale edited/in-flight responses never navigate old matches
//  7. zero/error searches stay compact and never replace the chain
//  8. Enter on a focused row (not the input) still opens its raw JSON editor
//  9. the active match highlight survives a virtualization rebuild
// 10. legacy flat-list Search responses still render (compatibility path)
// 11. malformed backend match rows are filtered so no jump can stall
// 12. the current-match marker stays pinned when another row is clicked
//
// Run: node --test test/harness/searchKeyboard.test.js
'use strict';

const { test, before, after } = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const puppeteer = require('puppeteer-core');

const EXT_ROOT = path.join(__dirname, '..', '..');
const HARNESS = 'file://' + path.join(EXT_ROOT, 'test', 'harness', 'index.html');

function searchControlMarkup(source) {
  const start = source.indexOf('<div id="search-control"');
  assert.notEqual(start, -1, 'integrated search-control markup exists');
  const end = source.indexOf('</div>', start);
  assert.notEqual(end, -1, 'integrated search-control markup is closed');
  return source.slice(start, end + '</div>'.length).replace(/\r\n/g, '\n');
}

const CHROME = process.env.CHROME_PATH ||
  '/mnt/hot/ambientlight/.cache/puppeteer/chrome/linux-151.0.7922.71/chrome-linux64/chrome';

let browser;

before(async () => {
  browser = await puppeteer.launch({
    executablePath: CHROME,
    headless: 'new',
    args: ['--no-sandbox', '--disable-setuid-sandbox'],
  });
});

after(async () => {
  if (browser) await browser.close();
});

async function newPage() {
  const page = await browser.newPage();
  await page.setViewport({ width: 1440, height: 900 });
  const errors = [];
  page.on('pageerror', (e) => errors.push(e.message));
  await page.goto(HARNESS, { waitUntil: 'networkidle0' });
  return { page, errors };
}

/** Load a fixture scenario through the real harness startup handshake. */
async function bootScenario(page, scenario) {
  await page.evaluate((name) => {
    window.__editchainSetScenario(name);
    window.__editchainStart();
  }, scenario);
  await page.evaluate(() => window.__editchainDebug.whenIdle(20000));
}

/** Put focus on the search input (real focus, so focus-dependent handlers
 * behave like a keyboard user). */
async function focusInput(page) {
  await page.evaluate(() => document.getElementById('search').focus());
}

/** Run a search through the REAL renderer path (fill + Enter keydown). */
async function runSearch(page, query) {
  await page.evaluate((q) => {
    const input = document.getElementById('search');
    input.value = q;
    input.dispatchEvent(new KeyboardEvent('keydown', { key: 'Enter', bubbles: true }));
  }, query);
  await page.evaluate(() => window.__editchainDebug.whenIdle(20000));
}

/** Dispatch a keydown on the search input (optionally with a modifier key,
 * e.g. Shift+Enter) on the currently focused element. */
async function pressInput(page, key, opts) {
  opts = opts || {};
  await page.evaluate(({ key, shiftKey, ctrlKey }) => {
    const input = document.getElementById('search');
    input.dispatchEvent(new KeyboardEvent('keydown', {
      key,
      shiftKey: !!shiftKey,
      ctrlKey: !!ctrlKey,
      bubbles: true,
      cancelable: true,
    }));
  }, { key, shiftKey: opts.shiftKey, ctrlKey: opts.ctrlKey });
}

/** Dispatch a keydown on a specific element (selector-targeted row focus first
 * so focus-dependent handlers see the real event target, matching how a
 * keyboard user activates a focused row). */
async function pressSelector(page, key, selector, opts) {
  opts = opts || {};
  await page.evaluate(({ key, selector, shiftKey }) => {
    const el = document.querySelector(selector);
    if (!el) throw new Error('no target element for key press: ' + key + ' at ' + selector);
    if (el.focus) el.focus();
    el.dispatchEvent(new KeyboardEvent('keydown', {
      key,
      shiftKey: !!shiftKey,
      bubbles: true,
      cancelable: true,
    }));
  }, { key, selector, shiftKey: opts.shiftKey });
}

/** Read the renderer's find/highlight/scroll contract as plain data. */
async function contract(page) {
  return page.evaluate(() => {
    const input = document.getElementById('search');
    const counter = document.getElementById('search-counter');
    const prevBtn = document.getElementById('search-prev');
    const nextBtn = document.getElementById('search-next');
    const rowsEl = document.getElementById('rows');
    const rows = Array.from(document.querySelectorAll('.row'));
    const selected = document.querySelector('.row.row-selected');
    const findCurrent = document.querySelector('.row.row-find-current');
    return {
      rowCount: rows.length,
      total: window.__editchainGetTotal ? window.__editchainGetTotal() : -1,
      counter: counter ? (counter.textContent || '').trim() : '',
      counterClass: counter ? (counter.className || '') : '',
      counterBusy: counter ? (counter.getAttribute('aria-busy') || '') : '',
      counterLabel: counter ? (counter.getAttribute('aria-label') || '') : '',
      counterTitle: counter ? (counter.getAttribute('title') || '') : '',
      focusedIsInput: document.activeElement === input,
      scrollTop: rowsEl.scrollTop,
      selectedRow: selected ? Number(selected.getAttribute('data-row')) : null,
      selectedAria: selected ? selected.getAttribute('aria-selected') : null,
      selectedKey: selected ? selected.getAttribute('data-key') : null,
      findCurrentRow: findCurrent ? Number(findCurrent.getAttribute('data-row')) : null,
      rowKeys: rows.map((r) => r.getAttribute('data-key')),
      inputValue: input.value,
      hasPlaceholders: document.querySelectorAll('.row-placeholder').length > 0,
      prevDisabled: prevBtn ? prevBtn.disabled : null,
      nextDisabled: nextBtn ? nextBtn.disabled : null,
      prevHidden: prevBtn ? prevBtn.hidden : null,
      nextHidden: nextBtn ? nextBtn.hidden : null,
      prevTitle: prevBtn ? (prevBtn.getAttribute('title') || '') : null,
      nextTitle: nextBtn ? (nextBtn.getAttribute('title') || '') : null,
      prevLabel: prevBtn ? (prevBtn.getAttribute('aria-label') || '') : null,
      nextLabel: nextBtn ? (nextBtn.getAttribute('aria-label') || '') : null,
      prevType: prevBtn ? prevBtn.getAttribute('type') : null,
      nextType: nextBtn ? nextBtn.getAttribute('type') : null,
      navVisible: prevBtn && nextBtn
        ? prevBtn.offsetParent !== null && nextBtn.offsetParent !== null
        : false,
    };
  });
}

/** Read the composite search field's DOM, geometry, and computed chrome. */
async function searchControlLayout(page) {
  return page.evaluate(() => {
    const controls = document.getElementById('controls');
    const profile = document.getElementById('profile-control');
    const shell = document.getElementById('search-control');
    const input = document.getElementById('search');
    const counter = document.getElementById('search-counter');
    const prev = document.getElementById('search-prev');
    const next = document.getElementById('search-next');
    if (!controls || !profile || !shell || !input || !counter || !prev || !next) {
      throw new Error('integrated search control is incomplete');
    }
    const rect = (el) => {
      const r = el.getBoundingClientRect();
      return { left: r.left, top: r.top, right: r.right, bottom: r.bottom,
        width: r.width, height: r.height };
    };
    const shellStyle = getComputedStyle(shell);
    const inputStyle = getComputedStyle(input);
    const counterStyle = getComputedStyle(counter);
    const prevStyle = getComputedStyle(prev);
    return {
      children: Array.from(shell.children).map((el) => el.id),
      directChildren: [input, counter, prev, next].every((el) => el.parentElement === shell),
      role: shell.getAttribute('role'),
      shell: rect(shell),
      controls: rect(controls),
      profile: rect(profile),
      input: rect(input),
      counter: rect(counter),
      prev: rect(prev),
      next: rect(next),
      shellDisplay: shellStyle.display,
      shellOverflow: shellStyle.overflow,
      shellBackground: shellStyle.backgroundColor,
      shellBorderWidth: shellStyle.borderTopWidth,
      shellBorderStyle: shellStyle.borderTopStyle,
      shellBorderColor: shellStyle.borderTopColor,
      shellBoxShadow: shellStyle.boxShadow,
      shellRadius: shellStyle.borderTopLeftRadius,
      inputBackground: inputStyle.backgroundColor,
      inputBorderWidths: [inputStyle.borderTopWidth, inputStyle.borderRightWidth,
        inputStyle.borderBottomWidth, inputStyle.borderLeftWidth],
      inputBoxShadow: inputStyle.boxShadow,
      inputOutlineStyle: inputStyle.outlineStyle,
      counterDivider: counterStyle.borderLeftWidth,
      prevDivider: prevStyle.borderLeftWidth,
    };
  });
}

/** Snapshot the pending spinner pseudo-element (.search-counter-pending
 * ::before) as computed styles: whether it exists, is visible, and animates. */
async function pendingSpinner(page) {
  return page.evaluate(() => {
    const counter = document.getElementById('search-counter');
    if (!counter) return null;
    const s = getComputedStyle(counter, '::before');
    return {
      content: s.content,
      display: s.display,
      width: s.width,
      height: s.height,
      borderTopWidth: s.borderTopWidth,
      borderTopColor: s.borderTopColor,
      borderRadius: s.borderRadius,
      animationName: s.animationName,
      animationDuration: s.animationDuration,
      animationIterationCount: s.animationIterationCount,
    };
  });
}

/** Real-mouse click on a find-navigation button (mousedown + mouseup + click
 * through the browser input pipeline, so focus-stealing behaviour is exercised
 * exactly like a user's click). Disabled buttons swallow the click; returns
 * whether the click was actually delivered. */
async function clickFindNav(page, which) {
  const box = await page.evaluate((id) => {
    const el = document.getElementById(id);
    if (!el) return null;
    const r = el.getBoundingClientRect();
    return { x: r.left + r.width / 2, y: r.top + r.height / 2, disabled: el.disabled };
  }, which === 'prev' ? 'search-prev' : 'search-next');
  if (!box || box.disabled) return false;
  await page.mouse.click(box.x, box.y);
  return true;
}

/** Snapshot of the request log (bodies only) the renderer issued so far,
 * sliced from a starting index. */
async function requestLog(page, from) {
  return page.evaluate((start) => {
    const log = window.__editchainRequestLog || [];
    return log.slice(start).map((body) => JSON.stringify(body));
  }, from === undefined ? 0 : from);
}

/** Run a search entirely inside one page context and report whether the
 * rendered row ELEMENTS and their graph cells survived byte-for-byte (the
 * strongest possible proof that the chain DOM was never replaced/rebuilt). */
async function searchKeepingDom(page, query) {
  return page.evaluate((q) => {
    const rowsEl = document.getElementById('rows');
    const input = document.getElementById('search');
    const before = Array.from(document.querySelectorAll('.row'));
    const beforeGraph = before.map((r) => r.querySelector('.graph-cell').innerHTML).join('\u0000');
    const beforeScroll = rowsEl.scrollTop;
    input.value = q;
    input.dispatchEvent(new KeyboardEvent('keydown', { key: 'Enter', bubbles: true }));
    const after = Array.from(document.querySelectorAll('.row'));
    return {
      sameEls: before.length === after.length && before.every((el, i) => el === after[i]),
      graphUnchanged: after.map((r) => r.querySelector('.graph-cell').innerHTML).join('\u0000') === beforeGraph,
      rowCountBefore: before.length,
      rowCountAfter: after.length,
      scrollBefore: beforeScroll,
      scrollAfter: rowsEl.scrollTop,
    };
  }, query);
}

test('production and harness share the exact integrated search-control markup', () => {
  const production = fs.readFileSync(path.join(EXT_ROOT, 'src', 'extension.ts'), 'utf8');
  const harness = fs.readFileSync(path.join(EXT_ROOT, 'test', 'harness', 'index.html'), 'utf8');
  assert.equal(searchControlMarkup(production), searchControlMarkup(harness));
});

test('input, counter, and arrows form one focus-aware responsive search rectangle', async () => {
  const { page, errors } = await newPage();
  try {
    await bootScenario(page, 'badges');
    const initial = await contract(page);
    assert.equal(initial.navVisible, false,
      'the composite starts as a plain input with no result arrows');
    assert.equal(initial.prevHidden, true);
    assert.equal(initial.nextHidden, true);

    await runSearch(page, 'the');
    const settled = await contract(page);
    assert.equal(settled.navVisible, true,
      'both arrows join the composite after non-empty results settle');
    assert.equal(settled.prevHidden, false);
    assert.equal(settled.nextHidden, false);
    await page.evaluate(() => {
      if (document.activeElement && document.activeElement.blur) document.activeElement.blur();
    });
    const idle = await searchControlLayout(page);

    assert.deepEqual(idle.children,
      ['search', 'search-counter', 'search-prev', 'search-next'],
      'the text, result count, and arrows are ordered inside one shell');
    assert.equal(idle.directChildren, true, 'all search parts are direct children of the shell');
    assert.equal(idle.role, 'group', 'the composite exposes one labelled control group');
    assert.equal(idle.shellDisplay, 'flex');
    assert.equal(idle.shellOverflow, 'hidden', 'outer rounding clips all inner controls');
    assert.equal(idle.shellBorderWidth, '1px', 'only the shell draws the input border');
    assert.equal(idle.shellBorderStyle, 'solid');
    assert.notEqual(idle.shellBackground, 'rgba(0, 0, 0, 0)', 'the shell owns the input background');
    assert.notEqual(idle.shellRadius, '0px', 'the shared rectangle has rounded corners');
    assert.deepEqual(idle.inputBorderWidths, ['0px', '0px', '0px', '0px'],
      'the inner text input cannot draw a second rectangle');
    assert.equal(idle.inputBackground, 'rgba(0, 0, 0, 0)',
      'the input lets the shared shell background show through');
    assert.equal(idle.inputBoxShadow, 'none', 'the inner input cannot draw a second focus ring');
    assert.equal(idle.inputOutlineStyle, 'none', 'the inner input delegates focus chrome to the shell');
    assert.equal(idle.counterDivider, '1px', 'a hairline separates text from the count');
    assert.equal(idle.prevDivider, '1px', 'a hairline separates the count from navigation');

    const epsilon = 1.1;
    assert.ok(idle.input.left >= idle.shell.left - epsilon);
    assert.ok(idle.next.right <= idle.shell.right + epsilon);
    assert.ok(Math.abs(idle.input.right - idle.counter.left) <= epsilon,
      'input and counter are flush inside the shell');
    assert.ok(Math.abs(idle.counter.right - idle.prev.left) <= epsilon,
      'counter and Previous are flush inside the shell');
    assert.ok(Math.abs(idle.prev.right - idle.next.left) <= epsilon,
      'Previous and Next are flush inside the shell');
    for (const part of [idle.input, idle.counter, idle.prev, idle.next]) {
      assert.ok(part.top >= idle.shell.top - epsilon && part.bottom <= idle.shell.bottom + epsilon,
        'every child stays vertically inside the shared rectangle');
    }

    await focusInput(page);
    const focused = await searchControlLayout(page);
    assert.ok(focused.shellBorderColor !== idle.shellBorderColor ||
      focused.shellBoxShadow !== idle.shellBoxShadow,
      'focus-within changes the shell chrome');
    assert.deepEqual(focused.inputBorderWidths, ['0px', '0px', '0px', '0px']);
    assert.equal(focused.inputBoxShadow, 'none', 'focused input still has no inner ring');

    await page.setViewport({ width: 360, height: 900 });
    const narrow = await searchControlLayout(page);
    assert.ok(narrow.shell.top >= narrow.profile.bottom - epsilon,
      'at narrow width the whole composite wraps below the profile as one unit');
    assert.ok(narrow.shell.left >= narrow.controls.left - epsilon &&
      narrow.shell.right <= narrow.controls.right + epsilon,
      'the composite remains inside the controls row');
    assert.ok(narrow.input.width > 0, 'the text field remains usable at narrow width');
    assert.ok(narrow.next.right <= narrow.shell.right + epsilon,
      'the count and both arrows remain inside the narrow shell');
    assert.ok(Math.abs(narrow.input.right - narrow.counter.left) <= epsilon &&
      Math.abs(narrow.counter.right - narrow.prev.left) <= epsilon &&
      Math.abs(narrow.prev.right - narrow.next.left) <= epsilon,
      'no search part detaches or gains an external flex gap at narrow width');

    assert.deepEqual(errors, [], 'no uncaught page errors: ' + errors.join(' | '));
  } finally {
    await page.close();
  }
});

test('search is in-place find over the real history DOM; initial auto-jump + "1 of N"', async () => {
  const { page, errors } = await newPage();
  try {
    await bootScenario(page, 'badges');
    await focusInput(page);
    const before = await contract(page);
    assert.equal(before.rowCount, 11, 'badges renders its full 11-row history');
    assert.equal(before.prevDisabled, true, 'prev is disabled before any find settles');
    assert.equal(before.nextDisabled, true, 'next is disabled before any find settles');
    assert.equal(before.navVisible, false, 'result arrows stay hidden before any find settles');
    assert.equal(before.prevHidden, true);
    assert.equal(before.nextHidden, true);

    const dom = await searchKeepingDom(page, 'the');
    assert.equal(dom.sameEls, true, 'the row ELEMENTS survive the search unchanged (no rebuild/replacement)');
    assert.equal(dom.graphUnchanged, true, 'graph cells survive byte-for-byte through the find');
    assert.equal(dom.rowCountBefore, dom.rowCountAfter, 'row count never changes through find');

    await page.evaluate(() => window.__editchainDebug.whenIdle(20000));
    const settled = await contract(page);
    // Initial auto-jump to match 1 (newest match first: exec at row 0).
    assert.equal(settled.counter, '1 of 4', 'counter shows "1 of N"');
    assert.equal(settled.selectedRow, 0, 'match 1 is auto-selected');
    assert.equal(settled.selectedKey, 'node:b:exec', 'the selected row is the real history row key');
    assert.equal(settled.findCurrentRow, 0, 'the current-match marker sits on match 1');
    assert.equal(settled.focusedIsInput, true, 'focus stays in the search input');
    assert.equal(settled.hasPlaceholders, false, 'no placeholder rows');
    assert.equal(settled.prevDisabled, false, 'prev enables once matches settle');
    assert.equal(settled.nextDisabled, false, 'next enables once matches settle');
    assert.equal(settled.navVisible, true, 'result arrows appear once matches settle');
    assert.equal(settled.prevHidden, false);
    assert.equal(settled.nextHidden, false);
    assert.equal(settled.prevTitle, 'Previous match (Shift+Enter)', 'prev carries a native-style tooltip');
    assert.equal(settled.nextTitle, 'Next match (Enter)', 'next carries a native-style tooltip');
    assert.equal(settled.prevLabel, 'Previous match', 'prev has a stable accessible label');
    assert.equal(settled.nextLabel, 'Next match', 'next has a stable accessible label');
    assert.equal(settled.prevType, 'button', 'prev is a plain button (no implicit submit semantics)');
    assert.equal(settled.nextType, 'button', 'next is a plain button (no implicit submit semantics)');

    // The wire request mirrors the active view: FindInHistory + chain filter +
    // hide_submodules + search filters + a candidate cap.
    const log = await requestLog(page);
    const bodies = log.map((s) => JSON.parse(s));
    const findReq = bodies.find((b) => b.FindInHistory !== undefined);
    assert.ok(findReq, 'production issues FindInHistory, never the legacy Search');
    assert.equal(findReq.FindInHistory.query, 'the');
    assert.equal(findReq.FindInHistory.top_k, 50, 'sensible candidate cap');
    assert.equal(findReq.FindInHistory.hide_submodules, true);
    assert.equal(findReq.FindInHistory.filter.hide_trace, true, 'activity profile filter rides in the request');
    assert.ok(findReq.FindInHistory.filters && typeof findReq.FindInHistory.filters === 'object');
    assert.equal(bodies.some((b) => b.Search !== undefined), false,
      'production never issues the legacy Search request');

    assert.deepEqual(errors, [], 'no uncaught page errors: ' + errors.join(' | '));
  } finally {
    await page.close();
  }
});

test('malformed backend match rows are filtered so the find never stalls', async () => {
  const { page, errors } = await newPage();
  try {
    await bootScenario(page, 'badges');
    // Answer the FindInHistory request with a crafted response mixing invalid
    // row coordinates (missing/null, negative, fractional, beyond the view
    // total) with one valid row. The renderer must drop every un-jumpable
    // match — a missing row must never silently map to row 0 — count only the
    // navigable row, and jump to it without stalling.
    await page.evaluate(() => {
      const origPost = window.vscode.postMessage.bind(window.vscode);
      window.vscode.postMessage = function (msg) {
        if (msg && msg.body && msg.body.FindInHistory !== undefined) {
          window.dispatchEvent(new MessageEvent('message', {
            data: {
              id: msg.id,
              body: {
                Ok: {
                  matches: [
                    { node_key: 'node:b:exec', row: null, summary: 'Run the test suite', op_id: 'node:b:exec' },
                    { node_key: 'node:b:chg', row: -5, summary: 'Update main.css', op_id: 'node:b:chg' },
                    { node_key: 'node:b:plan', row: 1.5, summary: 'Stop the plan run', op_id: 'node:b:plan' },
                    { node_key: 'node:b:exp', row: 999, summary: 'Search the workspace', op_id: 'node:b:exp' },
                    { node_key: 'node:b:diag', row: 6, summary: 'Investigate the failure', op_id: 'node:b:diag' },
                  ],
                  returned: 5,
                  more: false,
                },
              },
            },
          }));
          return;
        }
        return origPost(msg);
      };
      const input = document.getElementById('search');
      input.value = 'the';
      input.dispatchEvent(new KeyboardEvent('keydown', { key: 'Enter', bubbles: true }));
    });
    await page.evaluate(() => window.__editchainDebug.whenIdle(20000));
    let state = await contract(page);
    assert.equal(state.counter, '1 of 1', 'only the navigable match is counted');
    assert.equal(state.selectedRow, 6, 'the valid row is jumped to');
    assert.equal(state.selectedKey, 'node:b:diag', 'the real history row is highlighted');
    assert.equal(state.findCurrentRow, 6, 'the current-match marker sits on the valid row');
    assert.equal(state.hasPlaceholders, false);

    // The session stays navigable (wrap on a single match) — no stalled jump.
    await focusInput(page);
    await pressInput(page, 'ArrowDown');
    state = await contract(page);
    assert.equal(state.counter, '1 of 1', 'wrap on a single navigable match');
    assert.equal(state.selectedRow, 6);
    assert.equal(state.focusedIsInput, true, 'focus stays in the input');

    assert.deepEqual(errors, [], 'no uncaught page errors: ' + errors.join(' | '));
  } finally {
    await page.close();
  }
});

test('the current-match marker stays pinned when another row is manually selected', async () => {
  const { page, errors } = await newPage();
  try {
    await bootScenario(page, 'badges');
    await runSearch(page, 'the'); // 4 hits, match 1 = exec at row 0
    let state = await contract(page);
    assert.equal(state.counter, '1 of 4');
    assert.equal(state.selectedRow, 0);
    assert.equal(state.findCurrentRow, 0);

    // Manually click a different row: the inline selection moves, but the
    // current-match accent stays pinned to the real match row.
    await page.evaluate(() => {
      document.querySelector('.row[data-row="5"]').click();
    });
    state = await contract(page);
    assert.equal(state.selectedRow, 5, 'the manual click moves the inline selection');
    assert.equal(state.selectedKey, 'node:b:ver');
    assert.equal(state.findCurrentRow, 0, 'the find marker stays on match 1');
    assert.equal(state.counter, '1 of 4', 'the find session is untouched');
    const marker = await page.evaluate(() => {
      const el = document.querySelector('.row[data-row="0"]');
      return el ? getComputedStyle(el).boxShadow : 'none';
    });
    assert.ok(marker !== 'none', 'the current-match accent stays visible without the inline selection');

    // Navigating from the input re-selects the match and moves the marker.
    await focusInput(page);
    await pressInput(page, 'ArrowDown');
    state = await contract(page);
    assert.equal(state.counter, '2 of 4');
    assert.equal(state.selectedRow, 3);
    assert.equal(state.findCurrentRow, 3);
    assert.equal(state.focusedIsInput, true);

    assert.deepEqual(errors, [], 'no uncaught page errors: ' + errors.join(' | '));
  } finally {
    await page.close();
  }
});

test('ArrowDown/ArrowUp move next/previous and wrap; same-query Enter advances, Shift+Enter steps back', async () => {
  const { page, errors } = await newPage();
  try {
    await bootScenario(page, 'badges');
    await runSearch(page, 'the'); // 4 hits: exec(0), plan(3), exp(4), diag(6)
    let state = await contract(page);
    assert.equal(state.counter, '1 of 4');
    assert.equal(state.selectedKey, 'node:b:exec');

    // ArrowDown walks the ranked matches forward.
    await focusInput(page);
    const expected = [
      ['2 of 4', 'node:b:plan', 3],
      ['3 of 4', 'node:b:exp', 4],
      ['4 of 4', 'node:b:diag', 6],
    ];
    for (const [counter, key, row] of expected) {
      await pressInput(page, 'ArrowDown');
      state = await contract(page);
      assert.equal(state.counter, counter, 'counter after ArrowDown');
      assert.equal(state.selectedKey, key, 'selection follows the ranked match');
      assert.equal(state.selectedRow, row);
      assert.equal(state.focusedIsInput, true, 'focus never leaves the input');
    }
    // ArrowDown past the last match wraps to the first.
    await pressInput(page, 'ArrowDown');
    state = await contract(page);
    assert.equal(state.counter, '1 of 4', 'wrap last -> first');
    assert.equal(state.selectedKey, 'node:b:exec');

    // ArrowUp wraps first -> last.
    await pressInput(page, 'ArrowUp');
    state = await contract(page);
    assert.equal(state.counter, '4 of 4', 'wrap first -> last');
    assert.equal(state.selectedKey, 'node:b:diag');

    // Same-query Enter advances (query unchanged); Shift+Enter steps back.
    await pressInput(page, 'Enter');
    state = await contract(page);
    assert.equal(state.counter, '1 of 4', 'Enter from the last match wraps to the first');
    await pressInput(page, 'Enter');
    state = await contract(page);
    assert.equal(state.counter, '2 of 4', 'same-query Enter advances');
    assert.equal(state.selectedKey, 'node:b:plan');
    await pressInput(page, 'Enter', { shiftKey: true });
    state = await contract(page);
    assert.equal(state.counter, '1 of 4', 'Shift+Enter steps back');
    assert.equal(state.selectedKey, 'node:b:exec');
    assert.equal(state.inputValue, 'the', 'input text is untouched by navigation');
    assert.equal(state.focusedIsInput, true);

    // Same-query Enter after a wrap advances again.
    await pressInput(page, 'Enter');
    state = await contract(page);
    assert.equal(state.selectedKey, 'node:b:plan', 'same-query Enter after wrap advances again');

    assert.deepEqual(errors, [], 'no uncaught page errors: ' + errors.join(' | '));
  } finally {
    await page.close();
  }
});

test('Previous/Next buttons navigate, wrap, reveal/select/highlight, and keep focus in the input', async () => {
  const { page, errors } = await newPage();
  try {
    await bootScenario(page, 'badges');
    let state = await contract(page);
    assert.equal(state.prevDisabled, true, 'prev is disabled before any find settles');
    assert.equal(state.nextDisabled, true, 'next is disabled before any find settles');
    assert.equal(state.navVisible, false, 'buttons are absent before results are fetched');

    await runSearch(page, 'the'); // 4 hits: exec(0), plan(3), exp(4), diag(6)
    state = await contract(page);
    assert.equal(state.counter, '1 of 4');
    assert.equal(state.prevDisabled, false, 'prev enables once matches settle');
    assert.equal(state.nextDisabled, false, 'next enables once matches settle');
    assert.equal(state.navVisible, true, 'both buttons appear with the fetched matches');

    // Real-mouse click: the mousedown must not steal focus from the input, and
    // the click must drive the SAME navigateFind path as ArrowUp/ArrowDown.
    await focusInput(page);
    assert.equal(await clickFindNav(page, 'next'), true, 'next is clickable');
    state = await contract(page);
    assert.equal(state.counter, '2 of 4', 'next click advances the counter');
    assert.equal(state.selectedRow, 3, 'next click selects the real match row');
    assert.equal(state.selectedKey, 'node:b:plan');
    assert.equal(state.findCurrentRow, 3, 'the current-match marker follows the click');
    assert.equal(state.focusedIsInput, true, 'the mouse click never blurs the search input');
    assert.equal(state.inputValue, 'the', 'button clicks leave the query text untouched');

    // Clicking with the input NOT focused still returns focus to it, so
    // editing and keyboard navigation stay immediate after any click.
    await page.evaluate(() => document.activeElement.blur());
    assert.equal(await clickFindNav(page, 'next'), true);
    state = await contract(page);
    assert.equal(state.counter, '3 of 4');
    assert.equal(state.selectedKey, 'node:b:exp');
    assert.equal(state.focusedIsInput, true, 'a click refocuses the search input');

    // Next wraps last -> first; Prev wraps first -> last.
    assert.equal(await clickFindNav(page, 'next'), true); // diag
    assert.equal(await clickFindNav(page, 'next'), true); // wrap -> exec
    state = await contract(page);
    assert.equal(state.counter, '1 of 4', 'next click wraps last -> first');
    assert.equal(state.selectedKey, 'node:b:exec');
    assert.equal(await clickFindNav(page, 'prev'), true);
    state = await contract(page);
    assert.equal(state.counter, '4 of 4', 'prev click wraps first -> last');
    assert.equal(state.selectedKey, 'node:b:diag');
    await clickFindNav(page, 'prev'); // exp
    await clickFindNav(page, 'prev'); // plan
    await clickFindNav(page, 'prev'); // exec
    state = await contract(page);
    assert.equal(state.counter, '1 of 4');
    assert.equal(state.selectedKey, 'node:b:exec');

    // Button navigation never replaces or rebuilds the chain: the row
    // ELEMENTS and their graph cells survive byte-for-byte.
    const dom = await page.evaluate(() => {
      const before = Array.from(document.querySelectorAll('.row'));
      const graphBefore = before.map((r) => r.querySelector('.graph-cell').innerHTML).join('\u0000');
      document.getElementById('search-next').click();
      const after = Array.from(document.querySelectorAll('.row'));
      return {
        sameEls: before.length === after.length && before.every((el, i) => el === after[i]),
        graphUnchanged: after.map((r) => r.querySelector('.graph-cell').innerHTML).join('\u0000') === graphBefore,
      };
    });
    assert.equal(dom.sameEls, true, 'button navigation never replaces/rebuilds the chain DOM');
    assert.equal(dom.graphUnchanged, true, 'graph cells survive button navigation byte-for-byte');

    // Disabled buttons swallow clicks: a click on a disabled button changes
    // nothing (also exercised for every non-settled state elsewhere).
    await page.evaluate(() => {
      const input = document.getElementById('search');
      input.value = 'thee';
      input.dispatchEvent(new Event('input', { bubbles: true }));
    });
    state = await contract(page);
    assert.equal(state.nextDisabled, true, 'edited text disables next');
    assert.equal(state.navVisible, false, 'edited text hides stale-result navigation');
    assert.equal(await clickFindNav(page, 'next'), false, 'a disabled next button swallows the click');
    state = await contract(page);
    assert.equal(state.counter, '2 of 4', 'no navigation happens while the button is disabled');

    assert.deepEqual(errors, [], 'no uncaught page errors: ' + errors.join(' | '));
  } finally {
    await page.close();
  }
});

test('a far/off-cache target is fetched around the backend row and scrolled to without loading the chain', async () => {
  const { page, errors } = await newPage();
  try {
    await bootScenario(page, 'multigroup'); // 600 git rows, no sub-ops
    await focusInput(page);
    await runSearch(page, 'multi-group op #599'); // exactly one match at abs 599
    let state = await contract(page);
    assert.equal(state.counter, '1 of 1');
    assert.equal(state.selectedRow, 599, 'the far row is selected by its absolute index');
    assert.equal(state.selectedKey, 'node:G0599', 'the real history row key is selected');
    assert.ok(state.scrollTop > 10000, 'the scroller jumps to the deep row (got ' + state.scrollTop + ')');
    assert.ok(state.rowCount < state.total, 'only a bounded window is rendered, never the full chain');
    assert.equal(state.hasPlaceholders, false, 'the target window is fully populated');
    assert.equal(state.focusedIsInput, true);

    // Sparse random access: every GetWindow is bounded by PAGE and the request
    // covering row 599 arrives without any full-chain scan.
    const log = await requestLog(page);
    const windows = log.map((s) => JSON.parse(s))
      .filter((b) => b.GetWindow !== undefined)
      .map((b) => b.GetWindow);
    assert.ok(windows.length > 0);
    for (const w of windows) {
      assert.ok(w.limit <= 500, 'every window fetch is bounded by PAGE');
      assert.ok(w.limit > 0);
    }
    assert.ok(windows.some((w) => w.offset <= 599 && w.offset + w.limit > 599),
      'a window request covers the backend-provided absolute row');

    // The selected row is actually on screen (revealed below the sticky header).
    const visible = await page.evaluate(() => {
      const rowsEl = document.getElementById('rows');
      const header = document.querySelector('.tbl-header').getBoundingClientRect();
      const rect = document.querySelector('.row[data-row="599"]').getBoundingClientRect();
      const viewport = rowsEl.getBoundingClientRect();
      return rect.top >= header.bottom - 0.5 && rect.bottom <= viewport.bottom + 0.5;
    });
    assert.equal(visible, true, 'the far match is revealed in the viewport');
    assert.equal(state.prevDisabled, false, 'a single settled match enables prev');
    assert.equal(state.nextDisabled, false, 'a single settled match enables next');
    assert.equal(state.navVisible, true, 'a single fetched match still exposes navigation');
    const settledScroll = state.scrollTop;

    // One-match click wraps in place: the counter, selection, and scroll
    // position stay put and the chain is never rebuilt.
    assert.equal(await clickFindNav(page, 'next'), true, 'next is clickable with one match');
    state = await contract(page);
    assert.equal(state.counter, '1 of 1', 'a one-match next click wraps in place');
    assert.equal(state.selectedRow, 599, 'the single match stays selected after the wrap');
    assert.equal(state.selectedKey, 'node:G0599');
    assert.equal(state.scrollTop, settledScroll, 'the wrap does not move the viewport');
    assert.equal(state.focusedIsInput, true, 'the click keeps focus in the search input');
    assert.equal(await clickFindNav(page, 'prev'), true, 'prev is clickable with one match');
    state = await contract(page);
    assert.equal(state.counter, '1 of 1', 'a one-match prev click wraps in place too');
    assert.equal(state.selectedRow, 599);

    assert.deepEqual(errors, [], 'no uncaught page errors: ' + errors.join(' | '));
  } finally {
    await page.close();
  }
});

test('absolute->visible mapping respects expansion state; expansion and profile survive a find', async () => {
  const { page, errors } = await newPage();
  try {
    await bootScenario(page, 'workUnitsDeep'); // blocks of 27 absolute slots (13 rows + 14 sub-ops)

    // 'Update main.css' (wu:chg) exists in every block; newest block wins.
    // All blocks collapsed by default: block 0's chg is at ABS 14, but 8
    // hidden sub-op slots precede it, so its VISIBLE index is 6.
    await runSearch(page, 'Update main.css');
    let state = await contract(page);
    assert.equal(state.counter, '1 of 50+', '96 matches are capped at 50 and the + is honest');
    assert.equal(state.selectedRow, 14, 'the selected row keeps its ABSOLUTE data-row');
    assert.equal(state.selectedKey, 'wud:0:wu:chg');
    const mapped = await page.evaluate(() => {
      const rowsEl = document.getElementById('rows');
      const header = document.querySelector('.tbl-header').getBoundingClientRect();
      const rect = document.querySelector('.row[data-row="14"]').getBoundingClientRect();
      const viewportTop = Math.floor(rowsEl.scrollTop / 34);
      return { viewportTop, revealed: rect.top >= header.bottom - 0.5 };
    });
    assert.ok(mapped.viewportTop <= 8,
      'scroll maps ABS 14 through the collapsed blocks to a near-top VISIBLE index (got viewportTop ' + mapped.viewportTop + ')');
    assert.equal(mapped.revealed, true);

    // Expand block 0's execute-run bundle (abs 2, sub-ops at 3..5).
    await page.evaluate(() => {
      const row = document.querySelector('.row[data-row="2"]');
      const chevron = row.querySelector('.subop-chevron');
      chevron.click();
    });
    await page.evaluate(() => window.__editchainDebug.whenIdle(20000));
    const expanded = await contract(page);
    assert.equal(expanded.rowKeys.includes('wud:0:wu:run1::sub:0'), true, 'expanded sub-ops render inline');
    assert.equal(expanded.rowKeys.includes('wud:0:wu:run1::sub:2'), true);
    // The chevron click is the existing disclosure control: it selects the
    // toggled row (normal history behaviour); the find state itself is
    // untouched and the next jump re-selects the match.

    // Escape clears the find session but keeps the expansion intact.
    await focusInput(page);
    await pressInput(page, 'Escape');
    await page.evaluate(() => window.__editchainDebug.whenIdle(20000));
    let cleared = await contract(page);
    assert.equal(cleared.counter, '', 'Escape clears the counter');
    assert.equal(cleared.selectedRow, null, 'Escape clears the highlight');
    assert.equal(cleared.rowKeys.includes('wud:0:wu:run1::sub:0'), true,
      'clearing the find never collapses an expanded block');

    // Re-search the same query: with run1 EXPANDED the visible index of ABS 14
    // is now 9 (fewer hidden slots precede it) — the mapping tracks expansion.
    await pressInput(page, 'Enter');
    await page.evaluate(() => window.__editchainDebug.whenIdle(20000));
    state = await contract(page);
    assert.equal(state.counter, '1 of 50+');
    assert.equal(state.selectedRow, 14);
    assert.equal(state.selectedKey, 'wud:0:wu:chg');
    assert.equal(state.rowKeys.includes('wud:0:wu:run1::sub:0'), true,
      'the find jump preserves the expanded block');
    assert.equal(state.rowKeys.includes('wud:0:wu:run1::sub:2'), true);
    const expandedMapped = await page.evaluate(() => {
      const rowsEl = document.getElementById('rows');
      const header = document.querySelector('.tbl-header').getBoundingClientRect();
      const rect = document.querySelector('.row[data-row="14"]').getBoundingClientRect();
      return {
        viewportTop: Math.floor(rowsEl.scrollTop / 34),
        revealed: rect.top >= header.bottom - 0.5,
        parentExpanded: document.querySelector('.row[data-row="2"]').getAttribute('aria-expanded'),
      };
    });
    assert.ok(expandedMapped.viewportTop <= 10,
      'the expanded-block mapping stays in sync (got viewportTop ' + expandedMapped.viewportTop + ')');
    assert.equal(expandedMapped.revealed, true);
    assert.equal(expandedMapped.parentExpanded, 'true', 'the bundle stays expanded after the jump');
    assert.equal(await page.evaluate(() => window.__editchainGetProfile()), 'activity',
      'the active profile is preserved through find');

    // A profile switch resets the view coherently: the find session clears and
    // the nav buttons must disappear with it, on every reset path.
    await page.evaluate(() => window.__editchainSetProfile('raw'));
    await page.evaluate(() => window.__editchainDebug.whenIdle(20000));
    let switched = await contract(page);
    assert.equal(switched.counter, '', 'a profile switch clears the find counter');
    assert.equal(switched.counterBusy, '', 'a profile switch clears the busy state');
    assert.equal(switched.prevDisabled, true, 'a profile switch disables prev');
    assert.equal(switched.nextDisabled, true, 'a profile switch disables next');
    assert.equal(switched.navVisible, false, 'a profile switch hides result navigation');
    await page.evaluate(() => window.__editchainSetProfile('activity'));
    await page.evaluate(() => window.__editchainDebug.whenIdle(20000));
    switched = await contract(page);
    assert.equal(switched.prevDisabled, true, 'the cleared find stays disabled after switching back');
    assert.equal(switched.nextDisabled, true);
    assert.equal(switched.navVisible, false);

    assert.deepEqual(errors, [], 'no uncaught page errors: ' + errors.join(' | '));
  } finally {
    await page.close();
  }
});

test('clearing the input or Escape clears find state without refetching or moving the scroll', async () => {
  const { page, errors } = await newPage();
  try {
    await bootScenario(page, 'multigroup');
    await runSearch(page, 'multi-group op #599');
    const settled = await contract(page);
    assert.equal(settled.selectedRow, 599);
    assert.equal(settled.nextDisabled, false, 'buttons are enabled while the find is settled');
    assert.equal(settled.navVisible, true);
    const logLenBefore = (await requestLog(page)).length;

    // Clear the input: find state goes away, the scroll and the rendered
    // window stay exactly where they are, and no request is issued.
    await page.evaluate(() => {
      const input = document.getElementById('search');
      input.value = '';
      input.dispatchEvent(new Event('input', { bubbles: true }));
    });
    await page.evaluate(() => window.__editchainDebug.whenIdle(20000));
    let cleared = await contract(page);
    assert.equal(cleared.counter, '', 'clearing the input clears the counter');
    assert.equal(cleared.counterBusy, '', 'clearing the input clears the busy state');
    assert.equal(cleared.selectedRow, null, 'clearing the input clears the highlight');
    assert.equal(cleared.scrollTop, settled.scrollTop, 'the resulting scroll position is preserved');
    assert.equal(cleared.rowCount, settled.rowCount, 'the rendered window is untouched');
    assert.equal(cleared.prevDisabled, true, 'clearing the input disables prev');
    assert.equal(cleared.nextDisabled, true, 'clearing the input disables next');
    assert.equal(cleared.navVisible, false, 'clearing the input hides result navigation');
    assert.equal((await requestLog(page)).length, logLenBefore,
      'clearing the input issues no request (no reload/reset/refetch)');

    // Re-search (Enter re-submits the same text), then Escape: same contract,
    // but Escape KEEPS the typed text for easy editing.
    await runSearch(page, 'multi-group op #599');
    const researched = await contract(page);
    assert.equal(researched.selectedRow, 599);
    const logLenResearched = (await requestLog(page)).length;
    await pressInput(page, 'Escape');
    await page.evaluate(() => window.__editchainDebug.whenIdle(20000));
    cleared = await contract(page);
    assert.equal(cleared.counter, '', 'Escape clears the counter');
    assert.equal(cleared.counterBusy, '', 'Escape clears the busy state');
    assert.equal(cleared.selectedRow, null, 'Escape clears the highlight');
    assert.equal(cleared.scrollTop, researched.scrollTop, 'Escape preserves the scroll position');
    assert.equal(cleared.inputValue, 'multi-group op #599', 'Escape keeps the typed query');
    assert.equal(cleared.rowCount, researched.rowCount, 'Escape leaves the rendered window untouched');
    assert.equal(cleared.prevDisabled, true, 'Escape disables prev');
    assert.equal(cleared.nextDisabled, true, 'Escape disables next');
    assert.equal(cleared.navVisible, false, 'Escape hides result navigation');
    assert.equal((await requestLog(page)).length, logLenResearched,
      'Escape issues no request (no reload/reset/refetch)');

    assert.deepEqual(errors, [], 'no uncaught page errors: ' + errors.join(' | '));
  } finally {
    await page.close();
  }
});

test('edited-but-unsubmitted text and in-flight replacements never navigate stale matches', async () => {
  const { page, errors } = await newPage();
  try {
    await bootScenario(page, 'badges');
    await runSearch(page, 'the'); // 4 hits
    let state = await contract(page);
    assert.equal(state.counter, '1 of 4');
    assert.equal(state.nextDisabled, false, 'buttons enable on the settled query');
    assert.equal(state.navVisible, true);

    // Edit the input without submitting: the rendered matches belong to the
    // OLD query, so arrows must not enter them.
    await page.evaluate(() => {
      const input = document.getElementById('search');
      input.value = 'thee';
      input.focus();
      input.dispatchEvent(new Event('input', { bubbles: true }));
    });
    await pressInput(page, 'ArrowDown');
    state = await contract(page);
    assert.equal(state.focusedIsInput, true, 'ArrowDown stays in the input');
    assert.equal(state.selectedKey, 'node:b:exec', 'the stale highlight is never navigated');
    assert.equal(state.counter, '1 of 4', 'the old counter is not advanced');
    assert.equal(state.prevDisabled, true, 'edited-but-unsubmitted text disables prev');
    assert.equal(state.nextDisabled, true, 'edited-but-unsubmitted text disables next');
    assert.equal(state.navVisible, false, 'edited text hides arrows for the stale result set');
    await pressInput(page, 'ArrowUp');
    state = await contract(page);
    assert.equal(state.selectedKey, 'node:b:exec', 'ArrowUp also stays put');

    // Retyping the exact submitted query restores the buttons (the arrows
    // would navigate again too — same guard, same state).
    await page.evaluate(() => {
      const input = document.getElementById('search');
      input.value = 'the';
      input.dispatchEvent(new Event('input', { bubbles: true }));
    });
    state = await contract(page);
    assert.equal(state.nextDisabled, false, 'retyping the exact query re-enables the buttons');
    assert.equal(state.navVisible, true, 'retyping the matching query reveals its fetched-result arrows');
    // Back to the edited text so Enter below submits a NEW query (the same
    // original flow: 'thee' settles its own zero-result search).
    await page.evaluate(() => {
      const input = document.getElementById('search');
      input.value = 'thee';
      input.dispatchEvent(new Event('input', { bubbles: true }));
    });

    // Submitting the edited text settles ITS OWN results.
    await pressInput(page, 'Enter');
    await page.evaluate(() => window.__editchainDebug.whenIdle(20000));
    state = await contract(page);
    assert.equal(state.counter, '0 of 0', 'thee has no matches');
    assert.equal(state.selectedRow, null);
    assert.equal(state.rowCount, 11, 'zero-result search keeps the chain DOM');
    assert.equal(state.prevDisabled, true, 'zero results disable prev');
    assert.equal(state.nextDisabled, true, 'zero results disable next');
    assert.equal(state.navVisible, false, 'zero results expose no arrow affordances');

    // An in-flight replacement holds the arrows in the input (no stale match
    // can be entered while the new query is pending).
    await runSearch(page, 'the');
    state = await contract(page);
    assert.equal(state.counter, '1 of 4');
    const held = await page.evaluate(() => {
      const input = document.getElementById('search');
      input.value = 'test';
      window.__editchainHoldFind = {};
      input.dispatchEvent(new KeyboardEvent('keydown', { key: 'Enter', bubbles: true }));
      const hold = window.__editchainHoldFind;
      const ok = !!(hold && hold.taken && typeof hold.release === 'function');
      window.__editchainHeldFindRelease = ok ? hold.release : null;
      return ok;
    });
    assert.equal(held, true, 'the replacement find response is held');
    state = await contract(page);
    assert.equal(state.counter, '', 'pending shows no visible text');
    assert.equal(state.counterClass.includes('search-counter-pending'), true,
      'pending exposes the spinner state class');
    assert.equal(state.counterBusy, 'true', 'pending marks the counter busy');
    assert.equal(state.counterLabel, 'Searching…', 'pending keeps the accessible label');
    assert.equal(state.counterTitle, '', 'pending carries no tooltip');
    assert.equal(state.selectedRow, null, 'the old highlight is cleared while the new search is pending');
    assert.equal(state.prevDisabled, true, 'a pending replacement disables prev');
    assert.equal(state.nextDisabled, true, 'a pending replacement disables next');
    assert.equal(state.navVisible, false, 'pending results hide both arrows');
    let spinner = await pendingSpinner(page);
    assert.ok(spinner && spinner.content !== 'none', 'the pending spinner pseudo-element exists');
    assert.notEqual(spinner.display, 'none', 'the pending spinner is displayed');
    assert.ok(parseFloat(spinner.width) > 0 && parseFloat(spinner.height) > 0,
      'the pending spinner has visible size');
    assert.notEqual(spinner.borderTopWidth, '0px', 'the pending spinner draws its ring');
    assert.notEqual(spinner.borderRadius, '0px', 'the pending spinner ring is round');
    assert.equal(spinner.animationName, 'ec-search-spin', 'the pending spinner animates under normal motion');
    assert.notEqual(spinner.animationDuration, '0s', 'the pending spinner has a real rotation period');
    assert.equal(spinner.animationIterationCount, 'infinite', 'the pending spinner loops');
    // Reduced motion: the ring stays visible but never rotates.
    await page.emulateMediaFeatures([{ name: 'prefers-reduced-motion', value: 'reduce' }]);
    spinner = await pendingSpinner(page);
    assert.equal(spinner.animationName, 'none', 'reduced motion disables the spinner animation');
    assert.ok(parseFloat(spinner.width) > 0 && parseFloat(spinner.height) > 0,
      'reduced motion keeps the ring visible');
    assert.notEqual(spinner.borderTopColor, 'rgba(0, 0, 0, 0)',
      'reduced motion closes the ring into a full visible glyph');
    await page.emulateMediaFeatures([{ name: 'prefers-reduced-motion', value: 'no-preference' }]);
    spinner = await pendingSpinner(page);
    assert.equal(spinner.animationName, 'ec-search-spin', 'normal motion restores the spinner animation');
    await focusInput(page);
    await pressInput(page, 'ArrowDown');
    state = await contract(page);
    assert.equal(state.selectedRow, null, 'an in-flight replacement never navigates stale matches');
    await pressInput(page, 'ArrowUp');
    state = await contract(page);
    assert.equal(state.selectedRow, null);

    // Release: the replacement settles and its own results navigate normally.
    await page.evaluate(() => {
      window.__editchainHeldFindRelease();
      window.__editchainHeldFindRelease = null;
    });
    await page.evaluate(() => window.__editchainDebug.whenIdle(20000));
    state = await contract(page);
    assert.equal(state.counter, '1 of 2', 'the settled replacement reports its own count');
    assert.equal(state.counterClass.includes('search-counter-pending'), false,
      'settled results clear the pending spinner class');
    assert.equal(state.counterBusy, '', 'settled results clear the busy state');
    assert.equal(state.counterLabel, '', 'settled results clear the pending label');
    spinner = await pendingSpinner(page);
    assert.equal(spinner.content, 'none', 'settled results remove the spinner pseudo-element');
    assert.equal(state.selectedKey, 'node:b:exec');
    assert.equal(state.nextDisabled, false, 'the settled replacement re-enables the buttons');
    assert.equal(state.navVisible, true, 'the arrows return only after replacement results settle');
    await pressInput(page, 'ArrowDown');
    state = await contract(page);
    assert.equal(state.counter, '2 of 2', 'the replacement navigates its own matches');
    assert.equal(state.selectedKey, 'node:b:ver');

    assert.deepEqual(errors, [], 'no uncaught page errors: ' + errors.join(' | '));
  } finally {
    await page.close();
  }
});

test('zero-result and error searches stay compact and never replace the chain', async () => {
  const { page, errors } = await newPage();
  try {
    await bootScenario(page, 'badges');
    const before = await contract(page);

    // Zero results: compact "0 of 0", no highlight, chain untouched.
    await runSearch(page, 'zzz-no-match');
    let state = await contract(page);
    assert.equal(state.counter, '0 of 0');
    assert.equal(state.counterBusy, '', 'zero results clear the busy state');
    assert.equal(state.counterLabel, '', 'zero results clear the pending label');
    assert.equal(state.counterClass.includes('search-counter-pending'), false,
      'zero results clear the spinner class');
    assert.equal(state.selectedRow, null);
    assert.equal(state.findCurrentRow, null);
    assert.deepEqual(state.rowKeys, before.rowKeys, 'the chain rows are unchanged');
    assert.equal(state.rowCount, 11);
    await focusInput(page);
    await pressInput(page, 'ArrowDown');
    state = await contract(page);
    assert.equal(state.focusedIsInput, true, 'arrows stay in the input on zero results');
    assert.equal(state.selectedRow, null);
    assert.equal(state.prevDisabled, true, 'zero results disable prev');
    assert.equal(state.nextDisabled, true, 'zero results disable next');
    assert.equal(state.navVisible, false, 'zero results keep both arrows hidden');

    // Error: compact "error" in the counter, no full-pane replacement.
    await page.evaluate(() => { window.__editchainFindError = 'index unavailable'; });
    await runSearch(page, 'the');
    state = await contract(page);
    assert.equal(state.counter, 'error', 'error state is compact');
    assert.equal(state.counterBusy, '', 'an error clears the busy state');
    assert.equal(state.counterLabel, 'Find failed: index unavailable', 'the reason stays accessible');
    assert.equal(state.counterTitle, 'index unavailable', 'the reason rides as a tooltip');
    assert.equal(state.selectedRow, null, 'no stale highlight after an error');
    assert.deepEqual(state.rowKeys, before.rowKeys, 'an error never replaces the chain DOM');
    await focusInput(page);
    await pressInput(page, 'ArrowDown');
    state = await contract(page);
    assert.equal(state.focusedIsInput, true, 'arrows stay in the input after an error');
    assert.equal(state.prevDisabled, true, 'an error disables prev');
    assert.equal(state.nextDisabled, true, 'an error disables next');
    assert.equal(state.navVisible, false, 'an error keeps both arrows hidden');

    // Recovery: clearing the error and pressing Enter re-searches.
    await page.evaluate(() => { window.__editchainFindError = null; });
    await pressInput(page, 'Enter');
    await page.evaluate(() => window.__editchainDebug.whenIdle(20000));
    state = await contract(page);
    assert.equal(state.counter, '1 of 4', 'Enter recovers after an error');
    assert.equal(state.selectedKey, 'node:b:exec');
    assert.equal(state.nextDisabled, false, 'recovery re-enables the buttons');
    assert.equal(state.navVisible, true, 'arrows reappear after recovery results settle');

    assert.deepEqual(errors, [], 'no uncaught page errors: ' + errors.join(' | '));
  } finally {
    await page.close();
  }
});

test('Enter on a focused row (not the input) still opens its raw JSON editor', async () => {
  const { page, errors } = await newPage();
  try {
    await bootScenario(page, 'badges');
    await runSearch(page, 'the');

    const enter = await page.evaluate(() => {
      const captured = [];
      const origPost = window.vscode.postMessage;
      window.vscode.postMessage = function (msg) {
        if (msg && msg.type === 'openJson') captured.push(msg);
        return origPost(msg);
      };
      // Focus a real history row and press Enter: raw JSON, not navigation.
      const row = document.querySelector('.row[data-row="0"]');
      row.focus();
      row.dispatchEvent(new KeyboardEvent('keydown', { key: 'Enter', bubbles: true }));
      const selectedKey = document.querySelector('.row.row-selected').getAttribute('data-key');
      const secondaryPane = !!document.getElementById('detail') ||
        document.getElementById('layout').classList.contains('has-detail');
      window.vscode.postMessage = origPost;
      return { captured: captured.length, selectedKey, secondaryPane };
    });
    assert.equal(enter.captured, 1, 'Enter on a focused row opens its raw JSON editor');
    assert.equal(enter.selectedKey, 'node:b:exec', 'the selected row is the real key');
    assert.equal(enter.secondaryPane, false, 'activation never splits the history surface');

    // Double-click also opens raw JSON.
    const dbl = await page.evaluate(() => {
      const captured = [];
      const origPost = window.vscode.postMessage;
      window.vscode.postMessage = function (msg) {
        if (msg && msg.type === 'openJson') captured.push(msg);
        return origPost(msg);
      };
      const row = document.querySelector('.row[data-row="5"]');
      row.dispatchEvent(new MouseEvent('dblclick', { bubbles: true }));
      window.vscode.postMessage = origPost;
      return captured.map((m) => m.op_id);
    });
    assert.deepEqual(dbl, ['node:b:ver'], 'double-click opens the right raw record');

    // Find navigation from the INPUT never opens raw JSON.
    await focusInput(page);
    await pressInput(page, 'ArrowDown');
    const nav = await page.evaluate(() => {
      let openJson = 0;
      const origPost = window.vscode.postMessage;
      window.vscode.postMessage = function (msg) {
        if (msg && msg.type === 'openJson') openJson++;
        return origPost(msg);
      };
      window.vscode.postMessage = origPost;
      return openJson;
    });
    assert.equal(nav, 0, 'arrow navigation from the input never opens raw JSON');

    assert.deepEqual(errors, [], 'no uncaught page errors: ' + errors.join(' | '));
  } finally {
    await page.close();
  }
});

test('the active match highlight survives a virtualization rebuild', async () => {
  const { page, errors } = await newPage();
  try {
    await bootScenario(page, 'badges');
    await runSearch(page, 'the');
    await focusInput(page);
    await pressInput(page, 'ArrowDown'); // match 2 of 4: plan at row 3
    let state = await contract(page);
    assert.equal(state.counter, '2 of 4');
    assert.equal(state.selectedRow, 3);
    assert.equal(state.selectedKey, 'node:b:plan');

    // Dispatch a resize; onViewportResize rebuilds the row DOM after its
    // debounce. Poll deterministically for the rebuild: a NEW selected node,
    // re-applied by the same key, with the find-current marker intact.
    const rebuilt = await page.evaluate(() => {
      const oldSelected = document.querySelector('.row.row-selected');
      const oldKey = oldSelected.getAttribute('data-key');
      window.dispatchEvent(new Event('resize'));
      return new Promise((resolve, reject) => {
        const deadline = Date.now() + 10000;
        const check = () => {
          const selected = document.querySelector('.row.row-selected');
          const findCurrent = document.querySelector('.row.row-find-current');
          const counter = document.getElementById('search-counter');
          if (selected && selected !== oldSelected &&
              selected.getAttribute('data-key') === oldKey &&
              findCurrent && findCurrent === selected) {
            resolve({
              selectedRow: Number(selected.getAttribute('data-row')),
              selectedKey: selected.getAttribute('data-key'),
              findCurrentRow: Number(findCurrent.getAttribute('data-row')),
              counter: (counter.textContent || '').trim(),
              focusedIsInput: document.activeElement === document.getElementById('search'),
            });
          } else if (Date.now() > deadline) {
            reject(new Error('highlight rebuild did not settle within 10s'));
          } else {
            setTimeout(check, 25);
          }
        };
        check();
      });
    });
    assert.equal(rebuilt.selectedRow, 3, 'selection survives the rebuild by key');
    assert.equal(rebuilt.selectedKey, 'node:b:plan');
    assert.equal(rebuilt.findCurrentRow, 3, 'the current-match marker survives the rebuild');
    assert.equal(rebuilt.counter, '2 of 4', 'the counter survives the rebuild');
    assert.equal(rebuilt.focusedIsInput, true, 'focus stays in the input through the rebuild');

    assert.deepEqual(errors, [], 'no uncaught page errors: ' + errors.join(' | '));
  } finally {
    await page.close();
  }
});

test('legacy Search responses still render the flat result list (compatibility path)', async () => {
  const { page, errors } = await newPage();
  try {
    await bootScenario(page, 'badges');

    // Force the renderer's FindInHistory request down the LEGACY Search path
    // (as an older service would answer): the flat result list must still
    // render and navigate, keeping the old compatibility contract alive.
    await page.evaluate(() => {
      const origPost = window.vscode.postMessage.bind(window.vscode);
      window.__editchainLegacySearchPost = function (msg) {
        if (msg && msg.body && msg.body.FindInHistory) {
          const f = msg.body.FindInHistory;
          msg.body = { Search: { query: f.query, mode: 'Lexical', top_k: f.top_k, filters: f.filters || {} } };
        }
        return origPost(msg);
      };
      window.vscode.postMessage = window.__editchainLegacySearchPost;
    });
    await runSearch(page, 'the');
    const legacy = await contract(page);
    assert.equal(legacy.counter, '', 'the legacy flat list hides the find counter');
    assert.equal(legacy.prevDisabled, true, 'the legacy flat list keeps prev disabled (no find session)');
    assert.equal(legacy.nextDisabled, true, 'the legacy flat list keeps next disabled (no find session)');
    assert.equal(legacy.navVisible, false, 'legacy flat results do not expose find-in-chain arrows');
    assert.equal(legacy.rowCount, 4, 'legacy Search renders its own flat result list');
    assert.deepEqual(legacy.rowKeys, ['node:b:exec', 'node:b:plan', 'node:b:exp', 'node:b:diag'],
      'legacy results navigate with the real op ids');
    const banner = await page.evaluate(() => {
      const el = document.querySelector('.search-banner');
      return el ? (el.textContent || '').trim() : null;
    });
    assert.match(banner || '', /4 results? for "the"/, 'the legacy banner is intact');

    // Clearing the input restores the full history from the legacy flat list.
    await page.evaluate(() => {
      window.vscode.postMessage = window.__editchainLegacySearchPost;
      const input = document.getElementById('search');
      input.value = '';
      input.dispatchEvent(new Event('input', { bubbles: true }));
    });
    await page.evaluate(() => window.__editchainDebug.whenIdle(20000));
    const restored = await contract(page);
    assert.equal(restored.rowCount, 11, 'clearing the legacy flat list restores the full history');
    assert.equal(restored.rowKeys[0], 'node:b:exec');
    assert.equal(restored.prevDisabled, true, 'the restored history has no find session to navigate');
    assert.equal(restored.nextDisabled, true);
    assert.equal(restored.navVisible, false);

    assert.deepEqual(errors, [], 'no uncaught page errors: ' + errors.join(' | '));
  } finally {
    await page.close();
  }
});
