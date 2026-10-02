# History graph migration (f30)

The sibling web-ui repository now owns the reusable graph implementation:

| Old source | Replacement |
| --- | --- |
| `editchain-project/src/layout.rs` | Compatibility export of `history-geometry::layout` |
| `editchain-protocol/src/live_graph/` | `history-geometry::live`; the protocol file implements its generic adapters for existing wire rows |
| `app/graph_growth.rs` | Compatibility export of `history-geometry::motion` |
| `app/coordinates.rs` | Compatibility export of `history-geometry::viewport` |

Routing and growth regression tests moved with their implementation. Existing
project layout tests still exercise the compatibility export. The protocol adapter
retains its serialized graph structure and existing row-parent behavior, including
detail slots whose parent metadata must remain untouched.

New browser and extension compositions mount
`web_ui::history::graph::HistoryGraph`. It consumes app-core's F23 view directly,
retains all observation/record references, distinguishes causal parents from
logical causes and Links, and owns SVG, virtualization, focus and scroll anchors.
It does not call this renderer's coordinate-based paging protocol.

The old viewer remains runnable for its existing consumers. Its direct DOM code,
row presentation, row-focus adapter, disclosure/cache coordinates, frame batching
and host/service wiring are compatibility code, not the implementation for new
surfaces. f31 replaces rich row/content presentation; f43 replaces the legacy
extension mount and removes the remaining viewer and geometry adapters. f60 owns
the browser's live host connection. These consumers must switch before removing
their old endpoints.

The f43 Idle composition now runs through app-core and web-ui without loading
this renderer. Its native history queries use the engine facade directly. The
legacy Node peer adapter now builds from `editchain-client-state`, so it carries
no viewer code. The old extension's live importer and standalone peer flows still
run until their f10/f18 switches; the [assembly handoff](../../extensions/vscode-editchain/ASSEMBLY-MIGRATION.md)
records the remaining source retirement conditions.

Check out `idleai/web-ui` beside `editchain` and `app-core`. Land the web-ui package
before the paired source imports. Both EditChain workflows now include this sibling
checkout. The viewer build remaps the sibling root so generated WASM assets do not
depend on a developer's checkout location.
