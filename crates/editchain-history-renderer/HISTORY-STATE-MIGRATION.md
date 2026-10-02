# Semantic history state

The shared Crux history model is implemented in the sibling `app-core` repository,
under `crates/app-core/src/history/`. It owns logical selection, filters, literal
search, disclosure, candidate paging, observation/evidence caches and content
replay. New graph/detail clients consume its typed views and send history events.
It consumes engine queries and the core `Operation::view` / `StreamState` adapters.

The existing viewer remains runnable while its graph/detail consumers migrate:

| Existing source | Current ownership and cleanup |
| --- | --- |
| `app/selection.rs` | Thin adapter over `idle_history::Selection`; only the roving row location remains here. Remove with f30. |
| `editchain-protocol::content` | Compatibility re-exports of `idle-history` text/content models. Remove the old protocol names with f31. |
| `editchain-node::history::details` | Calls `idle-history` legacy preview functions through protocol compatibility exports. Exact new evidence uses engine queries. Retire with f31/f40. |
| `app/find.rs`, `cache.rs`, `expansion.rs`, `state.rs` | Legacy coordinate/viewport adapters needed by this renderer. New semantic behavior is in Crux. Replace consumers with f30/f31, then remove these adapters. |
| `app/requests.rs`, `remote.rs`, `delta.rs`, `expansion/live.rs` | Thin adapters over app-core's `idle-history` request tracker and revision validator. Row coordinates, conditional-window validation and DOM plans stay here until f30/f31. |
| `../editchain-client-state/src/lib.rs` | f43 moved the temporary Node/WASM bridge out of this renderer. The existing peer host loads its independent bundle over app-core's join/connection state. Retire when f18 switches that consumer. |
| Node live disclosure/search/window endpoints | Existing coordinate-based viewer protocol remains available until f30/f31 switch its consumers. |

Graph layout, pixel virtualization, scrolling, focus and DOM/SVG rendering are not
in app-core. They remain here until their web-ui migration. The engine keeps
storage, immutable schema, replay and deterministic evidence queries; the new
`ChainQueries::contents` accessor shares the engine's existing field inventory
with complete evidence readers.

Check out `idleai/app-core` beside `editchain`. The portable `idle-history` package
does not depend on Crux or the history renderer. Engine crates have no dependency
on it. CI checks out both repositories; these paired source changes must be
available together before builds against the default sibling branches succeed.
