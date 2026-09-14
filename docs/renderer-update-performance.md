# Renderer update measurements

The native renderer now retains decoded content across revisions, resolves
anchors and a conditional window in one request, and mounts a small viewport
margin independently of its data cache. Host responses commit one final row
window per animation frame. Native control messages start requests immediately;
input feedback does not wait for the next frame or for native publication.

## Local comparison

Measured on September 14, 2026 with Chrome 151.0.7922.71, a 1440 × 900 viewport,
and 2,000 deterministic rows. Both runs load the production Rust/WASM renderer
and CSS. The baseline assets were saved before the conditional-window and
frame-batching refactor, after the graph-width and animation-duration fixes.

The fixture supplies synchronous native-shaped responses and a simple flat
graph. This isolates frontend work; it does **not** measure the real native
service, checkpoint writes, provider ingestion, or VS Code IPC. Each scenario
contains overlapping updates. Finite CSS animations finish between scenarios
so their remaining work is not charged to the following scenario.

| Work measured | Before | After |
| --- | ---: | ---: |
| Mounted DOM rows, including placeholders | 425 | 41 |
| Requests for a burst of 20 revision messages | 21 | 1 |
| Full row payloads for that burst | 25 | 1 |
| Visible-row DOM mutations for that burst | 17,470 | 45 |
| Full row payloads for 20 offscreen updates | 500 | 0 |
| Visible-row DOM mutations for those offscreen updates | 25,500 | 0 |
| Requests for 20 animated prepends | 40 | 20 |
| Full row payloads for those prepends | 500 | 20 |
| Requests for 10 group toggles | 20 | 10 |
| Full row payloads for those toggles | 250 | 10 |

Single-run timing observations, rounded to milliseconds:

| Scenario | Browser task time, before → after | Update-to-DOM acknowledgement p95, before → after |
| --- | ---: | ---: |
| 20-message burst | 106 → 32 ms | 103 → 29 ms |
| 20 sequential offscreen updates | 338 → 124 ms | 18 → 19 ms |
| 20 animated prepends | 501 → 436 ms | 33 → 26 ms |
| 10 group toggles | 221 → 172 ms | 25 → 22 ms |

These timings are observations, not latency guarantees. Frame scheduling still
sets a floor for an isolated update: offscreen updates use far less CPU but
retain roughly the same acknowledgement latency. The new window also fills its
mounted margin; the old native update fetched only the immediate viewport and
left many mounted nodes as placeholders. Row-rectangle reads therefore need not
decrease even though DOM size and mutations do. The strongest reproducible
results are the bounded work counts and retained node/content identities.

Native reconciliation still materializes, decorates and fingerprints its bounded
window. Native capture and checkpointing remain synchronous with the active
publication. Interactive jobs take priority over queued background jobs but
cannot interrupt a transaction already running. Measuring those remaining
costs requires the service profiler and an actual VS Code session.

## Reproduce

From `extensions/vscode-editchain`:

```sh
npm run build:renderer
npm run perf:renderer
```

To compare another renderer, pass a directory containing its
`editchain_history_renderer.js`, `editchain_history_renderer_bg.wasm`, and
`main.css`:

```sh
npm run perf:renderer -- /path/to/before-assets
```

The harness intercepts only those assets and prints JSON with request counts,
content reuse, DOM mutations, rectangle reads, Chrome performance counters,
and acknowledgement latency. It verifies every scenario reaches its final
revision. `CHROME_PATH` selects another Chrome binary.

Browser regressions separately check graph-width stability, row and content
reuse, stale response suppression, unchanged SVG animation clocks, reduced
motion, and six rapid prepends whose retargeting starts within 1.5 pixels of the
current visual position. Delayed-window tests require both disclosure clicks to
survive retired coordinates and pending feedback to clear after completion.
