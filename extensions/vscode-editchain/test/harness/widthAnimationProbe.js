'use strict';

/** Observe real CSS transitions on the production renderer. Pause only in the
 * test so intermediate frames and interrupted transitions can be inspected. */
async function installWidthProbe(page) {
  await page.evaluate(() => {
    const root = document.getElementById('rows');
    const probe = window.__widthProbe = { animations: [], transitions: 0 };
    probe.read = () => {
      const rect = selector => document.querySelector(selector).getBoundingClientRect();
      const header = rect('.tbl-header .th.graph');
      const rows = [...document.querySelectorAll('.row:not(.row-placeholder)')];
      const columns = ['graph', 'activity', 'tags', 'content', 'date'];
      const handles = [...document.querySelectorAll('.col-resize-handle')];
      const dividers = handles.map((handle, index) => {
        // The next track's start remains authoritative when a very narrow
        // content header's padding is wider than its available grid track.
        const boundary = index + 1 < handles.length
          ? rect('.tbl-header .th.' + handles[index + 1].dataset.col).left
          : rect('.tbl-header').right;
        const box = handle.getBoundingClientRect();
        return { col: handle.dataset.col, error: Math.abs(box.right - boundary -
          (index + 1 === handles.length ? 0 : 3)) };
      });
      return {
        graph: header.width, table: rect('.tbl-header').width, wrap: rect('.table-wrap').width,
        target: parseFloat(root.style.getPropertyValue('--graph-display-width')),
        minimum: parseFloat(root.style.getPropertyValue('--table-display-width')),
        viewport: root.clientWidth, dividers,
        rowWidths: rows.map(row => row.querySelector('.graph-cell').getBoundingClientRect().width),
        columnErrors: rows.flatMap(row => columns.map(col => {
          const headerCell = document.querySelector('.tbl-header .th.' + col);
          const cell = row.querySelector(col === 'content' ? '.text-cell' : '.' + col + '-cell');
          if (!cell || getComputedStyle(headerCell).display === 'none') return 0;
          return Math.abs(headerCell.getBoundingClientRect().left - cell.getBoundingClientRect().left);
        })),
        dotX: rows.flatMap(row => [...row.querySelectorAll('.graphDot')].map(dot => dot.getAttribute('cx'))),
        renderCount: window.__editchainRendererDebug.metrics().renderCount,
        transitions: probe.transitions,
        timing: probe.animations.map(animation => ({ property: animation.transitionProperty,
          duration: animation.effect.getTiming().duration, easing: animation.effect.getTiming().easing })),
      };
    };
    probe.at = time => {
      for (const animation of probe.animations) animation.currentTime = time;
      return probe.read();
    };
    probe.finish = () => {
      for (const animation of root.getAnimations()) animation.finish();
      return probe.read();
    };
    new MutationObserver(() => {
      const animations = root.getAnimations();
      if (!animations.length || animations.every(animation => probe.animations.includes(animation))) return;
      probe.animations = animations;
      probe.transitions++;
      for (const animation of animations) {
        animation.pause();
        animation.currentTime = 0;
      }
    }).observe(root, { attributes: true, attributeFilter: ['style', 'data-animate-width'] });
  });
}

module.exports = { installWidthProbe };
